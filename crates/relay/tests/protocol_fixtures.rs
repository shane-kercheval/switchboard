//! The relay messages' committed fixtures, one file per message, are the
//! contract the Mac and the phone decode against. This checks that the Rust
//! types encode to exactly those bytes and decode from them.
//!
//! To change the wire format deliberately, regenerate them with
//! `make protocol-fixtures` and review the diff.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use switchboard_relay::protocol::{
    ClientMessage, DeviceId, ErrorCode, RELAY_PROTOCOL, Register, Role, ServerMessage, token_hash,
};
use switchboard_remote_crypto::DeviceKeys;

const UPDATE: &str = "UPDATE_PROTOCOL_FIXTURES";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/protocol")
}

fn keys(noise: u8, identity: u8) -> DeviceKeys {
    let mut blob = vec![1];
    blob.extend_from_slice(&[noise; 32]);
    blob.extend_from_slice(&[identity; 32]);
    DeviceKeys::restore(&blob).unwrap()
}

fn id(keys: &DeviceKeys) -> DeviceId {
    DeviceId::from_identity_key(&keys.identity_public_key())
}

fn sequence<const N: usize>(start: u8) -> [u8; N] {
    std::array::from_fn(|i| start.wrapping_add(u8::try_from(i).unwrap()))
}

enum Fixture {
    ToRelay(ClientMessage),
    FromRelay(ServerMessage),
}

use Fixture::{FromRelay, ToRelay};

/// The values the fixtures are built from, fixed so the files never change
/// by accident.
struct Sample {
    mac: DeviceKeys,
    phone: DeviceKeys,
    nonce: [u8; 32],
    token: [u8; 32],
    record: Vec<u8>,
    pairing_record: Vec<u8>,
}

impl Sample {
    fn new() -> Self {
        Self {
            mac: keys(0x11, 0x21),
            phone: keys(0x12, 0x22),
            nonce: sequence(0),
            token: sequence(0x40),
            record: [&[4][..], &sequence::<24>(0xa0)].concat(),
            pairing_record: [&[1][..], &sequence::<32>(0x80)].concat(),
        }
    }
}

/// Every fixture, by its path under the fixture directory.
fn fixtures() -> Vec<(&'static str, Fixture)> {
    let sample = Sample::new();
    let mut fixtures = to_relay(&sample);
    fixtures.extend(from_relay(&sample));
    fixtures
}

fn to_relay(sample: &Sample) -> Vec<(&'static str, Fixture)> {
    let Sample { mac, phone, .. } = sample;
    vec![
        (
            "to_relay/register.json",
            ToRelay(ClientMessage::Register(Register {
                relay_protocol: RELAY_PROTOCOL,
                device_id: id(phone),
                identity_public_key: phone.identity_public_key().to_bytes(),
                signature: phone.sign_relay_challenge(&sample.nonce).to_bytes(),
                role: Role::Phone { mac: id(mac) },
            })),
        ),
        (
            "to_relay/register_mac.json",
            ToRelay(ClientMessage::Register(Register {
                relay_protocol: RELAY_PROTOCOL,
                device_id: id(mac),
                identity_public_key: mac.identity_public_key().to_bytes(),
                signature: mac.sign_relay_challenge(&sample.nonce).to_bytes(),
                role: Role::Mac {
                    paired_devices: vec![id(phone)],
                },
            })),
        ),
        (
            "to_relay/frame.json",
            ToRelay(ClientMessage::Frame {
                to: id(mac),
                record: sample.record.clone(),
                pairing_token: None,
            }),
        ),
        (
            "to_relay/frame_with_pairing_token.json",
            ToRelay(ClientMessage::Frame {
                to: id(mac),
                record: sample.pairing_record.clone(),
                pairing_token: Some(sample.token),
            }),
        ),
        (
            "to_relay/paired_devices.json",
            ToRelay(ClientMessage::PairedDevices {
                device_ids: vec![id(phone)],
            }),
        ),
        (
            "to_relay/pairing_open.json",
            ToRelay(ClientMessage::PairingOpen {
                token_hash: token_hash(&sample.token),
                expires_in: 300,
            }),
        ),
        (
            "to_relay/pairing_close.json",
            ToRelay(ClientMessage::PairingClose),
        ),
    ]
}

fn from_relay(sample: &Sample) -> Vec<(&'static str, Fixture)> {
    let Sample { mac, phone, .. } = sample;
    vec![
        (
            "from_relay/challenge.json",
            FromRelay(ServerMessage::Challenge {
                nonce: sample.nonce,
                relay_protocol: RELAY_PROTOCOL,
            }),
        ),
        (
            "from_relay/registered.json",
            FromRelay(ServerMessage::Registered),
        ),
        (
            "from_relay/frame.json",
            FromRelay(ServerMessage::Frame {
                to: id(phone),
                from: id(mac),
                record: sample.record.clone(),
                pairing_token: None,
            }),
        ),
        (
            "from_relay/frame_with_pairing_token.json",
            FromRelay(ServerMessage::Frame {
                to: id(mac),
                from: id(phone),
                record: sample.pairing_record.clone(),
                pairing_token: Some(sample.token),
            }),
        ),
        (
            "from_relay/peer_disconnected.json",
            FromRelay(ServerMessage::PeerDisconnected { device_id: id(mac) }),
        ),
        (
            "from_relay/error.json",
            FromRelay(ServerMessage::Error {
                code: ErrorCode::NotPaired,
                message: "this device is not paired with that one".to_owned(),
                device_id: Some(id(mac)),
                last_seen: None,
            }),
        ),
        (
            "from_relay/error_mac_offline.json",
            FromRelay(ServerMessage::Error {
                code: ErrorCode::MacOffline,
                message: "that Mac is not connected".to_owned(),
                device_id: Some(id(mac)),
                last_seen: Some(Utc.with_ymd_and_hms(2026, 10, 12, 9, 30, 0).unwrap()),
            }),
        ),
        (
            "from_relay/error_recipient_full.json",
            FromRelay(ServerMessage::Error {
                code: ErrorCode::RecipientFull,
                message: "that device had no room for the frame and has been disconnected"
                    .to_owned(),
                device_id: Some(id(phone)),
                last_seen: None,
            }),
        ),
        (
            "from_relay/error_registration_refused.json",
            FromRelay(ServerMessage::Error {
                code: ErrorCode::RegistrationRefused,
                message: "the signature does not answer the challenge".to_owned(),
                device_id: None,
                last_seen: None,
            }),
        ),
    ]
}

fn encode(message: &impl Serialize) -> String {
    serde_json::to_string_pretty(message).unwrap() + "\n"
}

fn check_decodes<T: DeserializeOwned + PartialEq + std::fmt::Debug>(
    path: &str,
    text: &str,
    expected: &T,
) {
    let decoded: T = serde_json::from_str(text).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(&decoded, expected, "{path}");
}

#[test]
fn messages_encode_to_and_decode_from_their_fixtures() {
    let update = std::env::var_os(UPDATE).is_some();
    for (path, fixture) in fixtures() {
        let file = fixture_dir().join(path);
        let encoded = match &fixture {
            ToRelay(message) => encode(message),
            FromRelay(message) => encode(message),
        };
        if update {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, &encoded).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("{path}: {e}; run `make protocol-fixtures`"));
        assert_eq!(
            committed, encoded,
            "{path} differs from the encoding; if the change is deliberate, run `make protocol-fixtures`"
        );
        match &fixture {
            ToRelay(message) => check_decodes(path, &committed, message),
            FromRelay(message) => check_decodes(path, &committed, message),
        }
    }
}

#[test]
fn every_fixture_file_is_checked() {
    // Regeneration writes the files concurrently with this test.
    if std::env::var_os(UPDATE).is_some() {
        return;
    }
    let expected: BTreeSet<String> = fixtures()
        .into_iter()
        .map(|(path, _)| path.to_owned())
        .collect();
    let mut found = BTreeSet::new();
    for dir in ["to_relay", "from_relay"] {
        for entry in std::fs::read_dir(fixture_dir().join(dir)).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            found.insert(format!("{dir}/{name}"));
        }
    }
    assert_eq!(
        found, expected,
        "a fixture file with no message, or the reverse"
    );
}
