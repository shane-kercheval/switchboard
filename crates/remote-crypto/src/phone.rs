//! The phone's one session with its Mac, and the rule that ends it when the
//! in-order stream from the Mac breaks. The Mac's half of the same routing is
//! `DeviceSessions`.
//!
//! The phone has one session, so a record it cannot decrypt has nowhere else
//! to go. Before the session has opened any record, such a record is ignored:
//! after a reconnect the Mac keeps sending on its old session until the
//! phone's first record confirms the new one, and the relay delivers those
//! stale records first. The Mac then sends nothing more on the old session,
//! and its first record on the new one is its `hello`. So after the first
//! record opens, a record that does not decrypt means the stream is broken:
//! the session closes and `open` returns `StreamBroken`. The phone's transport
//! bounds the first window with a deadline (`has_opened_record`), since a
//! damaged first record would otherwise leave it waiting.

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

    /// The envelope once its last record arrives, `None` before then.
    /// `RecordRejected` before the first record opens leaves the session as it
    /// was; after it, a record that does not decrypt closes the session and
    /// returns `StreamBroken`. Every other outcome is `Session::open`'s.
    pub fn open(&mut self, record: &[u8]) -> Result<Option<Vec<u8>>, CryptoError> {
        match self.session.open(record) {
            Ok(envelope) => {
                self.opened_record = true;
                Ok(envelope)
            }
            Err(CryptoError::RecordRejected) if self.opened_record => {
                self.session.close();
                Err(CryptoError::StreamBroken)
            }
            Err(error) => Err(error),
        }
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

    #[test]
    fn stale_records_before_the_first_open_are_ignored() {
        let (phone_keys, mac_keys) = keys();
        let (_, mut old) = connect(&phone_keys, &mac_keys);
        let (mut phone, mut new) = connect(&phone_keys, &mac_keys);
        for stale in old.seal(b"on the old session").unwrap() {
            assert_eq!(phone.open(&stale), Err(CryptoError::RecordRejected));
        }
        assert!(!phone.is_closed());
        assert!(!phone.has_opened_record());
        let record = new.seal(b"hello").unwrap().remove(0);
        assert_eq!(phone.open(&record), Ok(Some(b"hello".to_vec())));
        assert!(phone.has_opened_record());
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
