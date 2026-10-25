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
use switchboard_remote_crypto::frame::MAX_FRAME_LEN;
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

/// The encoded length of the longest message a client following the
/// protocol sends: a frame carrying the longest record and a pairing token,
/// or a Mac's registration naming `max_devices_per_mac` phones.
pub(crate) fn longest_client_message(max_devices_per_mac: usize) -> usize {
    let id = DeviceId("a".repeat(identity::DEVICE_ID_LEN));
    let frame = ClientMessage::Frame {
        to: id.clone(),
        record: vec![0; MAX_FRAME_LEN],
        pairing_token: Some([0; PAIRING_TOKEN_LEN]),
    };
    let register = ClientMessage::Register(Register {
        relay_protocol: RELAY_PROTOCOL,
        device_id: id.clone(),
        identity_public_key: [0; PUBLIC_KEY_LEN],
        signature: [0; SIGNATURE_LEN],
        role: Role::Mac {
            paired_devices: vec![id; max_devices_per_mac],
        },
    });
    [frame, register]
        .iter()
        .map(|message| serde_json::to_vec(message).map_or(usize::MAX, |json| json.len()))
        .max()
        .unwrap_or(usize::MAX)
}

/// The `relay_protocol` a `register` names, read without the rest of it, so
/// one from a client speaking another protocol, whose shape may differ, is
/// still recognized as one. `None` for anything that is not a `register`.
pub(crate) fn claimed_relay_protocol(text: &str) -> Option<u32> {
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Claim {
        Register { relay_protocol: u32 },
    }
    let Claim::Register { relay_protocol } = serde_json::from_str(text).ok()?;
    Some(relay_protocol)
}

/// A client's messages to the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientMessage {
    /// The answer to `challenge`, and the only message accepted before it.
    Register(Register),
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
    /// A Mac's full set of paired phones, replacing the set it registered
    /// with or last sent. Sent on every change. From a phone, `bad_message`.
    PairedDevices { device_ids: Vec<DeviceId> },
    /// Opens a Mac's pairing window, replacing any open one. `expires_in` is
    /// in seconds and is capped at 300. From a phone, `bad_message`.
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

/// What `register` claims: an identity, proof of holding its key, and a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Register {
    pub relay_protocol: u32,
    pub device_id: DeviceId,
    #[serde(with = "base64")]
    pub identity_public_key: [u8; PUBLIC_KEY_LEN],
    /// `DeviceKeys::sign_relay_challenge` over the challenge's nonce.
    #[serde(with = "base64")]
    pub signature: [u8; SIGNATURE_LEN],
    pub role: Role,
}

/// What a device registers as. The relay routes by it, so a device's role is
/// known from its first registered moment and cannot change on that
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    /// A Mac and its full set of paired phones. The set may be empty.
    Mac { paired_devices: Vec<DeviceId> },
    /// A phone and the one Mac it is paired with, or is pairing with: the
    /// relay lets only that Mac reach it. A phone with more than one Mac
    /// would need a list here, and a new `relay_protocol`.
    Phone { mac: DeviceId },
}

/// The relay's messages to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    /// The relay's first message on every connection. `relay_protocol` is
    /// the relay's own, so a client that speaks another knows which side is
    /// behind before it registers.
    Challenge {
        #[serde(with = "base64")]
        nonce: [u8; NONCE_LEN],
        relay_protocol: u32,
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
    /// A message that does not parse, a binary message, a message out of
    /// order or not allowed for the sender's role, or a record longer than
    /// any frame. Closes the connection.
    BadMessage,
    /// A paired set above `MAX_DEVICES_PER_MAC`. On `paired_devices` the
    /// relay keeps the set it held; on `register` it closes the connection.
    LimitExceeded,
    MacOffline,
    PhoneOffline,
    NotPaired,
    /// The pairing token is unknown, expired, closed, or bound to another
    /// device.
    PairingExpired,
    /// The recipient, named by `device_id`, had no room for this frame and
    /// has been closed. The frame was not delivered; the sender's connection
    /// stays open.
    RecipientFull,
    #[serde(other)]
    Unknown,
}

/// A byte field's type, with the most bytes it may hold. Text longer than
/// that many bytes encode to is refused before it is decoded, so an
/// oversized field costs no allocation beyond the text already read.
trait Bytes: TryFrom<Vec<u8>> {
    const MAX_LEN: usize;
}

impl<const N: usize> Bytes for [u8; N] {
    const MAX_LEN: usize = N;
}

/// A frame's record.
impl Bytes for Vec<u8> {
    const MAX_LEN: usize = MAX_FRAME_LEN;
}

mod base64 {
    use data_encoding::BASE64;
    use serde::{Deserialize, Deserializer, Serializer};

    use super::Bytes;

    pub(super) fn serialize<S: Serializer, T: AsRef<[u8]>>(
        bytes: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64.encode(bytes.as_ref()))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>, T: Bytes>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        decode(&String::deserialize(deserializer)?)
    }

    pub(super) fn decode<E: serde::de::Error, T: Bytes>(text: &str) -> Result<T, E> {
        if text.len() > BASE64.encode_len(T::MAX_LEN) {
            return Err(E::custom(format!("longer than {} bytes", T::MAX_LEN)));
        }
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

    pub(super) fn deserialize<'de, D: Deserializer<'de>, T: super::Bytes>(
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
    fn the_longest_client_message_is_a_full_frame_until_a_set_outgrows_it() {
        let frame = longest_client_message(1);
        assert!((87_000..131_072).contains(&frame), "{frame}");
        assert_eq!(longest_client_message(8), frame);
        assert!(longest_client_message(10_000) > frame);
    }

    #[test]
    fn a_byte_field_longer_than_its_type_allows_is_refused_before_decoding() {
        let frame = |len: usize| {
            let record = data_encoding::BASE64.encode(&vec![0; len]);
            serde_json::from_str::<ClientMessage>(&format!(
                r#"{{"type":"frame","to":"{ID}","record":"{record}"}}"#
            ))
        };
        assert!(frame(MAX_FRAME_LEN).is_ok());
        assert!(frame(MAX_FRAME_LEN + 1).is_err());

        let nonce = data_encoding::BASE64.encode(&[0; NONCE_LEN + 1]);
        let json = format!(r#"{{"type":"challenge","nonce":"{nonce}","relay_protocol":1}}"#);
        assert!(serde_json::from_str::<ServerMessage>(&json).is_err());
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
