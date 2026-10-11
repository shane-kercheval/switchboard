//! The wait budget every blocking operation takes, and the one driver that
//! honours it.
//!
//! The numbers are the client measurements in
//! `docs/research/mcp-client-behavior.md`: Claude Desktop sends no progress
//! token and cancels any call at 60 seconds, so a wait that cannot be kept
//! alive is capped at 50; Claude Code aborts a call silent for 300 seconds and
//! `rmcp`'s own session keep-alive ends a session silent for 300 seconds, so a
//! wait that can be kept alive fires its progress hook every 60; Claude Code
//! backgrounds a long call and delivers its result into the session, so long
//! waits are the intended use there. The limits key on whether the caller can
//! be kept alive (the tool layer sets it when the client sent a progress
//! token), never on the client's name.
//!
//! Every wait also takes a cancellation signal, because `rmcp` does not cancel
//! a tool call when its client hangs up: the server's shutdown is the only
//! thing that can end a wait early, and it must.
//!
//! **One driver, one deadline, one progress schedule.** The deadline is fixed
//! when the budget is created, which is when the call starts, and every
//! blocking phase of that call — a wait's observations, a forward's compose, a
//! retry joining an in-flight send — runs through [`WaitBudget::run`] against
//! that same deadline and the same 60-second progress interval. A phase cannot
//! restart the clock or the progress schedule, so a call never goes silent
//! past the limit and never outlives its budget by the sum of its parts.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::state::lock;

/// What a wait defaults to when the caller gives no `timeout_seconds`.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(50);
/// The most a wait may run when nothing can keep the client alive.
pub const MAX_WAIT_WITHOUT_KEEPALIVE: Duration = Duration::from_secs(50);
/// The most a wait may run when progress notifications keep the client alive.
pub const MAX_WAIT_WITH_KEEPALIVE: Duration = Duration::from_hours(1);
/// How often a kept-alive wait fires its progress hook.
pub const PROGRESS_INTERVAL: Duration = Duration::from_mins(1);
/// How far past the deadline the call's finishing steps may run, all together.
/// A deadline fixed at the call's start has already passed when the final
/// observation runs, so that look — and anything read after it, such as an
/// idle agent's output — needs an allowance, or the wait would return the
/// state it saw last rather than the state as of now. It is one fixed point
/// (the deadline plus five seconds) shared by every finishing step, never five
/// seconds per step, so a 50-second call ends by 55 seconds: inside Claude
/// Desktop's 60-second cancel.
pub const FINAL_OBSERVATION_GRACE: Duration = Duration::from_secs(5);

pub type ProgressHook = Arc<dyn Fn() + Send + Sync>;

/// How long a blocking operation may run, whether its caller can be kept
/// alive, what ends it early, and what it does every minute while it runs.
///
/// Cloneable: a clone shares the deadline, the cancel token, and the progress
/// schedule, so phases of one call that hold separate clones still behave as
/// one budget.
#[derive(Clone)]
pub struct WaitBudget {
    deadline: tokio::time::Instant,
    effective: Duration,
    cancel: CancellationToken,
    on_progress: Option<ProgressHook>,
    /// When the call's next progress notification is due, shared by every
    /// phase (and every clone) so a new phase cannot restart the schedule.
    /// Held only for the instant it takes to check and advance it, so phases
    /// that overlap never wait on each other, and a tick is claimed by exactly
    /// one of them.
    next_progress: Arc<Mutex<Option<tokio::time::Instant>>>,
}

impl WaitBudget {
    /// `timeout_seconds` as the caller gave it (`None` for the default), and
    /// whether the caller can be kept alive. The effective limit is clamped
    /// here, so a caller asking for more than it can survive gets a shorter
    /// wait, never a failure. The deadline starts now.
    #[must_use]
    pub fn new(timeout_seconds: Option<u64>, can_keep_alive: bool) -> Self {
        let requested = timeout_seconds.map_or(DEFAULT_WAIT, Duration::from_secs);
        let cap = if can_keep_alive {
            MAX_WAIT_WITH_KEEPALIVE
        } else {
            MAX_WAIT_WITHOUT_KEEPALIVE
        };
        let effective = requested.min(cap);
        Self {
            deadline: tokio::time::Instant::now() + effective,
            effective,
            cancel: CancellationToken::new(),
            on_progress: None,
            next_progress: Arc::new(Mutex::new(None)),
        }
    }

    /// The default budget: 50 seconds, not kept alive, never cancelled.
    #[must_use]
    pub fn default_wait() -> Self {
        Self::new(None, false)
    }

    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    #[must_use]
    pub fn with_progress(mut self, hook: ProgressHook) -> Self {
        self.on_progress = Some(hook);
        self
    }

    /// The requested timeout clamped to what the caller can survive.
    #[must_use]
    pub fn effective_timeout(&self) -> Duration {
        self.effective
    }

    #[must_use]
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Run `fut` against this budget: it completes, or the deadline passes, or
    /// the cancel token fires, whichever is first, with the progress hook
    /// firing on the call's schedule meanwhile. A future that loses is
    /// dropped, so whatever it was waiting on is released.
    pub async fn run<T>(&self, fut: impl Future<Output = T>) -> Outcome<T> {
        self.run_until(self.deadline, fut).await
    }

    /// [`Self::run`] against the call's finishing deadline — the deadline
    /// plus [`FINAL_OBSERVATION_GRACE`], one fixed point for every finishing
    /// step of the call.
    pub async fn run_with_grace<T>(&self, fut: impl Future<Output = T>) -> Outcome<T> {
        self.run_until(self.deadline + FINAL_OBSERVATION_GRACE, fut)
            .await
    }

    async fn run_until<T>(
        &self,
        deadline: tokio::time::Instant,
        fut: impl Future<Output = T>,
    ) -> Outcome<T> {
        tokio::pin!(fut);
        loop {
            let due = self.next_progress_due();
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Outcome::Cancelled,
                () = tokio::time::sleep_until(deadline) => return Outcome::Timeout,
                () = sleep_until_due(due) => {
                    if let Some(due) = due
                        && self.claim_progress_tick(due)
                        && let Some(hook) = &self.on_progress
                    {
                        hook();
                    }
                }
                value = &mut fut => return Outcome::Done(value),
            }
        }
    }

    /// When the next progress notification is due, starting the schedule on
    /// first use; `None` when the call has no progress hook.
    fn next_progress_due(&self) -> Option<tokio::time::Instant> {
        self.on_progress.as_ref()?;
        let mut next = lock(&self.next_progress);
        Some(*next.get_or_insert_with(|| tokio::time::Instant::now() + PROGRESS_INTERVAL))
    }

    /// Claim the tick due at `due`, if no overlapping phase has: advance the
    /// schedule by one interval, skipping any ticks already missed (a gap
    /// between phases produces one notification, not a burst). Returns whether
    /// this caller claimed it and so should fire the hook.
    fn claim_progress_tick(&self, due: tokio::time::Instant) -> bool {
        let mut next = lock(&self.next_progress);
        if *next != Some(due) {
            return false;
        }
        let now = tokio::time::Instant::now();
        let mut following = due + PROGRESS_INTERVAL;
        if following <= now {
            following = now + PROGRESS_INTERVAL;
        }
        *next = Some(following);
        true
    }
}

async fn sleep_until_due(due: Option<tokio::time::Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

/// How a phase run against a budget ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome<T> {
    Done(T),
    Timeout,
    Cancelled,
}

/// One observation of the thing being waited on.
pub enum Observed<T> {
    /// Finished; return this now.
    Done(T),
    /// Not finished; this is what to return if the wait ends first.
    NotYet(T),
}

/// Block until `observe` reports done, the budget's deadline passes, or its
/// cancellation fires, re-observing on every tick of `signal` and once more at
/// the end. Never polls in a loop: between observations it parks on the
/// signal, the deadline, the cancel token, and the progress interval, and each
/// observation itself runs under the budget, so an actor that cannot answer
/// (one still preparing a turn) cannot hold the wait past its deadline.
///
/// The final observation at the deadline is what lets a wait answer with the
/// state as of the moment it returns rather than the moment it last looked —
/// an agent whose shutdown finished during the wait reads as idle, not as the
/// running it was at the last signal. It runs with
/// [`FINAL_OBSERVATION_GRACE`]; if even that passes, the last state seen is
/// returned, or `unobserved()` when nothing was ever observed.
pub(crate) async fn wait_until<T>(
    budget: &WaitBudget,
    mut signal: watch::Receiver<u64>,
    mut observe: impl AsyncFnMut() -> Observed<T>,
    unobserved: impl FnOnce() -> T,
) -> T {
    let mut last: Option<T> = None;
    loop {
        match budget.run(observe()).await {
            Outcome::Done(Observed::Done(value)) => return value,
            Outcome::Done(Observed::NotYet(value)) => last = Some(value),
            Outcome::Timeout | Outcome::Cancelled => break,
        }
        match budget.run(signal.changed()).await {
            Outcome::Done(Ok(())) => {}
            Outcome::Done(Err(_)) | Outcome::Timeout | Outcome::Cancelled => break,
        }
    }
    match budget.run_with_grace(observe()).await {
        Outcome::Done(Observed::Done(value) | Observed::NotYet(value)) => value,
        Outcome::Timeout | Outcome::Cancelled => last.unwrap_or_else(unobserved),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn never() -> watch::Receiver<u64> {
        let tx = watch::Sender::new(0);
        let rx = tx.subscribe();
        // Keep the sender alive for the test's duration.
        std::mem::forget(tx);
        rx
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_that_cannot_be_kept_alive_returns_by_fifty_seconds_whatever_it_asked_for() {
        let budget = WaitBudget::new(Some(3_600), false);
        assert_eq!(budget.effective_timeout(), MAX_WAIT_WITHOUT_KEEPALIVE);
        let started = tokio::time::Instant::now();
        let observations = AtomicUsize::new(0);
        let value = wait_until(
            &budget,
            never(),
            async || {
                observations.fetch_add(1, Ordering::SeqCst);
                Observed::NotYet("still running")
            },
            || "unobserved",
        )
        .await;
        assert_eq!(value, "still running");
        assert_eq!(started.elapsed(), MAX_WAIT_WITHOUT_KEEPALIVE);
        assert_eq!(
            observations.load(Ordering::SeqCst),
            2,
            "one observation at the start and one at the deadline — never a poll loop"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_kept_alive_wait_is_held_to_an_hour_with_progress_every_minute() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ticks);
        let budget = WaitBudget::new(Some(100_000), true).with_progress(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(budget.effective_timeout(), MAX_WAIT_WITH_KEEPALIVE);
        let started = tokio::time::Instant::now();
        wait_until(&budget, never(), async || Observed::NotYet(()), || ()).await;
        assert_eq!(started.elapsed(), MAX_WAIT_WITH_KEEPALIVE);
        let fired = ticks.load(Ordering::SeqCst);
        assert!((59..=60).contains(&fired), "progress fired {fired} times");
    }

    #[tokio::test(start_paused = true)]
    async fn a_phase_run_against_the_budget_ends_at_the_deadline_and_releases_its_future() {
        let budget = WaitBudget::new(Some(10), false);
        let started = tokio::time::Instant::now();
        let outcome = budget.run(std::future::pending::<()>()).await;
        assert!(matches!(outcome, Outcome::Timeout));
        assert_eq!(started.elapsed(), Duration::from_secs(10));
        // A later phase of the same call gets no fresh deadline.
        let outcome = budget.run(std::future::pending::<()>()).await;
        assert!(matches!(outcome, Outcome::Timeout));
        assert_eq!(started.elapsed(), Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn an_observation_that_cannot_answer_ends_at_the_deadline() {
        let budget = WaitBudget::new(Some(1), false);
        let started = tokio::time::Instant::now();
        let value = wait_until(
            &budget,
            never(),
            async || {
                // An actor that never replies: the observation itself must be
                // bounded, or the wait would hang past its deadline.
                tokio::time::sleep(Duration::from_mins(10)).await;
                Observed::NotYet("stuck")
            },
            || "unobserved",
        )
        .await;
        assert_eq!(value, "unobserved");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(1) + FINAL_OBSERVATION_GRACE,
            "the first observation ends at the deadline, the final one at its grace"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn overlapping_phases_share_one_progress_schedule() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ticks);
        let budget = WaitBudget::new(Some(300), true).with_progress(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let other = budget.clone();
        let started = tokio::time::Instant::now();
        let (one, two) = tokio::join!(
            budget.run(tokio::time::sleep(Duration::from_secs(250))),
            other.run(tokio::time::sleep(Duration::from_secs(250))),
        );
        assert!(matches!(one, Outcome::Done(())));
        assert!(matches!(two, Outcome::Done(())));
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(250),
            "neither waited on the other"
        );
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            4,
            "one notification per minute for the call, not one per phase"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_gap_between_phases_produces_one_notification_not_a_burst() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ticks);
        let budget = WaitBudget::new(Some(3_600), true).with_progress(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let _ = budget
            .run(tokio::time::sleep(Duration::from_secs(10)))
            .await;
        // Five minutes pass outside any phase.
        tokio::time::sleep(Duration::from_mins(5)).await;
        let _ = budget.run(tokio::time::sleep(Duration::from_secs(1))).await;
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            1,
            "the missed ticks are skipped; the overdue one fires once"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn finishing_steps_share_one_allowance() {
        let budget = WaitBudget::new(Some(10), false);
        let started = tokio::time::Instant::now();
        let _ = budget.run(std::future::pending::<()>()).await;
        let first = budget
            .run_with_grace(tokio::time::sleep(Duration::from_secs(3)))
            .await;
        assert!(matches!(first, Outcome::Done(())));
        let second = budget.run_with_grace(std::future::pending::<()>()).await;
        assert!(matches!(second, Outcome::Timeout));
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(10) + FINAL_OBSERVATION_GRACE,
            "two finishing steps end at one fixed point, not five seconds each"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_default_is_fifty_seconds() {
        let budget = WaitBudget::new(None, true);
        assert_eq!(budget.effective_timeout(), DEFAULT_WAIT);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_ends_a_wait_at_once() {
        let cancel = CancellationToken::new();
        let budget = WaitBudget::new(Some(3_600), true).with_cancel(cancel.clone());
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(7)).await;
            canceller.cancel();
        });
        let started = tokio::time::Instant::now();
        let value = wait_until(
            &budget,
            never(),
            async || Observed::NotYet("cut short"),
            || "unobserved",
        )
        .await;
        assert_eq!(value, "cut short");
        assert_eq!(started.elapsed(), Duration::from_secs(7));
    }

    #[tokio::test(start_paused = true)]
    async fn a_signal_wakes_the_wait_and_done_returns_early() {
        let tx = watch::Sender::new(0);
        let rx = tx.subscribe();
        let budget = WaitBudget::new(Some(50), false);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            tx.send_modify(|v| *v += 1);
            // Keep the sender alive past the wake-up.
            tokio::time::sleep(Duration::from_mins(1)).await;
        });
        let started = tokio::time::Instant::now();
        let calls = AtomicUsize::new(0);
        let value = wait_until(
            &budget,
            rx,
            async || {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Observed::NotYet(false)
                } else {
                    Observed::Done(true)
                }
            },
            || false,
        )
        .await;
        assert!(value);
        assert_eq!(started.elapsed(), Duration::from_secs(3));
    }
}
