use std::collections::HashSet;
use std::time::Duration;

use switchboard_remote_crypto::frame::MAX_FRAME_LEN;

use crate::outbox::frame_cost;
use crate::protocol::{DeviceId, InvalidDeviceId, longest_client_message};
use crate::server::{HTTP_HEADROOM, MAX_REQUEST_HEAD};

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
    /// Bytes per second a connection may send, enforced the same way. A
    /// burst may always reach one message of `max_frame_bytes`.
    pub rate_limit_bytes: u32,
    /// The longest WebSocket message accepted. The default is above a frame
    /// carrying the longest record: 65,568 bytes, about 87,500 as base64.
    pub max_frame_bytes: usize,
    pub max_devices_per_mac: usize,
    /// WebSocket connections, registered or not. A full relay answers an
    /// upgrade with HTTP 503. A few sockets more are accepted for plain HTTP,
    /// each closed once it idles for `register_timeout`.
    pub max_connections: usize,
    pub deny_device_ids: HashSet<DeviceId>,
    /// A connection that sends nothing, pings included, for this long is
    /// closed. Clients ping every 25 seconds.
    pub idle_timeout: Duration,
    /// How long a new connection has to answer the challenge.
    pub register_timeout: Duration,
    /// Bytes of frames queued for one connection at most. A sender whose
    /// recipient is full waits for room rather than losing the frame.
    pub queue_bytes: usize,
    /// A recipient that completes no send for this long while a sender
    /// waits for room is closed as not reading. Below the phone's 10-second
    /// wait for its handshake reply, so another phone's stall cannot make a
    /// connecting phone give up.
    pub stall_timeout: Duration,
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
            register_timeout: Duration::from_secs(10),
            rate_limit_bytes: 16 * 1024 * 1024,
            queue_bytes: 4 * 1024 * 1024,
            stall_timeout: Duration::from_secs(5),
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

    /// Refuses settings the relay cannot work under. A message limit below
    /// the longest message a client sends would close any connection that
    /// sends it. A queue budget smaller than one frame of the longest record
    /// could never admit that frame, and its recipient would be closed for a
    /// stall it did not cause.
    pub fn validated(self) -> Result<Self, ConfigError> {
        if self.max_frame_bytes < longest_client_message(self.max_devices_per_mac) {
            return Err(ConfigError::Inconsistent(
                "MAX_FRAME_BYTES is below the longest message a client sends: a frame of \
                 the longest record, or a registration naming MAX_DEVICES_PER_MAC phones",
            ));
        }
        if self.queue_bytes < frame_cost(MAX_FRAME_LEN) {
            return Err(ConfigError::Inconsistent(
                "queue_bytes is below the cost of one frame of the longest record",
            ));
        }
        Ok(self)
    }

    /// The most memory connections' queues and read buffers can hold: each
    /// WebSocket's queue budget and one message being read, and every
    /// socket's request head, read and answered, before it upgrades.
    pub fn memory_ceiling(&self) -> usize {
        let websockets = self
            .max_connections
            .saturating_mul(self.queue_bytes.saturating_add(self.max_frame_bytes));
        let heads = self
            .max_connections
            .saturating_add(HTTP_HEADROOM)
            .saturating_mul(2 * MAX_REQUEST_HEAD);
        websockets.saturating_add(heads)
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
        for vars in [
            &[("MAX_FRAME_BYTES", "65536")][..],
            &[("MAX_DEVICES_PER_MAC", "10000")],
        ] {
            assert!(
                matches!(config(vars), Err(ConfigError::Inconsistent(_))),
                "{vars:?}"
            );
        }
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
