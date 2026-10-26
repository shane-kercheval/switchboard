//! A connection's outbound queue. Frames from other devices are bounded by
//! bytes, and a frame for a recipient with no room left is refused, never
//! waited on, and the sender told, so a sender's reader never blocks on a
//! slow recipient. The relay's own messages — `registered`, notices,
//! errors — hold no budget, and are bounded only by the queue's count of
//! messages; a connection that leaves that many unread is closed.
//!
//! A refused frame leaves a gap in the one session it belonged to, which
//! that session's ends recover from by starting a new one. A phone with no
//! room is closed, since its one session is broken anyway: closing it makes
//! the gap a reconnect, after which it catches up. A Mac with no room is
//! kept, since closing it would end its sessions with every other phone.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::Message;
use data_encoding::BASE64;
use futures::{Sink, SinkExt};
#[cfg(test)]
use switchboard_remote_crypto::frame::{MAX_FRAME_LEN, MAX_MESSAGE_FRAMES};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::protocol::ServerMessage;

/// The least a queued frame is charged, so the byte budget also bounds how
/// many frames a queue holds.
pub(crate) const MIN_FRAME_COST: usize = 4096;
/// Room for messages beyond the most frames the budget admits. Control
/// messages hold no budget, so they may take the room frames leave too: a
/// connection with no frames queued is closed once it leaves
/// `queue_bytes / MIN_FRAME_COST + CONTROL_HEADROOM` of them unread, about
/// 3,200 at the default budget. One that does is not reading its own errors.
pub(crate) const CONTROL_HEADROOM: usize = 64;
/// A frame's JSON around its base64 record: type, ids, an optional token.
const FRAME_OVERHEAD: usize = 256;

/// What a frame carrying `record_len` bytes is charged against its
/// recipient's budget: about its encoded size, and at least
/// `MIN_FRAME_COST`.
pub(crate) fn frame_cost(record_len: usize) -> usize {
    (BASE64.encode_len(record_len) + FRAME_OVERHEAD).max(MIN_FRAME_COST)
}

/// The most the longest message a session sends can be charged: every one of
/// its frames at the longest. A budget at least this large takes a whole
/// message however slowly its recipient reads, so a phone that asks for one
/// is never closed for its size alone.
#[cfg(test)]
pub(crate) fn longest_message_cost() -> usize {
    MAX_MESSAGE_FRAMES * frame_cost(MAX_FRAME_LEN)
}

/// What a recipient with no room for a frame does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WhenFull {
    /// Closes, and refuses every frame that reaches it while it closes.
    Close,
    /// Stays open, refusing only the frames it has no room for.
    Refuse,
}

/// Why a frame was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotQueued {
    /// The recipient had no room for it, or was closed earlier for having
    /// none.
    Full,
    /// The recipient is closing for another reason.
    Closed,
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
    /// Cancelled to close the connection.
    closed: CancellationToken,
    /// Set when a frame found no room, so frames that reach the connection
    /// while it closes are refused the same way.
    full: Arc<AtomicBool>,
}

/// The receiving side, owned by the connection's writer task.
pub(crate) struct Writer {
    queue: mpsc::Receiver<Outbound>,
    closed: CancellationToken,
}

impl Outbox {
    pub fn new(queue_bytes: usize, closed: CancellationToken) -> (Self, Writer) {
        let (sender, receiver) = mpsc::channel(queue_bytes / MIN_FRAME_COST + CONTROL_HEADROOM);
        let outbox = Self {
            queue: sender,
            budget: Arc::new(Semaphore::new(queue_bytes)),
            capacity: queue_bytes,
            closed: closed.clone(),
            full: Arc::new(AtomicBool::new(false)),
        };
        let writer = Writer {
            queue: receiver,
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
        match self.queue.try_send(Outbound {
            message,
            _permit: None,
        }) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Closed(_)) => {
                self.closed.cancel();
                false
            }
        }
    }

    /// Queues a frame of `cost` without waiting, or refuses it as `Full` if
    /// the connection has no room for it, and then closes the connection if
    /// `when_full` says to.
    pub fn queue_frame(
        &self,
        message: ServerMessage,
        cost: usize,
        when_full: WhenFull,
    ) -> Result<(), NotQueued> {
        if self.full.load(Ordering::Relaxed) {
            return Err(NotQueued::Full);
        }
        if self.closed.is_cancelled() {
            return Err(NotQueued::Closed);
        }
        let permits = u32::try_from(cost.min(self.capacity)).unwrap_or(u32::MAX);
        let Ok(permit) = Arc::clone(&self.budget).try_acquire_many_owned(permits) else {
            return Err(self.full(when_full));
        };
        match self.queue.try_send(Outbound {
            message,
            _permit: Some(permit),
        }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(self.full(when_full)),
            Err(TrySendError::Closed(_)) => {
                self.closed.cancel();
                Err(NotQueued::Closed)
            }
        }
    }

    fn full(&self, when_full: WhenFull) -> NotQueued {
        if when_full == WhenFull::Close {
            tracing::debug!("closing a connection with no room for a frame");
            self.full.store(true, Ordering::Relaxed);
            self.closed.cancel();
        }
        NotQueued::Full
    }
}

impl Writer {
    /// Sends everything queued, in order, until every `Outbox` is dropped,
    /// then closes the socket, and pings the client every `ping_interval`
    /// meanwhile: a keepalive for the client, which may take a relay it
    /// hears nothing from for two intervals for gone, and a pong from a
    /// client still reading keeps it inside its idle timeout. A failed send
    /// closes the connection, so a dead writer never leaves its registration
    /// swallowing frames.
    pub async fn run(mut self, mut sink: impl Sink<Message> + Unpin, ping_interval: Duration) {
        let mut ping = tokio::time::interval_at(Instant::now() + ping_interval, ping_interval);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            // A frame's share of the budget returns once it has been sent.
            let (message, _sending) = tokio::select! {
                outbound = self.queue.recv() => {
                    let Some(outbound) = outbound else { break };
                    match serde_json::to_string(&outbound.message) {
                        Ok(text) => (Message::Text(text.into()), Some(outbound)),
                        Err(error) => {
                            tracing::error!(%error, "could not encode a message");
                            continue;
                        }
                    }
                }
                _ = ping.tick() => (Message::Ping(Bytes::new()), None),
            };
            if sink.send(message).await.is_err() {
                self.closed.cancel();
                return;
            }
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
    use futures::StreamExt;

    use super::*;
    use crate::protocol::DeviceId;

    const BUDGET: usize = 4 * MIN_FRAME_COST;

    fn frame(n: u8) -> ServerMessage {
        let id = DeviceId::parse(&"a".repeat(26)).unwrap();
        ServerMessage::Frame {
            to: id.clone(),
            from: id,
            record: vec![n],
            pairing_token: None,
        }
    }

    fn record(message: &ServerMessage) -> u8 {
        match message {
            ServerMessage::Frame { record, .. } => record[0],
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    /// A connection's outbox, its writer's end, and its closing token.
    struct Recipient {
        outbox: Outbox,
        writer: Writer,
        closed: CancellationToken,
    }

    impl Recipient {
        fn new() -> Self {
            let closed = CancellationToken::new();
            let (outbox, writer) = Outbox::new(BUDGET, closed.clone());
            Self {
                outbox,
                writer,
                closed,
            }
        }

        fn send(&self, n: u8) -> Result<(), NotQueued> {
            self.outbox
                .queue_frame(frame(n), MIN_FRAME_COST, WhenFull::Close)
        }

        fn send_kept(&self, n: u8) -> Result<(), NotQueued> {
            self.outbox
                .queue_frame(frame(n), MIN_FRAME_COST, WhenFull::Refuse)
        }

        fn fill(&self) {
            for n in 0..4 {
                assert_eq!(self.send(n), Ok(()));
            }
        }
    }

    #[test]
    fn a_frame_costs_its_encoded_size_and_at_least_the_minimum() {
        assert_eq!(frame_cost(0), MIN_FRAME_COST);
        assert_eq!(frame_cost(30_000), 40_000 + FRAME_OVERHEAD);
    }

    #[test]
    fn frames_with_room_are_queued_in_order() {
        let mut recipient = Recipient::new();
        recipient.fill();
        let queued: Vec<u8> = std::iter::from_fn(|| recipient.writer.try_recv())
            .map(|message| record(&message))
            .collect();
        assert_eq!(queued, vec![0, 1, 2, 3]);
        assert!(!recipient.closed.is_cancelled());
    }

    #[test]
    fn a_frame_without_room_closes_the_recipient_and_so_does_every_later_one() {
        let mut recipient = Recipient::new();
        recipient.fill();
        assert_eq!(recipient.send(4), Err(NotQueued::Full));
        assert!(recipient.closed.is_cancelled());
        // Room has come back, but the recipient has lost a frame.
        while recipient.writer.try_recv().is_some() {}
        assert_eq!(recipient.send(5), Err(NotQueued::Full));
    }

    #[test]
    fn a_recipient_kept_when_full_refuses_only_the_frames_it_has_no_room_for() {
        let mut recipient = Recipient::new();
        recipient.fill();
        assert_eq!(recipient.send_kept(4), Err(NotQueued::Full));
        assert!(!recipient.closed.is_cancelled());
        assert_eq!(
            recipient.writer.try_recv().map(|message| record(&message)),
            Some(0)
        );
        assert_eq!(recipient.send_kept(5), Ok(()));
        let queued: Vec<u8> = std::iter::from_fn(|| recipient.writer.try_recv())
            .map(|message| record(&message))
            .collect();
        assert_eq!(queued, vec![1, 2, 3, 5]);
    }

    #[test]
    fn a_sent_frame_returns_its_share_of_the_budget() {
        let mut recipient = Recipient::new();
        recipient.fill();
        // Dropping a message as the writer does once it is sent.
        assert_eq!(
            recipient.writer.try_recv().map(|message| record(&message)),
            Some(0)
        );
        assert_eq!(recipient.send(4), Ok(()));
        assert!(!recipient.closed.is_cancelled());
    }

    #[test]
    fn a_frame_for_a_recipient_closing_for_another_reason_is_not_taken_for_full() {
        let recipient = Recipient::new();
        recipient.outbox.close();
        assert_eq!(recipient.send(0), Err(NotQueued::Closed));
    }

    #[test]
    fn control_messages_hold_no_budget() {
        let recipient = Recipient::new();
        recipient.fill();
        assert!(recipient.outbox.deliver(ServerMessage::Registered));
        assert!(!recipient.closed.is_cancelled());
    }

    #[test]
    fn a_control_message_to_a_dead_writer_closes_the_connection() {
        let Recipient {
            outbox,
            writer,
            closed,
        } = Recipient::new();
        drop(writer);
        assert!(!outbox.deliver(ServerMessage::Registered));
        assert!(closed.is_cancelled());
    }

    #[test]
    fn control_messages_past_the_headroom_close_the_connection() {
        let recipient = Recipient::new();
        let fits = BUDGET / MIN_FRAME_COST + CONTROL_HEADROOM;
        assert!((0..fits).all(|_| recipient.outbox.deliver(ServerMessage::Registered)));
        assert!(!recipient.closed.is_cancelled());
        assert!(!recipient.outbox.deliver(ServerMessage::Registered));
        assert!(recipient.closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn the_writer_pings_while_nothing_is_queued_and_closes_once_the_outbox_is_gone() {
        const PING: Duration = Duration::from_secs(25);
        let Recipient { outbox, writer, .. } = Recipient::new();
        let (sink, mut socket) = futures::channel::mpsc::unbounded();
        let running = tokio::spawn(writer.run(sink, PING));
        tokio::time::sleep(PING * 2 + PING / 2).await;
        outbox.deliver(ServerMessage::Registered);
        drop(outbox);
        running.await.unwrap();

        let mut sent = Vec::new();
        while let Some(message) = socket.next().await {
            sent.push(message);
        }
        assert!(
            matches!(
                &sent[..],
                [
                    Message::Ping(_),
                    Message::Ping(_),
                    Message::Text(_),
                    Message::Close(None)
                ]
            ),
            "{sent:?}"
        );
    }
}
