//! The relay over real sockets: registration, forwarding, pair scoping,
//! pairing tokens, peer notices, and limits. The routing rules themselves are
//! unit-tested in `src/state.rs`; these check that the server applies them in
//! the order the protocol promises.

use std::net::SocketAddr;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use switchboard_relay::protocol::{
    ClientMessage, DeviceId, ErrorCode, PAIRING_TOKEN_LEN, RELAY_PROTOCOL, ServerMessage,
    token_hash,
};
use switchboard_relay::{Config, RelayHandle};
use switchboard_remote_crypto::DeviceKeys;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
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

fn copy(keys: &DeviceKeys) -> DeviceKeys {
    DeviceKeys::restore(&keys.storage_bytes()).unwrap()
}

fn register_message(keys: &DeviceKeys, nonce: &[u8]) -> ClientMessage {
    ClientMessage::Register {
        relay_protocol: RELAY_PROTOCOL,
        device_id: DeviceId::from_identity_key(&keys.identity_public_key()),
        identity_public_key: keys.identity_public_key().to_bytes(),
        signature: keys.sign_relay_challenge(nonce).to_bytes(),
    }
}

/// Opens a socket and reads the challenge.
async fn open(url: &str) -> (Socket, [u8; 32]) {
    let (mut socket, _) = connect_async(url).await.unwrap();
    match next(&mut socket).await {
        Some(ServerMessage::Challenge { nonce }) => (socket, nonce),
        other => panic!("expected a challenge, got {other:?}"),
    }
}

/// The next relay message, skipping pongs; `None` once the socket closes.
async fn next(socket: &mut Socket) -> Option<ServerMessage> {
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

async fn send(socket: &mut Socket, message: &ClientMessage) {
    send_text(socket, &serde_json::to_string(message).unwrap()).await;
}

async fn send_text(socket: &mut Socket, text: &str) {
    socket.send(Message::Text(text.into())).await.unwrap();
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

struct Client {
    socket: Socket,
    keys: DeviceKeys,
    id: DeviceId,
}

impl Client {
    async fn connect(relay: &RelayHandle) -> Self {
        Self::connect_as(relay, DeviceKeys::generate().unwrap()).await
    }

    async fn connect_as(relay: &RelayHandle, keys: DeviceKeys) -> Self {
        let (mut socket, nonce) = open(&relay.url()).await;
        send(&mut socket, &register_message(&keys, &nonce)).await;
        assert_eq!(next(&mut socket).await, Some(ServerMessage::Registered));
        let id = DeviceId::from_identity_key(&keys.identity_public_key());
        Self { socket, keys, id }
    }

    async fn send(&mut self, message: &ClientMessage) {
        send(&mut self.socket, message).await;
    }

    async fn frame(&mut self, to: &DeviceId, record: &[u8]) {
        self.send(&ClientMessage::Frame {
            to: to.clone(),
            record: record.to_vec(),
            pairing_token: None,
        })
        .await;
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

    async fn pair(&mut self, phones: &[&DeviceId]) {
        self.send(&ClientMessage::PairedDevices {
            device_ids: phones.iter().map(|id| (*id).clone()).collect(),
        })
        .await;
        assert_eq!(self.sync().await, vec![]);
    }

    async fn close(mut self) {
        self.socket.close(None).await.unwrap();
    }
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
    let mut mac = Client::connect(&relay).await;
    let mut phone = Client::connect(&relay).await;
    mac.pair(&[&phone.id]).await;

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
    let mut mac = Client::connect(&relay).await;
    let mut phone = Client::connect(&relay).await;
    mac.pair(&[&phone.id]).await;
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
    let mut mac = Client::connect(&relay).await;
    let phone = Client::connect(&relay).await;
    let phone_id = phone.id.clone();
    mac.pair(&[&phone_id]).await;
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

fn codes(messages: &[ServerMessage]) -> Vec<ErrorCode> {
    messages.iter().map(error_code).collect()
}

#[tokio::test]
async fn registration_refuses_a_signature_over_another_challenge() {
    let keys = DeviceKeys::generate().unwrap();
    let said = registration_refusal(Config::default(), |_| register_message(&keys, &[0; 32])).await;
    assert_eq!(codes(&said), vec![ErrorCode::RegistrationRefused]);
}

#[tokio::test]
async fn registration_refuses_an_id_that_is_not_the_keys() {
    let keys = DeviceKeys::generate().unwrap();
    let other = DeviceKeys::generate().unwrap();
    let said = registration_refusal(Config::default(), |nonce| {
        let ClientMessage::Register {
            relay_protocol,
            identity_public_key,
            signature,
            ..
        } = register_message(&keys, nonce)
        else {
            unreachable!()
        };
        ClientMessage::Register {
            relay_protocol,
            device_id: DeviceId::from_identity_key(&other.identity_public_key()),
            identity_public_key,
            signature,
        }
    })
    .await;
    assert_eq!(codes(&said), vec![ErrorCode::RegistrationRefused]);
}

#[tokio::test]
async fn registration_refuses_an_unsupported_relay_protocol() {
    let keys = DeviceKeys::generate().unwrap();
    let said = registration_refusal(Config::default(), |nonce| {
        let ClientMessage::Register {
            device_id,
            identity_public_key,
            signature,
            ..
        } = register_message(&keys, nonce)
        else {
            unreachable!()
        };
        ClientMessage::Register {
            relay_protocol: RELAY_PROTOCOL + 1,
            device_id,
            identity_public_key,
            signature,
        }
    })
    .await;
    assert_eq!(codes(&said), vec![ErrorCode::ProtocolMismatch]);
}

#[tokio::test]
async fn registration_refuses_a_denied_device() {
    let keys = DeviceKeys::generate().unwrap();
    let id = DeviceId::from_identity_key(&keys.identity_public_key());
    let config = Config {
        deny_device_ids: [id].into(),
        ..Config::default()
    };
    let said = registration_refusal(config, |nonce| register_message(&keys, nonce)).await;
    assert_eq!(codes(&said), vec![ErrorCode::RegistrationRefused]);
}

#[tokio::test]
async fn a_frame_before_registration_is_refused() {
    let said = registration_refusal(Config::default(), |_| ClientMessage::Frame {
        to: nobody(),
        record: vec![1],
        pairing_token: None,
    })
    .await;
    assert_eq!(codes(&said), vec![ErrorCode::BadMessage]);
}

#[tokio::test]
async fn a_denied_device_cannot_be_reached() {
    let denied = DeviceKeys::generate().unwrap();
    let denied_id = DeviceId::from_identity_key(&denied.identity_public_key());
    let relay = start(Config {
        deny_device_ids: [denied_id.clone()].into(),
        ..Config::default()
    })
    .await;
    let mut mac = Client::connect(&relay).await;
    mac.pair(&[&denied_id]).await;

    mac.frame(&denied_id, b"x").await;
    let reply = mac.next().await;
    assert_eq!(error_code(&reply), ErrorCode::PhoneOffline);
}

#[tokio::test]
async fn an_impostor_is_refused_after_a_restart() {
    let relay = start_default().await;
    let addr = relay.addr();
    let real = DeviceKeys::generate().unwrap();
    let real_client = Client::connect_as(&relay, copy(&real)).await;
    drop(real_client);
    relay.shutdown().await.unwrap();

    let relay = RelayHandle::start(addr, Config::default()).await.unwrap();
    let impostor = DeviceKeys::generate().unwrap();
    let (mut socket, nonce) = open(&relay.url()).await;
    // The real device's id and public key, but the impostor's signature.
    send(
        &mut socket,
        &ClientMessage::Register {
            relay_protocol: RELAY_PROTOCOL,
            device_id: DeviceId::from_identity_key(&real.identity_public_key()),
            identity_public_key: real.identity_public_key().to_bytes(),
            signature: impostor.sign_relay_challenge(&nonce).to_bytes(),
        },
    )
    .await;
    assert_eq!(
        codes(&until_closed(&mut socket).await),
        vec![ErrorCode::RegistrationRefused]
    );
    Client::connect_as(&relay, real).await;
}

#[tokio::test]
async fn a_phone_reconnecting_before_its_mac_after_a_restart_is_told_mac_offline() {
    let relay = start_default().await;
    let addr = relay.addr();
    let mac_keys = DeviceKeys::generate().unwrap();
    let phone_keys = DeviceKeys::generate().unwrap();
    let mut mac = Client::connect_as(&relay, copy(&mac_keys)).await;
    let phone = Client::connect_as(&relay, copy(&phone_keys)).await;
    mac.pair(&[&phone.id]).await;
    let mac_id = mac.id.clone();
    relay.shutdown().await.unwrap();

    let relay = RelayHandle::start(addr, Config::default()).await.unwrap();
    let mut phone = Client::connect_as(&relay, phone_keys).await;
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

    let mut mac = Client::connect_as(&relay, mac_keys).await;
    mac.pair(&[&phone.id]).await;
    phone.frame(&mac_id, b"hello").await;
    assert_eq!(mac.next().await, frame_from(&phone.id, &mac_id, b"hello"));
}

#[tokio::test]
async fn a_device_outside_the_set_is_not_paired_whether_or_not_the_mac_is_connected() {
    let relay = start_default().await;
    let mut mac = Client::connect(&relay).await;
    let mut phone = Client::connect(&relay).await;
    let mut stranger = Client::connect(&relay).await;
    mac.pair(&[&phone.id]).await;
    let mac_id = mac.id.clone();

    stranger.frame(&mac_id, b"probe").await;
    assert_eq!(error_code(&stranger.next().await), ErrorCode::NotPaired);
    mac.close().await;
    assert_eq!(phone.next().await, notice(&mac_id));
    stranger.frame(&mac_id, b"probe").await;
    assert_eq!(error_code(&stranger.next().await), ErrorCode::NotPaired);
}

#[tokio::test]
async fn closing_notifies_paired_devices_and_no_one_else() {
    let relay = start_default().await;
    let mut mac = Client::connect(&relay).await;
    let phone = Client::connect(&relay).await;
    let mut other_phone = Client::connect(&relay).await;
    let mut stranger = Client::connect(&relay).await;
    let phone_id = phone.id.clone();
    mac.pair(&[&phone_id, &other_phone.id]).await;

    phone.close().await;
    assert_eq!(mac.next().await, notice(&phone_id));
    let mac_id = mac.id.clone();
    mac.close().await;
    assert_eq!(other_phone.next().await, notice(&mac_id));
    // Both notices went out before `other_phone` heard of the second, so
    // anything for the stranger would already be queued ahead of its reply.
    assert_eq!(stranger.sync().await, vec![]);
}

#[tokio::test]
async fn a_replacement_is_announced_before_its_first_frame_and_only_once() {
    let relay = start_default().await;
    let mut mac = Client::connect(&relay).await;
    let mut old_phone = Client::connect(&relay).await;
    mac.pair(&[&old_phone.id]).await;

    let mut new_phone = Client::connect_as(&relay, copy(&old_phone.keys)).await;
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
    let mut mac = Client::connect(&relay).await;
    let mut candidate = Client::connect(&relay).await;
    let mut second = Client::connect(&relay).await;
    mac.send(&ClientMessage::PairingOpen {
        token_hash: token_hash(&TOKEN),
        expires_in: 300,
    })
    .await;
    assert_eq!(mac.sync().await, vec![]);
    let with_token = |to: &DeviceId| ClientMessage::Frame {
        to: to.clone(),
        record: vec![1],
        pairing_token: Some(TOKEN),
    };

    candidate.send(&with_token(&mac.id)).await;
    assert_eq!(
        mac.next().await,
        ServerMessage::Frame {
            to: mac.id.clone(),
            from: candidate.id.clone(),
            record: vec![1],
            pairing_token: Some(TOKEN),
        }
    );
    mac.frame(&candidate.id, b"message 2").await;
    assert_eq!(
        candidate.next().await,
        frame_from(&mac.id, &candidate.id, b"message 2")
    );

    second.send(&with_token(&mac.id)).await;
    assert_eq!(error_code(&second.next().await), ErrorCode::PairingExpired);

    mac.pair(&[&candidate.id]).await;
    candidate.frame(&mac.id, b"paired").await;
    assert_eq!(
        mac.next().await,
        frame_from(&candidate.id, &mac.id, b"paired")
    );
    second.send(&with_token(&mac.id)).await;
    assert_eq!(error_code(&second.next().await), ErrorCode::PairingExpired);
}

#[tokio::test]
async fn a_set_above_the_limit_is_refused() {
    let relay = start(Config {
        max_devices_per_mac: 1,
        ..Config::default()
    })
    .await;
    let mut mac = Client::connect(&relay).await;
    let phone = Client::connect(&relay).await;
    mac.pair(&[&phone.id]).await;

    mac.send(&ClientMessage::PairedDevices {
        device_ids: vec![phone.id.clone(), nobody()],
    })
    .await;
    assert_eq!(
        codes(&mac.sync().await),
        vec![ErrorCode::LimitExceeded],
        "refused, and the connection stays open"
    );
}

#[tokio::test]
async fn connections_past_the_limit_are_refused() {
    let relay = start(Config {
        max_connections: 2,
        ..Config::default()
    })
    .await;
    let first = Client::connect(&relay).await;
    let (_second, _) = open(&relay.url()).await;

    let (mut third, _) = connect_async(relay.url()).await.unwrap();
    assert_eq!(
        codes(&until_closed(&mut third).await),
        vec![ErrorCode::RelayFull]
    );

    first.close().await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (mut socket, _) = connect_async(relay.url()).await.unwrap();
        if matches!(
            next(&mut socket).await,
            Some(ServerMessage::Challenge { .. })
        ) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the closed connection's slot was never released"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_silent_client_is_closed_and_a_pinging_one_is_not() {
    // Pings every 100 ms against a 2 s timeout, for longer than the timeout:
    // a twenty-fold margin, so a loaded machine cannot close the pinging one.
    let relay = start(Config {
        idle_timeout: Duration::from_secs(2),
        ..Config::default()
    })
    .await;
    let mut silent = Client::connect(&relay).await;
    let mut pinging = Client::connect(&relay).await;

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
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a client that only pings was never closed"
        );
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
}

#[tokio::test]
async fn pings_count_toward_the_rate_limit() {
    let relay = start(Config {
        rate_limit: 5,
        ..Config::default()
    })
    .await;
    let mut client = Client::connect(&relay).await;
    for _ in 0..20 {
        if client
            .socket
            .send(Message::Ping(b"flood".to_vec().into()))
            .await
            .is_err()
        {
            break;
        }
    }
    let said = until_closed(&mut client.socket).await;
    assert_eq!(codes(&said), vec![ErrorCode::RateLimited]);
}

#[tokio::test]
async fn a_refused_client_that_never_answers_the_close_does_not_keep_its_slot() {
    let relay = start(Config {
        max_connections: 1,
        ..Config::default()
    })
    .await;
    let mut refused = Client::connect(&relay).await;
    refused
        .socket
        .send(Message::Binary(vec![1].into()))
        .await
        .unwrap();
    // `refused` never reads again, so it never answers the relay's close.
    // Its slot must come free well before the relay stops waiting for that
    // answer.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let (mut socket, _) = connect_async(relay.url()).await.unwrap();
        if matches!(
            next(&mut socket).await,
            Some(ServerMessage::Challenge { .. })
        ) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the refused connection kept its slot while closing"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(refused);
}

#[tokio::test]
async fn a_client_over_the_rate_limit_is_closed() {
    let relay = start(Config {
        rate_limit: 5,
        ..Config::default()
    })
    .await;
    let mut client = Client::connect(&relay).await;
    for _ in 0..20 {
        client.frame(&nobody(), b"x").await;
    }
    let said = until_closed(&mut client.socket).await;
    assert_eq!(said.last().map(error_code), Some(ErrorCode::RateLimited));
    assert!(said.len() < 20, "{said:?}");
}

#[tokio::test]
async fn a_message_over_the_frame_limit_closes_the_connection() {
    let relay = start(Config {
        max_frame_bytes: 1024,
        ..Config::default()
    })
    .await;
    let mut client = Client::connect(&relay).await;
    client.frame(&nobody(), &[0; 256]).await;
    assert_eq!(error_code(&client.next().await), ErrorCode::MacOffline);

    client.frame(&nobody(), &[0; 1024]).await;
    assert_eq!(until_closed(&mut client.socket).await, vec![]);
}

#[tokio::test]
async fn malformed_and_binary_messages_close_the_connection_and_unknown_types_do_not() {
    let relay = start_default().await;
    let mut client = Client::connect(&relay).await;
    send_text(&mut client.socket, r#"{"type":"from_the_future"}"#).await;
    assert_eq!(client.sync().await, vec![]);

    send_text(&mut client.socket, r#"{"type":"frame","to":"short"}"#).await;
    assert_eq!(
        codes(&until_closed(&mut client.socket).await),
        vec![ErrorCode::BadMessage]
    );

    let mut client = Client::connect(&relay).await;
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

#[tokio::test]
async fn healthz_answers_with_counts() {
    let relay = start_default().await;
    let mut mac = Client::connect(&relay).await;
    let phone = Client::connect(&relay).await;
    mac.pair(&[&phone.id]).await;

    let mut stream = TcpStream::connect(relay.addr()).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let body: serde_json::Value =
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["registered_devices"], 2);
    assert_eq!(body["open_connections"], 2);
}
