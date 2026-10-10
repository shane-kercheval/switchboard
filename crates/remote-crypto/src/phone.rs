//! The phone's one session with its Mac, and whether anything has arrived on
//! it. The Mac's half is `DeviceSessions`.
//!
//! Every record names its session, so the phone needs no guess about where a
//! record came from. A record naming another session — a leftover from the
//! Mac's previous connection, which it may keep sending on until the phone's
//! `hello` confirms the new one — is `StaleRecord` and ignored. A record
//! naming this session that does not decrypt closes it (`StreamBroken`; see
//! `Session::open`). The Mac answers `hello` at once, so the phone's
//! transport ends a session that has opened no record within its deadline
//! (`has_opened_record`).

use crate::CryptoError;
use crate::session::Session;

pub struct PhoneSession {
    session: Session,
    opened_record: bool,
}

impl PhoneSession {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            opened_record: false,
        }
    }

    /// See `Session::seal`.
    pub fn seal(&mut self, envelope: &[u8]) -> Result<Vec<Vec<u8>>, CryptoError> {
        self.session.seal(envelope)
    }

    /// See `Session::open`.
    pub fn open(&mut self, record: &[u8]) -> Result<Option<Vec<u8>>, CryptoError> {
        let envelope = self.session.open(record)?;
        self.opened_record = true;
        Ok(envelope)
    }

    /// Whether any record has decrypted on this session.
    pub fn has_opened_record(&self) -> bool {
        self.opened_record
    }

    pub fn is_closed(&self) -> bool {
        self.session.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameKind;
    use crate::keys::DeviceKeys;
    use crate::session::{self, Session};

    /// The phone's session and the Mac's, over one connection.
    fn connect(phone: &DeviceKeys, mac: &DeviceKeys) -> (PhoneSession, Session) {
        let (mut initiator, message_1) = session::initiate(phone, &mac.noise_public_key()).unwrap();
        let (mac_session, message_2) =
            session::respond(mac, &phone.noise_public_key(), &message_1).unwrap();
        (
            PhoneSession::new(initiator.finish(&message_2).unwrap()),
            mac_session,
        )
    }

    fn keys() -> (DeviceKeys, DeviceKeys) {
        (
            DeviceKeys::generate().unwrap(),
            DeviceKeys::generate().unwrap(),
        )
    }

    /// Records from the Mac's old session are ignored whenever they arrive,
    /// before the new session's first record or after it.
    #[test]
    fn records_for_another_session_are_ignored() {
        let (phone_keys, mac_keys) = keys();
        let (_, mut old) = connect(&phone_keys, &mac_keys);
        let (mut phone, mut new) = connect(&phone_keys, &mac_keys);
        for stale in old.seal(b"on the old session").unwrap() {
            assert_eq!(phone.open(&stale), Err(CryptoError::StaleRecord));
        }
        assert!(!phone.is_closed());
        assert!(!phone.has_opened_record());
        let record = new.seal(b"hello").unwrap().remove(0);
        assert_eq!(phone.open(&record), Ok(Some(b"hello".to_vec())));
        assert!(phone.has_opened_record());
        let late = old.seal(b"late").unwrap().remove(0);
        assert_eq!(phone.open(&late), Err(CryptoError::StaleRecord));
        assert!(!phone.is_closed());
    }

    /// A damaged first record needs no deadline to be noticed: it names this
    /// session, so it breaks the stream at once.
    #[test]
    fn a_damaged_first_record_breaks_the_stream() {
        let (phone_keys, mac_keys) = keys();
        let (mut phone, mut mac) = connect(&phone_keys, &mac_keys);
        let mut record = mac.seal(b"hello").unwrap().remove(0);
        let last = record.len() - 1;
        record[last] ^= 1;
        assert_eq!(phone.open(&record), Err(CryptoError::StreamBroken));
        assert!(phone.is_closed());
    }

    /// A record lost in transit leaves the next a nonce ahead. Without this
    /// rule the phone would wait forever, every later record rejected.
    #[test]
    fn a_rejected_record_after_the_first_open_breaks_the_stream() {
        let (phone_keys, mac_keys) = keys();
        let (mut phone, mut mac) = connect(&phone_keys, &mac_keys);
        let first = mac.seal(b"hello").unwrap().remove(0);
        assert_eq!(phone.open(&first), Ok(Some(b"hello".to_vec())));
        let _lost = mac.seal(b"two").unwrap();
        let third = mac.seal(b"three").unwrap().remove(0);
        assert_eq!(phone.open(&third), Err(CryptoError::StreamBroken));
        assert!(phone.is_closed());
        assert_eq!(phone.open(&third), Err(CryptoError::SessionClosed));
    }

    /// Only a record that fails to decrypt breaks the stream: a frame of
    /// another type, such as a replayed handshake reply, is refused untouched.
    #[test]
    fn a_frame_of_another_type_after_the_first_open_leaves_the_session_open() {
        let (phone_keys, mac_keys) = keys();
        let (mut phone, mut mac) = connect(&phone_keys, &mac_keys);
        let first = mac.seal(b"hello").unwrap().remove(0);
        assert_eq!(phone.open(&first), Ok(Some(b"hello".to_vec())));
        assert_eq!(
            phone.open(&[FrameKind::SessionReply as u8, 0, 0]),
            Err(CryptoError::UnexpectedFrame)
        );
        assert!(!phone.is_closed());
        let next = mac.seal(b"next").unwrap().remove(0);
        assert_eq!(phone.open(&next), Ok(Some(b"next".to_vec())));
    }

    /// Under `MAX_FRAME_LEN` but longer than any record, and naming this
    /// session: refused as the wrong frame, so the session survives it.
    #[test]
    fn an_oversize_record_naming_this_session_leaves_it_open() {
        let (phone_keys, mac_keys) = keys();
        let (mut phone, mut mac) = connect(&phone_keys, &mac_keys);
        assert_eq!(
            phone.open(&session::oversize_record(mac.id())),
            Err(CryptoError::UnexpectedFrame)
        );
        assert!(!phone.is_closed());
        let record = mac.seal(b"hello").unwrap().remove(0);
        assert_eq!(phone.open(&record), Ok(Some(b"hello".to_vec())));
    }

    /// A malformed first record closes the session before anything has
    /// opened, so the transport's deadline and its terminal-error path both
    /// see this state; the error is the one that ends the session.
    #[test]
    fn a_malformed_first_record_closes_the_session_without_opening_one() {
        let (phone_keys, mac_keys) = keys();
        let (mut phone, mut mac) = connect(&phone_keys, &mac_keys);
        let record = mac.seal_raw_fragment(&[0, 0, 0, 0, 0, 1, 0, 2, 0xAB]);
        assert_eq!(phone.open(&record), Err(CryptoError::ProtocolViolation));
        assert!(phone.is_closed());
        assert!(!phone.has_opened_record());
    }
}
