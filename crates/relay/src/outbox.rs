//! A connection's outbound queue. Frames from other devices are bounded by
//! bytes, and a sender whose recipient is full waits for room instead of
//! losing the frame; the relay's own messages — `registered`, notices,
//! errors — never wait, and are bounded only by the queue's count of messages. A recipient is closed
//! only when it stops making progress, or when even its control messages no
//! longer fit, so a phone that reads slowly is slowed, not cut off. Progress
//! is any byte its socket accepts, so a long message going out slowly is
//! progress too.
//!
//! Waiting couples a sender to its slowest recipient: a Mac whose phone is
//! slow sends nothing to its other phones while it waits. Its traffic is one
//! ordered stream, so the relay cannot reorder around the wait; the budget
//! is sized so that ordinary replies never wait.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::Message;
use data_encoding::BASE64;
use futures::{Sink, SinkExt};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
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
/// 1,100 at the default budget. One that does is not reading its own errors.
pub(crate) const CONTROL_HEADROOM: usize = 64;
/// A frame's JSON around its base64 record: type, ids, an optional token.
const FRAME_OVERHEAD: usize = 256;

/// What a frame carrying `record_len` bytes is charged against its
/// recipient's budget: about its encoded size, and at least
/// `MIN_FRAME_COST`.
pub(crate) fn frame_cost(record_len: usize) -> usize {
    (BASE64.encode_len(record_len) + FRAME_OVERHEAD).max(MIN_FRAME_COST)
}

/// Whether a recipient whose socket last accepted bytes at `last_progress`
/// has stopped reading.
pub(crate) fn is_stalled(last_progress: Instant, now: Instant, stall: Duration) -> bool {
    now.saturating_duration_since(last_progress) >= stall
}

/// How long a sender waits for room at a recipient: `stall` without the
/// recipient's socket accepting a byte, and `room` in all.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Wait {
    pub stall: Duration,
    pub room: Duration,
}

/// When a connection's socket last accepted bytes, stamped by the socket
/// itself on every write that made headway.
#[derive(Debug, Clone)]
pub(crate) struct Progress(Arc<Mutex<Instant>>);

impl Progress {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    pub fn stamp(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    pub fn last(&self) -> Instant {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
    progress: Progress,
    /// Cancelled to close the connection.
    closed: CancellationToken,
}

/// The receiving side, owned by the connection's writer task.
pub(crate) struct Writer {
    queue: mpsc::Receiver<Outbound>,
    closed: CancellationToken,
}

impl Outbox {
    /// `progress` is the connection's socket's.
    pub fn new(
        queue_bytes: usize,
        closed: CancellationToken,
        progress: Progress,
    ) -> (Self, Writer) {
        let (sender, receiver) = mpsc::channel(queue_bytes / MIN_FRAME_COST + CONTROL_HEADROOM);
        let outbox = Self {
            queue: sender,
            budget: Arc::new(Semaphore::new(queue_bytes)),
            capacity: queue_bytes,
            progress,
            closed: closed.clone(),
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
        self.enqueue(Outbound {
            message,
            _permit: None,
        })
    }

    /// Reserves room for a frame of `cost`, waiting while the budget is
    /// full. Gives up, returning `None`, if the recipient closes, if
    /// `sender_closed` is cancelled, if the recipient's socket accepts
    /// nothing for `wait.stall`, or if no room comes within `wait.room`
    /// however much it accepts — in either of the last two cases the
    /// recipient is closed, as not reading or as too slow to read.
    /// `wait.room` bounds how long one frame holds its sender, which reads
    /// nothing else meanwhile. The frame is queued separately, by
    /// `enqueue_frame`, so the caller can check that it may still go.
    pub async fn reserve(
        &self,
        cost: usize,
        wait: Wait,
        sender_closed: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        if sender_closed.is_cancelled() {
            return None;
        }
        let permits = u32::try_from(cost.min(self.capacity)).unwrap_or(u32::MAX);
        match Arc::clone(&self.budget).try_acquire_many_owned(permits) {
            Ok(permit) => Some(permit),
            Err(TryAcquireError::Closed) => None,
            Err(TryAcquireError::NoPermits) => {
                self.wait_for_room(permits, wait, sender_closed).await
            }
        }
    }

    /// Queues a frame into room `reserve` made for it, without waiting.
    /// Returns whether it was queued.
    pub fn enqueue_frame(&self, message: ServerMessage, room: OwnedSemaphorePermit) -> bool {
        self.enqueue(Outbound {
            message,
            _permit: Some(room),
        })
    }

    async fn wait_for_room(
        &self,
        permits: u32,
        Wait { stall, room }: Wait,
        sender_closed: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        let acquire = Arc::clone(&self.budget).acquire_many_owned(permits);
        tokio::pin!(acquire);
        // Measured from the later of the wait's start and the last progress:
        // a queue idle before a burst has not stalled, and one that stops
        // just after the wait begins is closed a stall after it stopped.
        let started = Instant::now();
        loop {
            let since = self.progress.last().max(started);
            tokio::select! {
                // A closed side wins over room that came at the same moment.
                biased;
                () = self.closed.cancelled() => return None,
                () = sender_closed.cancelled() => return None,
                permit = &mut acquire => return permit.ok(),
                () = tokio::time::sleep_until(started + room) => {
                    tracing::debug!("closing a connection too slow to take a frame");
                    self.closed.cancel();
                    return None;
                }
                () = tokio::time::sleep_until(since + stall) => {
                    if is_stalled(self.progress.last().max(started), Instant::now(), stall) {
                        tracing::debug!("closing a connection that stopped reading");
                        self.closed.cancel();
                        return None;
                    }
                }
            }
        }
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
    /// then closes the socket, and pings the client every `ping_interval`
    /// meanwhile. The pings come from this task, not the reader, so they go
    /// out while the reader waits for room at a recipient: a client that
    /// hears nothing for two intervals may take the relay for gone. A failed
    /// send closes the connection, so a dead writer never leaves its
    /// registration swallowing frames.
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
impl Outbox {
    /// `reserve`, then `enqueue_frame`, as the server does with its check
    /// between them.
    pub async fn send_frame(
        &self,
        message: ServerMessage,
        cost: usize,
        wait: Wait,
        sender_closed: &CancellationToken,
    ) -> bool {
        match self.reserve(cost, wait, sender_closed).await {
            Some(room) => self.enqueue_frame(message, room),
            None => false,
        }
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
    const STALL: Duration = Duration::from_secs(5);
    const ROOM: Duration = Duration::from_mins(1);
    const WAIT: Wait = Wait {
        stall: STALL,
        room: ROOM,
    };

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

    /// A connection's outbox, the socket's progress, and its closing token.
    struct Recipient {
        outbox: Outbox,
        writer: Writer,
        progress: Progress,
        closed: CancellationToken,
    }

    impl Recipient {
        fn new() -> Self {
            let closed = CancellationToken::new();
            let progress = Progress::new();
            let (outbox, writer) = Outbox::new(BUDGET, closed.clone(), progress.clone());
            Self {
                outbox,
                writer,
                progress,
                closed,
            }
        }

        async fn send(&self, n: u8, sender_closed: &CancellationToken) -> bool {
            self.outbox
                .send_frame(frame(n), MIN_FRAME_COST, WAIT, sender_closed)
                .await
        }

        async fn fill(&self, sender_closed: &CancellationToken) {
            for n in 0..4 {
                assert!(self.send(n, sender_closed).await);
            }
        }
    }

    /// Sends one queued frame every `every`, as a socket that slowly accepts
    /// bytes would, and returns the records sent once `count` are.
    fn drain(
        writer: Writer,
        progress: Progress,
        every: Duration,
        count: usize,
    ) -> tokio::task::JoinHandle<Vec<u8>> {
        let mut writer = writer;
        tokio::spawn(async move {
            let mut sent = Vec::new();
            while sent.len() < count {
                tokio::time::sleep(every).await;
                let Some(outbound) = writer.queue.recv().await else {
                    break;
                };
                progress.stamp();
                sent.push(record(&outbound.message));
            }
            sent
        })
    }

    #[test]
    fn a_frame_costs_its_encoded_size_and_at_least_the_minimum() {
        assert_eq!(frame_cost(0), MIN_FRAME_COST);
        assert_eq!(frame_cost(30_000), 40_000 + FRAME_OVERHEAD);
    }

    #[test]
    fn stalled_means_no_progress_for_the_whole_window() {
        let start = Instant::now();
        assert!(!is_stalled(start, start + STALL / 2, STALL));
        assert!(is_stalled(start, start + STALL, STALL));
    }

    #[tokio::test]
    async fn a_control_message_to_a_dead_writer_closes_the_connection() {
        let Recipient {
            outbox,
            writer,
            closed,
            ..
        } = Recipient::new();
        drop(writer);
        assert!(!outbox.deliver(ServerMessage::Registered));
        assert!(closed.is_cancelled());
    }

    #[tokio::test]
    async fn control_messages_past_the_headroom_close_the_connection() {
        let recipient = Recipient::new();
        let fits = BUDGET / MIN_FRAME_COST + CONTROL_HEADROOM;
        assert!((0..fits).all(|_| recipient.outbox.deliver(ServerMessage::Registered)));
        assert!(!recipient.closed.is_cancelled());
        assert!(!recipient.outbox.deliver(ServerMessage::Registered));
        assert!(recipient.closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_sender_waits_for_room_and_proceeds_once_a_frame_is_sent() {
        let Recipient {
            outbox,
            mut writer,
            progress,
            closed,
        } = Recipient::new();
        let sender_closed = CancellationToken::new();
        for n in 0..4 {
            assert!(
                outbox
                    .send_frame(frame(n), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            );
        }
        let waiting = tokio::spawn({
            let outbox = outbox.clone();
            let sender_closed = sender_closed.clone();
            async move {
                outbox
                    .send_frame(frame(4), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            }
        });
        tokio::time::sleep(STALL / 2).await;
        assert!(!waiting.is_finished());

        // The writer sends one frame: its share returns and progress moves.
        let sent = writer.queue.recv().await.unwrap();
        progress.stamp();
        drop(sent);
        assert!(waiting.await.unwrap());
        assert!(!closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_recipient_that_keeps_sending_is_not_closed() {
        let Recipient {
            outbox,
            writer,
            progress,
            closed,
        } = Recipient::new();
        let sender_closed = CancellationToken::new();
        // One frame every 4 s: slower than the sender, inside the stall.
        let drained = drain(writer, progress, Duration::from_secs(4), 12);
        for n in 0..12 {
            assert!(
                outbox
                    .send_frame(frame(n), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            );
        }
        assert_eq!(drained.await.unwrap(), (0..12).collect::<Vec<_>>());
        assert!(!closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_recipient_that_sends_nothing_for_the_stall_is_closed() {
        let recipient = Recipient::new();
        let sender_closed = CancellationToken::new();
        recipient.fill(&sender_closed).await;
        assert!(!recipient.send(4, &sender_closed).await);
        assert!(recipient.closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_recipient_that_stops_just_after_a_wait_begins_is_closed_a_stall_later() {
        let recipient = Recipient::new();
        let sender_closed = CancellationToken::new();
        recipient.fill(&sender_closed).await;
        let started = Instant::now();
        let waiting = tokio::spawn({
            let outbox = recipient.outbox.clone();
            let sender_closed = sender_closed.clone();
            async move {
                outbox
                    .send_frame(frame(4), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            }
        });
        // The socket accepts part of a long message, then nothing more.
        tokio::time::sleep(Duration::from_secs(1)).await;
        recipient.progress.stamp();
        tokio::time::sleep_until(started + STALL + Duration::from_millis(500)).await;
        assert!(!recipient.closed.is_cancelled());
        tokio::time::sleep_until(
            started + Duration::from_secs(1) + STALL + Duration::from_millis(1),
        )
        .await;
        assert!(recipient.closed.is_cancelled());
        assert!(!waiting.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn a_recipient_that_keeps_accepting_bytes_but_frees_no_room_is_closed_in_time() {
        let recipient = Recipient::new();
        let sender_closed = CancellationToken::new();
        recipient.fill(&sender_closed).await;
        let started = Instant::now();
        let waiting = tokio::spawn({
            let outbox = recipient.outbox.clone();
            let sender_closed = sender_closed.clone();
            async move {
                outbox
                    .send_frame(frame(4), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            }
        });
        // A byte every second: never stalled, and never through a frame.
        let trickle = tokio::spawn({
            let progress = recipient.progress.clone();
            async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    progress.stamp();
                }
            }
        });
        let gave_up = tokio::time::timeout(ROOM * 2, waiting)
            .await
            .expect("the sender waited on a recipient that never made room");
        assert!(!gave_up.unwrap());
        assert_eq!(Instant::now(), started + ROOM);
        assert!(recipient.closed.is_cancelled());
        trickle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_after_a_long_idle_is_not_taken_for_a_stall() {
        let Recipient {
            outbox,
            writer,
            progress,
            closed,
        } = Recipient::new();
        let sender_closed = CancellationToken::new();
        tokio::time::sleep(Duration::from_mins(1)).await;
        let drained = drain(writer, progress, Duration::from_secs(4), 8);
        for n in 0..8 {
            assert!(
                outbox
                    .send_frame(frame(n), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            );
        }
        drained.await.unwrap();
        assert!(!closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_recipient_is_closed_and_the_senders_other_recipient_gets_every_frame() {
        let stalled = Recipient::new();
        let Recipient {
            outbox,
            writer,
            progress,
            closed,
        } = Recipient::new();
        let sender_closed = CancellationToken::new();
        let drained = drain(writer, progress, Duration::from_millis(10), 12);
        for n in 0..12 {
            stalled.send(n, &sender_closed).await;
            assert!(
                outbox
                    .send_frame(frame(n), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            );
        }
        assert!(stalled.closed.is_cancelled());
        assert_eq!(drained.await.unwrap(), (0..12).collect::<Vec<_>>());
        assert!(!closed.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_recipient_slows_the_sender_and_its_other_recipient_still_gets_every_frame() {
        let slow = Recipient::new();
        let quick = Recipient::new();
        let sender_closed = CancellationToken::new();
        let slow_drained = drain(slow.writer, slow.progress, Duration::from_secs(4), 12);
        let quick_drained = drain(quick.writer, quick.progress, Duration::from_millis(10), 12);
        for n in 0..12 {
            assert!(
                slow.outbox
                    .send_frame(frame(n), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            );
            assert!(
                quick
                    .outbox
                    .send_frame(frame(n), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            );
        }
        assert_eq!(slow_drained.await.unwrap(), (0..12).collect::<Vec<_>>());
        assert_eq!(quick_drained.await.unwrap(), (0..12).collect::<Vec<_>>());
        assert!(!slow.closed.is_cancelled() && !quick.closed.is_cancelled());
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

    #[tokio::test(start_paused = true)]
    async fn a_waiting_sender_gives_up_when_it_is_itself_closed() {
        let recipient = Recipient::new();
        let sender_closed = CancellationToken::new();
        recipient.fill(&sender_closed).await;
        let waiting = tokio::spawn({
            let outbox = recipient.outbox.clone();
            let sender_closed = sender_closed.clone();
            async move {
                outbox
                    .send_frame(frame(4), MIN_FRAME_COST, WAIT, &sender_closed)
                    .await
            }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        sender_closed.cancel();
        assert!(!waiting.await.unwrap());
        assert!(
            !recipient.closed.is_cancelled(),
            "the recipient is not to blame"
        );
    }
}
