//! A connection between two paired devices: a fresh `Noise_KK` handshake with
//! both static keys pinned at pairing, then an encrypted, fragmented record
//! stream. The phone initiates. A new connection is a new handshake, so no
//! key or nonce outlives it, and a record from an earlier connection never
//! opens. The Mac routes a phone's records between its sessions with
//! `DeviceSessions`.

use snow::{Builder, HandshakeState, TransportState};

use crate::CryptoError;
use crate::fragment::{Fragmenter, Reassembler, TAG_LEN};
use crate::frame::{self, FrameKind};
use crate::keys::{DeviceKeys, KEY_LEN};
use crate::noise::{self, ECHO_LEN, Peer, echo_of, read, strip_echo, with_echo, write};

const PATTERN: &str = "Noise_KK_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"switchboard session v1";

/// The phone's state after sending message 1. A frame of another type, or a
/// reply to another attempt, leaves it waiting; any other failure, or
/// success, uses it up.
pub struct SessionInitiator {
    handshake: Option<HandshakeState>,
    echo: [u8; ECHO_LEN],
}

/// An established connection. `seal` and `open` take `&mut self`, so a whole
/// envelope's records are produced without interleaving; records must be
/// transmitted in the order `seal` returns them. A record that does not
/// decrypt changes nothing, because a device may have two sessions and the
/// record may be the other's; `DeviceSessions` ends a device's sessions when
/// none of them opens a record. A record that decrypts but is malformed
/// closes the session for good: every later call returns `SessionClosed`.
///
/// A session is *confirmed* once it has evidence the peer is live and holds
/// the session's keys. The phone's session is confirmed when it is created,
/// because the Mac's message 2 carries a fresh ephemeral. The Mac's is not: a
/// `Noise_KK` message 1 can be replayed by whoever saw it, and `respond`
/// accepts the replay. It becomes confirmed when its first record opens.
/// Treat an unconfirmed session as a candidate, never as "this phone is here".
pub struct Session {
    transport: TransportState,
    fragmenter: Fragmenter,
    reassembler: Reassembler,
    state: State,
}

/// A session's lifecycle. A closed session is never confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Unconfirmed,
    Confirmed,
    Closed,
}

/// Phone: open a connection to the paired Mac whose key it pinned.
pub fn initiate(
    keys: &DeviceKeys,
    peer_noise_public_key: &[u8; KEY_LEN],
) -> Result<(SessionInitiator, Vec<u8>), CryptoError> {
    initiate_from(builder(keys, peer_noise_public_key)?)
}

/// Mac: accept a connection from the paired phone whose key it pinned. A phone
/// holding any other key fails here. The session starts unconfirmed, because
/// message 1 may be a replay. `DeviceSessions::accept_handshake` wraps this
/// with the routing the Mac needs.
pub fn respond(
    keys: &DeviceKeys,
    peer_noise_public_key: &[u8; KEY_LEN],
    message_1: &[u8],
) -> Result<(Session, Vec<u8>), CryptoError> {
    respond_from(builder(keys, peer_noise_public_key)?, message_1)
}

impl SessionInitiator {
    /// Reads the Mac's message 2. A frame of another type, or a reply to an
    /// earlier attempt (one this initiator's message 1 did not ask for),
    /// returns `UnexpectedFrame` and leaves the initiator waiting for its own
    /// reply; once that is read, the initiator is used up, whatever the
    /// outcome.
    pub fn finish(&mut self, message_2: &[u8]) -> Result<Session, CryptoError> {
        let message_2 = strip_echo(&self.echo, frame::body(FrameKind::SessionReply, message_2)?)?;
        let mut handshake = self.handshake.take().ok_or(CryptoError::HandshakeFailed)?;
        if !read(&mut handshake, message_2)?.is_empty() {
            return Err(CryptoError::InvalidPayload);
        }
        Session::from_handshake(handshake, State::Confirmed)
    }
}

impl Session {
    /// Fragments `envelope` and encrypts every fragment, in order, each as a
    /// `Record` frame. An envelope over the size limit is refused without
    /// closing the session; a failure to encrypt closes it (`SendFailed`).
    pub fn seal(&mut self, envelope: &[u8]) -> Result<Vec<Vec<u8>>, CryptoError> {
        if self.state == State::Closed {
            return Err(CryptoError::SessionClosed);
        }
        let fragments = self.fragmenter.fragment(envelope).map_err(|too_large| {
            CryptoError::MessageTooLarge {
                length: too_large.len as u64,
            }
        })?;
        let mut records = Vec::with_capacity(fragments.len());
        for fragment in fragments {
            let mut record = vec![0u8; 1 + fragment.len() + TAG_LEN];
            record[0] = FrameKind::Record as u8;
            let Ok(len) = self.transport.write_message(&fragment, &mut record[1..]) else {
                self.close();
                return Err(CryptoError::SendFailed);
            };
            record.truncate(1 + len);
            records.push(record);
        }
        Ok(records)
    }

    /// Decrypts one `Record` frame. Returns the envelope once its last record
    /// arrives, `None` while it is still incomplete. The first record that
    /// decrypts confirms the session.
    ///
    /// Failures say which kind they are, because a device's records may be
    /// offered to more than one session:
    /// - `UnexpectedFrame`: not a record, or oversize. Nothing is touched.
    /// - `RecordRejected`: did not decrypt here. Nothing is touched either — a
    ///   failed decryption does not advance the nonce — because the record may
    ///   belong to another session of the same device.
    /// - `ProtocolViolation`: decrypted, but broke the fragment rules. The
    ///   session is closed; the record belongs to no other session.
    pub fn open(&mut self, record: &[u8]) -> Result<Option<Vec<u8>>, CryptoError> {
        if self.state == State::Closed {
            return Err(CryptoError::SessionClosed);
        }
        // Checked, including the length, before allocating anything: the
        // record comes straight off the relay.
        let record = frame::body(FrameKind::Record, record)?;
        let mut fragment = vec![0u8; record.len()];
        let Ok(len) = self.transport.read_message(record, &mut fragment) else {
            return Err(CryptoError::RecordRejected);
        };
        self.state = State::Confirmed;
        fragment.truncate(len);
        self.reassembler.accept(&fragment).map_err(|_| {
            self.close();
            CryptoError::ProtocolViolation
        })
    }

    /// Live and proven: confirmed, and not closed since.
    pub fn is_confirmed(&self) -> bool {
        self.state == State::Confirmed
    }

    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    fn from_handshake(handshake: HandshakeState, state: State) -> Result<Self, CryptoError> {
        Ok(Self {
            transport: handshake
                .into_transport_mode()
                .map_err(|_| CryptoError::HandshakeFailed)?,
            fragmenter: Fragmenter::default(),
            reassembler: Reassembler::default(),
            state,
        })
    }

    pub(crate) fn close(&mut self) {
        self.state = State::Closed;
        self.reassembler.reset();
    }

    /// A record encrypting `fragment` as-is, header and all: the only way to
    /// produce an authentic record that breaks the fragment rules.
    #[cfg(test)]
    pub(crate) fn seal_raw_fragment(&mut self, fragment: &[u8]) -> Vec<u8> {
        let mut record = vec![0u8; 1 + fragment.len() + TAG_LEN];
        record[0] = FrameKind::Record as u8;
        let len = self
            .transport
            .write_message(fragment, &mut record[1..])
            .unwrap();
        record.truncate(1 + len);
        record
    }
}

fn initiate_from(builder: Builder<'_>) -> Result<(SessionInitiator, Vec<u8>), CryptoError> {
    let mut handshake = builder
        .build_initiator()
        .map_err(|_| CryptoError::HandshakeFailed)?;
    let message_1 = write(&mut handshake, &[])?;
    Ok((
        SessionInitiator {
            handshake: Some(handshake),
            echo: echo_of(&message_1)?,
        },
        frame::tagged(FrameKind::SessionRequest, &message_1),
    ))
}

fn respond_from(builder: Builder<'_>, message_1: &[u8]) -> Result<(Session, Vec<u8>), CryptoError> {
    let message_1 = frame::body(FrameKind::SessionRequest, message_1)?;
    let mut handshake = builder
        .build_responder()
        .map_err(|_| CryptoError::HandshakeFailed)?;
    if !read(&mut handshake, message_1)?.is_empty() {
        return Err(CryptoError::InvalidPayload);
    }
    let message_2 = write(&mut handshake, &[])?;
    Ok((
        Session::from_handshake(handshake, State::Unconfirmed)?,
        frame::tagged(
            FrameKind::SessionReply,
            &with_echo(&echo_of(message_1)?, &message_2),
        ),
    ))
}

fn builder<'a>(
    keys: &'a DeviceKeys,
    peer_noise_public_key: &'a [u8; KEY_LEN],
) -> Result<Builder<'a>, CryptoError> {
    noise::builder(PATTERN, PROLOGUE, keys, Peer::Pinned(peer_noise_public_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::{MAX_CHUNK_LEN, MAX_MESSAGE_LEN, MAX_RECORD_LEN};
    use crate::test_support::hex;

    fn keys(seed: u8) -> DeviceKeys {
        DeviceKeys::from_secrets(&[seed; KEY_LEN], &[seed.wrapping_add(100); KEY_LEN]).unwrap()
    }

    /// Phone and Mac sessions over one connection.
    fn connect(phone: &DeviceKeys, mac: &DeviceKeys) -> (Session, Session) {
        let (mut initiator, message_1) = initiate(phone, &mac.noise_public_key()).unwrap();
        let (mac_session, message_2) = respond(mac, &phone.noise_public_key(), &message_1).unwrap();
        (initiator.finish(&message_2).unwrap(), mac_session)
    }

    fn deliver(from: &mut Session, to: &mut Session, envelope: &[u8]) -> Option<Vec<u8>> {
        let mut result = None;
        for record in from.seal(envelope).unwrap() {
            assert!(record.len() <= 1 + MAX_RECORD_LEN);
            result = to.open(&record).unwrap();
        }
        result
    }

    #[test]
    fn envelopes_cross_in_both_directions() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        assert_eq!(
            deliver(&mut phone, &mut mac, b"{\"type\":\"ping\"}"),
            Some(b"{\"type\":\"ping\"}".to_vec())
        );
        assert_eq!(deliver(&mut mac, &mut phone, b""), Some(Vec::new()));
        let large: Vec<u8> = (0..3 * MAX_CHUNK_LEN + 99)
            .map(|i| u8::try_from(i % 253).unwrap())
            .collect();
        assert_eq!(deliver(&mut mac, &mut phone, &large), Some(large));
    }

    #[test]
    fn an_envelope_above_the_limit_is_refused_without_closing_the_session() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        assert_eq!(
            phone.seal(&vec![0; MAX_MESSAGE_LEN + 1]).err(),
            Some(CryptoError::MessageTooLarge {
                length: (MAX_MESSAGE_LEN + 1) as u64
            })
        );
        assert_eq!(
            deliver(&mut phone, &mut mac, b"still open"),
            Some(b"still open".to_vec())
        );
    }

    #[test]
    fn a_phone_with_a_key_other_than_the_pinned_one_is_refused() {
        let (phone, mac, stranger) = (keys(1), keys(2), keys(3));
        let (_, message_1) = initiate(&stranger, &mac.noise_public_key()).unwrap();
        assert_eq!(
            respond(&mac, &phone.noise_public_key(), &message_1).err(),
            Some(CryptoError::HandshakeFailed)
        );
    }

    #[test]
    fn a_mac_with_a_key_other_than_the_pinned_one_is_refused() {
        let (phone, mac, stranger) = (keys(1), keys(2), keys(3));
        let (mut initiator, message_1) = initiate(&phone, &mac.noise_public_key()).unwrap();
        assert_eq!(
            respond(&stranger, &phone.noise_public_key(), &message_1).err(),
            Some(CryptoError::HandshakeFailed)
        );
        let (_, message_2) = respond(&mac, &phone.noise_public_key(), &message_1).unwrap();
        let mut forged = message_2.clone();
        forged[1 + ECHO_LEN + 4] ^= 1;
        assert_eq!(
            initiator.finish(&forged).err(),
            Some(CryptoError::HandshakeFailed)
        );
    }

    #[test]
    fn the_phone_is_confirmed_at_once_and_the_mac_on_its_first_record() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        assert!(phone.is_confirmed());
        assert!(!mac.is_confirmed());
        deliver(&mut phone, &mut mac, b"hello");
        assert!(mac.is_confirmed());
    }

    /// `Noise_KK` lets anyone who saw message 1 replay it to the Mac. The
    /// replay yields a session, but one that stays unconfirmed: it cannot
    /// open the real phone's records, and the first one it is offered closes
    /// it.
    #[test]
    fn a_replayed_first_message_yields_only_an_unconfirmed_session() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (mut initiator, message_1) =
            initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        let (mut real, message_2) =
            respond(&mac_keys, &phone_keys.noise_public_key(), &message_1).unwrap();
        let mut phone = initiator.finish(&message_2).unwrap();
        let (mut replayed, _) =
            respond(&mac_keys, &phone_keys.noise_public_key(), &message_1).unwrap();
        assert!(!replayed.is_confirmed());

        let record = phone.seal(b"hello").unwrap().remove(0);
        assert_eq!(
            replayed.open(&record).err(),
            Some(CryptoError::RecordRejected)
        );
        assert!(!replayed.is_confirmed());
        // The failed attempt leaves the real session untouched, so the same
        // record still opens there.
        assert_eq!(real.open(&record), Ok(Some(b"hello".to_vec())));
        assert!(real.is_confirmed());
    }

    #[test]
    fn an_oversize_record_is_refused_before_anything_is_allocated_for_it() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let mut oversize = vec![FrameKind::Record as u8];
        oversize.resize(2 + MAX_RECORD_LEN, 0);
        assert_eq!(
            mac.open(&oversize).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        assert!(!mac.is_closed());
        assert_eq!(
            deliver(&mut phone, &mut mac, b"still open"),
            Some(b"still open".to_vec())
        );
    }

    /// The replay the confirmation rule exists for: a replayed connection
    /// request reaching a live session is refused by its type, and the session
    /// carries on.
    #[test]
    fn a_handshake_frame_offered_to_a_session_leaves_it_open() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (mut initiator, message_1) =
            initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        let (mut mac, message_2) =
            respond(&mac_keys, &phone_keys.noise_public_key(), &message_1).unwrap();
        let mut phone = initiator.finish(&message_2).unwrap();
        assert_eq!(
            deliver(&mut phone, &mut mac, b"hello"),
            Some(b"hello".to_vec())
        );
        assert_eq!(
            mac.open(&message_1).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        assert_eq!(
            phone.open(&message_2).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        assert_eq!(
            deliver(&mut phone, &mut mac, b"after"),
            Some(b"after".to_vec())
        );
        assert_eq!(
            deliver(&mut mac, &mut phone, b"back"),
            Some(b"back".to_vec())
        );
    }

    #[test]
    fn each_handshake_step_refuses_a_frame_of_another_kind() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (_, message_1) = initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        let mut as_record = message_1.clone();
        as_record[0] = FrameKind::Record as u8;
        assert_eq!(
            respond(&mac_keys, &phone_keys.noise_public_key(), &as_record).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        let (mut initiator, _) = initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        assert_eq!(
            initiator.finish(&message_1).err(),
            Some(CryptoError::UnexpectedFrame)
        );
    }

    /// A record the phone encrypted under this session's keys whose fragment
    /// breaks the sequence rules.
    fn authentic_but_malformed_record(from: &mut Session) -> Vec<u8> {
        from.seal_raw_fragment(&[0, 0, 0, 0, 0, 1, 0, 2, 0xAB])
    }

    #[test]
    fn an_authentic_malformed_record_is_a_protocol_violation_that_closes_the_session() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let record = authentic_but_malformed_record(&mut phone);
        assert_eq!(
            mac.open(&record).err(),
            Some(CryptoError::ProtocolViolation)
        );
        assert!(mac.is_closed());
        assert!(!mac.is_confirmed());
        assert_eq!(mac.seal(b"x").err(), Some(CryptoError::SessionClosed));
        assert_eq!(mac.open(&record).err(), Some(CryptoError::SessionClosed));
    }

    #[test]
    fn an_empty_record_is_rejected_and_leaves_the_session_open() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        assert_eq!(
            mac.open(&[FrameKind::Record as u8]).err(),
            Some(CryptoError::RecordRejected)
        );
        assert!(!mac.is_closed());
        assert_eq!(
            deliver(&mut phone, &mut mac, b"hello"),
            Some(b"hello".to_vec())
        );
    }

    #[test]
    fn a_session_confirmed_and_then_closed_is_no_longer_confirmed() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        deliver(&mut phone, &mut mac, b"hello");
        assert!(mac.is_confirmed());
        let record = authentic_but_malformed_record(&mut phone);
        assert_eq!(
            mac.open(&record).err(),
            Some(CryptoError::ProtocolViolation)
        );
        assert!(!mac.is_confirmed());
    }

    /// A phone's initiator waiting for the Mac's reply survives a stray frame:
    /// the wrong type is refused, and the real reply still completes it.
    #[test]
    fn a_stray_frame_leaves_the_initiator_waiting_for_its_reply() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (mut initiator, message_1) =
            initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        let (mut mac, message_2) =
            respond(&mac_keys, &phone_keys.noise_public_key(), &message_1).unwrap();
        assert_eq!(
            initiator.finish(&message_1).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        let mut phone = initiator.finish(&message_2).unwrap();
        assert_eq!(
            deliver(&mut phone, &mut mac, b"hello"),
            Some(b"hello".to_vec())
        );
        assert_eq!(
            initiator.finish(&message_2).err(),
            Some(CryptoError::HandshakeFailed)
        );
    }

    /// The phone gave up on one attempt and started another; the Mac's late
    /// answer to the first must not use up the second.
    #[test]
    fn a_reply_to_an_abandoned_attempt_leaves_the_initiator_waiting() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (_, abandoned) = initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        let (mut initiator, message_1) =
            initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        let (_, late) = respond(&mac_keys, &phone_keys.noise_public_key(), &abandoned).unwrap();
        let (mut mac, message_2) =
            respond(&mac_keys, &phone_keys.noise_public_key(), &message_1).unwrap();
        assert_eq!(
            initiator.finish(&late).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        let mut phone = initiator.finish(&message_2).unwrap();
        assert_eq!(
            deliver(&mut phone, &mut mac, b"hello"),
            Some(b"hello".to_vec())
        );
    }

    /// The echo is not authenticated: a relay that alters it only gets the
    /// reply ignored, and the initiator still completes on the real one.
    #[test]
    fn a_reply_with_an_altered_echo_is_ignored() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (mut initiator, message_1) =
            initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        let (_, message_2) =
            respond(&mac_keys, &phone_keys.noise_public_key(), &message_1).unwrap();
        let mut altered = message_2.clone();
        altered[1] ^= 1;
        assert_eq!(
            initiator.finish(&altered).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        assert_eq!(
            initiator.finish(&message_2[..ECHO_LEN]).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        assert!(initiator.finish(&message_2).is_ok());
    }

    #[test]
    fn records_are_sized_to_their_content() {
        let (mut phone, _) = connect(&keys(1), &keys(2));
        let record = phone.seal(b"small").unwrap().remove(0);
        assert!(record.capacity() < 128, "{}", record.capacity());
    }

    /// A rejected record changes nothing: the session is still open, and the
    /// genuine record it was forged from still opens. This relies on `snow` not
    /// advancing the nonce on a failed decryption, which was the bug in
    /// RUSTSEC-2024-0011 (fixed in 0.9.5): the canary for any `snow` upgrade,
    /// and the fact `DeviceSessions`' routing rests on.
    #[test]
    fn a_tampered_record_is_rejected_without_disturbing_the_session() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let record = phone.seal(b"hello").unwrap().remove(0);
        let mut tampered = record.clone();
        tampered[1] ^= 1;
        assert_eq!(mac.open(&tampered).err(), Some(CryptoError::RecordRejected));
        assert!(!mac.is_closed());
        assert_eq!(mac.open(&record), Ok(Some(b"hello".to_vec())));
    }

    #[test]
    fn a_repeated_record_is_refused() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let record = phone.seal(b"once").unwrap().remove(0);
        assert_eq!(mac.open(&record), Ok(Some(b"once".to_vec())));
        assert_eq!(mac.open(&record).err(), Some(CryptoError::RecordRejected));
    }

    #[test]
    fn a_record_from_an_earlier_connection_is_refused() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (mut old_phone, _) = connect(&phone_keys, &mac_keys);
        let old_record = old_phone.seal(b"stale").unwrap().remove(0);
        let (_, mut mac) = connect(&phone_keys, &mac_keys);
        assert_eq!(
            mac.open(&old_record).err(),
            Some(CryptoError::RecordRejected)
        );
    }

    /// A record ahead of its turn does not open; the session still expects
    /// the one before it.
    #[test]
    fn a_record_delivered_early_is_refused() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let first = phone.seal(b"first").unwrap().remove(0);
        let second = phone.seal(b"second").unwrap().remove(0);
        assert_eq!(mac.open(&second).err(), Some(CryptoError::RecordRejected));
        assert_eq!(mac.open(&first), Ok(Some(b"first".to_vec())));
    }

    /// Regression vector: with every key and ephemeral fixed, the handshake and
    /// the first record are deterministic. Pinned from this implementation, so
    /// it catches a change to the pattern, prologue, frame type, or record
    /// layout; `snow` itself is checked against the published Noise test
    /// vectors. Each frame is its one-byte type followed by the Noise message;
    /// the reply has message 1's ephemeral key between the two.
    #[test]
    fn the_handshake_and_first_record_are_pinned() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let mac_key = mac_keys.noise_public_key();
        let phone_key = phone_keys.noise_public_key();
        let (mut initiator, message_1) = initiate_from(
            builder(&phone_keys, &mac_key)
                .unwrap()
                .fixed_ephemeral_key_for_testing_only(&[6; KEY_LEN]),
        )
        .unwrap();
        let (_, message_2) = respond_from(
            builder(&mac_keys, &phone_key)
                .unwrap()
                .fixed_ephemeral_key_for_testing_only(&[7; KEY_LEN]),
            &message_1,
        )
        .unwrap();
        let mut phone = initiator.finish(&message_2).unwrap();
        assert_eq!(
            hex(&message_1),
            "02f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d681745575982a1191dddb28db59d58e8665828c0"
        );
        assert_eq!(
            hex(&message_2),
            "03f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d6817455713be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6db1c3c32ce0efa125eeb87c45de97f9de"
        );
        assert_eq!(
            hex(&phone.seal(b"pinned").unwrap()[0]),
            "04d0a3c36bf810c91dc84648de70358b28b3078407f1ec69ae53a290ab8352"
        );
    }
}
