//! The WebSocket server over the routing table: one task per connection
//! reading the socket, and one writer task draining that connection's queue.

use std::io;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::Utc;
use futures::stream::SplitStream;
use futures::{FutureExt, StreamExt};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde_json::json;
use switchboard_remote_crypto::frame::MAX_FRAME_LEN;
use switchboard_remote_crypto::identity::{Signature, VerifyingKey, verify_relay_challenge};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::Config;
use crate::outbox::Outbox;
use crate::protocol::{
    ClientMessage, DeviceId, NONCE_LEN, RELAY_PROTOCOL, Register, Role, ServerMessage,
    claimed_relay_protocol,
};
use crate::state::{MAX_HELD_SETS, Refusal, Relay};

/// How long a closing connection's queued messages may take to go out.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// Messages read and discarded while a closing connection waits for the
/// client to answer its close; past this the socket is simply dropped.
const DRAIN_LIMIT: usize = 64;
/// Sockets accepted beyond `MAX_CONNECTIONS`, for plain HTTP: health checks,
/// and upgrades refused because the relay is full. Past this the relay
/// closes a new socket as soon as it accepts it, which a client sees as a
/// reset; answering it would take a task per socket, which is what the limit
/// is there to bound.
pub(crate) const HTTP_HEADROOM: usize = 16;
/// The longest request head read, and so the most a socket's HTTP buffers
/// hold before it upgrades.
const MAX_REQUEST_HEAD: usize = 16 * 1024;

/// A generous estimate of what one device id costs in the routing table: its
/// text, its `String`, and its slot in a hash set or map, with the value
/// beside it.
const TABLE_ID_BYTES: usize = 128;

struct Shared {
    config: Config,
    relay: Mutex<Relay>,
    /// One permit per WebSocket, taken before the upgrade and held until the
    /// socket is dropped, so `MAX_CONNECTIONS` bounds the upgraded sockets.
    admission: Arc<Semaphore>,
    shutdown: CancellationToken,
    /// Every accepted socket and every upgraded connection, so shutdown can
    /// wait for them.
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
) -> io::Result<()> {
    serve_validated(listener, validate(config)?, shutdown).await;
    Ok(())
}

fn validate(config: Config) -> io::Result<Config> {
    config
        .validated()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

/// The most memory connections' queues and message buffers can hold: each
/// WebSocket's queue budget and one message being read — its text, the copy
/// decoding an internally tagged message makes, and one record decoded —
/// and every socket's request head, read and answered, before it upgrades —
/// and the routing table's sets: one for every Mac connected or held after
/// it left, and beside each the Mac's own entries and the phone bound to its
/// pairing window. A bound on what clients can make the relay hold, not on
/// the process.
fn memory_ceiling(config: &Config) -> usize {
    let message = config
        .max_frame_bytes
        .saturating_mul(2)
        .saturating_add(MAX_FRAME_LEN);
    let websockets = config
        .max_connections
        .saturating_mul(config.queue_bytes.saturating_add(message));
    let heads = config
        .max_connections
        .saturating_add(HTTP_HEADROOM)
        .saturating_mul(2 * MAX_REQUEST_HEAD);
    let sets = MAX_HELD_SETS
        .saturating_add(config.max_connections)
        .saturating_mul(config.max_devices_per_mac.saturating_add(4))
        .saturating_mul(TABLE_ID_BYTES);
    websockets.saturating_add(heads).saturating_add(sets)
}

/// Accepts sockets itself rather than through `axum::serve`, so that every
/// socket counts against a limit, not only upgraded ones, and a socket that
/// has not upgraded is closed once a request head is overdue.
async fn serve_validated(listener: TcpListener, config: Config, shutdown: CancellationToken) {
    tracing::info!(
        memory_ceiling_bytes = memory_ceiling(&config),
        "queues and message buffers are bounded"
    );
    // `validated` keeps this within the semaphore's limit.
    let sockets = Arc::new(Semaphore::new(config.max_connections + HTTP_HEADROOM));
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
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error) if is_connection_error(&error) => continue,
            Err(error) => {
                tracing::warn!(%error, "could not accept a connection");
                // Out of file descriptors, most likely: let some close.
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_secs(1)) => continue,
                }
            }
        };
        let Ok(permit) = Arc::clone(&sockets).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let socket = Admitted {
            stream,
            _permit: permit,
        };
        shared
            .connections
            .spawn(serve_http(socket, app.clone(), Arc::clone(&shared)));
    }
    drop(listener);
    shared.connections.close();
    let closing = FLUSH_TIMEOUT + Duration::from_secs(1);
    if tokio::time::timeout(closing, shared.connections.wait())
        .await
        .is_err()
    {
        tracing::warn!("connections still closing at shutdown were abandoned");
    }
}

/// Whether an `accept` error is one connection's, gone before it was
/// accepted, rather than the listener's.
fn is_connection_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
    )
}

/// HTTP on one socket until it upgrades or closes. Each request head, the
/// first included, must arrive within the time a client has to register, so
/// a socket that sends nothing, or idles between requests, is closed.
async fn serve_http(socket: Admitted, app: Router, shared: Arc<Shared>) {
    let mut http = hyper::server::conn::http1::Builder::new();
    http.timer(TokioTimer::new())
        .header_read_timeout(shared.config.register_timeout)
        .max_buf_size(MAX_REQUEST_HEAD);
    let mut connection = pin!(
        http.serve_connection(TokioIo::new(socket), TowerToHyperService::new(app))
            .with_upgrades()
    );
    let served = tokio::select! {
        served = connection.as_mut() => served,
        () = shared.shutdown.cancelled() => {
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    };
    if let Err(error) = served {
        tracing::debug!(%error, "an HTTP connection failed");
    }
}

/// An accepted socket holding its share of the socket limit until it is
/// dropped: after an upgrade, with the WebSocket.
struct Admitted {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
}

impl AsyncRead for Admitted {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Admitted {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

/// A relay running in this process, for tests and local use.
pub struct RelayHandle {
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl RelayHandle {
    pub async fn start(addr: SocketAddr, config: Config) -> io::Result<Self> {
        let config = validate(config)?;
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve_validated(listener, config, shutdown.clone()));
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
    pub async fn shutdown(self) -> io::Result<()> {
        self.shutdown.cancel();
        self.task.await.map_err(io::Error::other)
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
    let ping_interval = shared.config.ping_interval;
    let writer_closed = closed.clone();
    let mut writer = tokio::spawn(async move {
        // The reader is watching for the connection to close; a writer that
        // died without closing it would leave its registration swallowing
        // frames, as a failed send would.
        if unwinding(writer.run(sink, ping_interval)).await.is_none() {
            writer_closed.cancel();
        }
    });
    let mut connection = Connection {
        limiter: RateLimiter::new(&shared.config, Instant::now()),
        shared: Arc::clone(&shared),
        stream,
        outbox,
        closed,
        registration: None,
        heard: Instant::now(),
    };
    unwinding(connection.run()).await;
    // Frames routed here before the registration ends below are dropped,
    // not queued. The writer does not watch it, so what is queued still
    // goes out.
    connection.closed.cancel();
    if let Some(registration) = connection.registration.take()
        && shared.relay().unregister(
            &registration.id,
            registration.generation,
            Utc::now(),
            table_now(),
        )
    {
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
        // An aborted task drops its half of the socket when it next runs,
        // not when it is aborted.
        let _ = writer.await;
    }
    drop(stream);
    // Only now are both halves of the socket gone, and with them its share
    // of the socket limit, so only now is its admission returned.
    drop(permit);
}

/// Runs `task` to its end, or until it panics, which is logged. Either way
/// what follows runs: a panic in one connection's task ends that
/// connection, which closes as it would have anyway, rather than leaving its
/// registration in the routing table with no one to end it.
async fn unwinding<T>(task: impl Future<Output = T>) -> Option<T> {
    let output = AssertUnwindSafe(task).catch_unwind().await.ok();
    if output.is_none() {
        tracing::error!("a connection's task panicked; closing the connection");
    }
    output
}

struct Registration {
    id: DeviceId,
    generation: u64,
}

struct Connection {
    shared: Arc<Shared>,
    stream: SplitStream<WebSocket>,
    outbox: Outbox,
    /// Cancelled when the relay shuts down, when a newer registration
    /// replaces this one, when a frame for this connection finds no room in
    /// its queue, or when it ends.
    closed: CancellationToken,
    limiter: RateLimiter,
    registration: Option<Registration>,
    /// When the client's last message of any kind arrived, or the
    /// connection opened.
    heard: Instant,
}

#[derive(Debug, Clone, Copy)]
enum Deadline {
    /// A fixed moment, unmoved by anything the client sends: the window to
    /// register, which pings must not stretch.
    At(Instant),
    /// This long after the client's last message of any kind, however long
    /// the relay took to act on it.
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
        let register_by = Instant::now() + self.shared.config.register_timeout;
        match self.next(Deadline::At(register_by)).await {
            Incoming::Message(ClientMessage::Register(claim)) => {
                if !self.register(&nonce, claim) {
                    return;
                }
            }
            Incoming::Message(_) => {
                self.refuse(Refusal::bad_message("register first"));
                return;
            }
            Incoming::End => return,
        }
        let idle_timeout = self.shared.config.idle_timeout;
        while let Incoming::Message(message) = self.next(Deadline::Idle(idle_timeout)).await {
            if !self.handle(message) {
                return;
            }
        }
    }

    /// Registers the connection, or queues the refusal and returns false.
    fn register(&mut self, nonce: &[u8; NONCE_LEN], claim: Register) -> bool {
        if claim.relay_protocol != RELAY_PROTOCOL {
            self.refuse(Refusal::protocol_mismatch());
            return false;
        }
        let Ok(key) = VerifyingKey::from_bytes(&claim.identity_public_key) else {
            self.refuse(Refusal::registration_refused("not an Ed25519 public key"));
            return false;
        };
        let id = claim.device_id;
        if DeviceId::from_identity_key(&key) != id {
            self.refuse(Refusal::registration_refused(
                "the device id is not this key's",
            ));
            return false;
        }
        if verify_relay_challenge(&key, nonce, &Signature::from_bytes(&claim.signature)).is_err() {
            self.refuse(Refusal::registration_refused(
                "the signature does not answer the challenge",
            ));
            return false;
        }
        if self.shared.config.deny_device_ids.contains(&id) {
            tracing::info!(device_id = %id, "refused a denied device");
            self.refuse(Refusal::registration_refused("this device is denied"));
            return false;
        }
        let registered = self.shared.relay().register(
            id.clone(),
            &claim.role,
            self.outbox.clone(),
            Utc::now(),
            table_now(),
        );
        match registered {
            Ok(generation) => {
                match &claim.role {
                    Role::Mac { paired_devices } => {
                        tracing::info!(device_id = %id, "registered a Mac");
                        log_set(&id, paired_devices);
                    }
                    Role::Phone { mac } => {
                        tracing::info!(device_id = %id, %mac, "registered a phone");
                    }
                }
                self.registration = Some(Registration { id, generation });
                true
            }
            Err(refusal) => {
                self.refuse(refusal);
                false
            }
        }
    }

    /// Acts on one message from a registered client; false ends the
    /// connection.
    fn handle(&self, message: ClientMessage) -> bool {
        let Some(Registration { id, generation }) = &self.registration else {
            return false;
        };
        let (id, generation) = (id, *generation);
        let refusal = match message {
            ClientMessage::Frame {
                to,
                record,
                pairing_token,
            } => {
                #[cfg(test)]
                assert_ne!(record, tests::PANIC_RECORD, "a test's panic");
                // A record longer than any frame does not decode, so it
                // never gets here: it is `bad_message` as not a relay message.
                self.shared
                    .relay()
                    .forward(id, generation, &to, record, pairing_token, table_now())
            }
            ClientMessage::PairedDevices { device_ids } => {
                let changed = self
                    .shared
                    .relay()
                    .set_paired_devices(id, generation, &device_ids);
                if changed == Ok(true) {
                    log_set(id, &device_ids);
                }
                changed.map(|_| ())
            }
            ClientMessage::PairingOpen {
                token_hash,
                expires_in,
            } => self.shared.relay().open_pairing(
                id,
                generation,
                token_hash,
                expires_in,
                table_now(),
            ),
            ClientMessage::PairingClose => self.shared.relay().close_pairing(id, generation),
            ClientMessage::Register(_) => Err(Refusal::bad_message("already registered")),
            ClientMessage::Unknown => Ok(()),
        };
        match refusal {
            Ok(()) => true,
            Err(refusal) => {
                let ends = refusal.ends_connection();
                self.refuse(refusal);
                !ends
            }
        }
    }

    /// The next client message, or `End` once the connection is over or
    /// `deadline` passes. Every message, pings and pongs included, is
    /// charged to the rate limit; a client over it is not refused but waited
    /// out before its message is acted on, which slows it through TCP.
    async fn next(&mut self, deadline: Deadline) -> Incoming {
        loop {
            let until = match deadline {
                Deadline::At(at) => at,
                Deadline::Idle(idle) => self.heard + idle,
            };
            // Checked before reading, not only by the read's timeout, which
            // never fires while a message is always ready.
            if Instant::now() >= until {
                tracing::debug!("closing a connection past its deadline");
                return Incoming::End;
            }
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
            self.heard = Instant::now();
            let (len, text) = match message {
                Message::Close(_) => return Incoming::End,
                Message::Binary(_) => {
                    self.refuse(Refusal::bad_message("messages are JSON text"));
                    return Incoming::End;
                }
                Message::Text(text) => (text.len(), Some(text)),
                Message::Ping(bytes) | Message::Pong(bytes) => (bytes.len(), None),
            };
            let wait = self.limiter.charge(Instant::now(), len);
            if !wait.is_zero() {
                let resume = Instant::now() + wait;
                let (until, past_deadline) = match deadline {
                    Deadline::At(at) if at < resume => (at, true),
                    _ => (resume, false),
                };
                tokio::select! {
                    () = self.closed.cancelled() => return Incoming::End,
                    () = tokio::time::sleep_until(until) => {}
                }
                if past_deadline {
                    tracing::debug!("closing a connection past its deadline");
                    return Incoming::End;
                }
            }
            let Some(text) = text else { continue };
            if let Ok(message) = serde_json::from_str(text.as_str()) {
                return Incoming::Message(message);
            }
            self.refuse(match claimed_relay_protocol(text.as_str()) {
                Some(theirs) if theirs != RELAY_PROTOCOL => Refusal::protocol_mismatch(),
                _ => Refusal::bad_message("not a relay message"),
            });
            return Incoming::End;
        }
    }

    fn send(&self, message: ServerMessage) {
        self.outbox.deliver(message);
    }

    /// Queues the error for a refusal.
    fn refuse(&self, refusal: Refusal) {
        self.send(refusal.into_message());
    }
}

/// The time the routing table is given: tokio's clock, which tests can
/// pause, as the type the clock-free table takes.
fn table_now() -> std::time::Instant {
    Instant::now().into_std()
}

/// A Mac's set, at debug: a client may send its set as often as its rate
/// allows. `tracing` formats it only when debug logging is on.
fn log_set(id: &DeviceId, paired_devices: &[DeviceId]) {
    tracing::debug!(device_id = %id, ?paired_devices, "paired devices");
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
    use futures::SinkExt;
    use switchboard_remote_crypto::DeviceKeys;
    use tokio_tungstenite::tungstenite;

    use super::*;
    use crate::protocol::ErrorCode;

    /// A frame carrying this record panics the reader that handles it.
    pub(super) const PANIC_RECORD: &[u8] = b"panic";

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn next(socket: &mut Socket) -> Option<ServerMessage> {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("the relay said nothing")
            {
                Some(Ok(tungstenite::Message::Text(text))) => {
                    return Some(serde_json::from_str(&text).unwrap());
                }
                Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => return None,
                Some(Ok(_)) => {}
            }
        }
    }

    async fn send(socket: &mut Socket, message: &ClientMessage) {
        let text = serde_json::to_string(message).unwrap();
        socket
            .send(tungstenite::Message::Text(text.into()))
            .await
            .unwrap();
    }

    async fn connect(relay: &RelayHandle, keys: &DeviceKeys, role: Role) -> Socket {
        let (mut socket, _) = tokio_tungstenite::connect_async(relay.url()).await.unwrap();
        let Some(ServerMessage::Challenge { nonce, .. }) = next(&mut socket).await else {
            panic!("no challenge");
        };
        let register = Register {
            relay_protocol: RELAY_PROTOCOL,
            device_id: DeviceId::from_identity_key(&keys.identity_public_key()),
            identity_public_key: keys.identity_public_key().to_bytes(),
            signature: keys.sign_relay_challenge(&nonce).to_bytes(),
            role,
        };
        send(&mut socket, &ClientMessage::Register(register)).await;
        assert_eq!(next(&mut socket).await, Some(ServerMessage::Registered));
        socket
    }

    fn frame(to: &DeviceId, record: &[u8]) -> ClientMessage {
        ClientMessage::Frame {
            to: to.clone(),
            record: record.to_vec(),
            pairing_token: None,
        }
    }

    #[tokio::test]
    async fn a_connection_whose_reader_panics_is_closed_and_its_end_announced() {
        let relay = RelayHandle::start(SocketAddr::from(([127, 0, 0, 1], 0)), Config::default())
            .await
            .unwrap();
        let (mac_keys, phone_keys, other_keys) = (
            DeviceKeys::generate().unwrap(),
            DeviceKeys::generate().unwrap(),
            DeviceKeys::generate().unwrap(),
        );
        let id_of = |keys: &DeviceKeys| DeviceId::from_identity_key(&keys.identity_public_key());
        let (mac_id, phone_id, other_id) =
            (id_of(&mac_keys), id_of(&phone_keys), id_of(&other_keys));
        let mac_of = || Role::Phone {
            mac: mac_id.clone(),
        };
        let mut phone = connect(&relay, &phone_keys, mac_of()).await;
        let mut other = connect(&relay, &other_keys, mac_of()).await;
        let mut mac = connect(
            &relay,
            &mac_keys,
            Role::Mac {
                paired_devices: vec![phone_id.clone(), other_id.clone()],
            },
        )
        .await;

        send(&mut phone, &frame(&mac_id, PANIC_RECORD)).await;
        assert_eq!(next(&mut phone).await, None);
        assert_eq!(
            next(&mut mac).await,
            Some(ServerMessage::PeerDisconnected {
                device_id: phone_id.clone()
            })
        );
        // The relay goes on serving everyone else.
        send(&mut other, &frame(&mac_id, b"still here")).await;
        assert_eq!(
            next(&mut mac).await,
            Some(ServerMessage::Frame {
                to: mac_id.clone(),
                from: other_id,
                record: b"still here".to_vec(),
                pairing_token: None,
            })
        );
        send(&mut mac, &frame(&phone_id, b"gone")).await;
        assert!(matches!(
            next(&mut mac).await,
            Some(ServerMessage::Error {
                code: ErrorCode::PhoneOffline,
                ..
            })
        ));
        relay.shutdown().await.unwrap();
    }

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
    fn the_default_memory_ceiling_is_about_792_mib() {
        // 64 queues of 12 MiB, each with a 128 KiB message read twice and a
        // decoded record, 80 request heads of 32 KiB, and 1,088 Macs' sets
        // of 8 phones.
        assert_eq!(
            memory_ceiling(&Config::default()),
            64 * (12 * 1024 * 1024 + 2 * 131_072 + MAX_FRAME_LEN)
                + 80 * 32 * 1024
                + 1088 * (8 + 4) * 128
        );
    }

    #[test]
    fn bytes_are_limited_too_and_one_full_message_always_fits_the_burst() {
        let (mut limiter, start) = limiter(1_000, 1_000, 4_000);
        assert!(limiter.charge(start, 4_000).is_zero());
        assert_eq!(limiter.charge(start, 500), Duration::from_millis(500));
    }
}
