use std::collections::HashSet;
use std::time::Duration;

use switchboard_remote_crypto::frame::MAX_FRAME_LEN;
use tokio::sync::Semaphore;

use crate::outbox::frame_cost;
use crate::protocol::{DeviceId, InvalidDeviceId, longest_client_message};
use crate::server::HTTP_HEADROOM;

/// The most phones a Mac's set may hold, whatever `MAX_DEVICES_PER_MAC` says.
const MAX_DEVICES_PER_MAC_CAP: usize = 1024;
/// The longest message any setting may admit: well above the longest a
/// client sends, and a bound on what one read may buffer.
const MAX_FRAME_BYTES_CAP: usize = 1024 * 1024;

/// The relay's limits and settings. `from_env` reads the deployment's
/// environment; the timeouts and the queue budget are fixed there and
/// settable only in code, for tests.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    /// Messages per second a connection may send, sustained; a connection
    /// may also burst up to this many at once. Pings and pongs count. A
    /// connection over it is not refused: the relay stops reading from it
    /// until it is back under, which slows the sender through TCP.
    pub rate_limit: u32,
    /// Bytes per second a connection may send, enforced the same way. At
    /// least `max_frame_bytes`, so one message never waits longer than a
    /// second, and a burst may always reach one message of that size.
    pub rate_limit_bytes: u32,
    /// The longest WebSocket message accepted. The default is above a frame
    /// carrying the longest record: 65,568 bytes, about 87,500 as base64.
    pub max_frame_bytes: usize,
    /// At most 1024.
    pub max_devices_per_mac: usize,
    /// WebSocket connections, registered or not. A full relay answers an
    /// upgrade with HTTP 503. A few sockets more are accepted for plain HTTP,
    /// each closed once it idles for `register_timeout`.
    pub max_connections: usize,
    pub deny_device_ids: HashSet<DeviceId>,
    /// A connection that sends nothing, pings and pongs included, for this
    /// long is closed. The Mac pings every 25 seconds, and both clients
    /// answer the relay's pings.
    pub idle_timeout: Duration,
    /// How often the relay pings each connection. Below half the 50 seconds
    /// of silence after which the Mac reconnects, so one late ping does not
    /// make it give up on a working relay.
    pub ping_interval: Duration,
    /// How long a new connection has to answer the challenge.
    pub register_timeout: Duration,
    /// Bytes of frames queued for one connection at most. A frame for a
    /// connection with no room left is refused as `recipient_full`, and the
    /// connection closed.
    pub queue_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8080,
            rate_limit: 256,
            max_frame_bytes: 131_072,
            max_devices_per_mac: 8,
            max_connections: 64,
            deny_device_ids: HashSet::new(),
            idle_timeout: Duration::from_secs(90),
            ping_interval: Duration::from_secs(25),
            register_timeout: Duration::from_secs(10),
            rate_limit_bytes: 16 * 1024 * 1024,
            queue_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{name} must be a positive integer, not {value:?}")]
    NotAPositiveInteger { name: &'static str, value: String },
    #[error("DENY_DEVICE_IDS: {0}")]
    DenyList(#[from] InvalidDeviceId),
    #[error("{0}")]
    Inconsistent(&'static str),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// `from_env` over any lookup, so tests need not set process variables.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        let deny_device_ids = var("DENY_DEVICE_IDS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(DeviceId::parse)
            .collect::<Result<_, _>>()?;
        Self {
            port: positive(&var, "PORT", defaults.port)?,
            rate_limit: positive(&var, "RATE_LIMIT", defaults.rate_limit)?,
            max_frame_bytes: positive(&var, "MAX_FRAME_BYTES", defaults.max_frame_bytes)?,
            max_devices_per_mac: positive(
                &var,
                "MAX_DEVICES_PER_MAC",
                defaults.max_devices_per_mac,
            )?,
            max_connections: positive(&var, "MAX_CONNECTIONS", defaults.max_connections)?,
            rate_limit_bytes: positive(&var, "RATE_LIMIT_BYTES", defaults.rate_limit_bytes)?,
            deny_device_ids,
            ..defaults
        }
        .validated()
    }

    /// Refuses settings the relay cannot work under. The caps come first,
    /// so nothing after them sizes anything by an unbounded setting. A
    /// message limit below the longest message a client sends would close
    /// any connection that sends it, and a byte rate below it would hold
    /// such a message for longer than a second. A queue budget smaller than
    /// one frame of the longest record could never admit that frame, and
    /// its recipient would be closed by the first one it was sent.
    pub fn validated(self) -> Result<Self, ConfigError> {
        if self.max_devices_per_mac > MAX_DEVICES_PER_MAC_CAP {
            return Err(ConfigError::Inconsistent(
                "MAX_DEVICES_PER_MAC may be at most 1024",
            ));
        }
        if self.max_frame_bytes > MAX_FRAME_BYTES_CAP {
            return Err(ConfigError::Inconsistent(
                "MAX_FRAME_BYTES may be at most 1 MiB",
            ));
        }
        if self
            .max_connections
            .checked_add(HTTP_HEADROOM)
            .is_none_or(|sockets| sockets > Semaphore::MAX_PERMITS)
        {
            return Err(ConfigError::Inconsistent("MAX_CONNECTIONS is too large"));
        }
        if self.queue_bytes > Semaphore::MAX_PERMITS {
            return Err(ConfigError::Inconsistent("queue_bytes is too large"));
        }
        if self.rate_limit == 0 {
            return Err(ConfigError::Inconsistent("RATE_LIMIT must be positive"));
        }
        if self.ping_interval.is_zero() {
            return Err(ConfigError::Inconsistent("ping_interval must be positive"));
        }
        if self.max_frame_bytes < longest_client_message(self.max_devices_per_mac) {
            return Err(ConfigError::Inconsistent(
                "MAX_FRAME_BYTES is below the longest message a client sends: a frame of \
                 the longest record, or a registration naming MAX_DEVICES_PER_MAC phones",
            ));
        }
        if usize::try_from(self.rate_limit_bytes).is_ok_and(|rate| rate < self.max_frame_bytes) {
            return Err(ConfigError::Inconsistent(
                "RATE_LIMIT_BYTES is below MAX_FRAME_BYTES",
            ));
        }
        if self.queue_bytes < frame_cost(MAX_FRAME_LEN) {
            return Err(ConfigError::Inconsistent(
                "queue_bytes is below the cost of one frame of the longest record",
            ));
        }
        Ok(self)
    }
}

fn positive<T: std::str::FromStr + Default + PartialEq>(
    var: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: T,
) -> Result<T, ConfigError> {
    let Some(value) = var(name) else {
        return Ok(default);
    };
    match value.trim().parse::<T>() {
        Ok(parsed) if parsed != T::default() => Ok(parsed),
        _ => Err(ConfigError::NotAPositiveInteger { name, value }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_vars(|name| vars.get(name).cloned())
    }

    #[test]
    fn unset_variables_keep_the_defaults() {
        let config = config(&[]).unwrap();
        assert_eq!(config.port, 8080);
        assert_eq!(config.max_frame_bytes, 131_072);
        assert_eq!(config.max_connections, 64);
        assert!(config.deny_device_ids.is_empty());
    }

    #[test]
    fn variables_override_the_defaults() {
        let config = config(&[
            ("PORT", "9000"),
            ("RATE_LIMIT", "10"),
            ("MAX_FRAME_BYTES", "200000"),
            ("MAX_DEVICES_PER_MAC", "2"),
            ("MAX_CONNECTIONS", "3"),
            (
                "DENY_DEVICE_IDS",
                " abcdefghijklmnopqrstuvwxyz, ,234567abcdefghijklmnopqrst,",
            ),
        ])
        .unwrap();
        assert_eq!(
            (
                config.port,
                config.rate_limit,
                config.max_frame_bytes,
                config.max_devices_per_mac,
                config.max_connections,
            ),
            (9000, 10, 200_000, 2, 3)
        );
        assert_eq!(config.deny_device_ids.len(), 2);
    }

    #[test]
    fn a_queue_budget_below_one_full_frame_is_refused() {
        let config = Config {
            queue_bytes: frame_cost(MAX_FRAME_LEN) - 1,
            ..Config::default()
        };
        assert!(matches!(
            config.validated(),
            Err(ConfigError::Inconsistent(_))
        ));
        assert!(Config::default().validated().is_ok());
    }

    #[test]
    fn a_message_limit_below_the_longest_client_message_is_refused() {
        assert!(matches!(
            config(&[("MAX_FRAME_BYTES", "65536")]),
            Err(ConfigError::Inconsistent(_))
        ));
    }

    #[test]
    fn settings_past_their_caps_are_refused() {
        let huge = usize::MAX.to_string();
        for vars in [
            &[("MAX_DEVICES_PER_MAC", "1025")][..],
            &[("MAX_DEVICES_PER_MAC", huge.as_str())],
            &[("MAX_FRAME_BYTES", "1048577")],
            &[("MAX_FRAME_BYTES", huge.as_str())],
            &[("MAX_CONNECTIONS", huge.as_str())],
        ] {
            assert!(
                matches!(config(vars), Err(ConfigError::Inconsistent(_))),
                "{vars:?}"
            );
        }
        assert!(config(&[("MAX_DEVICES_PER_MAC", "1024")]).is_ok());
        assert!(
            config(&[
                ("MAX_FRAME_BYTES", "1048576"),
                ("RATE_LIMIT_BYTES", "1048576")
            ])
            .is_ok()
        );
    }

    #[test]
    fn settings_built_in_code_are_checked_too() {
        // A `rate_limit_bytes` of zero falls under the message-limit floor.
        for config in [
            Config {
                rate_limit: 0,
                ..Config::default()
            },
            Config {
                rate_limit_bytes: 0,
                ..Config::default()
            },
            Config {
                queue_bytes: usize::MAX,
                ..Config::default()
            },
            Config {
                ping_interval: Duration::ZERO,
                ..Config::default()
            },
        ] {
            assert!(
                matches!(
                    config.clone().validated(),
                    Err(ConfigError::Inconsistent(_))
                ),
                "{config:?}"
            );
        }
    }

    #[test]
    fn a_byte_rate_below_the_message_limit_is_refused() {
        assert!(matches!(
            config(&[("RATE_LIMIT_BYTES", "131071")]),
            Err(ConfigError::Inconsistent(_))
        ));
        assert!(config(&[("RATE_LIMIT_BYTES", "131072")]).is_ok());
    }

    #[test]
    fn the_default_timings_keep_their_order() {
        // A client reconnects after this long with nothing from the relay.
        const CLIENT_SILENCE: Duration = Duration::from_secs(50);
        let config = Config::default();
        assert!(config.ping_interval * 2 <= CLIENT_SILENCE);
        assert!(config.ping_interval < config.idle_timeout);
    }

    #[test]
    fn zero_and_non_numbers_are_refused() {
        assert!(config(&[("MAX_CONNECTIONS", "0")]).is_err());
        assert!(config(&[("RATE_LIMIT", "fast")]).is_err());
        assert!(config(&[("PORT", "70000")]).is_err());
    }

    #[test]
    fn a_malformed_denied_id_is_refused() {
        assert!(matches!(
            config(&[("DENY_DEVICE_IDS", "not-an-id")]),
            Err(ConfigError::DenyList(_))
        ));
    }
}
