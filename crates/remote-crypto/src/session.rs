//! A connection between two paired devices: a fresh `Noise_KK` handshake with
//! both static keys pinned at pairing, then an encrypted, fragmented record
//! stream. The phone initiates. A new connection is a new handshake, so no
//! key or nonce outlives it, and a record from an earlier connection never
//! opens.

use snow::{Builder, HandshakeState, TransportState};

use crate::CryptoError;
use crate::fragment::{FragmentError, Fragmenter, Reassembler, TAG_LEN};
use crate::frame::{self, FrameKind};
use crate::keys::{DeviceKeys, KEY_LEN};
use crate::pairing::{read, write};

const PATTERN: &str = "Noise_KK_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"switchboard session v1";

/// The phone's state after sending message 1.
pub struct SessionInitiator {
    handshake: HandshakeState,
}

/// An established connection. `seal` and `open` take `&mut self`, so a whole
/// envelope's records are produced without interleaving; records must be
/// transmitted in the order `seal` returns them. The first record that fails
/// to open closes the session for good: every later call returns
/// `SessionClosed`, and the caller reconnects.
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
    confirmed: bool,
    closed: bool,
}

/// Phone: open a connection to the paired Mac whose key it pinned.
pub fn initiate(
    keys: &DeviceKeys,
    peer_noise_public_key: &[u8; KEY_LEN],
) -> Result<(SessionInitiator, Vec<u8>), CryptoError> {
    initiate_with(keys, peer_noise_public_key, None)
}

/// Mac: accept a connection from the paired phone whose key it pinned. A phone
/// holding any other key fails here. The session starts unconfirmed, because
/// message 1 may be a replay.
pub fn respond(
    keys: &DeviceKeys,
    peer_noise_public_key: &[u8; KEY_LEN],
    message_1: &[u8],
) -> Result<(Session, Vec<u8>), CryptoError> {
    respond_with(keys, peer_noise_public_key, message_1, None)
}

impl SessionInitiator {
    pub fn finish(mut self, message_2: &[u8]) -> Result<Session, CryptoError> {
        let message_2 = frame::body(FrameKind::SessionReply, message_2)?;
        if !read(&mut self.handshake, message_2)?.is_empty() {
            return Err(CryptoError::InvalidPayload);
        }
        Session::from_handshake(self.handshake, true)
    }
}

impl Session {
    /// Fragments `envelope` and encrypts every fragment, in order, each as a
    /// `Record` frame.
    pub fn seal(&mut self, envelope: &[u8]) -> Result<Vec<Vec<u8>>, CryptoError> {
        if self.closed {
            return Err(CryptoError::SessionClosed);
        }
        let fragments = match self.fragmenter.fragment(envelope) {
            Ok(fragments) => fragments,
            Err(FragmentError::MessageTooLarge { len }) => {
                return Err(CryptoError::MessageTooLarge { length: len as u64 });
            }
            Err(_) => return Err(self.close()),
        };
        let mut records = Vec::with_capacity(fragments.len());
        for fragment in fragments {
            let mut record = vec![0u8; 1 + fragment.len() + TAG_LEN];
            record[0] = FrameKind::Record as u8;
            match self.transport.write_message(&fragment, &mut record[1..]) {
                Ok(len) => {
                    record.truncate(1 + len);
                    records.push(record);
                }
                Err(_) => return Err(self.close()),
            }
        }
        Ok(records)
    }

    /// Decrypts one `Record` frame. Returns the envelope once its last record
    /// arrives, `None` while it is still incomplete. The first record that
    /// decrypts confirms the session.
    ///
    /// Failures say which kind they are, because a device's records may be
    /// offered to more than one session:
    /// - `UnexpectedFrame`: not a record, or oversize. Nothing is touched; the
    ///   session stays open.
    /// - `RecordRejected`: did not decrypt here. The session is closed; the
    ///   record may belong to another session of the same device.
    /// - `ProtocolViolation`: decrypted, but broke the fragment rules. The
    ///   session is closed; the record belongs to no other session.
    pub fn open(&mut self, record: &[u8]) -> Result<Option<Vec<u8>>, CryptoError> {
        if self.closed {
            return Err(CryptoError::SessionClosed);
        }
        // Checked, including the length, before allocating anything: the
        // record comes straight off the relay.
        let record = frame::body(FrameKind::Record, record)?;
        let mut fragment = vec![0u8; record.len()];
        let Ok(len) = self.transport.read_message(record, &mut fragment) else {
            self.close();
            return Err(CryptoError::RecordRejected);
        };
        self.confirmed = true;
        fragment.truncate(len);
        self.reassembler.accept(&fragment).map_err(|_| {
            self.close();
            CryptoError::ProtocolViolation
        })
    }

    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn from_handshake(handshake: HandshakeState, confirmed: bool) -> Result<Self, CryptoError> {
        Ok(Self {
            transport: handshake
                .into_transport_mode()
                .map_err(|_| CryptoError::HandshakeFailed)?,
            fragmenter: Fragmenter::default(),
            reassembler: Reassembler::default(),
            confirmed,
            closed: false,
        })
    }

    fn close(&mut self) -> CryptoError {
        self.closed = true;
        self.reassembler.reset();
        CryptoError::SessionClosed
    }
}

fn initiate_with(
    keys: &DeviceKeys,
    peer_noise_public_key: &[u8; KEY_LEN],
    ephemeral: Option<&[u8; KEY_LEN]>,
) -> Result<(SessionInitiator, Vec<u8>), CryptoError> {
    let mut handshake = builder(keys, peer_noise_public_key, ephemeral)?
        .build_initiator()
        .map_err(|_| CryptoError::HandshakeFailed)?;
    let message_1 = write(&mut handshake, &[])?;
    Ok((
        SessionInitiator { handshake },
        frame::tagged(FrameKind::SessionRequest, &message_1),
    ))
}

fn respond_with(
    keys: &DeviceKeys,
    peer_noise_public_key: &[u8; KEY_LEN],
    message_1: &[u8],
    ephemeral: Option<&[u8; KEY_LEN]>,
) -> Result<(Session, Vec<u8>), CryptoError> {
    let mut handshake = builder(keys, peer_noise_public_key, ephemeral)?
        .build_responder()
        .map_err(|_| CryptoError::HandshakeFailed)?;
    let message_1 = frame::body(FrameKind::SessionRequest, message_1)?;
    if !read(&mut handshake, message_1)?.is_empty() {
        return Err(CryptoError::InvalidPayload);
    }
    let message_2 = write(&mut handshake, &[])?;
    Ok((
        Session::from_handshake(handshake, false)?,
        frame::tagged(FrameKind::SessionReply, &message_2),
    ))
}

fn builder<'a>(
    keys: &'a DeviceKeys,
    peer_noise_public_key: &'a [u8; KEY_LEN],
    ephemeral: Option<&'a [u8; KEY_LEN]>,
) -> Result<Builder<'a>, CryptoError> {
    let params = PATTERN.parse().map_err(|_| CryptoError::HandshakeFailed)?;
    let mut builder = Builder::new(params)
        .local_private_key(keys.noise_private_key())
        .and_then(|b| b.remote_public_key(peer_noise_public_key))
        .and_then(|b| b.prologue(PROLOGUE))
        .map_err(|_| CryptoError::HandshakeFailed)?;
    if let Some(ephemeral) = ephemeral {
        builder = builder.fixed_ephemeral_key_for_testing_only(ephemeral);
    }
    Ok(builder)
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
        let (initiator, message_1) = initiate(phone, &mac.noise_public_key()).unwrap();
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
        let (initiator, message_1) = initiate(&phone, &mac.noise_public_key()).unwrap();
        assert_eq!(
            respond(&stranger, &phone.noise_public_key(), &message_1).err(),
            Some(CryptoError::HandshakeFailed)
        );
        let (_, message_2) = respond(&mac, &phone.noise_public_key(), &message_1).unwrap();
        let mut forged = message_2.clone();
        forged[5] ^= 1;
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
        let (initiator, message_1) = initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
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
        let (initiator, message_1) = initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
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
        let (initiator, _) = initiate(&phone_keys, &mac_keys.noise_public_key()).unwrap();
        assert_eq!(
            initiator.finish(&message_1).err(),
            Some(CryptoError::UnexpectedFrame)
        );
    }

    /// A record the phone encrypted under this session's keys whose fragment
    /// breaks the sequence rules.
    fn authentic_but_malformed_record(from: &mut Session) -> Vec<u8> {
        let fragment = [0, 0, 0, 0, 0, 1, 0, 2, 0xAB];
        let mut record = vec![0u8; 1 + fragment.len() + TAG_LEN];
        record[0] = FrameKind::Record as u8;
        let len = from
            .transport
            .write_message(&fragment, &mut record[1..])
            .unwrap();
        record.truncate(1 + len);
        record
    }

    #[test]
    fn an_authentic_malformed_record_is_a_protocol_violation_and_confirms() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let record = authentic_but_malformed_record(&mut phone);
        assert_eq!(
            mac.open(&record).err(),
            Some(CryptoError::ProtocolViolation)
        );
        assert!(mac.is_confirmed());
        assert!(mac.is_closed());
    }

    /// The Mac's routing while a phone reconnects: a record that does not
    /// decrypt on the new, unconfirmed session falls back to the old one; one
    /// that decrypts but is malformed does not, and the old session survives.
    #[test]
    fn routing_falls_back_only_after_a_rejected_record() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (mut old_phone, mut old_mac) = connect(&phone_keys, &mac_keys);
        deliver(&mut old_phone, &mut old_mac, b"hello");

        let (_, mut pending) = connect(&phone_keys, &mac_keys);
        let old_record = old_phone.seal(b"on the old keys").unwrap().remove(0);
        assert_eq!(
            pending.open(&old_record).err(),
            Some(CryptoError::RecordRejected)
        );
        assert_eq!(
            old_mac.open(&old_record),
            Ok(Some(b"on the old keys".to_vec()))
        );

        let (mut new_phone, mut pending) = connect(&phone_keys, &mac_keys);
        let malformed = authentic_but_malformed_record(&mut new_phone);
        assert_eq!(
            pending.open(&malformed).err(),
            Some(CryptoError::ProtocolViolation)
        );
        assert!(!old_mac.is_closed());
        assert_eq!(
            deliver(&mut old_phone, &mut old_mac, b"still here"),
            Some(b"still here".to_vec())
        );
    }

    #[test]
    fn records_are_sized_to_their_content() {
        let (mut phone, _) = connect(&keys(1), &keys(2));
        let record = phone.seal(b"small").unwrap().remove(0);
        assert!(record.capacity() < 128, "{}", record.capacity());
    }

    #[test]
    fn a_tampered_record_closes_the_session() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let mut record = phone.seal(b"hello").unwrap().remove(0);
        record[1] ^= 1;
        assert_eq!(mac.open(&record).err(), Some(CryptoError::RecordRejected));
        assert!(mac.is_closed());
        assert_eq!(mac.seal(b"after").err(), Some(CryptoError::SessionClosed));
        let next = phone.seal(b"next").unwrap().remove(0);
        assert_eq!(mac.open(&next).err(), Some(CryptoError::SessionClosed));
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

    #[test]
    fn records_delivered_out_of_order_are_refused() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let first = phone.seal(b"first").unwrap().remove(0);
        let second = phone.seal(b"second").unwrap().remove(0);
        assert_eq!(mac.open(&second).err(), Some(CryptoError::RecordRejected));
        assert_eq!(mac.open(&first).err(), Some(CryptoError::SessionClosed));
    }

    /// Regression vector: with every key and ephemeral fixed, the handshake and
    /// the first record are deterministic. Pinned from this implementation, so
    /// it catches a change to the pattern, prologue, frame type, or record
    /// layout; `snow` itself is checked against the published Noise test
    /// vectors. Each frame is its one-byte type followed by the Noise message.
    #[test]
    fn the_handshake_and_first_record_are_pinned() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (initiator, message_1) = initiate_with(
            &phone_keys,
            &mac_keys.noise_public_key(),
            Some(&[6; KEY_LEN]),
        )
        .unwrap();
        let (_, message_2) = respond_with(
            &mac_keys,
            &phone_keys.noise_public_key(),
            &message_1,
            Some(&[7; KEY_LEN]),
        )
        .unwrap();
        let mut phone = initiator.finish(&message_2).unwrap();
        assert_eq!(
            hex(&message_1),
            "02f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d681745575982a1191dddb28db59d58e8665828c0"
        );
        assert_eq!(
            hex(&message_2),
            "0313be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6db1c3c32ce0efa125eeb87c45de97f9de"
        );
        assert_eq!(
            hex(&phone.seal(b"pinned").unwrap()[0]),
            "04d0a3c36bf810c91dc84648de70358b28b3078407f1ec69ae53a290ab8352"
        );
    }
}
