//! The WebSocket server over the routing table: one task per connection
//! reading the socket, and one writer task draining that connection's queue.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::Utc;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use switchboard_remote_crypto::identity::{Signature, VerifyingKey, verify_relay_challenge};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::protocol::{
    ClientMessage, DeviceId, ErrorCode, NONCE_LEN, RELAY_PROTOCOL, ServerMessage,
};
use crate::state::{Outbox, Relay};

/// Messages queued for one connection before it counts as not reading and
/// is closed. Holds a whole 8 MiB envelope's records with room to spare.
const OUTBOUND_QUEUE: usize = 256;
/// How long a closing connection's queued messages may take to go out.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

struct Shared {
    config: Config,
    relay: Mutex<Relay>,
    open_connections: AtomicUsize,
    shutdown: CancellationToken,
}

impl Shared {
    fn relay(&self) -> MutexGuard<'_, Relay> {
        self.relay.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The relay's routes: `/connect` for the WebSocket, `/healthz` for the
/// host's health check. Cancelling `shutdown` closes every connection.
pub fn router(config: Config, shutdown: CancellationToken) -> Router {
    let shared = Arc::new(Shared {
        relay: Mutex::new(Relay::new(config.max_devices_per_mac)),
        config,
        open_connections: AtomicUsize::new(0),
        shutdown,
    });
    Router::new()
        .route("/connect", get(connect))
        .route("/healthz", get(healthz))
        .with_state(shared)
}

/// Serves until `shutdown` is cancelled and every connection has closed.
pub async fn serve(
    listener: TcpListener,
    config: Config,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let app = router(config, shutdown.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await
}

/// A relay running in this process, for tests and local use.
pub struct RelayHandle {
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<std::io::Result<()>>,
}

impl RelayHandle {
    pub async fn start(addr: SocketAddr, config: Config) -> std::io::Result<Self> {
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

    /// Closes every connection and stops listening. Everything the relay
    /// held is gone, as after a restart.
    pub async fn shutdown(self) -> std::io::Result<()> {
        self.shutdown.cancel();
        self.task.await.map_err(std::io::Error::other)?
    }
}

async fn healthz(State(shared): State<Arc<Shared>>) -> Response {
    let (registered, forwarded) = {
        let relay = shared.relay();
        (relay.registered_count(), relay.forwarded_count())
    };
    axum::Json(json!({
        "status": "ok",
        "open_connections": shared.open_connections.load(Ordering::Relaxed),
        "registered_devices": registered,
        "forwarded_frames": forwarded,
    }))
    .into_response()
}

async fn connect(State(shared): State<Arc<Shared>>, upgrade: WebSocketUpgrade) -> Response {
    let max = shared.config.max_frame_bytes;
    upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| run_connection(socket, shared))
}

/// A slot under `MAX_CONNECTIONS`, released when dropped.
struct Slot(Arc<Shared>);

impl Slot {
    fn acquire(shared: &Arc<Shared>) -> Option<Self> {
        let max = shared.config.max_connections;
        shared
            .open_connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |open| {
                (open < max).then_some(open + 1)
            })
            .ok()
            .map(|_| Self(Arc::clone(shared)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.open_connections.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn run_connection(socket: WebSocket, shared: Arc<Shared>) {
    let (sink, stream) = socket.split();
    let (outbound, queue) = mpsc::channel(OUTBOUND_QUEUE);
    let mut writer = tokio::spawn(write_queue(sink, queue));
    let slot = Slot::acquire(&shared);
    let closed = shared.shutdown.child_token();
    let mut connection = Connection {
        shared: Arc::clone(&shared),
        stream,
        outbox: Outbox::new(outbound, closed.clone()),
        closed,
        limiter: RateLimiter::new(shared.config.rate_limit, Instant::now()),
        registration: None,
    };
    if slot.is_some() {
        connection.run().await;
    } else {
        connection.refuse(
            ErrorCode::RelayFull,
            "this relay is at its connection limit",
        );
    }
    if let Some((id, generation)) = connection.registration.take() {
        shared.relay().unregister(&id, generation, Utc::now());
        tracing::info!(device_id = %id, "disconnected");
    }
    // Dropping the outbox, the queue's last sender, lets the writer flush
    // and send its close. Reading on until the client answers that close
    // matters: closing a socket with the client's messages still unread
    // resets the connection, and the reset can discard the error just sent.
    let mut stream = connection.into_stream();
    let drain = async { while let Some(Ok(_)) = stream.next().await {} };
    let flushed = tokio::time::timeout(FLUSH_TIMEOUT, async {
        let ((), _) = tokio::join!(drain, &mut writer);
    });
    if flushed.await.is_err() {
        writer.abort();
    }
    drop(slot);
}

async fn write_queue(
    mut sink: SplitSink<WebSocket, Message>,
    mut queue: mpsc::Receiver<ServerMessage>,
) {
    while let Some(message) = queue.recv().await {
        let text = match serde_json::to_string(&message) {
            Ok(text) => text,
            Err(error) => {
                tracing::error!(%error, "could not encode a message");
                continue;
            }
        };
        if sink.send(Message::Text(text.into())).await.is_err() {
            return;
        }
    }
    let _ = sink.send(Message::Close(None)).await;
    let _ = sink.close().await;
}

struct Connection {
    shared: Arc<Shared>,
    stream: SplitStream<WebSocket>,
    outbox: Outbox,
    /// Cancelled when the relay shuts down, when a newer registration
    /// replaces this one, or when this connection stops reading.
    closed: CancellationToken,
    limiter: RateLimiter,
    registration: Option<(DeviceId, u64)>,
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
        self.send(ServerMessage::Challenge { nonce });
        let register_timeout = self.shared.config.register_timeout;
        match self.next(register_timeout).await {
            Incoming::Message(ClientMessage::Register {
                relay_protocol,
                device_id,
                identity_public_key,
                signature,
            }) => {
                if !self.register(
                    &nonce,
                    relay_protocol,
                    device_id,
                    &identity_public_key,
                    &signature,
                ) {
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
        while let Incoming::Message(message) = self.next(idle_timeout).await {
            if !self.handle(message) {
                return;
            }
        }
    }

    /// Registers the connection, or queues the refusal and returns false.
    fn register(
        &mut self,
        nonce: &[u8; NONCE_LEN],
        relay_protocol: u32,
        claimed: DeviceId,
        identity_public_key: &[u8; 32],
        signature: &[u8; 64],
    ) -> bool {
        if relay_protocol != RELAY_PROTOCOL {
            self.refuse(
                ErrorCode::ProtocolMismatch,
                "this relay speaks relay protocol 1",
            );
            return false;
        }
        let Ok(key) = VerifyingKey::from_bytes(identity_public_key) else {
            self.refuse(ErrorCode::RegistrationRefused, "not an Ed25519 public key");
            return false;
        };
        if DeviceId::from_identity_key(&key) != claimed {
            self.refuse(
                ErrorCode::RegistrationRefused,
                "the device id is not this key's",
            );
            return false;
        }
        if verify_relay_challenge(&key, nonce, &Signature::from_bytes(signature)).is_err() {
            self.refuse(
                ErrorCode::RegistrationRefused,
                "the signature does not answer the challenge",
            );
            return false;
        }
        if self.shared.config.deny_device_ids.contains(&claimed) {
            tracing::info!(device_id = %claimed, "refused a denied device");
            self.refuse(ErrorCode::RegistrationRefused, "this device is denied");
            return false;
        }
        let generation =
            self.shared
                .relay()
                .register(claimed.clone(), self.outbox.clone(), Utc::now());
        tracing::info!(device_id = %claimed, "registered");
        self.registration = Some((claimed, generation));
        true
    }

    /// Acts on one message from a registered client; false ends the
    /// connection.
    fn handle(&mut self, message: ClientMessage) -> bool {
        let Some((id, _)) = &self.registration else {
            return false;
        };
        let id = id.clone();
        let refusal = match message {
            ClientMessage::Frame {
                to,
                record,
                pairing_token,
            } => self
                .shared
                .relay()
                .forward(&id, to, record, pairing_token, Instant::now()),
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

    /// The next client message, or `End` once the connection is over. Pings
    /// and pongs keep the connection alive and are otherwise skipped.
    async fn next(&mut self, wait: Duration) -> Incoming {
        loop {
            let received = tokio::select! {
                () = self.closed.cancelled() => return Incoming::End,
                received = tokio::time::timeout(wait, self.stream.next()) => received,
            };
            let message = match received {
                Err(_) => {
                    tracing::debug!("closing an idle connection");
                    return Incoming::End;
                }
                Ok(Some(Ok(message))) => message,
                Ok(None | Some(Err(_))) => return Incoming::End,
            };
            let text = match message {
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => return Incoming::End,
                Message::Text(text) => text,
                Message::Binary(_) => {
                    self.refuse(ErrorCode::BadMessage, "messages are JSON text");
                    return Incoming::End;
                }
            };
            if !self.limiter.allow(Instant::now()) {
                self.refuse(ErrorCode::RateLimited, "too many messages");
                return Incoming::End;
            }
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

/// A token bucket: `rate` messages a second, sustained, with bursts up to
/// `rate`.
struct RateLimiter {
    rate: f64,
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    fn new(rate: u32, now: Instant) -> Self {
        let rate = f64::from(rate);
        Self {
            rate,
            tokens: rate,
            last: now,
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.rate);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limiter_allows_a_burst_of_its_rate_then_refills_over_time() {
        let start = Instant::now();
        let mut limiter = RateLimiter::new(4, start);
        assert!((0..4).all(|_| limiter.allow(start)));
        assert!(!limiter.allow(start));
        assert!(!limiter.allow(start + Duration::from_millis(200)));
        assert!(limiter.allow(start + Duration::from_millis(260)));
        assert!(!limiter.allow(start + Duration::from_millis(260)));
        let later = start + Duration::from_mins(1);
        assert!((0..4).all(|_| limiter.allow(later)));
        assert!(!limiter.allow(later));
    }
}
