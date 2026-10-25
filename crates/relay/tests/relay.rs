//! The relay over real sockets: registration, forwarding, pair scoping,
//! pairing tokens, peer notices, flow control, and limits. The routing rules
//! and the timing decisions are unit-tested in `src/`; these check that the
//! server applies them in the order the protocol promises. They assert what
//! happens, never how long it took.

use std::net::SocketAddr;
use std::time::Duration;

use futures::{Sink, SinkExt, Stream, StreamExt};
use switchboard_relay::protocol::{
    ClientMessage, DeviceId, ErrorCode, PAIRING_TOKEN_LEN, RELAY_PROTOCOL, Register, Role,
    ServerMessage, token_hash,
};
use switchboard_relay::{Config, RelayHandle};
use switchboard_remote_crypto::DeviceKeys;
use switchboard_remote_crypto::frame::MAX_FRAME_LEN;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WAIT: Duration = Duration::from_secs(5);
const TOKEN: [u8; PAIRING_TOKEN_LEN] = [3; PAIRING_TOKEN_LEN];

async fn start(config: Config) -> RelayHandle {
    RelayHandle::start(SocketAddr::from(([127, 0, 0, 1], 0)), config)
        .await
        .unwrap()
}

async fn start_default() -> RelayHandle {
    start(Config::default()).await
}

/// An id no device here registers, used to provoke a reply.
fn nobody() -> DeviceId {
    DeviceId::parse(&"z".repeat(26)).unwrap()
}

fn new_keys() -> DeviceKeys {
    DeviceKeys::generate().unwrap()
}

fn id_of(keys: &DeviceKeys) -> DeviceId {
    DeviceId::from_identity_key(&keys.identity_public_key())
}

fn copy(keys: &DeviceKeys) -> DeviceKeys {
    DeviceKeys::restore(&keys.storage_bytes()).unwrap()
}

fn mac_role(phones: &[&DeviceId]) -> Role {
    Role::Mac {
        paired_devices: phones.iter().map(|id| (*id).clone()).collect(),
    }
}

fn phone_role(mac: &DeviceId) -> Role {
    Role::Phone { mac: mac.clone() }
}

fn register_message(keys: &DeviceKeys, nonce: &[u8], role: Role) -> ClientMessage {
    ClientMessage::Register(Register {
        relay_protocol: RELAY_PROTOCOL,
        device_id: id_of(keys),
        identity_public_key: keys.identity_public_key().to_bytes(),
        signature: keys.sign_relay_challenge(nonce).to_bytes(),
        role,
    })
}

/// Opens a socket and reads the challenge, which names the relay's protocol.
async fn open(url: &str) -> (Socket, [u8; 32]) {
    let (mut socket, _) = connect_async(url).await.unwrap();
    match next(&mut socket).await {
        Some(ServerMessage::Challenge {
            nonce,
            relay_protocol,
        }) => {
            assert_eq!(relay_protocol, RELAY_PROTOCOL);
            (socket, nonce)
        }
        other => panic!("expected a challenge, got {other:?}"),
    }
}

/// The next relay message, skipping pongs; `None` once the socket closes.
async fn next(
    socket: &mut (impl Stream<Item = Result<Message, tungstenite::Error>> + Unpin),
) -> Option<ServerMessage> {
    loop {
        let message = tokio::time::timeout(WAIT, socket.next())
            .await
            .expect("the relay said nothing");
        match message {
            Some(Ok(Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap()),
            Some(Ok(Message::Pong(_) | Message::Ping(_))) => {}
            Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
            Some(Ok(other)) => panic!("unexpected {other:?}"),
        }
    }
}

async fn send(
    socket: &mut (impl Sink<Message, Error = tungstenite::Error> + Unpin),
    message: &ClientMessage,
) {
    send_text(socket, &serde_json::to_string(message).unwrap()).await;
}

async fn send_text(
    socket: &mut (impl Sink<Message, Error = tungstenite::Error> + Unpin),
    text: &str,
) {
    socket.send(Message::Text(text.into())).await.unwrap();
}

fn frame_message(to: &DeviceId, record: &[u8]) -> ClientMessage {
    ClientMessage::Frame {
        to: to.clone(),
        record: record.to_vec(),
        pairing_token: None,
    }
}

/// Reads until the relay closes the socket; returns what it said first.
async fn until_closed(socket: &mut Socket) -> Vec<ServerMessage> {
    let mut said = Vec::new();
    while let Some(message) = next(socket).await {
        said.push(message);
    }
    said
}

fn error_code(message: &ServerMessage) -> ErrorCode {
    match message {
        ServerMessage::Error { code, .. } => *code,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn codes(messages: &[ServerMessage]) -> Vec<ErrorCode> {
    messages.iter().map(error_code).collect()
}

struct Client {
    socket: Socket,
    keys: DeviceKeys,
    id: DeviceId,
}

impl Client {
    async fn mac(relay: &RelayHandle, keys: DeviceKeys, phones: &[&DeviceId]) -> Self {
        Self::connect_as(relay, keys, mac_role(phones)).await
    }

    async fn phone(relay: &RelayHandle, keys: DeviceKeys, mac: &DeviceId) -> Self {
        Self::connect_as(relay, keys, phone_role(mac)).await
    }

    async fn connect_as(relay: &RelayHandle, keys: DeviceKeys, role: Role) -> Self {
        let (mut socket, nonce) = open(&relay.url()).await;
        send(&mut socket, &register_message(&keys, &nonce, role)).await;
        assert_eq!(next(&mut socket).await, Some(ServerMessage::Registered));
        let id = id_of(&keys);
        Self { socket, keys, id }
    }

    async fn send(&mut self, message: &ClientMessage) {
        send(&mut self.socket, message).await;
    }

    async fn frame(&mut self, to: &DeviceId, record: &[u8]) {
        self.send(&frame_message(to, record)).await;
    }

    async fn next(&mut self) -> ServerMessage {
        next(&mut self.socket).await.expect("the relay closed")
    }

    /// Waits until the relay has acted on everything this client sent: the
    /// relay handles one connection's messages in order, so the reply to a
    /// frame for nobody comes after all of them. Returns what arrived before.
    async fn sync(&mut self) -> Vec<ServerMessage> {
        self.frame(&nobody(), b"sync").await;
        let mut before = Vec::new();
        loop {
            let message = self.next().await;
            if matches!(&message, ServerMessage::Error { device_id: Some(id), .. } if *id == nobody())
            {
                return before;
            }
            before.push(message);
        }
    }

    async fn close(mut self) {
        self.socket.close(None).await.unwrap();
    }
}

/// A connected Mac with one connected phone paired.
async fn paired(relay: &RelayHandle) -> (Client, Client) {
    let (mac_keys, phone_keys) = (new_keys(), new_keys());
    let mac_id = id_of(&mac_keys);
    let phone = Client::phone(relay, phone_keys, &mac_id).await;
    let mac = Client::mac(relay, mac_keys, &[&phone.id]).await;
    (mac, phone)
}

fn frame_from(from: &DeviceId, to: &DeviceId, record: &[u8]) -> ServerMessage {
    ServerMessage::Frame {
        to: to.clone(),
        from: from.clone(),
        record: record.to_vec(),
        pairing_token: None,
    }
}

fn notice(device_id: &DeviceId) -> ServerMessage {
    ServerMessage::PeerDisconnected {
        device_id: device_id.clone(),
    }
}

#[tokio::test]
async fn forwards_a_pairs_frames_in_order_with_the_relays_from() {
    let relay = start_default().await;
    let (mut mac, mut phone) = paired(&relay).await;

    for n in 0..100u8 {
        // A client-supplied `from`, naming the Mac itself, is overwritten.
        let text = serde_json::json!({
            "type": "frame",
            "to": mac.id,
            "from": mac.id,
            "record": data_encoding::BASE64.encode(&[4, n]),
        });
        send_text(&mut phone.socket, &text.to_string()).await;
    }
    for n in 0..100u8 {
        assert_eq!(mac.next().await, frame_from(&phone.id, &mac.id, &[4, n]));
    }
    mac.frame(&phone.id, b"reply").await;
    assert_eq!(phone.next().await, frame_from(&mac.id, &phone.id, b"reply"));
}

#[tokio::test]
async fn an_offline_mac_is_reported_with_its_last_disconnect() {
    let relay = start_default().await;
    let (mac, mut phone) = paired(&relay).await;
    let mac_id = mac.id.clone();
    mac.close().await;
    assert_eq!(phone.next().await, notice(&mac_id));

    phone.frame(&mac_id, b"hello").await;
    match phone.next().await {
        ServerMessage::Error {
            code: ErrorCode::MacOffline,
            device_id: Some(device_id),
            last_seen: Some(_),
            ..
        } => assert_eq!(device_id, mac_id),
        other => panic!("expected mac_offline with last_seen, got {other:?}"),
    }
}

#[tokio::test]
async fn an_offline_phone_is_reported_to_its_mac() {
    let relay = start_default().await;
    let (mut mac, phone) = paired(&relay).await;
    let phone_id = phone.id.clone();
    phone.close().await;
    assert_eq!(mac.next().await, notice(&phone_id));

    mac.frame(&phone_id, b"event").await;
    assert_eq!(
        mac.next().await,
        ServerMessage::Error {
            code: ErrorCode::PhoneOffline,
            message: "that device is not connected".to_owned(),
            device_id: Some(phone_id),
            last_seen: None,
        }
    );
}

async fn registration_refusal(
    config: Config,
    register: impl FnOnce(&[u8; 32]) -> ClientMessage,
) -> Vec<ServerMessage> {
    let relay = start(config).await;
    let (mut socket, nonce) = open(&relay.url()).await;
    send(&mut socket, &register(&nonce)).await;
    until_closed(&mut socket).await
}

fn with_register(message: ClientMessage, edit: impl FnOnce(&mut Register)) -> ClientMessage {
    let ClientMessage::Register(mut register) = message else {
        unreachable!()
    };
    edit(&mut register);
    ClientMessage::Register(register)
}

#[tokio::test]
async fn registration_refuses_a_signature_over_another_challenge() {
    let keys = new_keys();
    let said = registration_refusal(Config::default(), |_| {
        register_message(&keys, &[0; 32], phone_role(&nobody()))
    })
    .await;
    assert_eq!(codes(&said), vec![ErrorCode::RegistrationRefused]);
}

#[tokio::test]
async fn registration_refuses_an_id_that_is_not_the_keys() {
    let keys = new_keys();
    let other = id_of(&new_keys());
    let said = registration_refusal(Config::default(), |nonce| {
        with_register(
            register_message(&keys, nonce, phone_role(&nobody())),
            |register| register.device_id = other,
        )
    })
    .await;
    assert_eq!(codes(&said), vec![ErrorCode::RegistrationRefused]);
}

#[tokio::test]
async fn registration_refuses_an_unsupported_relay_protocol() {
    let keys = new_keys();
    let said = registration_refusal(Config::default(), |nonce| {
        with_register(
            register_message(&keys, nonce, phone_role(&nobody())),
            |register| register.relay_protocol = RELAY_PROTOCOL + 1,
        )
    })
    .await;
    assert_eq!(codes(&said), vec![ErrorCode::ProtocolMismatch]);
}

#[tokio::test]
async fn a_register_in_another_protocols_shape_is_a_protocol_mismatch() {
    let relay = start_default().await;
    let newer = serde_json::json!({
        "type": "register",
        "relay_protocol": RELAY_PROTOCOL + 1,
        "identity": { "kind": "a shape this relay does not know" },
    });
    let (mut socket, _) = open(&relay.url()).await;
    send_text(&mut socket, &newer.to_string()).await;
    assert_eq!(
        codes(&until_closed(&mut socket).await),
        vec![ErrorCode::ProtocolMismatch]
    );

    // In this protocol's number, the same shape is just malformed.
    let current = serde_json::json!({
        "type": "register",
        "relay_protocol": RELAY_PROTOCOL,
        "identity": { "kind": "a shape this relay does not know" },
    });
    let (mut socket, _) = open(&relay.url()).await;
    send_text(&mut socket, &current.to_string()).await;
    assert_eq!(
        codes(&until_closed(&mut socket).await),
        vec![ErrorCode::BadMessage]
    );
}

#[tokio::test]
async fn registration_refuses_a_denied_device() {
    let keys = new_keys();
    let config = Config {
        deny_device_ids: [id_of(&keys)].into(),
        ..Config::default()
    };
    let said = registration_refusal(config, |nonce| {
        register_message(&keys, nonce, phone_role(&nobody()))
    })
    .await;
    assert_eq!(codes(&said), vec![ErrorCode::RegistrationRefused]);
}

#[tokio::test]
async fn registration_refuses_a_mac_with_too_many_phones() {
    let keys = new_keys();
    let config = Config {
        max_devices_per_mac: 1,
        ..Config::default()
    };
    let phones = [id_of(&new_keys()), id_of(&new_keys())];
    let said = registration_refusal(config, |nonce| {
        register_message(&keys, nonce, mac_role(&[&phones[0], &phones[1]]))
    })
    .await;
    assert_eq!(codes(&said), vec![ErrorCode::LimitExceeded]);
}

#[tokio::test]
async fn a_frame_before_registration_is_refused() {
    let said = registration_refusal(Config::default(), |_| frame_message(&nobody(), &[1])).await;
    assert_eq!(codes(&said), vec![ErrorCode::BadMessage]);
}

#[tokio::test]
async fn a_denied_device_cannot_be_reached() {
    let denied = id_of(&new_keys());
    let relay = start(Config {
        deny_device_ids: [denied.clone()].into(),
        ..Config::default()
    })
    .await;
    let mut mac = Client::mac(&relay, new_keys(), &[&denied]).await;
    mac.frame(&denied, b"x").await;
    assert_eq!(error_code(&mac.next().await), ErrorCode::PhoneOffline);
}

#[tokio::test]
async fn an_impostor_is_refused_after_a_restart() {
    let relay = start_default().await;
    let addr = relay.addr();
    let real = new_keys();
    let real_client = Client::phone(&relay, copy(&real), &nobody()).await;
    drop(real_client);
    relay.shutdown().await.unwrap();

    let relay = RelayHandle::start(addr, Config::default()).await.unwrap();
    let impostor = new_keys();
    let (mut socket, nonce) = open(&relay.url()).await;
    // The real device's id and public key, but the impostor's signature.
    send(
        &mut socket,
        &ClientMessage::Register(Register {
            relay_protocol: RELAY_PROTOCOL,
            device_id: id_of(&real),
            identity_public_key: real.identity_public_key().to_bytes(),
            signature: impostor.sign_relay_challenge(&nonce).to_bytes(),
            role: phone_role(&nobody()),
        }),
    )
    .await;
    assert_eq!(
        codes(&until_closed(&mut socket).await),
        vec![ErrorCode::RegistrationRefused]
    );
    Client::phone(&relay, real, &nobody()).await;
}

#[tokio::test]
async fn a_phone_reconnecting_before_its_mac_after_a_restart_is_told_mac_offline() {
    let relay = start_default().await;
    let addr = relay.addr();
    let (mac_keys, phone_keys) = (new_keys(), new_keys());
    let (mac_id, phone_id) = (id_of(&mac_keys), id_of(&phone_keys));
    let _phone = Client::phone(&relay, copy(&phone_keys), &mac_id).await;
    let _mac = Client::mac(&relay, copy(&mac_keys), &[&phone_id]).await;
    relay.shutdown().await.unwrap();

    let relay = RelayHandle::start(addr, Config::default()).await.unwrap();
    let mut phone = Client::phone(&relay, phone_keys, &mac_id).await;
    phone.frame(&mac_id, b"hello").await;
    assert_eq!(
        phone.next().await,
        ServerMessage::Error {
            code: ErrorCode::MacOffline,
            message: "that Mac is not connected".to_owned(),
            device_id: Some(mac_id.clone()),
            last_seen: None,
        }
    );

    let mut mac = Client::mac(&relay, mac_keys, &[&phone_id]).await;
    phone.frame(&mac_id, b"hello").await;
    assert_eq!(mac.next().await, frame_from(&phone_id, &mac_id, b"hello"));
}

#[tokio::test]
async fn a_phone_outside_the_set_is_not_paired_whether_or_not_the_mac_is_connected() {
    let relay = start_default().await;
    let (mac, mut phone) = paired(&relay).await;
    let mac_id = mac.id.clone();
    let mut stranger = Client::phone(&relay, new_keys(), &mac_id).await;

    stranger.frame(&mac_id, b"probe").await;
    assert_eq!(error_code(&stranger.next().await), ErrorCode::NotPaired);
    mac.close().await;
    assert_eq!(phone.next().await, notice(&mac_id));
    stranger.frame(&mac_id, b"probe").await;
    assert_eq!(error_code(&stranger.next().await), ErrorCode::NotPaired);
}

#[tokio::test]
async fn a_macs_set_applies_from_its_first_registered_moment() {
    let relay = start_default().await;
    let mac_keys = new_keys();
    let mac_id = id_of(&mac_keys);
    let mut stranger = Client::phone(&relay, new_keys(), &mac_id).await;
    let mut lister = Client::mac(&relay, new_keys(), &[&mac_id]).await;
    let mut mac = Client::mac(&relay, mac_keys, &[&nobody()]).await;

    stranger.frame(&mac_id, b"first").await;
    assert_eq!(error_code(&stranger.next().await), ErrorCode::NotPaired);
    lister.frame(&mac_id, b"first").await;
    assert_eq!(error_code(&lister.next().await), ErrorCode::NotPaired);
    assert_eq!(mac.sync().await, vec![]);
}

#[tokio::test]
async fn a_phone_may_not_send_a_macs_messages() {
    let relay = start_default().await;
    let (_mac, mut phone) = paired(&relay).await;
    phone
        .send(&ClientMessage::PairedDevices {
            device_ids: vec![nobody()],
        })
        .await;
    assert_eq!(
        codes(&until_closed(&mut phone.socket).await),
        vec![ErrorCode::BadMessage]
    );
}

#[tokio::test]
async fn closing_notifies_paired_devices_and_no_one_else() {
    let relay = start_default().await;
    let mac_keys = new_keys();
    let mac_id = id_of(&mac_keys);
    let phone = Client::phone(&relay, new_keys(), &mac_id).await;
    let mut other_phone = Client::phone(&relay, new_keys(), &mac_id).await;
    let mut stranger = Client::phone(&relay, new_keys(), &mac_id).await;
    let mut lister = Client::mac(&relay, new_keys(), &[&mac_id, &phone.id]).await;
    let mut mac = Client::mac(&relay, mac_keys, &[&phone.id, &other_phone.id]).await;
    let phone_id = phone.id.clone();

    phone.close().await;
    assert_eq!(mac.next().await, notice(&phone_id));
    mac.close().await;
    assert_eq!(other_phone.next().await, notice(&mac_id));
    // Every notice went out before `other_phone` heard of the second, so
    // anything for the stranger or the lister is already queued ahead of
    // its reply.
    assert_eq!(stranger.sync().await, vec![]);
    assert_eq!(lister.sync().await, vec![]);
}

#[tokio::test]
async fn a_replacement_is_announced_before_its_first_frame_and_only_once() {
    let relay = start_default().await;
    let (mut mac, mut old_phone) = paired(&relay).await;

    let mut new_phone = Client::phone(&relay, copy(&old_phone.keys), &mac.id).await;
    let phone_id = new_phone.id.clone();
    new_phone.frame(&mac.id, b"first").await;
    assert_eq!(mac.next().await, notice(&phone_id));
    assert_eq!(mac.next().await, frame_from(&phone_id, &mac.id, b"first"));

    // The relay closes the replaced socket only after its registration has
    // ended, so once it is closed a second notice would already be queued.
    assert_eq!(until_closed(&mut old_phone.socket).await, vec![]);
    new_phone.frame(&mac.id, b"second").await;
    assert_eq!(mac.next().await, frame_from(&phone_id, &mac.id, b"second"));
}

#[tokio::test]
async fn a_pairing_token_admits_its_first_device_until_confirmed() {
    let relay = start_default().await;
    let mac_keys = new_keys();
    let mac_id = id_of(&mac_keys);
    let mut mac = Client::mac(&relay, mac_keys, &[]).await;
    let mut candidate = Client::phone(&relay, new_keys(), &mac_id).await;
    let mut second = Client::phone(&relay, new_keys(), &mac_id).await;
    mac.send(&ClientMessage::PairingOpen {
        token_hash: token_hash(&TOKEN),
        expires_in: 300,
    })
    .await;
    assert_eq!(mac.sync().await, vec![]);
    let with_token = ClientMessage::Frame {
        to: mac_id.clone(),
        record: vec![1],
        pairing_token: Some(TOKEN),
    };

    candidate.send(&with_token).await;
    assert_eq!(
        mac.next().await,
        ServerMessage::Frame {
            to: mac_id.clone(),
            from: candidate.id.clone(),
            record: vec![1],
            pairing_token: Some(TOKEN),
        }
    );
    mac.frame(&candidate.id, b"message 2").await;
    assert_eq!(
        candidate.next().await,
        frame_from(&mac_id, &candidate.id, b"message 2")
    );

    second.send(&with_token).await;
    assert_eq!(error_code(&second.next().await), ErrorCode::PairingExpired);

    mac.send(&ClientMessage::PairedDevices {
        device_ids: vec![candidate.id.clone()],
    })
    .await;
    assert_eq!(mac.sync().await, vec![]);
    candidate.frame(&mac_id, b"paired").await;
    assert_eq!(
        mac.next().await,
        frame_from(&candidate.id, &mac_id, b"paired")
    );
    second.send(&with_token).await;
    assert_eq!(error_code(&second.next().await), ErrorCode::PairingExpired);
}

#[tokio::test]
async fn a_set_update_above_the_limit_is_refused_and_the_connection_kept() {
    let relay = start(Config {
        max_devices_per_mac: 1,
        ..Config::default()
    })
    .await;
    let (mut mac, mut phone) = paired(&relay).await;
    mac.send(&ClientMessage::PairedDevices {
        device_ids: vec![phone.id.clone(), nobody()],
    })
    .await;
    assert_eq!(codes(&mac.sync().await), vec![ErrorCode::LimitExceeded]);
    phone.frame(&mac.id, b"still paired").await;
    assert_eq!(
        mac.next().await,
        frame_from(&phone.id, &mac.id, b"still paired")
    );
}

/// Polls until a new connection is admitted, as the slot of a closed one
/// comes free.
async fn admitted_eventually(relay: &RelayHandle, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Ok((mut socket, _)) = connect_async(relay.url()).await
            && matches!(
                next(&mut socket).await,
                Some(ServerMessage::Challenge { .. })
            )
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no connection slot came free"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn connections_past_the_limit_are_refused_before_the_upgrade() {
    let relay = start(Config {
        max_connections: 2,
        ..Config::default()
    })
    .await;
    let first = Client::phone(&relay, new_keys(), &nobody()).await;
    let (_second, _) = open(&relay.url()).await;

    match connect_async(relay.url()).await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 503),
        other => panic!("expected HTTP 503, got {other:?}"),
    }

    first.close().await;
    admitted_eventually(&relay, WAIT).await;
}

#[tokio::test]
async fn a_refused_client_that_never_answers_the_close_keeps_its_slot_only_briefly() {
    let relay = start(Config {
        max_connections: 1,
        ..Config::default()
    })
    .await;
    let mut refused = Client::phone(&relay, new_keys(), &nobody()).await;
    refused
        .socket
        .send(Message::Binary(vec![1].into()))
        .await
        .unwrap();
    // `refused` never reads again, so it never answers the relay's close.
    // Its socket, and with it its slot, is dropped once the relay stops
    // waiting for that answer.
    admitted_eventually(&relay, WAIT).await;
    drop(refused);
}

#[tokio::test]
async fn a_silent_client_is_closed_and_a_pinging_one_is_not() {
    // Pings every 100 ms against a 2 s timeout, for longer than the timeout:
    // a twenty-fold margin, so a loaded machine cannot close the pinging one.
    // The relay's own pings, 25 s apart, come too late to draw a pong from
    // the silent one.
    let relay = start(Config {
        idle_timeout: Duration::from_secs(2),
        ..Config::default()
    })
    .await;
    let mut silent = Client::phone(&relay, new_keys(), &nobody()).await;
    let mut pinging = Client::phone(&relay, new_keys(), &nobody()).await;

    for _ in 0..30 {
        pinging
            .socket
            .send(Message::Ping(b"keepalive".to_vec().into()))
            .await
            .unwrap();
        let pong = tokio::time::timeout(WAIT, pinging.socket.next()).await;
        assert!(matches!(pong, Ok(Some(Ok(Message::Pong(_))))), "{pong:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(until_closed(&mut silent.socket).await, vec![]);
    assert_eq!(pinging.sync().await, vec![]);
}

#[tokio::test]
async fn pings_do_not_extend_the_registration_deadline() {
    let relay = start(Config {
        register_timeout: Duration::from_millis(300),
        ..Config::default()
    })
    .await;
    let (mut socket, _) = open(&relay.url()).await;
    for _ in 0..100 {
        if socket
            .send(Message::Ping(b"stalling".to_vec().into()))
            .await
            .is_err()
        {
            return;
        }
        match tokio::time::timeout(WAIT, socket.next()).await {
            Ok(Some(Ok(Message::Pong(_)))) => {}
            Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) => return,
            other => panic!("unexpected {other:?}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("a client that only pings was never closed");
}

#[tokio::test]
async fn pings_sent_back_to_back_do_not_extend_the_registration_deadline() {
    // At the default rate limit the relay takes about eight seconds to read
    // these, and one is always ready; it must close the connection at the
    // deadline with most of them unread, answering only those it read.
    const PINGS: usize = 2_000;
    let relay = start(Config {
        register_timeout: Duration::from_millis(300),
        ..Config::default()
    })
    .await;
    let (mut socket, _) = open(&relay.url()).await;
    for _ in 0..PINGS {
        socket
            .feed(Message::Ping(b"stalling".to_vec().into()))
            .await
            .unwrap();
    }
    socket.flush().await.unwrap();
    let pongs = tokio::time::timeout(WAIT, async {
        let mut pongs = 0;
        loop {
            match socket.next().await {
                Some(Ok(Message::Pong(_))) => pongs += 1,
                Some(Ok(Message::Close(_)) | Err(_)) | None => return pongs,
                other => panic!("unexpected {other:?}"),
            }
        }
    })
    .await
    .expect("a client flooding pings was never closed");
    assert!(pongs < PINGS / 2, "{pongs} of {PINGS} pings were read");
}

#[tokio::test]
async fn a_burst_over_the_rate_limit_is_slowed_and_delivered_whole() {
    let relay = start(Config {
        rate_limit: 20,
        ..Config::default()
    })
    .await;
    let (mut mac, mut phone) = paired(&relay).await;
    for n in 0..60u8 {
        phone.frame(&mac.id, &[n]).await;
    }
    for n in 0..60u8 {
        assert_eq!(mac.next().await, frame_from(&phone.id, &mac.id, &[n]));
    }
    assert_eq!(phone.sync().await, vec![]);
}

/// A budget of about one large frame, and no rate limit to speak of: a
/// recipient that stops reading runs out of room once the socket buffers
/// between it and the relay are full.
fn small_budget() -> Config {
    Config {
        queue_bytes: 96 * 1024,
        rate_limit: 1_000_000,
        rate_limit_bytes: 1 << 30,
        ..Config::default()
    }
}

fn large_record() -> Vec<u8> {
    vec![0; 60_000]
}

fn is_recipient_full(message: &ServerMessage, recipient: &DeviceId) -> bool {
    matches!(
        message,
        ServerMessage::Error { code: ErrorCode::RecipientFull, device_id: Some(id), .. }
            if id == recipient
    )
}

/// Sends `sender` large frames for `to`, which reads nothing, until the
/// relay refuses one. Returns what the relay said, the refusal and the
/// recipient's end announced once.
async fn send_until_full(sender: &mut Client, to: &DeviceId) -> Vec<ServerMessage> {
    let record = large_record();
    for _ in 0..2_000 {
        sender.frame(to, &record).await;
        let mut said = sender.sync().await;
        if said.is_empty() {
            continue;
        }
        if said.len() == 1 {
            said.push(sender.next().await);
        }
        return said;
    }
    panic!("a recipient that read nothing never ran out of room");
}

/// Reads the frames left for a closed client; whether the relay ended the
/// socket with a close frame.
async fn closes_with_a_close_frame(socket: &mut Socket) -> bool {
    loop {
        match tokio::time::timeout(WAIT, socket.next())
            .await
            .expect("the relay never closed the socket")
        {
            Some(Ok(Message::Close(_))) => return true,
            Some(Ok(Message::Text(text))) => assert!(matches!(
                serde_json::from_str(&text).unwrap(),
                ServerMessage::Frame { .. }
            )),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return false,
        }
    }
}

#[tokio::test]
async fn a_phone_with_no_room_is_closed_and_its_mac_told_and_kept() {
    let relay = start(small_budget()).await;
    let (mut mac, mut phone) = paired(&relay).await;
    let phone_id = phone.id.clone();

    let said = send_until_full(&mut mac, &phone_id).await;
    assert!(is_recipient_full(&said[0], &phone_id), "{said:?}");
    assert_eq!(said[1], notice(&phone_id));
    assert_eq!(mac.sync().await, vec![]);
    assert!(closes_with_a_close_frame(&mut phone.socket).await);
}

#[tokio::test]
async fn a_phone_with_no_room_is_closed_and_the_macs_other_phone_gets_every_frame_in_order() {
    let relay = start(small_budget()).await;
    let mac_keys = new_keys();
    let mac_id = id_of(&mac_keys);
    let stopped = Client::phone(&relay, new_keys(), &mac_id).await;
    let mut reading = Client::phone(&relay, new_keys(), &mac_id).await;
    let mut mac = Client::mac(&relay, mac_keys, &[&stopped.id, &reading.id]).await;
    let (stopped_id, reading_id) = (stopped.id.clone(), reading.id.clone());
    let record = large_record();

    let mut n = 0u32;
    let mut said = loop {
        assert!(n < 2_000, "a phone that read nothing never ran out of room");
        mac.frame(&stopped_id, &record).await;
        mac.frame(&reading_id, &n.to_be_bytes()).await;
        assert_eq!(
            reading.next().await,
            frame_from(&mac_id, &reading_id, &n.to_be_bytes())
        );
        n += 1;
        let said = mac.sync().await;
        if !said.is_empty() {
            break said;
        }
    };
    assert!(is_recipient_full(&said[0], &stopped_id), "{said:?}");
    if said.len() == 1 {
        said.push(mac.next().await);
    }
    assert_eq!(said[1], notice(&stopped_id));

    // The Mac was never held: what it sends next reaches the other phone.
    for n in n..n + 10 {
        mac.frame(&reading_id, &n.to_be_bytes()).await;
        assert_eq!(
            reading.next().await,
            frame_from(&mac_id, &reading_id, &n.to_be_bytes())
        );
    }
    assert_eq!(reading.sync().await, vec![]);
    drop(stopped);
}

#[tokio::test]
async fn a_mac_with_no_room_is_closed_and_its_phone_told_and_kept() {
    let relay = start(small_budget()).await;
    let (mut mac, mut phone) = paired(&relay).await;
    let mac_id = mac.id.clone();

    let said = send_until_full(&mut phone, &mac_id).await;
    assert!(is_recipient_full(&said[0], &mac_id), "{said:?}");
    assert_eq!(said[1], notice(&mac_id));
    phone.frame(&mac_id, b"after").await;
    assert_eq!(error_code(&phone.next().await), ErrorCode::MacOffline);
    assert!(closes_with_a_close_frame(&mut mac.socket).await);
}

#[tokio::test]
async fn a_burst_within_the_budget_to_a_slow_reader_is_delivered_whole() {
    // Twenty frames at the least a frame is charged fit a budget of 96 KiB
    // with none of them sent.
    let relay = start(small_budget()).await;
    let (mut mac, mut phone) = paired(&relay).await;
    for n in 0..20u8 {
        mac.frame(&phone.id, &[n]).await;
    }
    for n in 0..20u8 {
        assert_eq!(phone.next().await, frame_from(&mac.id, &phone.id, &[n]));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(mac.sync().await, vec![]);
    assert_eq!(phone.sync().await, vec![]);
}

#[tokio::test]
async fn a_phone_that_stops_reading_and_is_sent_nothing_is_closed_when_idle() {
    // The relay pings both. The Mac reads, so answers; the phone reads
    // nothing, so answers nothing, and its idle timeout runs out.
    let relay = start(Config {
        idle_timeout: Duration::from_secs(2),
        ping_interval: Duration::from_millis(200),
        ..Config::default()
    })
    .await;
    let (mut mac, mut phone) = paired(&relay).await;
    assert_eq!(mac.next().await, notice(&phone.id));
    assert_eq!(mac.sync().await, vec![]);
    assert_eq!(until_closed(&mut phone.socket).await, vec![]);
}

#[tokio::test]
async fn a_client_that_never_reads_its_errors_is_closed() {
    let relay = start(Config {
        queue_bytes: 96 * 1024,
        rate_limit: 1_000_000,
        rate_limit_bytes: 1 << 30,
        ..Config::default()
    })
    .await;
    let mut flooder = Client::phone(&relay, new_keys(), &nobody()).await;
    // Each frame earns an error the flooder never reads. Once the socket
    // buffers are full they pile up in its queue, and past its room for
    // control messages the relay closes it; its sends then fail.
    let text = serde_json::to_string(&frame_message(&id_of(&new_keys()), b"x")).unwrap();
    for _ in 0..2_000_000 {
        if flooder
            .socket
            .send(Message::Text(text.clone().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    panic!("a client that never read its errors was never closed");
}

#[tokio::test]
async fn a_message_over_the_frame_limit_closes_the_connection() {
    let relay = start_default().await;
    let mut client = Client::phone(&relay, new_keys(), &nobody()).await;
    client.frame(&nobody(), &[0; 256]).await;
    assert_eq!(error_code(&client.next().await), ErrorCode::MacOffline);

    // About 133,000 bytes as base64, over the default 131,072.
    client.frame(&nobody(), &vec![0; 100_000]).await;
    assert_eq!(until_closed(&mut client.socket).await, vec![]);
}

#[tokio::test]
async fn a_record_longer_than_any_frame_is_refused() {
    let relay = start_default().await;
    let (mut mac, mut phone) = paired(&relay).await;
    phone.frame(&mac.id, &vec![0; MAX_FRAME_LEN]).await;
    assert!(matches!(mac.next().await, ServerMessage::Frame { .. }));
    phone.frame(&mac.id, &vec![0; MAX_FRAME_LEN + 1]).await;
    assert_eq!(
        codes(&until_closed(&mut phone.socket).await),
        vec![ErrorCode::BadMessage]
    );
}

#[tokio::test]
async fn malformed_and_binary_messages_close_the_connection_and_unknown_types_do_not() {
    let relay = start_default().await;
    let mut client = Client::phone(&relay, new_keys(), &nobody()).await;
    send_text(&mut client.socket, r#"{"type":"from_the_future"}"#).await;
    assert_eq!(client.sync().await, vec![]);

    send_text(&mut client.socket, r#"{"type":"frame","to":"short"}"#).await;
    assert_eq!(
        codes(&until_closed(&mut client.socket).await),
        vec![ErrorCode::BadMessage]
    );

    let mut client = Client::phone(&relay, new_keys(), &nobody()).await;
    client
        .socket
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert_eq!(
        codes(&until_closed(&mut client.socket).await),
        vec![ErrorCode::BadMessage]
    );
}

async fn healthz(relay: &RelayHandle) -> String {
    let mut stream = TcpStream::connect(relay.addr()).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    tokio::time::timeout(WAIT, stream.read_to_string(&mut response))
        .await
        .expect("the response did not end the connection")
        .unwrap();
    response
}

/// Asks for `/healthz` and leaves the connection open. `None` if the relay
/// closed the socket instead of answering.
async fn keep_alive_healthz(socket: &mut TcpStream) -> Option<String> {
    let request = b"GET /healthz HTTP/1.1\r\nHost: relay\r\n\r\n";
    if socket.write_all(request).await.is_err() {
        return None;
    }
    let mut response = Vec::new();
    let mut buffer = [0; 1024];
    while !response.ends_with(b"}") {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => response.extend_from_slice(&buffer[..read]),
        }
    }
    Some(String::from_utf8_lossy(&response).into_owned())
}

#[tokio::test]
async fn healthz_answers_only_its_status() {
    let relay = start_default().await;
    let (_mac, _phone) = paired(&relay).await;
    let response = healthz(&relay).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let body: serde_json::Value =
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body, serde_json::json!({ "status": "ok" }));
}

#[tokio::test]
async fn a_socket_that_sends_no_request_or_idles_after_one_is_closed() {
    let relay = start(Config {
        register_timeout: Duration::from_millis(300),
        ..Config::default()
    })
    .await;
    let silent = TcpStream::connect(relay.addr()).await.unwrap();
    let mut idle = TcpStream::connect(relay.addr()).await.unwrap();
    idle.write_all(b"GET /healthz HTTP/1.1\r\nHost: relay\r\n\r\n")
        .await
        .unwrap();
    for (name, mut socket) in [("silent", silent), ("idle", idle)] {
        let mut response = Vec::new();
        let ended = tokio::time::timeout(WAIT, socket.read_to_end(&mut response)).await;
        assert!(ended.is_ok(), "the {name} socket was kept open");
    }
}

#[tokio::test]
async fn a_relay_full_of_websockets_still_answers_http_until_its_headroom_is_taken() {
    let relay = start(Config {
        max_connections: 1,
        ..Config::default()
    })
    .await;
    let (_websocket, _) = open(&relay.url()).await;
    let response = healthz(&relay).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    // Each socket answered on a kept-alive connection holds a place until its
    // next request head is overdue. Once they fill the headroom, the relay
    // closes the next socket as soon as it accepts it, unanswered. Telling
    // the two apart takes no clock: one is answered, the other ends.
    let mut held = Vec::new();
    loop {
        let mut socket = TcpStream::connect(relay.addr()).await.unwrap();
        match tokio::time::timeout(WAIT, keep_alive_healthz(&mut socket))
            .await
            .expect("a socket was neither answered nor closed")
        {
            Some(response) => {
                assert!(response.starts_with("HTTP/1.1 200"), "{response}");
                held.push(socket);
            }
            None => break,
        }
        assert!(
            held.len() < 100,
            "sockets past the limit were never refused"
        );
    }
    assert!(!held.is_empty());

    drop(held);
    let deadline = tokio::time::Instant::now() + WAIT;
    while !healthz(&relay).await.starts_with("HTTP/1.1 200") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "closed sockets' places never came free"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn shutdown_closes_every_connection_with_a_close_frame_before_returning() {
    let relay = start_default().await;
    let (mut mac, _phone) = paired(&relay).await;
    let (_unread, _) = open(&relay.url()).await;
    let stopping = tokio::spawn(relay.shutdown());
    let closing = tokio::time::timeout(WAIT, async {
        loop {
            match mac.socket.next().await {
                Some(Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return false,
            }
        }
    })
    .await
    .unwrap();
    assert!(closing, "the connection ended without a close frame");
    // The unread connection never answers its close, so shutdown waits for
    // it only as long as the relay waits for any closing connection.
    tokio::time::timeout(WAIT, stopping)
        .await
        .expect("shutdown did not return")
        .unwrap()
        .unwrap();
}
