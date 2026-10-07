//! A connection between two paired devices: a fresh `Noise_KK` handshake with
//! both static keys pinned at pairing, then an encrypted, fragmented record
//! stream. The phone initiates. A new connection is a new handshake, so no
//! key or nonce outlives it, and a record from an earlier connection never
//! opens.

use snow::{Builder, HandshakeState, TransportState};

use crate::CryptoError;
use crate::fragment::{FragmentError, Fragmenter, MAX_FRAGMENT_LEN, MAX_RECORD_LEN, Reassembler};
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
pub struct Session {
    transport: TransportState,
    fragmenter: Fragmenter,
    reassembler: Reassembler,
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
/// holding any other key fails here.
pub fn respond(
    keys: &DeviceKeys,
    peer_noise_public_key: &[u8; KEY_LEN],
    message_1: &[u8],
) -> Result<(Session, Vec<u8>), CryptoError> {
    respond_with(keys, peer_noise_public_key, message_1, None)
}

impl SessionInitiator {
    pub fn finish(mut self, message_2: &[u8]) -> Result<Session, CryptoError> {
        if !read(&mut self.handshake, message_2)?.is_empty() {
            return Err(CryptoError::InvalidPayload);
        }
        Session::from_handshake(self.handshake)
    }
}

impl Session {
    /// Fragments `envelope` and encrypts every fragment, in order.
    pub fn seal(&mut self, envelope: &[u8]) -> Result<Vec<Vec<u8>>, CryptoError> {
        if self.closed {
            return Err(CryptoError::SessionClosed);
        }
        let fragments = self
            .fragmenter
            .fragment(envelope)
            .map_err(|error| match error {
                FragmentError::MessageTooLarge { len } => {
                    CryptoError::MessageTooLarge { length: len as u64 }
                }
                _ => CryptoError::SessionClosed,
            })?;
        let mut records = Vec::with_capacity(fragments.len());
        for fragment in fragments {
            let mut record = vec![0u8; MAX_RECORD_LEN];
            match self.transport.write_message(&fragment, &mut record) {
                Ok(len) => {
                    record.truncate(len);
                    records.push(record);
                }
                Err(_) => return Err(self.close()),
            }
        }
        Ok(records)
    }

    /// Decrypts one record. Returns the envelope once its last record arrives,
    /// `None` while it is still incomplete.
    pub fn open(&mut self, record: &[u8]) -> Result<Option<Vec<u8>>, CryptoError> {
        if self.closed {
            return Err(CryptoError::SessionClosed);
        }
        let mut fragment = vec![0u8; MAX_FRAGMENT_LEN];
        let Ok(len) = self.transport.read_message(record, &mut fragment) else {
            return Err(self.close());
        };
        fragment.truncate(len);
        self.reassembler.accept(&fragment).map_err(|_| self.close())
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn from_handshake(handshake: HandshakeState) -> Result<Self, CryptoError> {
        Ok(Self {
            transport: handshake
                .into_transport_mode()
                .map_err(|_| CryptoError::HandshakeFailed)?,
            fragmenter: Fragmenter::default(),
            reassembler: Reassembler::default(),
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
    Ok((SessionInitiator { handshake }, message_1))
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
    if !read(&mut handshake, message_1)?.is_empty() {
        return Err(CryptoError::InvalidPayload);
    }
    let message_2 = write(&mut handshake, &[])?;
    Ok((Session::from_handshake(handshake)?, message_2))
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
    use crate::fragment::{MAX_CHUNK_LEN, MAX_MESSAGE_LEN};
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
            assert!(record.len() <= MAX_RECORD_LEN);
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
    fn a_tampered_record_closes_the_session() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let mut record = phone.seal(b"hello").unwrap().remove(0);
        record[0] ^= 1;
        assert_eq!(mac.open(&record).err(), Some(CryptoError::SessionClosed));
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
        assert_eq!(mac.open(&record).err(), Some(CryptoError::SessionClosed));
    }

    #[test]
    fn a_record_from_an_earlier_connection_is_refused() {
        let (phone_keys, mac_keys) = (keys(1), keys(2));
        let (mut old_phone, _) = connect(&phone_keys, &mac_keys);
        let old_record = old_phone.seal(b"stale").unwrap().remove(0);
        let (_, mut mac) = connect(&phone_keys, &mac_keys);
        assert_eq!(
            mac.open(&old_record).err(),
            Some(CryptoError::SessionClosed)
        );
    }

    #[test]
    fn records_delivered_out_of_order_are_refused() {
        let (mut phone, mut mac) = connect(&keys(1), &keys(2));
        let first = phone.seal(b"first").unwrap().remove(0);
        let second = phone.seal(b"second").unwrap().remove(0);
        assert_eq!(mac.open(&second).err(), Some(CryptoError::SessionClosed));
        assert_eq!(mac.open(&first).err(), Some(CryptoError::SessionClosed));
    }

    /// Regression vector: with every key and ephemeral fixed, the handshake and
    /// the first record are deterministic. Pinned from this implementation, so
    /// it catches a change to the pattern, prologue, or record layout; `snow`
    /// itself is checked against the published Noise test vectors.
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
            "f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d681745575982a1191dddb28db59d58e8665828c0"
        );
        assert_eq!(
            hex(&message_2),
            "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6db1c3c32ce0efa125eeb87c45de97f9de"
        );
        assert_eq!(
            hex(&phone.seal(b"pinned").unwrap()[0]),
            "d0a3c36bf810c91dc84648de70358b28b3078407f1ec69ae53a290ab8352"
        );
    }
}
