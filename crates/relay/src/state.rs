//! The relay's routing table: who is registered, which phones each Mac has
//! paired, open pairing windows, and when each Mac was last seen. Synchronous
//! and clock-free — callers pass the time — so every routing rule is tested
//! here without sockets.
//!
//! A device becomes a Mac to the relay by sending `paired_devices` or
//! `pairing_open`; the relay then *holds a set* for it until the relay
//! restarts, connected or not.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_util::sync::CancellationToken;

use crate::protocol::{
    DeviceId, ErrorCode, PAIRING_TOKEN_LEN, ServerMessage, TOKEN_HASH_LEN, token_hash,
};

pub(crate) const MAX_PAIRING_WINDOW: Duration = Duration::from_mins(5);

/// Held sets of disconnected Macs kept at most; past it the longest
/// disconnected is forgotten, and its phones are told `mac_offline` as if
/// the relay had restarted. Registration is open to any key, so without a
/// bound the table would grow with every identity ever registered.
pub(crate) const MAX_HELD_SETS: usize = 1024;

/// A connection's outbound queue and the token that closes it.
#[derive(Debug, Clone)]
pub(crate) struct Outbox {
    queue: mpsc::Sender<ServerMessage>,
    /// Cancelled to close the connection: on replacement, or when its queue
    /// is full because it is not reading.
    closed: CancellationToken,
}

impl Outbox {
    pub fn new(queue: mpsc::Sender<ServerMessage>, closed: CancellationToken) -> Self {
        Self { queue, closed }
    }

    /// Queues `message` without waiting. A full queue means the connection
    /// is not reading, so it is closed rather than allowed to hold up the
    /// devices sending to it.
    pub fn deliver(&self, message: ServerMessage) {
        if let Err(TrySendError::Full(_)) = self.queue.try_send(message) {
            self.closed.cancel();
        }
    }
}

#[derive(Debug)]
struct Peer {
    generation: u64,
    outbox: Outbox,
}

#[derive(Debug)]
struct Pairing {
    token_hash: [u8; TOKEN_HASH_LEN],
    expires_at: Instant,
    /// The first device to present the token; no other may.
    bound: Option<DeviceId>,
}

/// Why a frame or a set was refused, sent back to the sender as `error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub code: ErrorCode,
    pub message: &'static str,
    pub device_id: Option<DeviceId>,
    pub last_seen: Option<DateTime<Utc>>,
}

impl Refusal {
    fn about(code: ErrorCode, message: &'static str, device_id: &DeviceId) -> Self {
        Self {
            code,
            message,
            device_id: Some(device_id.clone()),
            last_seen: None,
        }
    }

    pub fn into_message(self) -> ServerMessage {
        ServerMessage::Error {
            code: self.code,
            message: self.message.to_owned(),
            device_id: self.device_id,
            last_seen: self.last_seen,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Relay {
    max_devices_per_mac: usize,
    next_generation: u64,
    registered: HashMap<DeviceId, Peer>,
    held_sets: HashMap<DeviceId, HashSet<DeviceId>>,
    last_seen: HashMap<DeviceId, DateTime<Utc>>,
    pairings: HashMap<DeviceId, Pairing>,
    forwarded: u64,
}

impl Relay {
    pub fn new(max_devices_per_mac: usize) -> Self {
        Self {
            max_devices_per_mac,
            next_generation: 0,
            registered: HashMap::new(),
            held_sets: HashMap::new(),
            last_seen: HashMap::new(),
            pairings: HashMap::new(),
            forwarded: 0,
        }
    }

    pub fn registered_count(&self) -> usize {
        self.registered.len()
    }

    pub fn forwarded_count(&self) -> u64 {
        self.forwarded
    }

    /// Registers `id`, replacing and closing any earlier registration for it,
    /// and returns the new registration's generation. The replaced
    /// registration's notices are queued, and `registered` is queued on the
    /// new connection, before this returns — so before anything the new
    /// connection sends is forwarded, and before any frame addressed to it.
    pub fn register(&mut self, id: DeviceId, outbox: Outbox, now: DateTime<Utc>) -> u64 {
        if let Some(old) = self.registered.remove(&id) {
            old.outbox.closed.cancel();
            self.end_registration(&id, now);
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        outbox.deliver(ServerMessage::Registered);
        self.registered.insert(id, Peer { generation, outbox });
        generation
    }

    /// Ends `id`'s registration if `generation` is still the current one. A
    /// replaced connection closing late finds a newer generation and changes
    /// nothing, so each registration's end is announced exactly once.
    pub fn unregister(&mut self, id: &DeviceId, generation: u64, now: DateTime<Utc>) {
        if self
            .registered
            .get(id)
            .is_some_and(|peer| peer.generation == generation)
        {
            self.registered.remove(id);
            self.end_registration(id, now);
        }
    }

    /// Announces the end to the devices paired with `id`, once each. A Mac
    /// is announced only to the phones in its own set, never to a device
    /// that merely lists it, so no one learns a Mac's comings and goings by
    /// claiming to have paired it.
    fn end_registration(&mut self, id: &DeviceId, now: DateTime<Utc>) {
        let notice = ServerMessage::PeerDisconnected {
            device_id: id.clone(),
        };
        let recipients: Vec<&DeviceId> = if let Some(phones) = self.held_sets.get(id) {
            self.last_seen.insert(id.clone(), now);
            phones.iter().collect()
        } else {
            self.held_sets
                .iter()
                .filter(|(_, phones)| phones.contains(id))
                .map(|(mac, _)| mac)
                .collect()
        };
        for recipient in recipients {
            if let Some(peer) = self.registered.get(recipient) {
                peer.outbox.deliver(notice.clone());
            }
        }
        self.pairings.remove(id);
    }

    /// Replaces the set held for `mac`. Confirms its pairing window if the
    /// device bound to it is in the new set: the Mac sends the set before it
    /// tells the phone it is paired.
    pub fn set_paired_devices(
        &mut self,
        mac: &DeviceId,
        device_ids: Vec<DeviceId>,
    ) -> Result<(), Refusal> {
        let phones: HashSet<DeviceId> = device_ids.into_iter().collect();
        if phones.len() > self.max_devices_per_mac {
            return Err(Refusal {
                code: ErrorCode::LimitExceeded,
                message: "more paired devices than this relay allows; the previous set is kept",
                device_id: None,
                last_seen: None,
            });
        }
        if self
            .pairings
            .get(mac)
            .and_then(|pairing| pairing.bound.as_ref())
            .is_some_and(|bound| phones.contains(bound))
        {
            self.pairings.remove(mac);
        }
        self.hold_set(mac.clone(), phones);
        Ok(())
    }

    /// Opens `mac`'s pairing window, replacing any open one.
    pub fn open_pairing(
        &mut self,
        mac: &DeviceId,
        token_hash: [u8; TOKEN_HASH_LEN],
        expires_in: u64,
        now: Instant,
    ) {
        if !self.held_sets.contains_key(mac) {
            self.hold_set(mac.clone(), HashSet::new());
        }
        let window = Duration::from_secs(expires_in).min(MAX_PAIRING_WINDOW);
        self.pairings.insert(
            mac.clone(),
            Pairing {
                token_hash,
                expires_at: now + window,
                bound: None,
            },
        );
    }

    pub fn close_pairing(&mut self, mac: &DeviceId) {
        self.pairings.remove(mac);
    }

    fn hold_set(&mut self, mac: DeviceId, phones: HashSet<DeviceId>) {
        self.held_sets.insert(mac, phones);
        if self.held_sets.len() > MAX_HELD_SETS {
            let longest_gone = self
                .held_sets
                .keys()
                .filter(|id| !self.registered.contains_key(*id))
                .filter_map(|id| self.last_seen.get(id).map(|seen| (seen, id)))
                .min()
                .map(|(_, id)| id.clone());
            if let Some(id) = longest_gone {
                self.held_sets.remove(&id);
                self.last_seen.remove(&id);
            }
        }
    }

    /// Forwards a frame from `from` to `to`, or says why not. A Mac — a
    /// device the relay holds a set for — is reached only by the phones in
    /// its own set and by the holder of its pairing token; any other device,
    /// by a Mac whose set names it or that is answering its pairing
    /// candidate. A sender's own set never admits it to a Mac, since any
    /// device may send `paired_devices`. Pairing is checked before
    /// connectivity, so a refused device never learns whether the Mac is
    /// connected.
    pub fn forward(
        &mut self,
        from: &DeviceId,
        to: DeviceId,
        record: Vec<u8>,
        pairing_token: Option<[u8; PAIRING_TOKEN_LEN]>,
        now: Instant,
    ) -> Result<(), Refusal> {
        let paired = match self.held_sets.get(&to) {
            Some(phones) => phones.contains(from),
            None => self
                .held_sets
                .get(from)
                .is_some_and(|phones| phones.contains(&to)),
        };
        if !paired {
            self.admit_unpaired(from, &to, pairing_token.as_ref(), now)?;
        }
        let Some(peer) = self.registered.get(&to) else {
            return Err(self.offline(&to));
        };
        peer.outbox.deliver(ServerMessage::Frame {
            to,
            from: from.clone(),
            record,
            pairing_token,
        });
        self.forwarded += 1;
        Ok(())
    }

    fn admit_unpaired(
        &mut self,
        from: &DeviceId,
        to: &DeviceId,
        pairing_token: Option<&[u8; PAIRING_TOKEN_LEN]>,
        now: Instant,
    ) -> Result<(), Refusal> {
        if let Some(token) = pairing_token {
            if !self.held_sets.contains_key(to) {
                return Err(Refusal::about(
                    ErrorCode::MacOffline,
                    "that Mac is not connected",
                    to,
                ));
            }
            let expired = Refusal::about(
                ErrorCode::PairingExpired,
                "this pairing code has expired or was used",
                to,
            );
            let Some(pairing) = self.pairings.get_mut(to) else {
                return Err(expired);
            };
            if pairing.token_hash != token_hash(token) || now >= pairing.expires_at {
                return Err(expired);
            }
            return match &pairing.bound {
                None => {
                    pairing.bound = Some(from.clone());
                    Ok(())
                }
                Some(bound) if bound == from => Ok(()),
                Some(_) => Err(expired),
            };
        }
        let replying_to_candidate = !self.held_sets.contains_key(to)
            && self.pairings.get(from).is_some_and(|pairing| {
                pairing.bound.as_ref() == Some(to) && now < pairing.expires_at
            });
        if replying_to_candidate {
            return Ok(());
        }
        if self.held_sets.contains_key(to) || self.held_sets.contains_key(from) {
            Err(Refusal::about(
                ErrorCode::NotPaired,
                "this device is not paired with that one",
                to,
            ))
        } else {
            // A Mac the relay holds no set for: one it does not know, or one
            // that has not re-registered since the relay restarted.
            Err(Refusal::about(
                ErrorCode::MacOffline,
                "that Mac is not connected",
                to,
            ))
        }
    }

    fn offline(&self, to: &DeviceId) -> Refusal {
        if self.held_sets.contains_key(to) {
            Refusal {
                last_seen: self.last_seen.get(to).copied(),
                ..Refusal::about(ErrorCode::MacOffline, "that Mac is not connected", to)
            }
        } else {
            Refusal::about(ErrorCode::PhoneOffline, "that device is not connected", to)
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    struct Device {
        id: DeviceId,
        inbox: mpsc::Receiver<ServerMessage>,
        closed: CancellationToken,
        generation: u64,
    }

    impl Device {
        fn received(&mut self) -> Vec<ServerMessage> {
            let mut messages = Vec::new();
            while let Ok(message) = self.inbox.try_recv() {
                messages.push(message);
            }
            messages
        }

        fn frames(&mut self) -> Vec<(DeviceId, Vec<u8>)> {
            self.received()
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::Frame { from, record, .. } => Some((from, record)),
                    _ => None,
                })
                .collect()
        }
    }

    fn id(n: u8) -> DeviceId {
        let letter = char::from(b'a' + n);
        DeviceId::parse(&letter.to_string().repeat(26)).unwrap()
    }

    fn wall(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + seconds, 0).unwrap()
    }

    fn connect(relay: &mut Relay, n: u8) -> Device {
        connect_with_queue(relay, n, 64)
    }

    fn connect_with_queue(relay: &mut Relay, n: u8, queue: usize) -> Device {
        let (tx, rx) = mpsc::channel(queue);
        let closed = CancellationToken::new();
        let generation = relay.register(id(n), Outbox::new(tx, closed.clone()), wall(0));
        let mut device = Device {
            id: id(n),
            inbox: rx,
            closed,
            generation,
        };
        assert_eq!(device.received(), vec![ServerMessage::Registered]);
        device
    }

    fn code(result: Result<(), Refusal>) -> ErrorCode {
        result.unwrap_err().code
    }

    const TOKEN: [u8; PAIRING_TOKEN_LEN] = [7; PAIRING_TOKEN_LEN];

    #[test]
    fn paired_devices_reach_each_other_in_both_directions() {
        let mut relay = Relay::new(8);
        let mut mac = connect(&mut relay, 0);
        let mut phone = connect(&mut relay, 1);
        relay
            .set_paired_devices(&mac.id, vec![phone.id.clone()])
            .unwrap();
        let now = Instant::now();

        relay
            .forward(&phone.id, mac.id.clone(), vec![1], None, now)
            .unwrap();
        relay
            .forward(&mac.id, phone.id.clone(), vec![2], None, now)
            .unwrap();

        assert_eq!(mac.frames(), vec![(phone.id.clone(), vec![1])]);
        assert_eq!(phone.frames(), vec![(mac.id.clone(), vec![2])]);
        assert_eq!(relay.forwarded_count(), 2);
    }

    #[test]
    fn a_device_outside_a_held_set_is_not_paired_whether_or_not_the_mac_is_connected() {
        let mut relay = Relay::new(8);
        let mac = connect(&mut relay, 0);
        let stranger = connect(&mut relay, 2);
        relay.set_paired_devices(&mac.id, vec![id(1)]).unwrap();
        let now = Instant::now();

        assert_eq!(
            code(relay.forward(&stranger.id, mac.id.clone(), vec![], None, now)),
            ErrorCode::NotPaired
        );
        relay.unregister(&mac.id, mac.generation, wall(5));
        assert_eq!(
            code(relay.forward(&stranger.id, mac.id.clone(), vec![], None, now)),
            ErrorCode::NotPaired
        );
    }

    #[test]
    fn a_mac_reaches_only_phones_in_its_set() {
        let mut relay = Relay::new(8);
        let mac = connect(&mut relay, 0);
        let mut stranger = connect(&mut relay, 2);
        relay.set_paired_devices(&mac.id, vec![id(1)]).unwrap();

        let refusal = relay
            .forward(&mac.id, stranger.id.clone(), vec![], None, Instant::now())
            .unwrap_err();
        assert_eq!(refusal.code, ErrorCode::NotPaired);
        assert!(stranger.received().is_empty());
    }

    #[test]
    fn a_senders_own_set_never_admits_it_to_a_mac() {
        let mut relay = Relay::new(8);
        let mut mac = connect(&mut relay, 0);
        let impostor = connect(&mut relay, 2);
        relay.set_paired_devices(&mac.id, vec![id(1)]).unwrap();
        relay
            .set_paired_devices(&impostor.id, vec![mac.id.clone()])
            .unwrap();
        let now = Instant::now();

        assert_eq!(
            code(relay.forward(&impostor.id, mac.id.clone(), vec![], None, now)),
            ErrorCode::NotPaired
        );
        relay.unregister(&mac.id, mac.generation, wall(5));
        let refusal = relay
            .forward(&impostor.id, mac.id.clone(), vec![], None, now)
            .unwrap_err();
        assert_eq!(refusal.code, ErrorCode::NotPaired);
        assert_eq!(refusal.last_seen, None);
        assert!(mac.received().is_empty());
    }

    #[test]
    fn a_mac_is_announced_only_to_its_own_set_and_each_once() {
        let mut relay = Relay::new(8);
        let mac = connect(&mut relay, 0);
        let mut phone = connect(&mut relay, 1);
        let mut lister = connect(&mut relay, 2);
        relay
            .set_paired_devices(&mac.id, vec![phone.id.clone()])
            .unwrap();
        relay
            .set_paired_devices(&lister.id, vec![mac.id.clone()])
            .unwrap();
        relay
            .set_paired_devices(&phone.id, vec![mac.id.clone()])
            .unwrap();

        relay.unregister(&mac.id, mac.generation, wall(1));
        assert_eq!(
            phone.received(),
            vec![ServerMessage::PeerDisconnected {
                device_id: mac.id.clone()
            }]
        );
        assert!(lister.received().is_empty());
    }

    #[test]
    fn a_mac_with_no_held_set_is_offline_without_last_seen() {
        let mut relay = Relay::new(8);
        let phone = connect(&mut relay, 1);
        let refusal = relay
            .forward(&phone.id, id(0), vec![], None, Instant::now())
            .unwrap_err();
        assert_eq!(
            refusal,
            Refusal::about(ErrorCode::MacOffline, "that Mac is not connected", &id(0))
        );
    }

    #[test]
    fn a_disconnected_mac_is_offline_with_its_last_disconnect() {
        let mut relay = Relay::new(8);
        let mac = connect(&mut relay, 0);
        let phone = connect(&mut relay, 1);
        relay
            .set_paired_devices(&mac.id, vec![phone.id.clone()])
            .unwrap();
        relay.unregister(&mac.id, mac.generation, wall(42));

        let refusal = relay
            .forward(&phone.id, mac.id.clone(), vec![], None, Instant::now())
            .unwrap_err();
        assert_eq!(refusal.code, ErrorCode::MacOffline);
        assert_eq!(refusal.device_id, Some(mac.id.clone()));
        assert_eq!(refusal.last_seen, Some(wall(42)));
    }

    #[test]
    fn a_paired_phone_that_is_not_connected_is_phone_offline() {
        let mut relay = Relay::new(8);
        let mac = connect(&mut relay, 0);
        relay.set_paired_devices(&mac.id, vec![id(1)]).unwrap();
        let refusal = relay
            .forward(&mac.id, id(1), vec![], None, Instant::now())
            .unwrap_err();
        assert_eq!(refusal.code, ErrorCode::PhoneOffline);
        assert_eq!(refusal.device_id, Some(id(1)));
        assert_eq!(refusal.last_seen, None);
    }

    #[test]
    fn a_new_set_replaces_the_old_one() {
        let mut relay = Relay::new(8);
        let mac = connect(&mut relay, 0);
        let old = connect(&mut relay, 1);
        let new = connect(&mut relay, 2);
        relay
            .set_paired_devices(&mac.id, vec![old.id.clone()])
            .unwrap();
        relay
            .set_paired_devices(&mac.id, vec![new.id.clone()])
            .unwrap();
        let now = Instant::now();

        assert_eq!(
            code(relay.forward(&old.id, mac.id.clone(), vec![], None, now)),
            ErrorCode::NotPaired
        );
        relay
            .forward(&new.id, mac.id.clone(), vec![], None, now)
            .unwrap();
    }

    #[test]
    fn a_set_above_the_limit_is_refused_and_the_old_one_kept() {
        let mut relay = Relay::new(2);
        let mac = connect(&mut relay, 0);
        let phone = connect(&mut relay, 1);
        relay
            .set_paired_devices(&mac.id, vec![phone.id.clone()])
            .unwrap();

        assert_eq!(
            code(relay.set_paired_devices(&mac.id, vec![id(2), id(3), id(4)])),
            ErrorCode::LimitExceeded
        );
        relay
            .forward(&phone.id, mac.id.clone(), vec![], None, Instant::now())
            .unwrap();
    }

    #[test]
    fn a_closed_registration_notifies_its_peers_once() {
        let mut relay = Relay::new(8);
        let mut mac = connect(&mut relay, 0);
        let phone = connect(&mut relay, 1);
        let mut other_phone = connect(&mut relay, 2);
        let mut stranger = connect(&mut relay, 3);
        relay
            .set_paired_devices(&mac.id, vec![phone.id.clone(), other_phone.id.clone()])
            .unwrap();

        relay.unregister(&phone.id, phone.generation, wall(1));
        relay.unregister(&phone.id, phone.generation, wall(2));
        let notice = |device_id: &DeviceId| ServerMessage::PeerDisconnected {
            device_id: device_id.clone(),
        };
        assert_eq!(mac.received(), vec![notice(&phone.id)]);

        relay.unregister(&mac.id, mac.generation, wall(3));
        assert_eq!(other_phone.received(), vec![notice(&mac.id)]);
        assert!(stranger.received().is_empty());
    }

    #[test]
    fn a_replaced_registration_is_announced_once_and_its_late_close_changes_nothing() {
        let mut relay = Relay::new(8);
        let mut mac = connect(&mut relay, 0);
        let old_phone = connect(&mut relay, 1);
        relay
            .set_paired_devices(&mac.id, vec![old_phone.id.clone()])
            .unwrap();

        let mut new_phone = connect(&mut relay, 1);
        assert!(old_phone.closed.is_cancelled());
        relay
            .forward(&new_phone.id, mac.id.clone(), vec![9], None, Instant::now())
            .unwrap();
        relay.unregister(&old_phone.id, old_phone.generation, wall(1));

        assert_eq!(
            mac.received(),
            vec![
                ServerMessage::PeerDisconnected {
                    device_id: old_phone.id.clone()
                },
                ServerMessage::Frame {
                    to: mac.id.clone(),
                    from: new_phone.id.clone(),
                    record: vec![9],
                    pairing_token: None,
                },
            ]
        );
        relay
            .forward(&mac.id, new_phone.id.clone(), vec![1], None, Instant::now())
            .unwrap();
        assert_eq!(new_phone.frames(), vec![(mac.id.clone(), vec![1])]);
    }

    #[test]
    fn a_pairing_token_admits_the_first_device_only() {
        let mut relay = Relay::new(8);
        let mut mac = connect(&mut relay, 0);
        let mut candidate = connect(&mut relay, 1);
        let second = connect(&mut relay, 2);
        let now = Instant::now();
        relay.open_pairing(&mac.id, token_hash(&TOKEN), 300, now);

        relay
            .forward(&candidate.id, mac.id.clone(), vec![1], Some(TOKEN), now)
            .unwrap();
        assert_eq!(
            mac.received(),
            vec![ServerMessage::Frame {
                to: mac.id.clone(),
                from: candidate.id.clone(),
                record: vec![1],
                pairing_token: Some(TOKEN),
            }]
        );
        relay
            .forward(&mac.id, candidate.id.clone(), vec![2], None, now)
            .unwrap();
        assert_eq!(candidate.frames(), vec![(mac.id.clone(), vec![2])]);

        assert_eq!(
            code(relay.forward(&second.id, mac.id.clone(), vec![], Some(TOKEN), now)),
            ErrorCode::PairingExpired
        );
        assert_eq!(
            code(relay.forward(&mac.id, second.id.clone(), vec![], None, now)),
            ErrorCode::NotPaired
        );
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let mut relay = Relay::new(8);
        let mac = connect(&mut relay, 0);
        let candidate = connect(&mut relay, 1);
        let now = Instant::now();
        relay.open_pairing(&mac.id, token_hash(&TOKEN), 300, now);

        assert_eq!(
            code(relay.forward(&candidate.id, mac.id.clone(), vec![], Some([8; 32]), now)),
            ErrorCode::PairingExpired
        );
    }

    #[test]
    fn a_pairing_window_expires_and_is_capped_at_five_minutes() {
        let mut relay = Relay::new(8);
        let mac = connect(&mut relay, 0);
        let candidate = connect(&mut relay, 1);
        let opened = Instant::now();
        relay.open_pairing(&mac.id, token_hash(&TOKEN), 10_000, opened);

        let just_before = opened + MAX_PAIRING_WINDOW.saturating_sub(Duration::from_millis(1));
        relay
            .forward(
                &candidate.id,
                mac.id.clone(),
                vec![],
                Some(TOKEN),
                just_before,
            )
            .unwrap();
        let at_expiry = opened + MAX_PAIRING_WINDOW;
        assert_eq!(
            code(relay.forward(
                &candidate.id,
                mac.id.clone(),
                vec![],
                Some(TOKEN),
                at_expiry
            )),
            ErrorCode::PairingExpired
        );
        assert_eq!(
            code(relay.forward(&mac.id, candidate.id.clone(), vec![], None, at_expiry)),
            ErrorCode::NotPaired
        );
    }

    #[test]
    fn a_pairing_window_ends_on_confirmation_close_reopen_and_disconnect() {
        let mut relay = Relay::new(8);
        let mut mac = connect(&mut relay, 0);
        let candidate = connect(&mut relay, 1);
        let now = Instant::now();
        let open = |relay: &mut Relay, mac: &Device| {
            relay.open_pairing(&mac.id, token_hash(&TOKEN), 300, now);
            relay
                .forward(&candidate.id, mac.id.clone(), vec![], Some(TOKEN), now)
                .unwrap();
        };
        let refused = |relay: &mut Relay, mac: &Device| {
            relay
                .forward(&candidate.id, mac.id.clone(), vec![], Some(TOKEN), now)
                .is_err_and(|refusal| refusal.code == ErrorCode::PairingExpired)
        };

        open(&mut relay, &mac);
        relay.set_paired_devices(&mac.id, vec![id(5)]).unwrap();
        assert!(
            !refused(&mut relay, &mac),
            "a set without the candidate keeps the window"
        );
        relay
            .set_paired_devices(&mac.id, vec![candidate.id.clone()])
            .unwrap();
        relay
            .forward(&candidate.id, mac.id.clone(), vec![], None, now)
            .unwrap();
        relay.set_paired_devices(&mac.id, vec![]).unwrap();
        assert!(refused(&mut relay, &mac), "confirmation ended the window");

        open(&mut relay, &mac);
        relay.close_pairing(&mac.id);
        assert!(refused(&mut relay, &mac));

        open(&mut relay, &mac);
        relay.open_pairing(&mac.id, token_hash(&[9; 32]), 300, now);
        assert!(
            refused(&mut relay, &mac),
            "a new window replaces the old one"
        );

        open(&mut relay, &mac);
        relay.unregister(&mac.id, mac.generation, wall(1));
        mac = connect(&mut relay, 0);
        assert!(
            refused(&mut relay, &mac),
            "the Mac's disconnect ends the window"
        );
    }

    #[test]
    fn a_token_to_a_mac_with_no_held_set_is_mac_offline() {
        let mut relay = Relay::new(8);
        let candidate = connect(&mut relay, 1);
        assert_eq!(
            code(relay.forward(&candidate.id, id(0), vec![], Some(TOKEN), Instant::now())),
            ErrorCode::MacOffline
        );
    }

    #[test]
    fn a_full_queue_closes_that_connection_and_not_the_sender() {
        let mut relay = Relay::new(8);
        let mac = connect_with_queue(&mut relay, 0, 2);
        let phone = connect(&mut relay, 1);
        relay
            .set_paired_devices(&mac.id, vec![phone.id.clone()])
            .unwrap();
        let now = Instant::now();

        relay
            .forward(&phone.id, mac.id.clone(), vec![], None, now)
            .unwrap();
        relay
            .forward(&phone.id, mac.id.clone(), vec![], None, now)
            .unwrap();
        assert!(!mac.closed.is_cancelled());
        relay
            .forward(&phone.id, mac.id.clone(), vec![], None, now)
            .unwrap();
        assert!(mac.closed.is_cancelled());
        assert!(!phone.closed.is_cancelled());
    }

    #[test]
    fn the_longest_disconnected_held_set_is_forgotten_past_the_bound() {
        let mut relay = Relay::new(8);
        let phone = connect(&mut relay, 1);
        let ids: Vec<DeviceId> = (0..=MAX_HELD_SETS)
            .map(|n| {
                let encoded = data_encoding::BASE32_NOPAD.encode(&(n as u64).to_be_bytes());
                DeviceId::parse(&format!("{:a>26}", encoded.to_ascii_lowercase())).unwrap()
            })
            .collect();
        for (n, mac) in ids.iter().enumerate().take(MAX_HELD_SETS) {
            let (tx, _rx) = mpsc::channel(4);
            let outbox = Outbox::new(tx, CancellationToken::new());
            let generation = relay.register(mac.clone(), outbox, wall(0));
            relay
                .set_paired_devices(mac, vec![phone.id.clone()])
                .unwrap();
            relay.unregister(mac, generation, wall(i64::try_from(n).unwrap()));
        }
        let now = Instant::now();
        assert_eq!(
            code(relay.forward(&phone.id, ids[0].clone(), vec![], None, now)),
            ErrorCode::MacOffline
        );

        relay
            .set_paired_devices(&ids[MAX_HELD_SETS], vec![])
            .unwrap();
        let first = relay
            .forward(&phone.id, ids[0].clone(), vec![], None, now)
            .unwrap_err();
        assert_eq!(first.last_seen, None, "the first Mac to leave is forgotten");
        let second = relay
            .forward(&phone.id, ids[1].clone(), vec![], None, now)
            .unwrap_err();
        assert_eq!(second.last_seen, Some(wall(1)));
    }
}
