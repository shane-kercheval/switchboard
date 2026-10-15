//! The relay's routing table: who is registered and as what, which phones
//! each Mac has paired, open pairing windows, and when each Mac was last
//! seen. Synchronous and clock-free — callers pass the time — so every
//! routing rule is tested here without sockets.
//!
//! A device registers as a Mac, with its set of paired phones, or as a phone,
//! naming its one Mac. The relay *holds a set* for a Mac from its first
//! registration after the relay started, connected or not, and routes only
//! between a Mac and the phones that both name it and are in its set (or the
//! phone it is pairing).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::outbox::{NotQueued, Outbox, frame_cost};
use crate::protocol::{
    DeviceId, ErrorCode, PAIRING_TOKEN_LEN, Role, ServerMessage, TOKEN_HASH_LEN, token_hash,
};

pub(crate) const MAX_PAIRING_WINDOW: Duration = Duration::from_mins(5);

/// Held sets of disconnected Macs kept at most; past it the longest
/// disconnected are forgotten, and their phones are told `mac_offline` as if
/// the relay had restarted. Registration is open to any key, so without a
/// bound the table would grow with every identity ever registered. Pruning
/// scans the table, which is cheap at this size; a much larger bound would
/// want an index ordered by `last_seen`.
pub(crate) const MAX_HELD_SETS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Mac,
    Phone { mac: DeviceId },
}

#[derive(Debug)]
struct Peer {
    generation: u64,
    kind: Kind,
    outbox: Outbox,
}

#[derive(Debug)]
struct Pairing {
    token_hash: [u8; TOKEN_HASH_LEN],
    expires_at: Instant,
    /// The first device to present the token; no other may.
    bound: Option<DeviceId>,
}

impl Pairing {
    /// The phone bound to this window while it is open: the one device
    /// outside the Mac's set that may reach it, and be told of its end.
    fn candidate(&self, now: Instant) -> Option<&DeviceId> {
        self.bound.as_ref().filter(|_| now < self.expires_at)
    }
}

/// Why a registration, a frame, or a set was refused, sent back as `error`.
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

    fn not_paired(device_id: &DeviceId) -> Self {
        Self::about(
            ErrorCode::NotPaired,
            "this device is not paired with that one",
            device_id,
        )
    }

    /// `last_seen` is when the Mac last disconnected, if the relay knows.
    fn mac_offline(mac: &DeviceId, last_seen: Option<DateTime<Utc>>) -> Self {
        Self {
            last_seen,
            ..Self::about(ErrorCode::MacOffline, "that Mac is not connected", mac)
        }
    }

    fn too_many_devices() -> Self {
        Self {
            code: ErrorCode::LimitExceeded,
            message: "more paired devices than this relay allows",
            device_id: None,
            last_seen: None,
        }
    }

    /// A protocol violation, which ends the connection.
    pub fn bad_message(message: &'static str) -> Self {
        Self {
            code: ErrorCode::BadMessage,
            message,
            device_id: None,
            last_seen: None,
        }
    }

    /// The recipient had no room for a frame and has been closed.
    fn recipient_full(recipient: &DeviceId) -> Self {
        Self::about(
            ErrorCode::RecipientFull,
            "that device had no room for the frame and has been disconnected",
            recipient,
        )
    }

    fn mac_only() -> Self {
        Self::bad_message("only a Mac may send this")
    }

    /// Whether a registered connection is closed after this refusal: one
    /// that breaks the protocol is, one over a limit is not. A refused
    /// registration always closes its connection, whatever the code.
    pub fn ends_connection(&self) -> bool {
        self.code == ErrorCode::BadMessage
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
        }
    }

    /// Registers `id` as `role`, replacing and closing any earlier
    /// registration for it, and returns the new registration's generation. A
    /// Mac's set is held from this moment, so no frame can reach it before
    /// its set applies. The replaced registration's notices are queued, and
    /// `registered` is queued on the new connection, before this returns —
    /// so before anything the new connection sends is forwarded, and before
    /// any frame addressed to it. Registering as a phone forgets anything
    /// held for the id as a Mac. `wall` is recorded as a Mac's last seen;
    /// `now` decides which pairing windows are still open.
    pub fn register(
        &mut self,
        id: DeviceId,
        role: Role,
        outbox: Outbox,
        wall: DateTime<Utc>,
        now: Instant,
    ) -> Result<u64, Refusal> {
        let (kind, phones) = match role {
            Role::Mac { paired_devices } => {
                // Counted before duplicates are dropped, so a long list of
                // one id costs no more than the limit to take in.
                if paired_devices.len() > self.max_devices_per_mac {
                    return Err(Refusal::too_many_devices());
                }
                (Kind::Mac, Some(paired_devices.into_iter().collect()))
            }
            Role::Phone { mac } => (Kind::Phone { mac }, None),
        };
        if let Some(old) = self.registered.remove(&id) {
            old.outbox.close();
            self.end_registration(&id, &old.kind, wall, now);
        }
        if let Some(phones) = phones {
            self.held_sets.insert(id.clone(), phones);
        } else {
            self.held_sets.remove(&id);
            self.last_seen.remove(&id);
            self.pairings.remove(&id);
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        outbox.deliver(ServerMessage::Registered);
        self.registered.insert(
            id,
            Peer {
                generation,
                kind,
                outbox,
            },
        );
        self.prune_held_sets();
        Ok(generation)
    }

    /// Whether `generation` is `id`'s current registration. Every call on
    /// behalf of a connection names its generation, because a replaced
    /// connection may still act on a message it read before it was
    /// replaced; finding a newer generation, the call changes nothing.
    fn is_current(&self, id: &DeviceId, generation: u64) -> bool {
        self.registered
            .get(id)
            .is_some_and(|peer| peer.generation == generation)
    }

    /// Ends `id`'s registration if `generation` is still the current one, so
    /// each registration's end is announced exactly once. Returns whether it
    /// ended one.
    pub fn unregister(
        &mut self,
        id: &DeviceId,
        generation: u64,
        wall: DateTime<Utc>,
        now: Instant,
    ) -> bool {
        if self.is_current(id, generation)
            && let Some(peer) = self.registered.remove(id)
        {
            self.end_registration(id, &peer.kind, wall, now);
            self.prune_held_sets();
            return true;
        }
        false
    }

    /// Announces the end to the devices paired with `id`, once each: a Mac's
    /// end to the connected phones in its set, and the phone bound to its
    /// open pairing window, that name it; a phone's to its Mac if the phone
    /// is in that Mac's set or bound to its open window. A device that
    /// merely lists another, or whose window has expired, learns nothing of
    /// its comings and goings. A phone's end leaves its binding: the window
    /// stays its own until it closes.
    fn end_registration(&mut self, id: &DeviceId, kind: &Kind, wall: DateTime<Utc>, now: Instant) {
        let notice = ServerMessage::PeerDisconnected {
            device_id: id.clone(),
        };
        match kind {
            Kind::Mac => {
                self.last_seen.insert(id.clone(), wall);
                let pairing = self.pairings.remove(id);
                let candidate = pairing.as_ref().and_then(|pairing| pairing.candidate(now));
                let phones: HashSet<&DeviceId> = self
                    .held_sets
                    .get(id)
                    .into_iter()
                    .flatten()
                    .chain(candidate)
                    .collect();
                for phone in phones {
                    if let Some(peer) = self.registered.get(phone)
                        && matches!(&peer.kind, Kind::Phone { mac } if mac == id)
                    {
                        peer.outbox.deliver(notice.clone());
                    }
                }
            }
            Kind::Phone { mac } => {
                let in_set = self
                    .held_sets
                    .get(mac)
                    .is_some_and(|phones| phones.contains(id));
                let candidate = self
                    .pairings
                    .get(mac)
                    .and_then(|pairing| pairing.candidate(now))
                    == Some(id);
                if (in_set || candidate)
                    && let Some(peer) = self.registered.get(mac)
                    && peer.kind == Kind::Mac
                {
                    peer.outbox.deliver(notice);
                }
            }
        }
    }

    /// Whether `id` may act as a Mac now: false, to change nothing, if
    /// `generation` has been replaced; refused if it is a phone's.
    fn acting_mac(&self, id: &DeviceId, generation: u64) -> Result<bool, Refusal> {
        match self.registered.get(id) {
            Some(peer) if peer.generation == generation => match peer.kind {
                Kind::Mac => Ok(true),
                Kind::Phone { .. } => Err(Refusal::mac_only()),
            },
            _ => Ok(false),
        }
    }

    /// Replaces the set held for `mac`, a registered Mac, and returns
    /// whether it changed. Confirms its pairing window if the device bound
    /// to it is in the new set: the Mac sends the set before it tells the
    /// phone it is paired.
    pub fn set_paired_devices(
        &mut self,
        mac: &DeviceId,
        generation: u64,
        device_ids: Vec<DeviceId>,
    ) -> Result<bool, Refusal> {
        if !self.acting_mac(mac, generation)? {
            return Ok(false);
        }
        if device_ids.len() > self.max_devices_per_mac {
            return Err(Refusal {
                message: "more paired devices than this relay allows; the previous set is kept",
                ..Refusal::too_many_devices()
            });
        }
        let phones: HashSet<DeviceId> = device_ids.into_iter().collect();
        if self
            .pairings
            .get(mac)
            .and_then(|pairing| pairing.bound.as_ref())
            .is_some_and(|bound| phones.contains(bound))
        {
            self.pairings.remove(mac);
        }
        let changed = self.held_sets.get(mac) != Some(&phones);
        self.held_sets.insert(mac.clone(), phones);
        Ok(changed)
    }

    /// Opens `mac`'s pairing window, replacing any open one.
    pub fn open_pairing(
        &mut self,
        mac: &DeviceId,
        generation: u64,
        token_hash: [u8; TOKEN_HASH_LEN],
        expires_in: u64,
        now: Instant,
    ) -> Result<(), Refusal> {
        if !self.acting_mac(mac, generation)? {
            return Ok(());
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
        Ok(())
    }

    pub fn close_pairing(&mut self, mac: &DeviceId, generation: u64) -> Result<(), Refusal> {
        if self.acting_mac(mac, generation)? {
            self.pairings.remove(mac);
        }
        Ok(())
    }

    /// Forgets the longest disconnected Macs until at most `MAX_HELD_SETS`
    /// disconnected ones are held. Connected Macs are never forgotten.
    fn prune_held_sets(&mut self) {
        loop {
            let disconnected = self.held_sets.keys().filter(|id| {
                !self
                    .registered
                    .get(*id)
                    .is_some_and(|peer| peer.kind == Kind::Mac)
            });
            if disconnected.clone().count() <= MAX_HELD_SETS {
                return;
            }
            let Some(longest_gone) = disconnected
                .map(|id| (self.last_seen.get(id), id))
                .min()
                .map(|(_, id)| id.clone())
            else {
                return;
            };
            self.held_sets.remove(&longest_gone);
            self.last_seen.remove(&longest_gone);
        }
    }

    /// Queues a frame from `from` for `to`, without waiting, or returns why
    /// it may not pass. A recipient with no room for it is closed, and this
    /// frame and any that reach it while it closes are refused as
    /// `recipient_full`. A frame for a recipient closing for another reason
    /// is dropped: its sender hears of that through `peer_disconnected`.
    pub fn forward(
        &mut self,
        from: &DeviceId,
        generation: u64,
        to: &DeviceId,
        record: Vec<u8>,
        pairing_token: Option<[u8; PAIRING_TOKEN_LEN]>,
        now: Instant,
    ) -> Result<(), Refusal> {
        let outbox = self.route(from, generation, to, pairing_token.as_ref(), now)?;
        let cost = frame_cost(record.len());
        let frame = ServerMessage::Frame {
            to: to.clone(),
            from: from.clone(),
            record,
            pairing_token,
        };
        match outbox.queue_frame(frame, cost) {
            Ok(()) | Err(NotQueued::Closed) => Ok(()),
            Err(NotQueued::Full) => Err(Refusal::recipient_full(to)),
        }
    }

    /// Where a frame from `from` to `to` goes, or why it may not. A phone
    /// reaches only the Mac it names, and only if that Mac's set holds it
    /// or it presents that Mac's open pairing token. A Mac reaches only a
    /// phone in its set or bound to its token, and only if that phone names
    /// it. Pairing is checked before connectivity, so a refused device never
    /// learns whether the other is connected.
    fn route(
        &mut self,
        from: &DeviceId,
        generation: u64,
        to: &DeviceId,
        pairing_token: Option<&[u8; PAIRING_TOKEN_LEN]>,
        now: Instant,
    ) -> Result<&Outbox, Refusal> {
        let Some(sender) = self
            .registered
            .get(from)
            .filter(|peer| peer.generation == generation)
        else {
            return Err(Refusal::not_paired(to));
        };
        match &sender.kind {
            Kind::Mac => self.route_from_mac(from, to, now),
            Kind::Phone { mac } if mac == to => self.route_from_phone(from, to, pairing_token, now),
            Kind::Phone { .. } => Err(Refusal::not_paired(to)),
        }
    }

    /// From `phone` to `to`, the Mac it names.
    fn route_from_phone(
        &mut self,
        phone: &DeviceId,
        to: &DeviceId,
        pairing_token: Option<&[u8; PAIRING_TOKEN_LEN]>,
        now: Instant,
    ) -> Result<&Outbox, Refusal> {
        let Some(phones) = self.held_sets.get(to) else {
            // A Mac the relay holds no set for: one it does not know, or one
            // that has not registered since the relay restarted.
            return Err(Refusal::mac_offline(to, None));
        };
        if !phones.contains(phone) {
            match pairing_token {
                Some(token) => self.bind_token(phone, to, token, now)?,
                None => return Err(Refusal::not_paired(to)),
            }
        }
        match self.registered.get(to) {
            Some(peer) if peer.kind == Kind::Mac => Ok(&peer.outbox),
            _ => Err(Refusal::mac_offline(to, self.last_seen.get(to).copied())),
        }
    }

    fn bind_token(
        &mut self,
        phone: &DeviceId,
        mac: &DeviceId,
        token: &[u8; PAIRING_TOKEN_LEN],
        now: Instant,
    ) -> Result<(), Refusal> {
        let expired = Refusal::about(
            ErrorCode::PairingExpired,
            "this pairing code has expired or was used",
            mac,
        );
        let Some(pairing) = self.pairings.get_mut(mac) else {
            return Err(expired);
        };
        if pairing.token_hash != token_hash(token) || now >= pairing.expires_at {
            return Err(expired);
        }
        match &pairing.bound {
            None => {
                pairing.bound = Some(phone.clone());
                Ok(())
            }
            Some(bound) if bound == phone => Ok(()),
            Some(_) => Err(expired),
        }
    }

    fn route_from_mac(
        &self,
        mac: &DeviceId,
        to: &DeviceId,
        now: Instant,
    ) -> Result<&Outbox, Refusal> {
        let in_set = self
            .held_sets
            .get(mac)
            .is_some_and(|phones| phones.contains(to));
        let candidate = self
            .pairings
            .get(mac)
            .and_then(|pairing| pairing.candidate(now))
            == Some(to);
        if !in_set && !candidate {
            return Err(Refusal::not_paired(to));
        }
        match self.registered.get(to) {
            Some(peer) if matches!(&peer.kind, Kind::Phone { mac: of } if of == mac) => {
                Ok(&peer.outbox)
            }
            Some(_) => Err(Refusal::not_paired(to)),
            None => Err(Refusal::about(
                ErrorCode::PhoneOffline,
                "that device is not connected",
                to,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::outbox::{MIN_FRAME_COST, Writer};

    struct Device {
        id: DeviceId,
        writer: Writer,
        closed: CancellationToken,
        generation: u64,
    }

    impl Device {
        fn received(&mut self) -> Vec<ServerMessage> {
            std::iter::from_fn(|| self.writer.try_recv()).collect()
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

    fn id(n: u16) -> DeviceId {
        let encoded = data_encoding::BASE32_NOPAD.encode(&n.to_be_bytes());
        DeviceId::parse(&format!("{:a>26}", encoded.to_ascii_lowercase())).unwrap()
    }

    fn wall(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + seconds, 0).unwrap()
    }

    fn mac_role(phones: &[&Device]) -> Role {
        Role::Mac {
            paired_devices: phones.iter().map(|phone| phone.id.clone()).collect(),
        }
    }

    fn phone_role(mac: u16) -> Role {
        Role::Phone { mac: id(mac) }
    }

    fn register(relay: &mut Relay, n: u16, role: Role) -> Device {
        register_with_budget(relay, n, role, 64 * MIN_FRAME_COST)
    }

    fn register_with_budget(relay: &mut Relay, n: u16, role: Role, queue_bytes: usize) -> Device {
        let closed = CancellationToken::new();
        let (outbox, writer) = Outbox::new(queue_bytes, closed.clone());
        let generation = relay
            .register(id(n), role, outbox, wall(0), Instant::now())
            .unwrap();
        let mut device = Device {
            id: id(n),
            writer,
            closed,
            generation,
        };
        assert_eq!(device.received(), vec![ServerMessage::Registered]);
        device
    }

    fn send(
        relay: &mut Relay,
        from: &Device,
        to: &DeviceId,
        record: &[u8],
        token: Option<&[u8; PAIRING_TOKEN_LEN]>,
        now: Instant,
    ) -> Result<(), Refusal> {
        relay.forward(
            &from.id,
            from.generation,
            to,
            record.to_vec(),
            token.copied(),
            now,
        )
    }

    fn code(result: Result<(), Refusal>) -> ErrorCode {
        result.unwrap_err().code
    }

    const MAC: u16 = 0;
    const TOKEN: [u8; PAIRING_TOKEN_LEN] = [7; PAIRING_TOKEN_LEN];

    /// A Mac with phone 1 paired, and that phone connected.
    fn paired() -> (Relay, Device, Device) {
        let mut relay = Relay::new(8);
        let phone = register(&mut relay, 1, phone_role(MAC));
        let mac = register(&mut relay, MAC, mac_role(&[&phone]));
        (relay, mac, phone)
    }

    #[test]
    fn a_paired_mac_and_phone_reach_each_other() {
        let (mut relay, mut mac, mut phone) = paired();
        let now = Instant::now();
        send(&mut relay, &phone, &mac.id, &[1], None, now).unwrap();
        send(&mut relay, &mac, &phone.id, &[2], None, now).unwrap();
        assert_eq!(mac.frames(), vec![(phone.id.clone(), vec![1])]);
        assert_eq!(phone.frames(), vec![(mac.id.clone(), vec![2])]);
    }

    #[test]
    fn a_macs_set_applies_from_registration() {
        let mut relay = Relay::new(8);
        let stranger = register(&mut relay, 2, phone_role(MAC));
        let _mac = register(
            &mut relay,
            MAC,
            Role::Mac {
                paired_devices: vec![id(1)],
            },
        );
        assert_eq!(
            code(send(
                &mut relay,
                &stranger,
                &id(MAC),
                &[],
                None,
                Instant::now()
            )),
            ErrorCode::NotPaired
        );
    }

    #[test]
    fn a_phone_outside_the_set_is_not_paired_whether_or_not_the_mac_is_connected() {
        let (mut relay, mac, _phone) = paired();
        let stranger = register(&mut relay, 2, phone_role(MAC));
        let now = Instant::now();
        assert_eq!(
            code(send(&mut relay, &stranger, &mac.id, &[], None, now)),
            ErrorCode::NotPaired
        );
        relay.unregister(&mac.id, mac.generation, wall(5), Instant::now());
        let refusal = send(&mut relay, &stranger, &mac.id, &[], None, now).unwrap_err();
        assert_eq!(refusal.code, ErrorCode::NotPaired);
        assert_eq!(refusal.last_seen, None);
    }

    #[test]
    fn a_phone_reaches_only_the_mac_it_names() {
        let (mut relay, _mac, phone) = paired();
        let other_mac = register(
            &mut relay,
            3,
            Role::Mac {
                paired_devices: vec![id(1)],
            },
        );
        assert_eq!(
            code(send(
                &mut relay,
                &phone,
                &other_mac.id,
                &[],
                None,
                Instant::now()
            )),
            ErrorCode::NotPaired
        );
    }

    #[test]
    fn a_mac_reaches_only_phones_in_its_set_that_name_it() {
        let (mut relay, mac, _phone) = paired();
        let mut stranger = register(&mut relay, 2, phone_role(MAC));
        let mut elsewhere = register(&mut relay, 4, phone_role(3));
        relay
            .set_paired_devices(&mac.id, mac.generation, vec![id(1), elsewhere.id.clone()])
            .unwrap();
        let now = Instant::now();
        assert_eq!(
            code(send(&mut relay, &mac, &stranger.id, &[], None, now)),
            ErrorCode::NotPaired
        );
        assert_eq!(
            code(send(&mut relay, &mac, &elsewhere.id, &[], None, now)),
            ErrorCode::NotPaired
        );
        assert!(stranger.received().is_empty());
        assert!(elsewhere.received().is_empty());
    }

    #[test]
    fn a_device_listing_another_mac_cannot_reach_it_or_a_phone() {
        let (mut relay, mut mac, mut phone) = paired();
        let impostor = register(
            &mut relay,
            2,
            Role::Mac {
                paired_devices: vec![mac.id.clone(), phone.id.clone()],
            },
        );
        let now = Instant::now();
        assert_eq!(
            code(send(&mut relay, &impostor, &mac.id, &[], None, now)),
            ErrorCode::NotPaired
        );
        assert_eq!(
            code(send(&mut relay, &impostor, &phone.id, &[], None, now)),
            ErrorCode::NotPaired
        );
        assert!(mac.received().is_empty());
        assert!(phone.received().is_empty());
    }

    #[test]
    fn a_mac_with_no_held_set_is_offline_without_last_seen() {
        let mut relay = Relay::new(8);
        let phone = register(&mut relay, 1, phone_role(MAC));
        let refusal = send(&mut relay, &phone, &id(MAC), &[], None, Instant::now()).unwrap_err();
        assert_eq!(refusal, Refusal::mac_offline(&id(MAC), None));
    }

    #[test]
    fn a_disconnected_mac_is_offline_with_its_last_disconnect() {
        let (mut relay, mac, phone) = paired();
        relay.unregister(&mac.id, mac.generation, wall(42), Instant::now());
        let refusal = send(&mut relay, &phone, &mac.id, &[], None, Instant::now()).unwrap_err();
        assert_eq!(refusal.code, ErrorCode::MacOffline);
        assert_eq!(refusal.device_id, Some(mac.id.clone()));
        assert_eq!(refusal.last_seen, Some(wall(42)));
    }

    #[test]
    fn a_paired_phone_that_is_not_connected_is_phone_offline() {
        let mut relay = Relay::new(8);
        let mac = register(
            &mut relay,
            MAC,
            Role::Mac {
                paired_devices: vec![id(1)],
            },
        );
        let refusal = send(&mut relay, &mac, &id(1), &[], None, Instant::now()).unwrap_err();
        assert_eq!(refusal.code, ErrorCode::PhoneOffline);
        assert_eq!(refusal.device_id, Some(id(1)));
    }

    #[test]
    fn a_new_set_replaces_the_old_one() {
        let (mut relay, mac, phone) = paired();
        let new = register(&mut relay, 2, phone_role(MAC));
        relay
            .set_paired_devices(&mac.id, mac.generation, vec![new.id.clone()])
            .unwrap();
        let now = Instant::now();
        assert_eq!(
            code(send(&mut relay, &phone, &mac.id, &[], None, now)),
            ErrorCode::NotPaired
        );
        send(&mut relay, &new, &mac.id, &[], None, now).unwrap();
    }

    #[test]
    fn a_set_above_the_limit_is_refused_at_registration_and_on_update() {
        // Duplicates count: the list is measured before it is made a set.
        for too_many in [vec![id(1), id(2), id(3)], vec![id(1); 3]] {
            let mut relay = Relay::new(2);
            let closed = CancellationToken::new();
            let (outbox, _writer) = Outbox::new(64 * MIN_FRAME_COST, closed);
            let refusal = relay
                .register(
                    id(MAC),
                    Role::Mac {
                        paired_devices: too_many.clone(),
                    },
                    outbox,
                    wall(0),
                    Instant::now(),
                )
                .unwrap_err();
            assert_eq!(refusal.code, ErrorCode::LimitExceeded);
            assert!(!refusal.ends_connection());

            let mut relay = Relay::new(2);
            let phone = register(&mut relay, 1, phone_role(MAC));
            let mac = register(&mut relay, MAC, mac_role(&[&phone]));
            assert_eq!(
                relay
                    .set_paired_devices(&mac.id, mac.generation, too_many)
                    .unwrap_err()
                    .code,
                ErrorCode::LimitExceeded
            );
            send(&mut relay, &phone, &mac.id, &[], None, Instant::now()).unwrap();
        }
    }

    #[test]
    fn a_set_update_reports_whether_the_set_changed() {
        let (mut relay, mac, phone) = paired();
        let mut update = |ids: Vec<DeviceId>| {
            relay
                .set_paired_devices(&mac.id, mac.generation, ids)
                .unwrap()
        };
        assert!(!update(vec![phone.id.clone()]));
        assert!(!update(vec![phone.id.clone(), phone.id.clone()]));
        assert!(update(vec![phone.id.clone(), id(2)]));
        assert!(!update(vec![id(2), phone.id.clone()]));
        assert!(update(vec![]));
    }

    #[test]
    fn only_a_mac_may_change_a_set_or_a_pairing_window() {
        let (mut relay, _mac, phone) = paired();
        let refusals = [
            relay
                .set_paired_devices(&phone.id, phone.generation, vec![])
                .map(|_| ()),
            relay.open_pairing(
                &phone.id,
                phone.generation,
                token_hash(&TOKEN),
                300,
                Instant::now(),
            ),
            relay.close_pairing(&phone.id, phone.generation),
        ];
        for refusal in refusals {
            let refusal = refusal.unwrap_err();
            assert_eq!(refusal.code, ErrorCode::BadMessage);
            assert!(refusal.ends_connection());
        }
    }

    #[test]
    fn registering_as_a_phone_forgets_what_was_held_for_the_id_as_a_mac() {
        let (mut relay, mac, phone) = paired();
        relay
            .open_pairing(
                &mac.id,
                mac.generation,
                token_hash(&TOKEN),
                300,
                Instant::now(),
            )
            .unwrap();
        relay.unregister(&mac.id, mac.generation, wall(1), Instant::now());
        let _as_phone = register(&mut relay, MAC, phone_role(5));
        assert!(!relay.held_sets.contains_key(&mac.id));
        assert!(!relay.pairings.contains_key(&mac.id));
        let refusal = send(&mut relay, &phone, &mac.id, &[], None, Instant::now()).unwrap_err();
        assert_eq!(refusal, Refusal::mac_offline(&mac.id, None));
    }

    #[test]
    fn notices_go_once_to_paired_devices_and_no_one_else() {
        let mut relay = Relay::new(8);
        let phone = register(&mut relay, 1, phone_role(MAC));
        let mut other_phone = register(&mut relay, 2, phone_role(MAC));
        let mut stranger = register(&mut relay, 3, phone_role(MAC));
        let mut lister = register(
            &mut relay,
            4,
            Role::Mac {
                paired_devices: vec![id(MAC), phone.id.clone()],
            },
        );
        let mut mac = register(
            &mut relay,
            MAC,
            Role::Mac {
                paired_devices: vec![phone.id.clone(), other_phone.id.clone()],
            },
        );
        let notice = |device_id: &DeviceId| ServerMessage::PeerDisconnected {
            device_id: device_id.clone(),
        };

        relay.unregister(&phone.id, phone.generation, wall(1), Instant::now());
        relay.unregister(&phone.id, phone.generation, wall(2), Instant::now());
        assert_eq!(mac.received(), vec![notice(&phone.id)]);

        relay.unregister(&mac.id, mac.generation, wall(3), Instant::now());
        assert_eq!(other_phone.received(), vec![notice(&mac.id)]);
        assert!(stranger.received().is_empty());
        assert!(lister.received().is_empty());
    }

    #[test]
    fn a_replaced_registration_is_announced_once_and_its_late_close_changes_nothing() {
        let (mut relay, mut mac, old_phone) = paired();
        let mut new_phone = register(&mut relay, 1, phone_role(MAC));
        assert!(old_phone.closed.is_cancelled());
        let now = Instant::now();
        send(&mut relay, &new_phone, &mac.id, &[9], None, now).unwrap();
        assert!(!relay.unregister(&old_phone.id, old_phone.generation, wall(1), Instant::now()));

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
        send(&mut relay, &mac, &new_phone.id, &[1], None, now).unwrap();
        assert_eq!(new_phone.frames(), vec![(mac.id.clone(), vec![1])]);
    }

    #[test]
    fn a_replaced_connection_acting_late_changes_nothing() {
        let (mut relay, old_mac, mut phone) = paired();
        let now = Instant::now();
        let new_mac = register(&mut relay, MAC, mac_role(&[&phone]));
        relay
            .open_pairing(
                &new_mac.id,
                new_mac.generation,
                token_hash(&TOKEN),
                300,
                now,
            )
            .unwrap();

        relay
            .set_paired_devices(&old_mac.id, old_mac.generation, vec![])
            .unwrap();
        relay
            .close_pairing(&old_mac.id, old_mac.generation)
            .unwrap();
        relay
            .open_pairing(
                &old_mac.id,
                old_mac.generation,
                token_hash(&[9; 32]),
                300,
                now,
            )
            .unwrap();
        assert_eq!(
            code(send(&mut relay, &old_mac, &phone.id, &[1], None, now)),
            ErrorCode::NotPaired
        );

        // The new registration's set and pairing window are as it left them.
        send(&mut relay, &new_mac, &phone.id, &[2], None, now).unwrap();
        assert_eq!(phone.frames(), vec![(new_mac.id.clone(), vec![2])]);
        let candidate = register(&mut relay, 2, phone_role(MAC));
        send(&mut relay, &candidate, &new_mac.id, &[3], Some(&TOKEN), now).unwrap();
    }

    fn pairing_mac() -> (Relay, Device, Device, Instant) {
        let mut relay = Relay::new(8);
        let mac = register(
            &mut relay,
            MAC,
            Role::Mac {
                paired_devices: vec![],
            },
        );
        let candidate = register(&mut relay, 1, phone_role(MAC));
        let now = Instant::now();
        relay
            .open_pairing(&mac.id, mac.generation, token_hash(&TOKEN), 300, now)
            .unwrap();
        (relay, mac, candidate, now)
    }

    #[test]
    fn the_phone_bound_to_a_pairing_window_and_its_mac_hear_of_each_others_end_once() {
        let (mut relay, mut mac, candidate, now) = pairing_mac();
        send(&mut relay, &candidate, &mac.id, &[1], Some(&TOKEN), now).unwrap();
        mac.received();
        relay.unregister(&candidate.id, candidate.generation, wall(1), Instant::now());
        assert_eq!(
            mac.received(),
            vec![ServerMessage::PeerDisconnected {
                device_id: candidate.id.clone()
            }]
        );

        // The window stays bound to the phone that left, which may return.
        let second = register(&mut relay, 2, phone_role(MAC));
        assert_eq!(
            code(send(&mut relay, &second, &mac.id, &[], Some(&TOKEN), now)),
            ErrorCode::PairingExpired
        );
        let mut candidate = register(&mut relay, 1, phone_role(MAC));
        send(&mut relay, &candidate, &mac.id, &[2], Some(&TOKEN), now).unwrap();

        relay.unregister(&mac.id, mac.generation, wall(2), Instant::now());
        assert_eq!(
            candidate.received(),
            vec![ServerMessage::PeerDisconnected {
                device_id: mac.id.clone()
            }]
        );
    }

    #[test]
    fn an_expired_pairing_window_carries_no_notices() {
        let (mut relay, mut mac, mut candidate, opened) = pairing_mac();
        send(&mut relay, &candidate, &mac.id, &[1], Some(&TOKEN), opened).unwrap();
        mac.received();
        let expired = opened + MAX_PAIRING_WINDOW;

        relay.unregister(&candidate.id, candidate.generation, wall(1), expired);
        assert!(mac.received().is_empty());

        let mut candidate_again = register(&mut relay, 1, phone_role(MAC));
        relay
            .open_pairing(&mac.id, mac.generation, token_hash(&TOKEN), 300, opened)
            .unwrap();
        send(
            &mut relay,
            &candidate_again,
            &mac.id,
            &[2],
            Some(&TOKEN),
            opened,
        )
        .unwrap();
        relay.unregister(&mac.id, mac.generation, wall(2), expired);
        assert!(candidate_again.received().is_empty());
        assert!(candidate.received().is_empty());
    }

    #[test]
    fn frames_racing_to_a_full_recipient_are_each_refused_and_it_is_closed_once() {
        let mut relay = Relay::new(8);
        let mut first = register(&mut relay, 1, phone_role(MAC));
        let mut second = register(&mut relay, 2, phone_role(MAC));
        let mut mac = register_with_budget(
            &mut relay,
            MAC,
            mac_role(&[&first, &second]),
            2 * MIN_FRAME_COST,
        );
        let now = Instant::now();
        send(&mut relay, &first, &mac.id, &[1], None, now).unwrap();
        send(&mut relay, &second, &mac.id, &[2], None, now).unwrap();

        for phone in [&first, &second] {
            let refusal = send(&mut relay, phone, &mac.id, &[3], None, now).unwrap_err();
            assert_eq!(refusal, Refusal::recipient_full(&mac.id));
            assert!(!refusal.ends_connection());
        }
        assert!(mac.closed.is_cancelled());
        assert_eq!(
            mac.frames(),
            vec![(first.id.clone(), vec![1]), (second.id.clone(), vec![2])]
        );
        // Room has come back, but the Mac has lost frames: it is still full.
        assert_eq!(
            code(send(&mut relay, &first, &mac.id, &[4], None, now)),
            ErrorCode::RecipientFull
        );

        // Its connection ends once, however many senders found it full.
        assert!(relay.unregister(&mac.id, mac.generation, wall(1), now));
        assert!(!relay.unregister(&mac.id, mac.generation, wall(2), now));
        for phone in [&mut first, &mut second] {
            assert_eq!(
                phone.received(),
                vec![ServerMessage::PeerDisconnected {
                    device_id: mac.id.clone()
                }]
            );
        }
        assert_eq!(
            code(send(&mut relay, &first, &mac.id, &[5], None, now)),
            ErrorCode::MacOffline
        );
    }

    #[test]
    fn a_frame_for_a_recipient_closing_for_another_reason_is_dropped_unrefused() {
        let (mut relay, mac, mut phone) = paired();
        phone.closed.cancel();
        send(&mut relay, &mac, &phone.id, &[1], None, Instant::now()).unwrap();
        assert!(phone.received().is_empty());
    }

    #[test]
    fn a_pairing_token_admits_the_first_device_only() {
        let (mut relay, mut mac, mut candidate, now) = pairing_mac();
        let second = register(&mut relay, 2, phone_role(MAC));

        send(&mut relay, &candidate, &mac.id, &[1], Some(&TOKEN), now).unwrap();
        assert_eq!(
            mac.received(),
            vec![ServerMessage::Frame {
                to: mac.id.clone(),
                from: candidate.id.clone(),
                record: vec![1],
                pairing_token: Some(TOKEN),
            }]
        );
        send(&mut relay, &mac, &candidate.id, &[2], None, now).unwrap();
        assert_eq!(candidate.frames(), vec![(mac.id.clone(), vec![2])]);

        assert_eq!(
            code(send(&mut relay, &second, &mac.id, &[], Some(&TOKEN), now)),
            ErrorCode::PairingExpired
        );
        assert_eq!(
            code(send(&mut relay, &mac, &second.id, &[], None, now)),
            ErrorCode::NotPaired
        );
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (mut relay, mac, candidate, now) = pairing_mac();
        assert_eq!(
            code(send(
                &mut relay,
                &candidate,
                &mac.id,
                &[],
                Some(&[8; 32]),
                now
            )),
            ErrorCode::PairingExpired
        );
    }

    #[test]
    fn a_pairing_window_expires_and_is_capped_at_five_minutes() {
        let (mut relay, mac, candidate, opened) = pairing_mac();
        relay
            .open_pairing(&mac.id, mac.generation, token_hash(&TOKEN), 10_000, opened)
            .unwrap();

        let just_before = opened + MAX_PAIRING_WINDOW.saturating_sub(Duration::from_millis(1));
        send(
            &mut relay,
            &candidate,
            &mac.id,
            &[],
            Some(&TOKEN),
            just_before,
        )
        .unwrap();
        let at_expiry = opened + MAX_PAIRING_WINDOW;
        assert_eq!(
            code(send(
                &mut relay,
                &candidate,
                &mac.id,
                &[],
                Some(&TOKEN),
                at_expiry
            )),
            ErrorCode::PairingExpired
        );
        // A decline sent just after expiry does not reach the phone, which
        // has its own deadline.
        assert_eq!(
            code(send(&mut relay, &mac, &candidate.id, &[], None, at_expiry)),
            ErrorCode::NotPaired
        );
    }

    #[test]
    fn a_pairing_window_ends_on_confirmation_close_reopen_and_disconnect() {
        let (mut relay, mut mac, candidate, now) = pairing_mac();
        let open = |relay: &mut Relay, mac: &Device| {
            relay
                .open_pairing(&mac.id, mac.generation, token_hash(&TOKEN), 300, now)
                .unwrap();
            send(relay, &candidate, &mac.id, &[], Some(&TOKEN), now).unwrap();
        };
        let refused = |relay: &mut Relay, mac: &Device| {
            send(relay, &candidate, &mac.id, &[], Some(&TOKEN), now)
                .is_err_and(|refusal| refusal.code == ErrorCode::PairingExpired)
        };

        open(&mut relay, &mac);
        relay
            .set_paired_devices(&mac.id, mac.generation, vec![id(5)])
            .unwrap();
        assert!(
            !refused(&mut relay, &mac),
            "a set without the candidate keeps the window"
        );
        relay
            .set_paired_devices(&mac.id, mac.generation, vec![candidate.id.clone()])
            .unwrap();
        send(&mut relay, &candidate, &mac.id, &[], None, now).unwrap();
        relay
            .set_paired_devices(&mac.id, mac.generation, vec![])
            .unwrap();
        assert!(refused(&mut relay, &mac), "confirmation ended the window");

        open(&mut relay, &mac);
        relay.close_pairing(&mac.id, mac.generation).unwrap();
        assert!(refused(&mut relay, &mac));

        open(&mut relay, &mac);
        relay
            .open_pairing(&mac.id, mac.generation, token_hash(&[9; 32]), 300, now)
            .unwrap();
        assert!(
            refused(&mut relay, &mac),
            "a new window replaces the old one"
        );

        open(&mut relay, &mac);
        relay.unregister(&mac.id, mac.generation, wall(1), Instant::now());
        mac = register(
            &mut relay,
            MAC,
            Role::Mac {
                paired_devices: vec![],
            },
        );
        assert!(
            refused(&mut relay, &mac),
            "the Mac's disconnect ends the window"
        );
    }

    #[test]
    fn the_longest_disconnected_held_sets_are_forgotten_past_the_bound() {
        let mut relay = Relay::new(8);
        let phone = register(&mut relay, 0xffff, phone_role(1));
        let count = u16::try_from(MAX_HELD_SETS).unwrap() + 6;
        // All connected at once, then all gone: pruning must follow the
        // disconnects, not only new sets.
        let macs: Vec<Device> = (1..=count)
            .map(|n| {
                register(
                    &mut relay,
                    n,
                    Role::Mac {
                        paired_devices: vec![phone.id.clone()],
                    },
                )
            })
            .collect();
        for (n, mac) in macs.iter().enumerate() {
            relay.unregister(
                &mac.id,
                mac.generation,
                wall(i64::try_from(n).unwrap()),
                Instant::now(),
            );
        }
        assert_eq!(relay.held_sets.len(), MAX_HELD_SETS);
        let now = Instant::now();
        let first = send(&mut relay, &phone, &id(1), &[], None, now).unwrap_err();
        assert_eq!(first.last_seen, None, "the first Mac to leave is forgotten");
        assert!(!relay.held_sets.contains_key(&id(6)));
        assert!(relay.held_sets.contains_key(&id(7)));
    }
}
