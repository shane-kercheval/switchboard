//! One paired phone's sessions on the Mac, and the rule for routing its
//! records between them. It lives here, synchronous and pure, so the rule is
//! written and tested once, inside the crypto review; the Mac's
//! `SessionManager` keeps one per device and adds timers, I/O, and the wake
//! lease.
//!
//! A device has at most one confirmed session and one unconfirmed one. A
//! connection request only ever creates the unconfirmed one, because a
//! `Noise_KK` message 1 can be replayed: a replay must never displace the live
//! session. Each record is offered to the unconfirmed session first, then the
//! confirmed one:
//! - it opens on the unconfirmed session: that session is confirmed and
//!   replaces the old one;
//! - it opens on the confirmed session: nothing else changes, so a phone that
//!   reconnects with records still in flight on its old keys loses none;
//! - it does not decrypt on a session: nothing changed there (a failed
//!   decryption does not advance the nonce), so it goes to the next;
//! - it decrypts but is malformed: that session is closed and dropped, and the
//!   record goes no further, since it belongs to no other session;
//! - no session opens it: the stream is broken, and both sessions end.

use crate::CryptoError;
use crate::frame::{self, FrameKind};
use crate::keys::{DeviceKeys, KEY_LEN};
use crate::session::{self, Session};

#[derive(Default)]
pub struct DeviceSessions {
    confirmed: Option<Session>,
    unconfirmed: Option<Session>,
}

impl DeviceSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// A connection request from this device. On success it becomes the
    /// device's unconfirmed session, replacing any earlier unconfirmed one but
    /// never the confirmed one, and the reply to send is returned. A request
    /// that fails changes nothing.
    pub fn accept_handshake(
        &mut self,
        keys: &DeviceKeys,
        phone_noise_public_key: &[u8; KEY_LEN],
        message_1: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let (session, message_2) = session::respond(keys, phone_noise_public_key, message_1)?;
        self.unconfirmed = Some(session);
        Ok(message_2)
    }

    /// Routes one record by the rule in the module documentation. Returns the
    /// envelope once its last record arrives, `None` while it is still
    /// incomplete. Errors:
    /// - `UnexpectedFrame`: not a record, or oversize; nothing changed.
    /// - `NotConnected`: the device has no session; nothing changed.
    /// - `ProtocolViolation`: a session decrypted it but it was malformed; that
    ///   session is gone.
    /// - `SessionsEnded`: no session opened it; both are gone.
    pub fn open(&mut self, record: &[u8]) -> Result<Option<Vec<u8>>, CryptoError> {
        frame::body(FrameKind::Record, record)?;
        if self.confirmed.is_none() && self.unconfirmed.is_none() {
            return Err(CryptoError::NotConnected);
        }
        if let Some(candidate) = self.unconfirmed.as_mut() {
            match candidate.open(record) {
                Ok(envelope) => {
                    self.confirmed = self.unconfirmed.take();
                    return Ok(envelope);
                }
                Err(CryptoError::RecordRejected) => {}
                Err(error) => {
                    self.unconfirmed = None;
                    return Err(error);
                }
            }
        }
        if let Some(live) = self.confirmed.as_mut() {
            match live.open(record) {
                Ok(envelope) => return Ok(envelope),
                Err(CryptoError::RecordRejected) => {}
                Err(error) => {
                    self.confirmed = None;
                    return Err(error);
                }
            }
        }
        self.end();
        Err(CryptoError::SessionsEnded)
    }

    /// Seals an envelope on the confirmed session. With no confirmed session —
    /// only a candidate, possibly a replay — there is no one proven to send to.
    pub fn seal(&mut self, envelope: &[u8]) -> Result<Vec<Vec<u8>>, CryptoError> {
        let live = self.confirmed.as_mut().ok_or(CryptoError::NotConnected)?;
        let result = live.seal(envelope);
        if live.is_closed() {
            self.confirmed = None;
        }
        result
    }

    /// Drops the unconfirmed session. The Mac calls this when a candidate has
    /// opened no record within its timeout.
    pub fn expire_unconfirmed(&mut self) {
        self.unconfirmed = None;
    }

    /// Ends both sessions.
    pub fn end(&mut self) {
        self.confirmed = None;
        self.unconfirmed = None;
    }

    /// Whether the phone is proven present: a confirmed session exists. Only
    /// this counts for the wake lease and for reporting the device connected.
    pub fn is_connected(&self) -> bool {
        self.confirmed.is_some()
    }

    pub fn has_unconfirmed(&self) -> bool {
        self.unconfirmed.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionInitiator, initiate};

    fn keys(seed: u8) -> DeviceKeys {
        DeviceKeys::from_secrets(&[seed; KEY_LEN], &[seed.wrapping_add(100); KEY_LEN]).unwrap()
    }

    struct Pair {
        phone_keys: DeviceKeys,
        mac_keys: DeviceKeys,
        device: DeviceSessions,
    }

    impl Pair {
        fn new() -> Self {
            Self {
                phone_keys: keys(1),
                mac_keys: keys(2),
                device: DeviceSessions::new(),
            }
        }

        /// The phone's half of a new connection, and its message 1.
        fn request(&self) -> (SessionInitiator, Vec<u8>) {
            initiate(&self.phone_keys, &self.mac_keys.noise_public_key()).unwrap()
        }

        fn accept(&mut self, message_1: &[u8]) -> Vec<u8> {
            self.device
                .accept_handshake(
                    &self.mac_keys,
                    &self.phone_keys.noise_public_key(),
                    message_1,
                )
                .unwrap()
        }

        /// A full connection: the phone's session, with the Mac's session
        /// accepted as unconfirmed.
        fn connect(&mut self) -> Session {
            let (mut initiator, message_1) = self.request();
            let message_2 = self.accept(&message_1);
            initiator.finish(&message_2).unwrap()
        }

        fn deliver(
            &mut self,
            phone: &mut Session,
            envelope: &[u8],
        ) -> Result<Option<Vec<u8>>, CryptoError> {
            let mut result = Ok(None);
            for record in phone.seal(envelope).unwrap() {
                result = self.device.open(&record);
            }
            result
        }
    }

    /// An authentic record whose fragment starts at index 1: it decrypts,
    /// then breaks the fragment rules.
    fn authentic_but_malformed(phone: &mut Session) -> Vec<u8> {
        phone.seal_raw_fragment(&[0, 0, 0, 0, 0, 1, 0, 2, 0xAB])
    }

    #[test]
    fn a_connection_is_unconfirmed_until_its_first_record() {
        let mut pair = Pair::new();
        let mut phone = pair.connect();
        assert!(!pair.device.is_connected());
        assert!(pair.device.has_unconfirmed());
        assert_eq!(
            pair.deliver(&mut phone, b"hello"),
            Ok(Some(b"hello".to_vec()))
        );
        assert!(pair.device.is_connected());
        assert!(!pair.device.has_unconfirmed());
    }

    #[test]
    fn nothing_is_sealed_for_an_unconfirmed_candidate() {
        let mut pair = Pair::new();
        pair.connect();
        assert_eq!(
            pair.device.seal(b"x").err(),
            Some(CryptoError::NotConnected)
        );
    }

    /// A replayed connection request during a live session creates only a
    /// candidate; the phone's next record falls back to the live session and
    /// arrives.
    #[test]
    fn a_replayed_request_costs_the_live_session_nothing() {
        let mut pair = Pair::new();
        let (mut initiator, message_1) = pair.request();
        let message_2 = pair.accept(&message_1);
        let mut phone = initiator.finish(&message_2).unwrap();
        pair.deliver(&mut phone, b"hello").unwrap();

        pair.accept(&message_1);
        assert!(pair.device.is_connected());
        assert_eq!(
            pair.deliver(&mut phone, b"next"),
            Ok(Some(b"next".to_vec()))
        );
        assert!(pair.device.is_connected());
        let reply = pair.device.seal(b"reply").unwrap().remove(0);
        assert_eq!(phone.open(&reply), Ok(Some(b"reply".to_vec())));
    }

    /// The phone reconnects while records on its old keys are still in flight:
    /// they are rejected by the new candidate without harm and open on the old
    /// session, and the phone's first record on the new keys promotes the
    /// candidate.
    #[test]
    fn in_flight_records_on_the_old_keys_survive_a_reconnect() {
        let mut pair = Pair::new();
        let mut old_phone = pair.connect();
        pair.deliver(&mut old_phone, b"hello").unwrap();
        let in_flight = old_phone.seal(b"in flight").unwrap().remove(0);

        let mut new_phone = pair.connect();
        assert_eq!(
            pair.device.open(&in_flight),
            Ok(Some(b"in flight".to_vec()))
        );
        assert!(pair.device.has_unconfirmed());

        assert_eq!(
            pair.deliver(&mut new_phone, b"again"),
            Ok(Some(b"again".to_vec()))
        );
        assert!(!pair.device.has_unconfirmed());
        let reply = pair.device.seal(b"reply").unwrap().remove(0);
        assert_eq!(new_phone.open(&reply), Ok(Some(b"reply".to_vec())));
    }

    #[test]
    fn a_malformed_record_on_the_candidate_drops_only_the_candidate() {
        let mut pair = Pair::new();
        let mut old_phone = pair.connect();
        pair.deliver(&mut old_phone, b"hello").unwrap();
        let mut new_phone = pair.connect();

        let malformed = authentic_but_malformed(&mut new_phone);
        assert_eq!(
            pair.device.open(&malformed).err(),
            Some(CryptoError::ProtocolViolation)
        );
        assert!(!pair.device.has_unconfirmed());
        assert!(pair.device.is_connected());
        assert_eq!(
            pair.deliver(&mut old_phone, b"still here"),
            Ok(Some(b"still here".to_vec()))
        );
    }

    #[test]
    fn a_record_no_session_opens_ends_them_all() {
        let mut pair = Pair::new();
        let mut phone = pair.connect();
        pair.deliver(&mut phone, b"hello").unwrap();
        pair.connect();
        let mut forged = phone.seal(b"x").unwrap().remove(0);
        forged[1] ^= 1;
        assert_eq!(
            pair.device.open(&forged).err(),
            Some(CryptoError::SessionsEnded)
        );
        assert!(!pair.device.is_connected());
        assert!(!pair.device.has_unconfirmed());
    }

    #[test]
    fn a_record_with_no_session_changes_nothing() {
        let mut pair = Pair::new();
        let mut phone = pair.connect();
        pair.device.end();
        let record = phone.seal(b"x").unwrap().remove(0);
        assert_eq!(
            pair.device.open(&record).err(),
            Some(CryptoError::NotConnected)
        );
    }

    #[test]
    fn a_frame_of_another_type_changes_nothing() {
        let mut pair = Pair::new();
        let (mut initiator, message_1) = pair.request();
        let message_2 = pair.accept(&message_1);
        let mut phone = initiator.finish(&message_2).unwrap();
        pair.deliver(&mut phone, b"hello").unwrap();
        assert_eq!(
            pair.device.open(&message_1).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        assert!(pair.device.is_connected());
    }

    #[test]
    fn an_expired_candidate_leaves_the_live_session() {
        let mut pair = Pair::new();
        let mut phone = pair.connect();
        pair.deliver(&mut phone, b"hello").unwrap();
        pair.connect();
        pair.device.expire_unconfirmed();
        assert!(!pair.device.has_unconfirmed());
        assert_eq!(
            pair.deliver(&mut phone, b"next"),
            Ok(Some(b"next".to_vec()))
        );
    }

    #[test]
    fn a_failed_request_changes_nothing() {
        let mut pair = Pair::new();
        let mut phone = pair.connect();
        pair.deliver(&mut phone, b"hello").unwrap();
        let stranger = keys(9);
        let (_, message_1) = initiate(&stranger, &pair.mac_keys.noise_public_key()).unwrap();
        assert!(
            pair.device
                .accept_handshake(
                    &pair.mac_keys,
                    &pair.phone_keys.noise_public_key(),
                    &message_1
                )
                .is_err()
        );
        assert!(pair.device.is_connected());
        assert!(!pair.device.has_unconfirmed());
    }
}
