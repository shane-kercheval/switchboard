//! The phone's side of the crate, exported to Swift through `UniFFI`. Thin
//! objects over the core types, which stay free of FFI concerns; the Mac uses
//! the core directly.
//!
//! `UniFFI` objects can be called from any thread, so each keeps its state
//! behind a `Mutex`. A handshake step is used up by its first real message, and
//! a later call returns `HandshakeFailed` rather than panicking. A poisoned
//! lock — a panic during an earlier call — reports the object unusable:
//! `HandshakeFailed` for a handshake, `SessionClosed` for a session.
//!
//! Bytes crossing to Swift are copied into Swift-owned memory, which this
//! crate cannot wipe. Only `DeviceKeys::storage_bytes` returns secret bytes,
//! and the Swift side hands them straight to the Keychain. On the way, `UniFFI`
//! also lowers them into a `RustBuffer`, which is freed without being wiped:
//! one more unwiped copy per Keychain write.
#![allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI lifts arguments from Swift as owned values"
)]

use std::sync::{Arc, Mutex, MutexGuard};

use zeroize::Zeroizing;

use crate::frame::{self, FrameKind as CoreFrameKind};
use crate::keys::{DeviceKeys as CoreKeys, KEY_LEN};
use crate::pairing::{self, PSK_LEN, PairingOffer, PhoneAwaitingResponse};
use crate::session::{self, Session as CoreSession, SessionInitiator};
use crate::{CryptoError, confirmation_code};

/// A device's long-term keys. Swift never sees a private key: only the
/// opaque storage blob, for the Keychain.
#[derive(uniffi::Object)]
pub struct DeviceKeys {
    keys: CoreKeys,
}

#[uniffi::export]
impl DeviceKeys {
    /// Fresh keys from the operating system's random number generator.
    #[uniffi::constructor]
    pub fn generate() -> Result<Arc<Self>, CryptoError> {
        Ok(Arc::new(Self {
            keys: CoreKeys::generate()?,
        }))
    }

    /// Keys from `storage_bytes`, as read back from the Keychain.
    #[uniffi::constructor]
    pub fn restore(blob: Vec<u8>) -> Result<Arc<Self>, CryptoError> {
        let blob = Zeroizing::new(blob);
        Ok(Arc::new(Self {
            keys: CoreKeys::restore(&blob)?,
        }))
    }

    /// The versioned blob the Keychain stores. Opaque: Swift stores and
    /// returns it, nothing more.
    pub fn storage_bytes(&self) -> Vec<u8> {
        self.keys.storage_bytes().to_vec()
    }

    pub fn device_id(&self) -> String {
        self.keys.device_id()
    }

    pub fn noise_public_key(&self) -> Vec<u8> {
        self.keys.noise_public_key().to_vec()
    }

    pub fn identity_public_key(&self) -> Vec<u8> {
        self.keys.identity_public_key().to_bytes().to_vec()
    }

    /// Answers the relay's registration challenge.
    pub fn sign_relay_challenge(&self, challenge: Vec<u8>) -> Vec<u8> {
        self.keys
            .sign_relay_challenge(&challenge)
            .to_bytes()
            .to_vec()
    }
}

/// The phone's side of pairing, from the QR code to message 3.
#[derive(uniffi::Object)]
pub struct PairingHandshake {
    state: Mutex<PhoneAwaitingResponse>,
    message_1: Vec<u8>,
}

/// What the phone learns from the Mac's message 2, and the message 3 to send.
/// Not trusted yet: the phone keeps the Mac's key only once the Mac reports
/// that the user typed `confirmation_code`.
#[derive(uniffi::Record)]
pub struct PairingResponse {
    pub mac_noise_public_key: Vec<u8>,
    pub mac_name: String,
    pub confirmation_code: String,
    pub message_3: Vec<u8>,
}

#[uniffi::export]
impl PairingHandshake {
    /// Starts pairing with the Mac described by the QR code: its Noise public
    /// key and the pre-shared key.
    #[uniffi::constructor]
    pub fn new(
        keys: Arc<DeviceKeys>,
        mac_noise_public_key: Vec<u8>,
        psk: Vec<u8>,
    ) -> Result<Arc<Self>, CryptoError> {
        let psk = Zeroizing::new(psk);
        let offer = PairingOffer {
            mac_noise_public_key: key(&mac_noise_public_key)?,
            psk: Zeroizing::new(
                <[u8; PSK_LEN]>::try_from(psk.as_slice()).map_err(|_| CryptoError::InvalidKey)?,
            ),
        };
        let (state, message_1) = pairing::phone_start(&keys.keys, &offer)?;
        Ok(Arc::new(Self {
            state: Mutex::new(state),
            message_1,
        }))
    }

    pub fn message_1(&self) -> Vec<u8> {
        self.message_1.clone()
    }

    /// Reads the Mac's message 2. A frame of another type returns
    /// `UnexpectedFrame` and leaves the handshake waiting; a real message 2
    /// uses it up, whatever the outcome.
    pub fn respond(
        &self,
        phone_name: String,
        message_2: Vec<u8>,
    ) -> Result<PairingResponse, CryptoError> {
        let mut state = lock(&self.state, CryptoError::HandshakeFailed)?;
        let (mac, message_3) = state.respond(&phone_name, &message_2)?;
        Ok(PairingResponse {
            mac_noise_public_key: mac.noise_public_key.to_vec(),
            mac_name: mac.name,
            confirmation_code: confirmation_code(mac.handshake_hash.to_vec())?,
            message_3,
        })
    }
}

/// The phone's side of opening a connection to its paired Mac.
#[derive(uniffi::Object)]
pub struct SessionHandshake {
    state: Mutex<SessionInitiator>,
    message_1: Vec<u8>,
}

#[uniffi::export]
impl SessionHandshake {
    #[uniffi::constructor]
    pub fn new(
        keys: Arc<DeviceKeys>,
        mac_noise_public_key: Vec<u8>,
    ) -> Result<Arc<Self>, CryptoError> {
        let (state, message_1) = session::initiate(&keys.keys, &key(&mac_noise_public_key)?)?;
        Ok(Arc::new(Self {
            state: Mutex::new(state),
            message_1,
        }))
    }

    pub fn message_1(&self) -> Vec<u8> {
        self.message_1.clone()
    }

    /// Reads the Mac's reply. A frame of another type returns
    /// `UnexpectedFrame` and leaves the handshake waiting; a real reply uses it
    /// up, whatever the outcome.
    pub fn finish(&self, message_2: Vec<u8>) -> Result<Arc<Session>, CryptoError> {
        let session = lock(&self.state, CryptoError::HandshakeFailed)?.finish(&message_2)?;
        Ok(Arc::new(Session::new(session)))
    }
}

/// The Mac's side of a connection, and its reply. The iPhone app never plays
/// the Mac; this exists so the binding's round trip can be tested on the
/// device, through the same code the Mac runs.
#[derive(uniffi::Record)]
pub struct SessionAcceptance {
    pub session: Arc<Session>,
    pub message_2: Vec<u8>,
}

/// The Mac's half of `SessionHandshake`. See `SessionAcceptance`.
#[uniffi::export]
pub fn accept_session(
    keys: Arc<DeviceKeys>,
    phone_noise_public_key: Vec<u8>,
    message_1: Vec<u8>,
) -> Result<SessionAcceptance, CryptoError> {
    let (session, message_2) =
        session::respond(&keys.keys, &key(&phone_noise_public_key)?, &message_1)?;
    Ok(SessionAcceptance {
        session: Arc::new(Session::new(session)),
        message_2,
    })
}

/// An established connection. One `seal` call produces all of an envelope's
/// records under the lock, so concurrent calls never interleave records; the
/// caller must still transmit them in the order returned, one envelope at a
/// time.
///
/// The phone has one session, so a record it cannot decrypt has nowhere else
/// to go. Before the session has opened any record, such a record is
/// ignored: after a reconnect the Mac keeps sending on its old session until
/// the phone's first record confirms the new one, and the relay delivers those
/// stale records first. After the first record opens, the Mac sends only on
/// this session, so a record that does not decrypt means the stream is broken:
/// the session closes and `open` returns `StreamBroken`. The transport bounds
/// the first window with a deadline (`has_opened_record`), since a damaged
/// first record would otherwise leave it waiting.
#[derive(uniffi::Object)]
pub struct Session {
    state: Mutex<SessionState>,
}

struct SessionState {
    session: CoreSession,
    opened_record: bool,
}

impl Session {
    fn new(session: CoreSession) -> Self {
        Self {
            state: Mutex::new(SessionState {
                session,
                opened_record: false,
            }),
        }
    }
}

#[uniffi::export]
impl Session {
    pub fn seal(&self, envelope: Vec<u8>) -> Result<Vec<Vec<u8>>, CryptoError> {
        lock(&self.state, CryptoError::SessionClosed)?
            .session
            .seal(&envelope)
    }

    /// The envelope once its last record arrives, `None` before then.
    /// `RecordRejected` before the first record opens leaves the session as it
    /// was; after it, a record that does not decrypt closes the session and
    /// returns `StreamBroken`.
    pub fn open(&self, record: Vec<u8>) -> Result<Option<Vec<u8>>, CryptoError> {
        let mut state = lock(&self.state, CryptoError::SessionClosed)?;
        match state.session.open(&record) {
            Ok(envelope) => {
                state.opened_record = true;
                Ok(envelope)
            }
            Err(CryptoError::RecordRejected) if state.opened_record => {
                state.session.close();
                Err(CryptoError::StreamBroken)
            }
            Err(error) => Err(error),
        }
    }

    /// Whether any record has decrypted on this session. The transport ends a
    /// session that has not opened one within its deadline.
    pub fn has_opened_record(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.opened_record)
    }

    /// Closed by a malformed record, a broken stream, or a failed send, or
    /// unusable because an earlier call panicked.
    pub fn is_closed(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |state| state.session.is_closed())
    }
}

/// What a frame is, so the phone routes it to the right step without touching
/// any state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FrameKind {
    Pairing,
    SessionRequest,
    SessionReply,
    Record,
}

/// Classifies a frame by its first byte alone, so routing does not copy the
/// whole frame across the boundary. The step that consumes the frame checks it
/// in full.
#[uniffi::export]
pub fn frame_kind(tag: u8) -> Result<FrameKind, CryptoError> {
    Ok(match frame::kind_of_tag(tag)? {
        CoreFrameKind::Pairing => FrameKind::Pairing,
        CoreFrameKind::SessionRequest => FrameKind::SessionRequest,
        CoreFrameKind::SessionReply => FrameKind::SessionReply,
        CoreFrameKind::Record => FrameKind::Record,
    })
}

fn key(bytes: &[u8]) -> Result<[u8; KEY_LEN], CryptoError> {
    <[u8; KEY_LEN]>::try_from(bytes).map_err(|_| CryptoError::InvalidKey)
}

fn lock<T>(mutex: &Mutex<T>, poisoned: CryptoError) -> Result<MutexGuard<'_, T>, CryptoError> {
    mutex.lock().map_err(|_| poisoned)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::thread;

    use super::*;
    use crate::fragment::MAX_CHUNK_LEN;

    struct Pair {
        phone: Arc<Session>,
        mac: Arc<Session>,
    }

    fn connect() -> Pair {
        let phone_keys = DeviceKeys::generate().unwrap();
        let mac_keys = DeviceKeys::generate().unwrap();
        let handshake =
            SessionHandshake::new(phone_keys.clone(), mac_keys.noise_public_key()).unwrap();
        let accepted = accept_session(
            mac_keys,
            phone_keys.noise_public_key(),
            handshake.message_1(),
        )
        .unwrap();
        Pair {
            phone: handshake.finish(accepted.message_2).unwrap(),
            mac: accepted.session,
        }
    }

    /// Several threads seal multi-record envelopes on one session. If any
    /// envelope's records were interleaved with another's, the nonces would
    /// not run contiguously within it, and no order of envelopes would open.
    /// The receiver is the core session, on which a record that does not
    /// decrypt changes nothing, so it can try each remaining envelope in turn
    /// and must always find the next one. (The FFI session's phone rule would
    /// end the stream at the first wrong guess; this test is about `seal`.)
    #[test]
    fn concurrent_seals_never_interleave_records() {
        let Pair { phone, mac } = connect();
        let batches: Vec<Vec<Vec<u8>>> = thread::scope(|scope| {
            let handles: Vec<_> = (0..8u8)
                .map(|n| {
                    let phone = &phone;
                    scope.spawn(move || phone.seal(vec![n; 2 * MAX_CHUNK_LEN + 7]).unwrap())
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(batches.iter().all(|records| records.len() == 3));

        let mut receiver = mac.state.lock().unwrap();
        let mut open = |record: &Vec<u8>| receiver.session.open(record);
        let mut remaining: Vec<usize> = (0..batches.len()).collect();
        let mut opened = HashSet::new();
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .position(|&batch| match open(&batches[batch][0]) {
                    Err(CryptoError::RecordRejected) => false,
                    Ok(None) => true,
                    other => panic!("unexpected {other:?}"),
                })
                .expect("no envelope's first record is next: records were interleaved");
            let batch = remaining.remove(next);
            assert_eq!(open(&batches[batch][1]), Ok(None));
            let envelope = open(&batches[batch][2]).unwrap().unwrap();
            assert!(envelope.iter().all(|&b| b == envelope[0]));
            assert!(opened.insert(envelope[0]));
        }
        assert_eq!(opened.len(), 8);
    }

    #[test]
    fn envelopes_cross_both_ways_through_the_ffi_objects() {
        let Pair { phone, mac } = connect();
        for (from, to) in [(&phone, &mac), (&mac, &phone)] {
            let envelope: Vec<u8> = (0..3 * MAX_CHUNK_LEN)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect();
            let mut result = None;
            for record in from.seal(envelope.clone()).unwrap() {
                result = to.open(record).unwrap();
            }
            assert_eq!(result, Some(envelope));
        }
    }

    #[test]
    fn a_session_handshake_is_used_up_by_its_reply() {
        let phone_keys = DeviceKeys::generate().unwrap();
        let mac_keys = DeviceKeys::generate().unwrap();
        let handshake =
            SessionHandshake::new(phone_keys.clone(), mac_keys.noise_public_key()).unwrap();
        let accepted = accept_session(
            mac_keys,
            phone_keys.noise_public_key(),
            handshake.message_1(),
        )
        .unwrap();
        assert_eq!(
            handshake.finish(handshake.message_1()).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        assert!(handshake.finish(accepted.message_2.clone()).is_ok());
        assert_eq!(
            handshake.finish(accepted.message_2).err(),
            Some(CryptoError::HandshakeFailed)
        );
    }

    #[test]
    fn a_pairing_handshake_is_used_up_by_its_message_2() {
        let phone_keys = DeviceKeys::generate().unwrap();
        let mac_keys = CoreKeys::generate().unwrap();
        let psk = [9u8; PSK_LEN];
        let handshake = PairingHandshake::new(
            phone_keys,
            mac_keys.noise_public_key().to_vec(),
            psk.to_vec(),
        )
        .unwrap();
        let (_, message_2) =
            pairing::mac_respond(&mac_keys, &psk, "Studio Mac", &handshake.message_1()).unwrap();
        let response = handshake
            .respond("Jo's iPhone".into(), message_2.clone())
            .unwrap();
        assert_eq!(response.mac_name, "Studio Mac");
        assert_eq!(
            response.mac_noise_public_key,
            mac_keys.noise_public_key().to_vec()
        );
        assert_eq!(response.confirmation_code.len(), 6);
        assert_eq!(
            handshake.respond("Jo's iPhone".into(), message_2).err(),
            Some(CryptoError::HandshakeFailed)
        );
    }

    #[test]
    fn keys_of_the_wrong_length_are_refused() {
        let keys = DeviceKeys::generate().unwrap();
        assert_eq!(
            SessionHandshake::new(keys.clone(), vec![0; KEY_LEN - 1]).err(),
            Some(CryptoError::InvalidKey)
        );
        assert_eq!(
            PairingHandshake::new(keys, vec![0; KEY_LEN], vec![0; PSK_LEN + 1]).err(),
            Some(CryptoError::InvalidKey)
        );
    }

    #[test]
    fn stored_keys_restore_to_the_same_device() {
        let keys = DeviceKeys::generate().unwrap();
        let restored = DeviceKeys::restore(keys.storage_bytes()).unwrap();
        assert_eq!(restored.device_id(), keys.device_id());
        assert_eq!(restored.noise_public_key(), keys.noise_public_key());
    }

    #[test]
    fn a_poisoned_session_reports_itself_closed() {
        let Pair { phone, .. } = connect();
        let _ = thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = phone.state.lock().unwrap();
                    panic!("poison the lock");
                })
                .join()
        });
        assert!(phone.is_closed());
        assert_eq!(
            phone.seal(b"x".to_vec()).err(),
            Some(CryptoError::SessionClosed)
        );
    }

    #[test]
    fn frames_are_classified_without_a_session() {
        let Pair { phone, .. } = connect();
        let record = phone.seal(b"hi".to_vec()).unwrap().remove(0);
        assert_eq!(frame_kind(record[0]), Ok(FrameKind::Record));
        for (tag, kind) in [
            (1, FrameKind::Pairing),
            (2, FrameKind::SessionRequest),
            (3, FrameKind::SessionReply),
            (4, FrameKind::Record),
        ] {
            assert_eq!(frame_kind(tag), Ok(kind));
        }
        for tag in [0, 5, u8::MAX] {
            assert_eq!(frame_kind(tag), Err(CryptoError::UnexpectedFrame));
        }
    }

    /// After a reconnect the Mac keeps sending on its old session until the
    /// phone's first record confirms the new one, and those records reach the
    /// phone first. They are ignored, and the new session then works.
    #[test]
    fn stale_records_before_the_first_open_are_ignored() {
        let phone_keys = DeviceKeys::generate().unwrap();
        let mac_keys = DeviceKeys::generate().unwrap();
        let first = SessionHandshake::new(phone_keys.clone(), mac_keys.noise_public_key()).unwrap();
        let old = accept_session(
            mac_keys.clone(),
            phone_keys.noise_public_key(),
            first.message_1(),
        )
        .unwrap();
        first.finish(old.message_2).unwrap();

        let second =
            SessionHandshake::new(phone_keys.clone(), mac_keys.noise_public_key()).unwrap();
        let new =
            accept_session(mac_keys, phone_keys.noise_public_key(), second.message_1()).unwrap();
        let phone = second.finish(new.message_2).unwrap();

        for stale in old.session.seal(b"on the old session".to_vec()).unwrap() {
            assert_eq!(phone.open(stale), Err(CryptoError::RecordRejected));
        }
        assert!(!phone.is_closed());
        assert!(!phone.has_opened_record());
        let record = new
            .session
            .seal(b"on the new one".to_vec())
            .unwrap()
            .remove(0);
        assert_eq!(phone.open(record), Ok(Some(b"on the new one".to_vec())));
        assert!(phone.has_opened_record());
    }

    /// Once a record has opened, the Mac sends only on this session, so a
    /// record that does not decrypt — here, one lost in transit, leaving the
    /// next a nonce ahead — means the stream is broken. Without this the phone
    /// would wait forever, every later record rejected.
    #[test]
    fn a_rejected_record_after_the_first_open_breaks_the_stream() {
        let Pair { phone, mac } = connect();
        let first = mac.seal(b"one".to_vec()).unwrap().remove(0);
        assert_eq!(phone.open(first), Ok(Some(b"one".to_vec())));
        let _lost = mac.seal(b"two".to_vec()).unwrap();
        let third = mac.seal(b"three".to_vec()).unwrap().remove(0);
        assert_eq!(phone.open(third.clone()), Err(CryptoError::StreamBroken));
        assert!(phone.is_closed());
        assert_eq!(phone.open(third), Err(CryptoError::SessionClosed));
    }
}
