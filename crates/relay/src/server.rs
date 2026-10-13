//! The WebSocket server over the routing table: one task per connection
//! reading the socket, and one writer task draining that connection's queue.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::Utc;
use futures::StreamExt;
use futures::stream::SplitStream;
use serde_json::json;
use switchboard_remote_crypto::frame::MAX_FRAME_LEN;
use switchboard_remote_crypto::identity::{Signature, VerifyingKey, verify_relay_challenge};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::Config;
use crate::outbox::{Outbox, frame_cost};
use crate::protocol::{
    ClientMessage, DeviceId, ErrorCode, NONCE_LEN, PAIRING_TOKEN_LEN, RELAY_PROTOCOL, Role,
    ServerMessage,
};
use crate::state::Relay;

/// How long a closing connection's queued messages may take to go out.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// Messages read and discarded while a closing connection waits for the
/// client to answer its close; past this the socket is simply dropped.
const DRAIN_LIMIT: usize = 64;

struct Shared {
    config: Config,
    relay: Mutex<Relay>,
    /// One permit per open socket, taken before the upgrade and held until
    /// the socket is dropped, so `MAX_CONNECTIONS` bounds sockets and tasks.
    admission: Arc<Semaphore>,
    shutdown: CancellationToken,
    /// Every upgraded connection, so shutdown can wait for them: axum runs
    /// them detached from the HTTP server's own graceful shutdown.
    connections: TaskTracker,
}

impl Shared {
    fn relay(&self) -> MutexGuard<'_, Relay> {
        self.relay.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Serves until `shutdown` is cancelled, then closes every connection and
/// waits, for a bounded time, for each to finish closing. A connection still
/// being upgraded when shutdown begins is not waited for.
pub async fn serve(
    listener: TcpListener,
    config: Config,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let config = config
        .validated()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    tracing::info!(
        memory_ceiling_bytes = config.memory_ceiling(),
        "queues and read buffers are bounded"
    );
    let shared = Arc::new(Shared {
        relay: Mutex::new(Relay::new(config.max_devices_per_mac)),
        admission: Arc::new(Semaphore::new(config.max_connections)),
        config,
        shutdown: shutdown.clone(),
        connections: TaskTracker::new(),
    });
    let app = Router::new()
        .route("/connect", get(connect))
        .route("/healthz", get(healthz))
        .with_state(Arc::clone(&shared));
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    shared.connections.close();
    let closing = FLUSH_TIMEOUT + Duration::from_secs(1);
    if tokio::time::timeout(closing, shared.connections.wait())
        .await
        .is_err()
    {
        tracing::warn!("connections still closing at shutdown were abandoned");
    }
    Ok(())
}

/// A relay running in this process, for tests and local use.
pub struct RelayHandle {
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<std::io::Result<()>>,
}

impl RelayHandle {
    pub async fn start(addr: SocketAddr, config: Config) -> std::io::Result<Self> {
        let config = config
            .validated()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(listener, config, shutdown.clone()));
        Ok(Self {
            addr,
            shutdown,
            task,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn url(&self) -> String {
        format!("ws://{}/connect", self.addr)
    }

    /// Closes every connection, waits for them to finish closing, and stops
    /// listening. Everything the relay held is gone, as after a restart.
    pub async fn shutdown(self) -> std::io::Result<()> {
        self.shutdown.cancel();
        self.task.await.map_err(std::io::Error::other)?
    }
}

/// Only the status: connection and device counts would tell anyone who can
/// reach the relay how it is used.
async fn healthz() -> Response {
    axum::Json(json!({ "status": "ok" })).into_response()
}

async fn connect(State(shared): State<Arc<Shared>>, upgrade: WebSocketUpgrade) -> Response {
    let Ok(permit) = Arc::clone(&shared.admission).try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "this relay is at its connection limit",
        )
            .into_response();
    };
    let max = shared.config.max_frame_bytes;
    let tracker = shared.connections.clone();
    // The permit moves into the closure: if the upgrade fails, axum drops
    // the closure and the permit with it.
    upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| tracker.track_future(run_connection(socket, shared, permit)))
}

async fn run_connection(socket: WebSocket, shared: Arc<Shared>, permit: OwnedSemaphorePermit) {
    let (sink, stream) = socket.split();
    let closed = shared.shutdown.child_token();
    let (outbox, writer) = Outbox::new(shared.config.queue_bytes, closed.clone());
    let mut writer = tokio::spawn(writer.run(sink));
    let mut connection = Connection {
        limiter: RateLimiter::new(&shared.config, Instant::now()),
        shared: Arc::clone(&shared),
        stream,
        outbox,
        closed,
        registration: None,
    };
    connection.run().await;
    if let Some(registration) = connection.registration.take() {
        shared
            .relay()
            .unregister(&registration.id, registration.generation, Utc::now());
        tracing::info!(device_id = %registration.id, "disconnected");
    }
    // Dropping the outbox, the queue's last sender, lets the writer flush
    // and send its close. Reading on until the client answers that close
    // matters: closing a socket with the client's messages still unread
    // resets the connection, and the reset can discard the error just sent.
    // The reading is bounded by count and time, and nothing read is acted on.
    let mut stream = connection.into_stream();
    let drain = async {
        let mut read = 0;
        while read < DRAIN_LIMIT
            && let Some(Ok(_)) = stream.next().await
        {
            read += 1;
        }
    };
    let flushed = tokio::time::timeout(FLUSH_TIMEOUT, async {
        let ((), _) = tokio::join!(drain, &mut writer);
    });
    if flushed.await.is_err() {
        writer.abort();
    }
    // Only now is the socket gone, so only now is its admission returned.
    drop(permit);
}

struct Registration {
    id: DeviceId,
    generation: u64,
    is_mac: bool,
}

/// What a `register` message claims, checked by `Connection::register`.
struct Claim {
    relay_protocol: u32,
    device_id: DeviceId,
    identity_public_key: [u8; 32],
    signature: [u8; 64],
    role: Role,
}

struct Connection {
    shared: Arc<Shared>,
    stream: SplitStream<WebSocket>,
    outbox: Outbox,
    /// Cancelled when the relay shuts down, when a newer registration
    /// replaces this one, or when this connection stops reading.
    closed: CancellationToken,
    limiter: RateLimiter,
    registration: Option<Registration>,
}

#[derive(Debug, Clone, Copy)]
enum Deadline {
    /// A fixed moment, unmoved by anything the client sends: the window to
    /// register, which pings must not stretch.
    At(tokio::time::Instant),
    /// This long after the client's last message of any kind.
    Idle(Duration),
}

enum Incoming {
    Message(ClientMessage),
    /// The connection is over; anything owed to the client is queued.
    End,
}

impl Connection {
    /// Drops everything else, the outbox included.
    fn into_stream(self) -> SplitStream<WebSocket> {
        self.stream
    }

    async fn run(&mut self) {
        let mut nonce = [0u8; NONCE_LEN];
        if let Err(error) = getrandom::fill(&mut nonce) {
            tracing::error!(%error, "no randomness for a challenge");
            return;
        }
        self.send(ServerMessage::Challenge {
            nonce,
            relay_protocol: RELAY_PROTOCOL,
        });
        let register_by = tokio::time::Instant::now() + self.shared.config.register_timeout;
        match self.next(Deadline::At(register_by)).await {
            Incoming::Message(ClientMessage::Register {
                relay_protocol,
                device_id,
                identity_public_key,
                signature,
                role,
            }) => {
                let claim = Claim {
                    relay_protocol,
                    device_id,
                    identity_public_key,
                    signature,
                    role,
                };
                if !self.register(&nonce, claim) {
                    return;
                }
            }
            Incoming::Message(_) => {
                self.refuse(ErrorCode::BadMessage, "register first");
                return;
            }
            Incoming::End => return,
        }
        let idle_timeout = self.shared.config.idle_timeout;
        while let Incoming::Message(message) = self.next(Deadline::Idle(idle_timeout)).await {
            if !self.handle(message).await {
                return;
            }
        }
    }

    /// Registers the connection, or queues the refusal and returns false.
    fn register(&mut self, nonce: &[u8; NONCE_LEN], claim: Claim) -> bool {
        if claim.relay_protocol != RELAY_PROTOCOL {
            self.refuse(
                ErrorCode::ProtocolMismatch,
                "this relay speaks relay protocol 1",
            );
            return false;
        }
        let Ok(key) = VerifyingKey::from_bytes(&claim.identity_public_key) else {
            self.refuse(ErrorCode::RegistrationRefused, "not an Ed25519 public key");
            return false;
        };
        let id = claim.device_id;
        if DeviceId::from_identity_key(&key) != id {
            self.refuse(
                ErrorCode::RegistrationRefused,
                "the device id is not this key's",
            );
            return false;
        }
        if verify_relay_challenge(&key, nonce, &Signature::from_bytes(&claim.signature)).is_err() {
            self.refuse(
                ErrorCode::RegistrationRefused,
                "the signature does not answer the challenge",
            );
            return false;
        }
        if self.shared.config.deny_device_ids.contains(&id) {
            tracing::info!(device_id = %id, "refused a denied device");
            self.refuse(ErrorCode::RegistrationRefused, "this device is denied");
            return false;
        }
        match &claim.role {
            Role::Mac { paired_devices } => tracing::info!(
                device_id = %id,
                paired_devices = ?paired_devices.iter().map(DeviceId::as_str).collect::<Vec<_>>(),
                "registered a Mac"
            ),
            Role::Phone { mac } => tracing::info!(device_id = %id, %mac, "registered a phone"),
        }
        let is_mac = matches!(claim.role, Role::Mac { .. });
        let registered =
            self.shared
                .relay()
                .register(id.clone(), claim.role, self.outbox.clone(), Utc::now());
        match registered {
            Ok(generation) => {
                self.registration = Some(Registration {
                    id,
                    generation,
                    is_mac,
                });
                true
            }
            Err(refusal) => {
                self.send(refusal.into_message());
                false
            }
        }
    }

    /// Acts on one message from a registered client; false ends the
    /// connection.
    async fn handle(&mut self, message: ClientMessage) -> bool {
        let Some(registration) = &self.registration else {
            return false;
        };
        let (id, is_mac) = (registration.id.clone(), registration.is_mac);
        let refusal = match message {
            ClientMessage::Frame {
                to,
                record,
                pairing_token,
            } => {
                if record.len() > MAX_FRAME_LEN {
                    self.refuse(ErrorCode::BadMessage, "a record longer than any frame");
                    return false;
                }
                self.forward(&id, to, record, pairing_token).await;
                Ok(())
            }
            ClientMessage::PairedDevices { .. }
            | ClientMessage::PairingOpen { .. }
            | ClientMessage::PairingClose
                if !is_mac =>
            {
                self.refuse(ErrorCode::BadMessage, "only a Mac may send this");
                return false;
            }
            ClientMessage::PairedDevices { device_ids } => {
                tracing::info!(
                    device_id = %id,
                    paired_devices = ?device_ids.iter().map(DeviceId::as_str).collect::<Vec<_>>(),
                    "paired devices"
                );
                self.shared.relay().set_paired_devices(&id, device_ids)
            }
            ClientMessage::PairingOpen {
                token_hash,
                expires_in,
            } => {
                self.shared
                    .relay()
                    .open_pairing(&id, token_hash, expires_in, Instant::now());
                Ok(())
            }
            ClientMessage::PairingClose => {
                self.shared.relay().close_pairing(&id);
                Ok(())
            }
            ClientMessage::Register { .. } => {
                self.refuse(ErrorCode::BadMessage, "already registered");
                return false;
            }
            ClientMessage::Unknown => Ok(()),
        };
        if let Err(refusal) = refusal {
            self.send(refusal.into_message());
        }
        true
    }

    /// Routes a frame and queues it for its recipient, waiting while the
    /// recipient's queue is full. A refusal goes back to the sender; a frame
    /// that cannot be queued because its recipient closed is dropped, and the
    /// sender hears of that close through `peer_disconnected`.
    async fn forward(
        &self,
        from: &DeviceId,
        to: DeviceId,
        record: Vec<u8>,
        pairing_token: Option<[u8; PAIRING_TOKEN_LEN]>,
    ) {
        let routed = self
            .shared
            .relay()
            .route(from, &to, pairing_token.as_ref(), Instant::now());
        let recipient = match routed {
            Ok(recipient) => recipient,
            Err(refusal) => {
                self.send(refusal.into_message());
                return;
            }
        };
        let cost = frame_cost(record.len());
        let frame = ServerMessage::Frame {
            to,
            from: from.clone(),
            record,
            pairing_token,
        };
        recipient
            .send_frame(frame, cost, self.shared.config.stall_timeout, &self.closed)
            .await;
    }

    /// The next client message, or `End` once the connection is over or
    /// `deadline` passes. Every message, pings and pongs included, is
    /// charged to the rate limit; a client over it is not refused but waited
    /// out before its message is acted on, which slows it through TCP.
    async fn next(&mut self, deadline: Deadline) -> Incoming {
        loop {
            let until = match deadline {
                Deadline::At(at) => at,
                Deadline::Idle(idle) => tokio::time::Instant::now() + idle,
            };
            let received = tokio::select! {
                () = self.closed.cancelled() => return Incoming::End,
                received = tokio::time::timeout_at(until, self.stream.next()) => received,
            };
            let message = match received {
                Err(_) => {
                    tracing::debug!("closing a connection past its deadline");
                    return Incoming::End;
                }
                Ok(Some(Ok(message))) => message,
                Ok(None | Some(Err(_))) => return Incoming::End,
            };
            let len = match &message {
                Message::Close(_) => return Incoming::End,
                Message::Text(text) => text.len(),
                Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => bytes.len(),
            };
            let wait = self.limiter.charge(Instant::now(), len);
            if !wait.is_zero() {
                tokio::select! {
                    () = self.closed.cancelled() => return Incoming::End,
                    () = tokio::time::sleep(wait) => {}
                }
            }
            let text = match message {
                Message::Ping(_) | Message::Pong(_) | Message::Close(_) => continue,
                Message::Text(text) => text,
                Message::Binary(_) => {
                    self.refuse(ErrorCode::BadMessage, "messages are JSON text");
                    return Incoming::End;
                }
            };
            if let Ok(message) = serde_json::from_str(text.as_str()) {
                return Incoming::Message(message);
            }
            self.refuse(ErrorCode::BadMessage, "not a relay message");
            return Incoming::End;
        }
    }

    fn send(&self, message: ServerMessage) {
        self.outbox.deliver(message);
    }

    /// Queues an error for a refusal that ends the connection.
    fn refuse(&self, code: ErrorCode, message: &str) {
        self.send(ServerMessage::Error {
            code,
            message: message.to_owned(),
            device_id: None,
            last_seen: None,
        });
    }
}

/// Two token buckets, messages and bytes, each refilling at its rate. A
/// charge may overdraw them; the overdraft is how long the reader waits.
struct RateLimiter {
    messages: Bucket,
    bytes: Bucket,
}

impl RateLimiter {
    fn new(config: &Config, now: Instant) -> Self {
        let byte_rate = f64::from(config.rate_limit_bytes);
        #[allow(clippy::cast_precision_loss)] // A frame limit is far below 2^52.
        let byte_burst = byte_rate.max(config.max_frame_bytes as f64);
        let message_rate = f64::from(config.rate_limit);
        Self {
            messages: Bucket::new(message_rate, message_rate, now),
            bytes: Bucket::new(byte_rate, byte_burst, now),
        }
    }

    /// Charges one message of `len` bytes and returns how long to wait
    /// before acting on it.
    fn charge(&mut self, now: Instant, len: usize) -> Duration {
        #[allow(clippy::cast_precision_loss)] // A message is far below 2^52 bytes.
        let len = len as f64;
        self.messages
            .charge(now, 1.0)
            .max(self.bytes.charge(now, len))
    }
}

struct Bucket {
    rate: f64,
    capacity: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(rate: f64, capacity: f64, now: Instant) -> Self {
        Self {
            rate,
            capacity,
            tokens: capacity,
            last: now,
        }
    }

    fn charge(&mut self, now: Instant, amount: f64) -> Duration {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity) - amount;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / self.rate)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(
        rate_limit: u32,
        rate_limit_bytes: u32,
        max_frame_bytes: usize,
    ) -> (RateLimiter, Instant) {
        let start = Instant::now();
        let config = Config {
            rate_limit,
            rate_limit_bytes,
            max_frame_bytes,
            ..Config::default()
        };
        (RateLimiter::new(&config, start), start)
    }

    #[test]
    fn a_burst_of_the_message_rate_waits_for_nothing_and_the_next_waits_its_share() {
        let (mut limiter, start) = limiter(4, 1_000_000, 1_000);
        assert!((0..4).all(|_| limiter.charge(start, 1).is_zero()));
        assert_eq!(limiter.charge(start, 1), Duration::from_millis(250));
        // Waiting that long repays it; the next is due a quarter second on.
        let repaid = start + Duration::from_millis(250);
        assert_eq!(limiter.charge(repaid, 1), Duration::from_millis(250));
        let later = start + Duration::from_mins(1);
        assert!((0..4).all(|_| limiter.charge(later, 1).is_zero()));
    }

    #[test]
    fn bytes_are_limited_too_and_one_full_message_always_fits_the_burst() {
        let (mut limiter, start) = limiter(1_000, 1_000, 4_000);
        assert!(limiter.charge(start, 4_000).is_zero());
        assert_eq!(limiter.charge(start, 500), Duration::from_millis(500));
    }
}
