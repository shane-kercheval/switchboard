//! What crosses a relay connection. Every message is one WebSocket text
//! message holding a JSON object tagged by `type`; byte fields are standard
//! base64 with padding. A binary WebSocket message is a protocol error.
//!
//! The relay reads `ClientMessage` and writes `ServerMessage`; a client does
//! the reverse. The committed fixtures under `tests/fixtures/protocol/` are
//! the contract the Mac and the phone decode against.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use switchboard_remote_crypto::identity::{self, VerifyingKey};

/// The relay protocol this build speaks. A client names its own in
/// `register`; any other value is refused with `protocol_mismatch`.
pub const RELAY_PROTOCOL: u32 = 1;
pub const NONCE_LEN: usize = 32;
pub const PAIRING_TOKEN_LEN: usize = 32;
pub const TOKEN_HASH_LEN: usize = 32;
pub const PUBLIC_KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;

/// A device's relay address, `identity::device_id` of its Ed25519 key.
/// Deserializing checks the shape, so a malformed id never reaches routing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct DeviceId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a device id: {0:?}")]
pub struct InvalidDeviceId(pub String);

impl DeviceId {
    pub fn parse(value: &str) -> Result<Self, InvalidDeviceId> {
        if identity::is_device_id(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidDeviceId(value.to_owned()))
        }
    }

    pub fn from_identity_key(key: &VerifyingKey) -> Self {
        Self(identity::device_id(key))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// What `pairing_open` carries instead of the token itself. The phone presents
/// the token on its pairing frames, and the relay hashes it to compare.
pub fn token_hash(token: &[u8; PAIRING_TOKEN_LEN]) -> [u8; TOKEN_HASH_LEN] {
    Sha256::digest(token).into()
}

/// A client's messages to the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientMessage {
    /// The answer to `challenge`, and the only message accepted before it.
    Register {
        relay_protocol: u32,
        device_id: DeviceId,
        #[serde(with = "base64")]
        identity_public_key: [u8; PUBLIC_KEY_LEN],
        /// `DeviceKeys::sign_relay_challenge` over the challenge's nonce.
        #[serde(with = "base64")]
        signature: [u8; SIGNATURE_LEN],
    },
    /// A frame for another device. Any `from` a client includes is ignored:
    /// the relay names the sender itself.
    Frame {
        to: DeviceId,
        #[serde(with = "base64")]
        record: Vec<u8>,
        /// Only on a phone's pairing frames, before it is paired.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "base64_option"
        )]
        pairing_token: Option<[u8; PAIRING_TOKEN_LEN]>,
    },
    /// The Mac's full set of paired phones, replacing any set the relay
    /// holds for it. Sent after every registration and on every change.
    PairedDevices { device_ids: Vec<DeviceId> },
    /// Opens a pairing window, replacing any open one. `expires_in` is in
    /// seconds and is capped at 300.
    PairingOpen {
        #[serde(with = "base64")]
        token_hash: [u8; TOKEN_HASH_LEN],
        expires_in: u64,
    },
    /// Ends the open pairing window, if any.
    PairingClose,
    /// A type this build does not know; ignored.
    #[serde(other)]
    Unknown,
}

/// The relay's messages to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    /// The relay's first message on every connection.
    Challenge {
        #[serde(with = "base64")]
        nonce: [u8; NONCE_LEN],
    },
    /// Registration succeeded. Precedes every frame and notice for this
    /// connection.
    Registered,
    /// A frame from another device, `from` set by the relay.
    Frame {
        to: DeviceId,
        from: DeviceId,
        #[serde(with = "base64")]
        record: Vec<u8>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "base64_option"
        )]
        pairing_token: Option<[u8; PAIRING_TOKEN_LEN]>,
    },
    /// A registration of `device_id` ended: its connection closed, timed
    /// out, or was replaced by a newer one. Sent once per registration, to
    /// the devices paired with it.
    PeerDisconnected { device_id: DeviceId },
    Error {
        code: ErrorCode,
        message: String,
        /// The device a refused frame was addressed to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_id: Option<DeviceId>,
        /// For `mac_offline`: when that Mac last disconnected, if this relay
        /// has seen it since it started.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_seen: Option<DateTime<Utc>>,
    },
    /// A type this build does not know; ignored.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The client's `relay_protocol` is not this relay's. Closes the
    /// connection.
    ProtocolMismatch,
    /// A bad signature, a key that does not hash to the claimed id, or a
    /// denied id. Closes the connection.
    RegistrationRefused,
    /// `MAX_CONNECTIONS` reached. Closes the connection.
    RelayFull,
    /// Over the per-connection message rate. Closes the connection.
    RateLimited,
    /// A message that does not parse, a binary message, or a message out of
    /// order. Closes the connection.
    BadMessage,
    /// A `paired_devices` set above `MAX_DEVICES_PER_MAC`; the relay keeps
    /// the set it held.
    LimitExceeded,
    MacOffline,
    PhoneOffline,
    NotPaired,
    /// The pairing token is unknown, expired, closed, or bound to another
    /// device.
    PairingExpired,
    #[serde(other)]
    Unknown,
}

mod base64 {
    use data_encoding::BASE64;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer, T: AsRef<[u8]>>(
        bytes: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64.encode(bytes.as_ref()))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>, T: TryFrom<Vec<u8>>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        decode(&String::deserialize(deserializer)?)
    }

    pub(super) fn decode<E: serde::de::Error, T: TryFrom<Vec<u8>>>(text: &str) -> Result<T, E> {
        let bytes = BASE64.decode(text.as_bytes()).map_err(E::custom)?;
        let len = bytes.len();
        T::try_from(bytes).map_err(|_| E::custom(format!("unexpected length {len}")))
    }
}

mod base64_option {
    use data_encoding::BASE64;
    use serde::{Deserialize, Deserializer, Serializer};

    // serde's `with` fixes this signature; `skip_serializing_if` keeps `None`
    // from reaching it.
    #[allow(clippy::ref_option)]
    pub(super) fn serialize<S: Serializer, T: AsRef<[u8]>>(
        bytes: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => serializer.serialize_str(&BASE64.encode(bytes.as_ref())),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>, T: TryFrom<Vec<u8>>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| super::base64::decode(&text))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "abcdefghijklmnopqrstuvwxyz";

    #[test]
    fn device_id_parses_by_the_crypto_crates_rule() {
        assert!(DeviceId::parse(ID).is_ok());
        assert!(DeviceId::parse("Abcdefghijklmnopqrstuvwxyz").is_err());
    }

    #[test]
    fn a_malformed_device_id_fails_to_decode() {
        let json = r#"{"type":"peer_disconnected","device_id":"NOT-AN-ID"}"#;
        assert!(serde_json::from_str::<ServerMessage>(json).is_err());
    }

    #[test]
    fn a_byte_field_of_the_wrong_length_fails_to_decode() {
        let json = r#"{"type":"challenge","nonce":"AAAA"}"#;
        assert!(serde_json::from_str::<ServerMessage>(json).is_err());
    }

    #[test]
    fn invalid_base64_fails_to_decode() {
        let json = format!(r#"{{"type":"frame","to":"{ID}","record":"not base64!"}}"#);
        assert!(serde_json::from_str::<ClientMessage>(&json).is_err());
    }

    #[test]
    fn unknown_types_and_codes_decode_to_unknown() {
        let client: ClientMessage = serde_json::from_str(r#"{"type":"later","x":1}"#).unwrap();
        assert_eq!(client, ClientMessage::Unknown);
        let server: ServerMessage = serde_json::from_str(r#"{"type":"later"}"#).unwrap();
        assert_eq!(server, ServerMessage::Unknown);
        let error: ServerMessage =
            serde_json::from_str(r#"{"type":"error","code":"later","message":"m"}"#).unwrap();
        assert!(matches!(
            error,
            ServerMessage::Error {
                code: ErrorCode::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn a_client_supplied_from_is_not_read() {
        let json =
            format!(r#"{{"type":"frame","to":"{ID}","from":"someone else","record":"BAE="}}"#);
        let message: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(
            message,
            ClientMessage::Frame {
                to: DeviceId::parse(ID).unwrap(),
                record: vec![4, 1],
                pairing_token: None,
            }
        );
    }

    #[test]
    fn token_hash_is_sha256_of_the_token() {
        let hash = token_hash(&[0; PAIRING_TOKEN_LEN]);
        assert_eq!(
            data_encoding::HEXLOWER.encode(&hash),
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
        );
    }
}
