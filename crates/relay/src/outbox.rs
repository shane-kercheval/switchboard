//! A connection's outbound queue. Frames from other devices are bounded by
//! bytes, and a sender whose recipient is full waits for room instead of
//! losing the frame; the relay's own messages — `registered`, notices,
//! errors — never wait, and bound the queue by count. A recipient is closed
//! only when it stops making progress, or when even its control messages no
//! longer fit, so a phone that reads slowly is slowed, not cut off.
//!
//! Waiting couples a sender to its slowest recipient: a Mac whose phone is
//! slow sends nothing to its other phones while it waits. Its traffic is one
//! ordered stream, so the relay cannot reorder around the wait; the budget
//! is sized so that ordinary replies never wait.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures::SinkExt;
use futures::stream::SplitSink;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::protocol::ServerMessage;

/// The least a queued frame is charged, so the byte budget also bounds how
/// many frames a queue holds.
pub(crate) const MIN_FRAME_COST: usize = 4096;
/// Room for control messages beyond the most frames the budget admits. A
/// connection that lets this many pile up is not reading its own errors.
pub(crate) const CONTROL_HEADROOM: usize = 64;
/// A frame's JSON around its base64 record: type, ids, an optional token.
const FRAME_OVERHEAD: usize = 256;

/// What a frame carrying `record_len` bytes is charged against its
/// recipient's budget: about its encoded size, and at least
/// `MIN_FRAME_COST`.
pub(crate) fn frame_cost(record_len: usize) -> usize {
    (record_len.div_ceil(3) * 4 + FRAME_OVERHEAD).max(MIN_FRAME_COST)
}

/// Whether a recipient whose last completed send was at `last_progress` has
/// stopped reading.
pub(crate) fn is_stalled(last_progress: Instant, now: Instant, stall: Duration) -> bool {
    now.saturating_duration_since(last_progress) >= stall
}

pub(crate) struct Outbound {
    message: ServerMessage,
    /// A frame's share of the budget, returned once it has been sent.
    _permit: Option<OwnedSemaphorePermit>,
}

/// The sending side of one connection's queue.
#[derive(Debug, Clone)]
pub(crate) struct Outbox {
    queue: mpsc::Sender<Outbound>,
    budget: Arc<Semaphore>,
    capacity: usize,
    progress: Arc<Mutex<Instant>>,
    /// Cancelled to close the connection.
    closed: CancellationToken,
}

/// The receiving side, owned by the connection's writer task.
pub(crate) struct Writer {
    queue: mpsc::Receiver<Outbound>,
    progress: Arc<Mutex<Instant>>,
    closed: CancellationToken,
}

impl Outbox {
    pub fn new(queue_bytes: usize, closed: CancellationToken) -> (Self, Writer) {
        let (sender, receiver) = mpsc::channel(queue_bytes / MIN_FRAME_COST + CONTROL_HEADROOM);
        let progress = Arc::new(Mutex::new(Instant::now()));
        let outbox = Self {
            queue: sender,
            budget: Arc::new(Semaphore::new(queue_bytes)),
            capacity: queue_bytes,
            progress: Arc::clone(&progress),
            closed: closed.clone(),
        };
        let writer = Writer {
            queue: receiver,
            progress,
            closed,
        };
        (outbox, writer)
    }

    pub fn close(&self) {
        self.closed.cancel();
    }

    /// Queues one of the relay's own messages without waiting. If it does
    /// not fit, or the writer is gone, the connection is closed. Returns
    /// whether it was queued.
    pub fn deliver(&self, message: ServerMessage) -> bool {
        self.enqueue(Outbound {
            message,
            _permit: None,
        })
    }

    /// Queues a frame, waiting while the budget is full. Gives up, and
    /// returns false, if the recipient closes, if `sender_closed` is
    /// cancelled, or if the recipient completes no send for `stall` — in
    /// which case it is closed as not reading.
    pub async fn send_frame(
        &self,
        message: ServerMessage,
        cost: usize,
        stall: Duration,
        sender_closed: &CancellationToken,
    ) -> bool {
        let permits = u32::try_from(cost.min(self.capacity)).unwrap_or(u32::MAX);
        let permit = match Arc::clone(&self.budget).try_acquire_many_owned(permits) {
            Ok(permit) => permit,
            Err(TryAcquireError::Closed) => return false,
            Err(TryAcquireError::NoPermits) => {
                match self.wait_for_room(permits, stall, sender_closed).await {
                    Some(permit) => permit,
                    None => return false,
                }
            }
        };
        self.enqueue(Outbound {
            message,
            _permit: Some(permit),
        })
    }

    async fn wait_for_room(
        &self,
        permits: u32,
        stall: Duration,
        sender_closed: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        let acquire = Arc::clone(&self.budget).acquire_many_owned(permits);
        tokio::pin!(acquire);
        loop {
            tokio::select! {
                permit = &mut acquire => return permit.ok(),
                () = self.closed.cancelled() => return None,
                () = sender_closed.cancelled() => return None,
                () = tokio::time::sleep(stall) => {
                    if is_stalled(self.last_progress(), Instant::now(), stall) {
                        tracing::debug!("closing a connection that stopped reading");
                        self.closed.cancel();
                        return None;
                    }
                }
            }
        }
    }

    fn last_progress(&self) -> Instant {
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn enqueue(&self, outbound: Outbound) -> bool {
        match self.queue.try_send(outbound) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Closed(_)) => {
                self.closed.cancel();
                false
            }
        }
    }
}

impl Writer {
    /// Sends everything queued, in order, until every `Outbox` is dropped,
    /// then closes the socket. A failed send closes the connection, so a
    /// dead writer never leaves its registration swallowing frames.
    pub async fn run(mut self, mut sink: SplitSink<WebSocket, Message>) {
        while let Some(outbound) = self.queue.recv().await {
            let text = match serde_json::to_string(&outbound.message) {
                Ok(text) => text,
                Err(error) => {
                    tracing::error!(%error, "could not encode a message");
                    continue;
                }
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                self.closed.cancel();
                return;
            }
            *self.progress.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
            drop(outbound);
        }
        let _ = sink.send(Message::Close(None)).await;
        let _ = sink.close().await;
    }
}

#[cfg(test)]
impl Writer {
    pub fn try_recv(&mut self) -> Option<ServerMessage> {
        self.queue.try_recv().ok().map(|outbound| outbound.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::DeviceId;

    const BUDGET: usize = 4 * MIN_FRAME_COST;
    const STALL: Duration = Duration::from_secs(5);

    fn frame() -> ServerMessage {
        let id = DeviceId::parse(&"a".repeat(26)).unwrap();
        ServerMessage::Frame {
            to: id.clone(),
            from: id,
            record: vec![4],
            pairing_token: None,
        }
    }

    #[test]
    fn a_frame_costs_its_encoded_size_and_at_least_the_minimum() {
        assert_eq!(frame_cost(0), MIN_FRAME_COST);
        assert_eq!(frame_cost(30_000), 40_000 + FRAME_OVERHEAD);
    }

    #[test]
    fn stalled_means_no_completed_send_for_the_whole_window() {
        let start = Instant::now();
        assert!(!is_stalled(start, start + STALL / 2, STALL));
        assert!(is_stalled(start, start + STALL, STALL));
    }

    #[tokio::test]
    async fn a_control_message_to_a_dead_writer_closes_the_connection() {
        let closed = CancellationToken::new();
        let (outbox, writer) = Outbox::new(BUDGET, closed.clone());
        drop(writer);
        assert!(!outbox.deliver(ServerMessage::Registered));
        assert!(closed.is_cancelled());
    }

    #[tokio::test]
    async fn control_messages_past_the_headroom_close_the_connection() {
        let closed = CancellationToken::new();
        let (outbox, _writer) = Outbox::new(BUDGET, closed.clone());
        let fits = BUDGET / MIN_FRAME_COST + CONTROL_HEADROOM;
        assert!((0..fits).all(|_| outbox.deliver(ServerMessage::Registered)));
        assert!(!closed.is_cancelled());
        assert!(!outbox.deliver(ServerMessage::Registered));
        assert!(closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_sender_waits_for_room_and_proceeds_once_a_frame_is_sent() {
        let closed = CancellationToken::new();
        let (outbox, mut writer) = Outbox::new(BUDGET, closed.clone());
        let sender_closed = CancellationToken::new();
        for _ in 0..4 {
            assert!(
                outbox
                    .send_frame(frame(), MIN_FRAME_COST, STALL, &sender_closed)
                    .await
            );
        }
        let waiting = tokio::spawn({
            let outbox = outbox.clone();
            let sender_closed = sender_closed.clone();
            async move {
                outbox
                    .send_frame(frame(), MIN_FRAME_COST, STALL, &sender_closed)
                    .await
            }
        });
        tokio::time::sleep(STALL / 2).await;
        assert!(!waiting.is_finished());

        // The writer sends one frame: its share returns and progress moves.
        let sent = writer.queue.recv().await.unwrap();
        *writer.progress.lock().unwrap() = Instant::now();
        drop(sent);
        assert!(waiting.await.unwrap());
        assert!(!closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_recipient_that_keeps_sending_is_not_closed() {
        let closed = CancellationToken::new();
        let (outbox, mut writer) = Outbox::new(BUDGET, closed.clone());
        let sender_closed = CancellationToken::new();
        let drained = tokio::spawn(async move {
            // One frame every 4 s: slower than the sender, inside the stall.
            for _ in 0..12 {
                tokio::time::sleep(Duration::from_secs(4)).await;
                let sent = writer.queue.recv().await.unwrap();
                *writer.progress.lock().unwrap() = Instant::now();
                drop(sent);
            }
        });
        for _ in 0..12 {
            assert!(
                outbox
                    .send_frame(frame(), MIN_FRAME_COST, STALL, &sender_closed)
                    .await
            );
        }
        drained.await.unwrap();
        assert!(!closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_recipient_that_sends_nothing_for_the_stall_is_closed() {
        let closed = CancellationToken::new();
        let (outbox, _writer) = Outbox::new(BUDGET, closed.clone());
        let sender_closed = CancellationToken::new();
        for _ in 0..4 {
            assert!(
                outbox
                    .send_frame(frame(), MIN_FRAME_COST, STALL, &sender_closed)
                    .await
            );
        }
        assert!(
            !outbox
                .send_frame(frame(), MIN_FRAME_COST, STALL, &sender_closed)
                .await
        );
        assert!(closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiting_sender_gives_up_when_it_is_itself_closed() {
        let closed = CancellationToken::new();
        let (outbox, _writer) = Outbox::new(BUDGET, closed.clone());
        let sender_closed = CancellationToken::new();
        for _ in 0..4 {
            outbox
                .send_frame(frame(), MIN_FRAME_COST, STALL, &sender_closed)
                .await;
        }
        let waiting = tokio::spawn({
            let outbox = outbox.clone();
            let sender_closed = sender_closed.clone();
            async move {
                outbox
                    .send_frame(frame(), MIN_FRAME_COST, STALL, &sender_closed)
                    .await
            }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        sender_closed.cancel();
        assert!(!waiting.await.unwrap());
        assert!(!closed.is_cancelled(), "the recipient is not to blame");
    }
}
