//! Keeps the machine awake while agent work is in progress, and releases the OS
//! wake lock [`RELEASE_GRACE`] after the last of it finishes — unless more work
//! starts first.
//!
//! "Work" is any live turn (tracked by [`WakeLockEmitter`] from the wire events)
//! or any held [`WakeLease`] (taken by a workflow run for its whole life, so the
//! lock can't lapse while a step prepares the next turn).
//!
//! **Why an emitter decorator for turns.** Turn liveness is already broadcast as
//! wire events: the dispatcher emits exactly one `turn_start` and exactly one
//! `turn_end` per turn, for every outcome (completed, failed, *and* cancelled —
//! the actor synthesizes a terminal `turn_end` when it force-fails or cancels).
//! So a decorator that tracks the live `turn_id` set across all agents knows
//! "is anything running" without the dispatcher knowing anything about power
//! management. The start/end pairing is what makes cancel/error free: this code
//! never reads the outcome, it only adds and removes the `turn_id`, so a failed
//! or cancelled terminal clears its turn identically to a completed one.
//!
//! **Why wrap the single global emitter, not the per-dispatch one.** The
//! per-dispatch `SessionMetaObservingEmitter` wraps the app's one base emitter
//! (`AppState::emitter`). Wrapping that base emitter once means every agent's
//! `turn_start`/`turn_end` funnels through one decorator with one shared set —
//! the only place that can answer "are ANY agents active across the whole app."
//! A per-agent wrapper could only see its own agent's turns.
//!
//! **Why traits for the OS call and the clock.** `KeepAwakeInhibitor` is the
//! only part that touches real power-management APIs (via the `keepawake`
//! crate); everything else — the bookkeeping, the engage-on-first /
//! release-after-grace edges — is pure and unit-tested against a fake
//! `SleepInhibitor` and a fake [`ReleaseTimer`]. Best-effort by design: a failed
//! `engage` logs and leaves the machine able to sleep rather than blocking a
//! turn, and the next piece of work to start tries again.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use switchboard_dispatcher::EventEmitter;

use crate::state::lock;

/// How long the wake lock is kept after the last piece of work ends, so a turn
/// that follows straight on from another never sees the lock dropped in between.
///
/// **Why any grace at all.** Once the display is off and the user has been idle
/// past the system's sleep timer, macOS starts idle sleep as soon as the last
/// assertion is released, and an assertion taken a moment later does not stop
/// it. The power log (`pmset -g log`) showed exactly that during a workflow run:
/// a step's turn ended and released the lock, the next step's turn re-took it
/// within about a second, and the Mac slept five seconds after the release with
/// the new turn running.
///
/// **Why 15 seconds.** Workflow runs hold a [`WakeLease`] from start to finish,
/// so hand-offs between their steps never depend on this. What it covers is a
/// queued message to the same agent: the dispatcher starts the next turn as soon
/// as one ends, but `turn_start` only fires once the harness process has
/// launched — after waiting for the login-shell PATH (at most 3 s, and normally
/// already resolved), journaling the send, and spawning the CLI. That usually
/// takes well under a few seconds. 15 s is a generous margin over it, **not** a
/// guaranteed bound: nothing caps how long a CLI takes to launch. A message the
/// user sends isn't a gap to cover — they are at the keyboard, which itself keeps
/// the Mac awake. The cost is at most 15 s of extra wakefulness after work
/// genuinely finishes.
pub const RELEASE_GRACE: Duration = Duration::from_secs(15);

/// Acquires and releases an OS-level "stay awake" lock. Idempotent on both
/// sides: repeated `engage`/`release` calls collapse to the underlying lock's
/// presence or absence. Abstracted so the bookkeeping can be tested without
/// touching real power assertions.
pub trait SleepInhibitor: Send + Sync {
    /// Ensure the machine is held awake, returning whether it now is. `true`
    /// without doing anything if already engaged.
    fn engage(&self) -> bool;
    /// Allow the machine to sleep again. A no-op if not engaged.
    fn release(&self);
}

/// Real inhibitor backed by the `keepawake` crate. Holds the RAII guard whose
/// `Drop` releases the OS lock (`IOKit` power assertion on macOS).
pub struct KeepAwakeInhibitor {
    guard: Mutex<Option<keepawake::KeepAwake>>,
}

impl KeepAwakeInhibitor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            guard: Mutex::new(None),
        }
    }
}

impl Default for KeepAwakeInhibitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SleepInhibitor for KeepAwakeInhibitor {
    fn engage(&self) -> bool {
        let mut held = lock(&self.guard);
        if held.is_some() {
            return true;
        }
        // Block *idle* system sleep only — keep the Mac from dozing off on its
        // own timer while an agent works. We deliberately do NOT request
        // `PreventSystemSleep` (`.sleep(true)`): it's a stronger assertion that
        // wouldn't reliably block explicit/lid-close sleep anyway, drains
        // battery harder, and — because `keepawake` acquires all requested
        // assertions all-or-nothing — its failure would void the idle
        // assertion too. The display is left alone so a background run doesn't
        // force the user's screen to stay lit. On macOS the visible
        // `pmset -g assertions` label is the `reason` string (not `app_name`),
        // so `reason` carries the product name for support/verification.
        match keepawake::Builder::default()
            .display(false)
            .idle(true)
            .sleep(false)
            .app_name("Switchboard")
            .reason("Switchboard: agent turn in progress")
            .create()
        {
            Ok(awake) => {
                *held = Some(awake);
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to acquire wake lock; system may sleep mid-turn");
                false
            }
        }
    }

    fn release(&self) {
        // Dropping the guard releases the OS lock.
        *lock(&self.guard) = None;
    }
}

/// Holds nothing. The default for an `AppState` built without real power
/// management (tests); production injects a [`KeepAwakeInhibitor`]-backed lock.
struct NoopInhibitor;

impl SleepInhibitor for NoopInhibitor {
    fn engage(&self) -> bool {
        true
    }
    fn release(&self) {}
}

/// Runs a task once a delay has elapsed. Abstracted so tests fire the delayed
/// release on demand instead of waiting on a clock.
pub trait ReleaseTimer: Send + Sync {
    fn after(&self, delay: Duration, task: Box<dyn FnOnce() + Send>);
}

/// Real timer: one short-lived thread per scheduled release, which needs no
/// async runtime at the call site (`emit` and `Drop` are synchronous). A release
/// is scheduled only when *all* work — every turn and every workflow run — has
/// finished, which happens at the pace of the user's own sends, so the
/// occasional sleeping thread is cheap. A release made stale by new work still
/// sleeps out its delay and then does nothing (see `LockState::generation`);
/// cancelling it instead would need a handle synchronized with new work, for no
/// real gain.
pub struct ThreadTimer;

impl ReleaseTimer for ThreadTimer {
    fn after(&self, delay: Duration, task: Box<dyn FnOnce() + Send>) {
        // The task moves into the closure, so a failed spawn can't hand it back.
        // Share it instead: whichever side still holds it runs it — the thread
        // after the delay, or the caller right away if the spawn failed. Running
        // it immediately degrades to releasing without grace, which is better
        // than never releasing and holding the machine awake indefinitely.
        let slot = Arc::new(Mutex::new(Some(task)));
        let in_thread = Arc::clone(&slot);
        let spawned = std::thread::Builder::new()
            .name("wake-lock-release".to_owned())
            .spawn(move || {
                std::thread::sleep(delay);
                if let Some(task) = lock(&in_thread).take() {
                    task();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "failed to schedule wake-lock release; releasing now");
            if let Some(task) = lock(&slot).take() {
                task();
            }
        }
    }
}

/// Runs the task at once. Pairs with [`NoopInhibitor`], where a release does
/// nothing and so has nothing to wait for.
struct InlineTimer;

impl ReleaseTimer for InlineTimer {
    fn after(&self, _delay: Duration, task: Box<dyn FnOnce() + Send>) {
        task();
    }
}

/// The app's one wake lock: counts live work and drives a [`SleepInhibitor`] —
/// engage when work starts while not engaged, release [`RELEASE_GRACE`] after
/// the last work ends if nothing started in between. Cheap to clone; clones
/// share the same lock.
#[derive(Clone)]
pub struct WakeLock {
    shared: Arc<Shared>,
}

/// **Every inhibitor call happens under `state`'s lock**, so an engage and a
/// delayed release can't interleave and leave the machine sleepable while work
/// is live. Releases are *scheduled* outside it: the release takes the lock, and
/// a timer may run it inline.
struct Shared {
    inhibitor: Box<dyn SleepInhibitor>,
    timer: Box<dyn ReleaseTimer>,
    state: Mutex<LockState>,
}

#[derive(Default)]
struct LockState {
    /// Live turns by `turn_id`. A set, not a counter: `turn_id` is a
    /// dispatcher-minted, process-wide-unique `UUIDv7`, so insert/remove are
    /// idempotent — a duplicate `turn_start` can't double-hold the lock and a
    /// duplicate `turn_end` can't release it while another turn is live. The
    /// wake lock is a global OS side effect, so it stays robust to a bad
    /// adapter/parser event rather than amplifying it.
    active_turns: HashSet<String>,
    /// Held [`WakeLease`]s. A counter is safe here: only `lease()` adds and only
    /// the lease's own `Drop` removes, exactly once each.
    leases: usize,
    /// Whether the inhibitor is actually holding the machine awake — `false`
    /// after a failed engage, so the next work to start retries it.
    engaged: bool,
    /// Bumped whenever work starts and on every drain. A scheduled release
    /// carries the value from its drain and does nothing if it has moved on —
    /// work started since (the lock is needed again) or a later drain scheduled
    /// its own release.
    generation: u64,
}

impl LockState {
    fn is_idle(&self) -> bool {
        self.active_turns.is_empty() && self.leases == 0
    }
}

impl WakeLock {
    pub fn new(
        inhibitor: impl SleepInhibitor + 'static,
        timer: impl ReleaseTimer + 'static,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                inhibitor: Box::new(inhibitor),
                timer: Box::new(timer),
                state: Mutex::new(LockState::default()),
            }),
        }
    }

    /// A lock that holds nothing — for states built without power management.
    #[must_use]
    pub fn inert() -> Self {
        Self::new(NoopInhibitor, InlineTimer)
    }

    /// Hold the machine awake until the returned lease is dropped, whether or
    /// not any turn is running.
    ///
    /// A workflow run holds one for its whole life. That is only right because
    /// nothing in a run waits on the user: a step that did (`pause_for_user`,
    /// not executable today) must drop the lease while it waits and take a new
    /// one when it resumes, or a paused run would keep the Mac awake overnight.
    #[must_use]
    pub fn lease(&self) -> WakeLease {
        self.shared.begin_work(|state| {
            state.leases += 1;
            true
        });
        WakeLease {
            shared: Arc::clone(&self.shared),
        }
    }

    fn turn_started(&self, turn_id: &str) {
        self.shared
            .begin_work(|state| state.active_turns.insert(turn_id.to_owned()));
    }

    /// A `turn_end` whose `turn_id` was never tracked is a no-op — failing safe
    /// toward staying awake rather than sleeping mid-turn.
    fn turn_ended(&self, turn_id: &str) {
        Shared::end_work(&self.shared, |state| state.active_turns.remove(turn_id));
    }
}

impl Shared {
    /// Apply `add`; if it added work, cancel any pending release and make sure
    /// the machine is held (retrying an engage that failed earlier).
    fn begin_work(&self, add: impl FnOnce(&mut LockState) -> bool) {
        let mut state = lock(&self.state);
        if add(&mut state) {
            state.generation += 1;
            if !state.engaged {
                state.engaged = self.inhibitor.engage();
            }
        }
    }

    /// Apply `remove`; if that left nothing running, schedule the release.
    fn end_work(this: &Arc<Self>, remove: impl FnOnce(&mut LockState) -> bool) {
        let drained_at = {
            let mut state = lock(&this.state);
            (remove(&mut state) && state.is_idle()).then(|| {
                state.generation += 1;
                state.generation
            })
        };
        if let Some(drained_at) = drained_at {
            let shared = Arc::clone(this);
            this.timer.after(
                RELEASE_GRACE,
                Box::new(move || shared.release_if_still_idle(drained_at)),
            );
        }
    }

    fn release_if_still_idle(&self, drained_at: u64) {
        let mut state = lock(&self.state);
        if state.generation == drained_at && state.engaged {
            self.inhibitor.release();
            state.engaged = false;
        }
    }
}

/// Holds the machine awake while alive. See [`WakeLock::lease`].
pub struct WakeLease {
    shared: Arc<Shared>,
}

impl Drop for WakeLease {
    fn drop(&mut self) {
        Shared::end_work(&self.shared, |state| {
            state.leases -= 1;
            true
        });
    }
}

/// Wraps an `EventEmitter`, feeding every `turn_start`/`turn_end` into the
/// [`WakeLock`] and forwarding every event to the inner emitter unchanged.
pub struct WakeLockEmitter {
    inner: Arc<dyn EventEmitter>,
    wake_lock: WakeLock,
}

impl WakeLockEmitter {
    pub fn new(inner: Arc<dyn EventEmitter>, wake_lock: WakeLock) -> Self {
        Self { inner, wake_lock }
    }
}

impl EventEmitter for WakeLockEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        match payload.get("type").and_then(serde_json::Value::as_str) {
            Some("turn_start") => {
                if let Some(turn_id) = turn_id_of(&payload) {
                    self.wake_lock.turn_started(turn_id);
                }
            }
            Some("turn_end") => {
                if let Some(turn_id) = turn_id_of(&payload) {
                    self.wake_lock.turn_ended(turn_id);
                }
            }
            _ => {}
        }
        self.inner.emit(name, payload);
    }
}

/// Extract a turn's `turn_id` as a string slice, if present and string-typed.
fn turn_id_of(payload: &serde_json::Value) -> Option<&str> {
    payload.get("turn_id").and_then(serde_json::Value::as_str)
}

/// Fakes for tests elsewhere in the crate that need to observe the wake lock —
/// e.g. that a workflow run holds a lease for its whole life.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{RELEASE_GRACE, ReleaseTimer, SleepInhibitor};
    use crate::state::lock;

    /// Records engage/release calls and tracks current engaged state so tests
    /// can assert both the edge transitions and the call counts. Cloneable so a
    /// test keeps a handle after the emitter takes ownership.
    #[derive(Clone, Default)]
    pub(crate) struct FakeInhibitor {
        pub(crate) state: Arc<Mutex<FakeState>>,
    }

    #[derive(Default)]
    pub(crate) struct FakeState {
        pub(crate) engaged: bool,
        pub(crate) engage_calls: usize,
        pub(crate) release_calls: usize,
        /// Engage attempts still to refuse, as the OS might transiently.
        pub(crate) refusals_left: usize,
    }

    impl SleepInhibitor for FakeInhibitor {
        fn engage(&self) -> bool {
            let mut s = lock(&self.state);
            s.engage_calls += 1;
            if s.refusals_left > 0 {
                s.refusals_left -= 1;
                return false;
            }
            s.engaged = true;
            true
        }
        fn release(&self) {
            let mut s = lock(&self.state);
            s.engaged = false;
            s.release_calls += 1;
        }
    }

    pub(crate) type ScheduledTasks = Vec<(Duration, Box<dyn FnOnce() + Send>)>;

    /// Holds scheduled releases until the test lets the grace period elapse.
    #[derive(Clone, Default)]
    pub(crate) struct ManualTimer {
        pub(crate) scheduled: Arc<Mutex<ScheduledTasks>>,
    }

    impl ReleaseTimer for ManualTimer {
        fn after(&self, delay: Duration, task: Box<dyn FnOnce() + Send>) {
            lock(&self.scheduled).push((delay, task));
        }
    }

    impl ManualTimer {
        /// Releases scheduled so far and not yet run.
        pub(crate) fn pending(&self) -> usize {
            lock(&self.scheduled).len()
        }

        /// Let the grace period pass: run every scheduled release, in order.
        pub(crate) fn elapse(&self) {
            let tasks = std::mem::take(&mut *lock(&self.scheduled));
            for (delay, task) in tasks {
                assert_eq!(delay, RELEASE_GRACE, "releases wait out the grace period");
                task();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{FakeInhibitor, FakeState, ManualTimer};
    use super::*;

    use std::sync::MutexGuard;

    use serde_json::json;
    use switchboard_dispatcher::RecordingEmitter;
    use uuid::Uuid;

    struct Rig {
        wake_lock: WakeLock,
        emitter: WakeLockEmitter,
        inner: Arc<RecordingEmitter>,
        inhibitor: FakeInhibitor,
        timer: ManualTimer,
    }

    impl Rig {
        fn new() -> Self {
            let inner = Arc::new(RecordingEmitter::new());
            let inhibitor = FakeInhibitor::default();
            let timer = ManualTimer::default();
            let wake_lock = WakeLock::new(inhibitor.clone(), timer.clone());
            let emitter = WakeLockEmitter::new(
                Arc::clone(&inner) as Arc<dyn EventEmitter>,
                wake_lock.clone(),
            );
            Self {
                wake_lock,
                emitter,
                inner,
                inhibitor,
                timer,
            }
        }

        fn start(&self, turn_id: &str) {
            self.emitter.emit("agent:x", turn_start(turn_id));
        }

        fn end(&self, turn_id: &str) {
            self.emitter
                .emit("agent:x", turn_end(turn_id, &completed()));
        }

        fn lock_state(&self) -> MutexGuard<'_, FakeState> {
            lock(&self.inhibitor.state)
        }

        fn engaged(&self) -> bool {
            self.lock_state().engaged
        }

        fn scheduled(&self) -> usize {
            self.timer.pending()
        }

        fn elapse_grace(&self) {
            self.timer.elapse();
        }
    }

    /// A fresh, process-unique turn id (the real shape: dispatcher-minted `UUIDv7`).
    fn tid() -> String {
        Uuid::now_v7().to_string()
    }

    fn turn_start(turn_id: &str) -> serde_json::Value {
        json!({
            "type": "turn_start",
            "turn_id": turn_id,
            "message_id": Uuid::now_v7().to_string(),
            "started_at": "2026-06-21T00:00:00Z",
        })
    }

    fn turn_end(turn_id: &str, outcome: &serde_json::Value) -> serde_json::Value {
        json!({
            "type": "turn_end",
            "turn_id": turn_id,
            "outcome": outcome,
            "ended_at": "2026-06-21T00:00:00Z",
        })
    }

    fn completed() -> serde_json::Value {
        json!({"status": "completed"})
    }

    #[test]
    fn first_turn_start_engages_and_forwards() {
        let rig = Rig::new();

        let payload = turn_start(&tid());
        rig.emitter.emit("agent:x", payload.clone());

        assert!(rig.engaged(), "first turn must engage");
        let recorded = rig.inner.snapshot();
        assert_eq!(recorded.len(), 1, "event must still be forwarded");
        assert_eq!(recorded[0].1, payload);
    }

    #[test]
    fn last_turn_end_holds_the_lock_until_the_grace_period_passes() {
        let rig = Rig::new();

        let t = tid();
        rig.start(&t);
        rig.end(&t);

        assert!(rig.engaged(), "still held right after the last turn ends");
        assert_eq!(rig.scheduled(), 1, "one release waits out the grace period");

        rig.elapse_grace();
        let s = rig.lock_state();
        assert!(!s.engaged, "machine may sleep once the grace period passes");
        assert_eq!(s.engage_calls, 1);
        assert_eq!(s.release_calls, 1);
    }

    /// The failure the grace exists for: work ends and more starts moments
    /// later. Releasing in between let macOS start idle sleep that the re-taken
    /// lock could not stop.
    #[test]
    fn a_turn_starting_within_the_grace_period_keeps_the_lock_without_a_gap() {
        let rig = Rig::new();

        let (queued_one, queued_two) = (tid(), tid());
        rig.start(&queued_one);
        rig.end(&queued_one);
        rig.start(&queued_two);
        rig.elapse_grace();

        let s = rig.lock_state();
        assert!(s.engaged, "the next turn is running, so the lock stays");
        assert_eq!(s.release_calls, 0, "never released between the two turns");
        assert_eq!(
            s.engage_calls, 1,
            "never re-taken either — it was never dropped"
        );
    }

    #[test]
    fn a_lease_holds_the_lock_between_turns_however_long_the_gap() {
        let rig = Rig::new();

        // A workflow run: its lease spans every step, including the gap while a
        // step prepares its next turn.
        let run = rig.wake_lock.lease();
        let (step_one, step_two) = (tid(), tid());
        rig.start(&step_one);
        rig.end(&step_one);
        assert_eq!(
            rig.scheduled(),
            0,
            "the run is still live, so nothing drains"
        );
        rig.start(&step_two);
        rig.end(&step_two);
        assert_eq!(rig.scheduled(), 0);
        assert!(rig.engaged());

        drop(run);
        assert!(rig.engaged(), "held through the grace after the run ends");
        rig.elapse_grace();
        let s = rig.lock_state();
        assert!(!s.engaged, "released once the grace passes");
        assert_eq!(s.engage_calls, 1);
        assert_eq!(s.release_calls, 1);
    }

    #[test]
    fn a_lease_alone_engages_before_any_turn_starts() {
        let rig = Rig::new();

        let _run = rig.wake_lock.lease();

        assert!(rig.engaged(), "held while the run prepares its first turn");
    }

    #[test]
    fn dropping_a_lease_while_a_turn_runs_schedules_no_release() {
        let rig = Rig::new();

        let t = tid();
        rig.start(&t);
        drop(rig.wake_lock.lease());
        assert_eq!(rig.scheduled(), 0, "the turn is still live");

        rig.end(&t);
        rig.elapse_grace();
        assert!(!rig.engaged());
    }

    #[test]
    fn a_lease_taken_during_the_grace_period_keeps_the_lock() {
        let rig = Rig::new();

        let t = tid();
        rig.start(&t);
        rig.end(&t);
        let _run = rig.wake_lock.lease();
        rig.elapse_grace();

        let s = rig.lock_state();
        assert!(s.engaged);
        assert_eq!(s.release_calls, 0);
    }

    #[test]
    fn a_refused_engage_is_retried_when_more_work_starts() {
        let rig = Rig::new();
        rig.lock_state().refusals_left = 1;

        // The run's lease is refused by the OS; its first turn must try again
        // rather than trust a lock that was never taken.
        let _run = rig.wake_lock.lease();
        assert!(!rig.engaged());
        rig.start(&tid());

        let s = rig.lock_state();
        assert!(s.engaged, "the retry took the lock");
        assert_eq!(s.engage_calls, 2);
    }

    #[test]
    fn only_the_latest_drain_releases_after_back_to_back_turns() {
        let rig = Rig::new();

        // Two hand-offs each schedule a release; the earlier one is stale by the
        // time it fires and must not release on the later drain's behalf early.
        let (a, b) = (tid(), tid());
        rig.start(&a);
        rig.end(&a);
        rig.start(&b);
        rig.end(&b);
        assert_eq!(rig.scheduled(), 2);

        rig.elapse_grace();
        let s = rig.lock_state();
        assert!(!s.engaged);
        assert_eq!(s.release_calls, 1, "released exactly once");
    }

    #[test]
    fn overlapping_turns_engage_once_release_only_after_last() {
        let rig = Rig::new();

        // Two agents' turns overlap: start, start, end, end.
        let (a, b) = (tid(), tid());
        rig.start(&a);
        rig.start(&b);
        assert!(rig.engaged());

        rig.end(&a);
        assert_eq!(rig.scheduled(), 0, "no release scheduled while `b` runs");
        rig.elapse_grace();
        assert!(rig.engaged(), "still engaged while the second turn runs");

        rig.end(&b);
        rig.elapse_grace();
        let s = rig.lock_state();
        assert!(!s.engaged, "released only after the last turn ends");
        assert_eq!(s.engage_calls, 1, "engaged exactly once across overlap");
        assert_eq!(s.release_calls, 1, "released exactly once across overlap");
    }

    #[test]
    fn failed_terminal_releases_like_completed() {
        let rig = Rig::new();

        let t = tid();
        rig.start(&t);
        rig.emitter.emit(
            "agent:x",
            turn_end(
                &t,
                &json!({
                    "status": "failed",
                    "kind": "adapter_failure",
                    "message": "boom",
                }),
            ),
        );
        rig.elapse_grace();

        assert!(
            !rig.engaged(),
            "a failed terminal clears its turn identically to a completed one"
        );
    }

    #[test]
    fn cancelled_terminal_releases_like_completed() {
        let rig = Rig::new();

        let t = tid();
        rig.start(&t);
        rig.emitter.emit(
            "agent:x",
            turn_end(&t, &json!({"status": "cancelled", "source": "user"})),
        );
        rig.elapse_grace();

        assert!(
            !rig.engaged(),
            "a cancelled terminal clears its turn identically to a completed one"
        );
    }

    #[test]
    fn duplicate_turn_start_does_not_double_track() {
        let rig = Rig::new();

        // Same turn's start arrives twice (e.g. a buggy adapter/parser).
        let t = tid();
        rig.start(&t);
        rig.start(&t);
        assert_eq!(
            rig.lock_state().engage_calls,
            1,
            "a duplicate start must not re-engage"
        );

        // A single matching end still fully drains and releases.
        rig.end(&t);
        rig.elapse_grace();
        let s = rig.lock_state();
        assert!(!s.engaged, "one end clears the (idempotently) tracked turn");
        assert_eq!(s.release_calls, 1);
    }

    #[test]
    fn duplicate_turn_end_does_not_release_while_another_turn_is_live() {
        let rig = Rig::new();

        // Two turns live; the first ends, then its terminal is delivered a
        // second time. The duplicate must NOT release while turn `b` runs —
        // the precise failure a bare counter would allow (2→1→0).
        let (a, b) = (tid(), tid());
        rig.start(&a);
        rig.start(&b);
        rig.end(&a);
        rig.end(&a); // duplicate
        rig.elapse_grace();

        assert!(
            rig.engaged(),
            "a duplicate end for an already-cleared turn must not release while `b` is live"
        );
        assert_eq!(
            rig.lock_state().release_calls,
            0,
            "no release yet — `b` is still running"
        );

        rig.end(&b);
        rig.elapse_grace();
        let s = rig.lock_state();
        assert!(!s.engaged, "released only once the last live turn ends");
        assert_eq!(s.release_calls, 1);
    }

    #[test]
    fn unpaired_turn_end_is_a_safe_no_op() {
        let rig = Rig::new();

        // A stray terminal with no live turn: must be a no-op for the lock.
        rig.end(&tid());
        assert_eq!(rig.scheduled(), 0, "nothing drained, so nothing to release");

        let s = rig.lock_state();
        assert!(!s.engaged);
        assert_eq!(s.engage_calls, 0);
        assert_eq!(s.release_calls, 0, "no release without a prior engage");
        drop(s);
        assert_eq!(rig.inner.snapshot().len(), 1, "event still forwarded");
    }

    #[test]
    fn turn_end_missing_turn_id_fails_safe_and_does_not_release() {
        let rig = Rig::new();

        // A live turn, then a malformed terminal with no `turn_id`: it can't be
        // matched, so it must not release (fail safe toward staying awake).
        rig.start(&tid());
        rig.emitter.emit(
            "agent:x",
            json!({"type": "turn_end", "outcome": completed(), "ended_at": "now"}),
        );
        rig.elapse_grace();

        assert!(
            rig.engaged(),
            "an unmatched terminal must not release a live turn's lock"
        );
        assert_eq!(rig.lock_state().release_calls, 0);
    }

    #[test]
    fn non_lifecycle_events_do_not_touch_the_lock_but_forward() {
        let rig = Rig::new();

        for payload in [
            json!({"type": "content_chunk", "turn_id": "t", "kind": "text", "text": "hi"}),
            json!({"type": "session_meta", "agent_id": Uuid::now_v7().to_string()}),
            json!({"type": "agent_idle", "agent_id": Uuid::now_v7().to_string()}),
            json!({"type": "rate_limit_event", "agent_id": Uuid::now_v7().to_string(), "info": {}}),
        ] {
            rig.emitter.emit("agent:x", payload);
        }

        let s = rig.lock_state();
        assert_eq!(s.engage_calls, 0);
        assert_eq!(s.release_calls, 0);
        drop(s);
        assert_eq!(rig.scheduled(), 0);
        assert_eq!(rig.inner.snapshot().len(), 4, "all events forwarded");
    }

    #[test]
    fn malformed_payload_is_forwarded_without_panic() {
        let rig = Rig::new();

        rig.emitter.emit("agent:x", json!({"type": 42}));
        rig.emitter.emit("agent:x", json!("not-an-object"));
        rig.emitter.emit("agent:x", json!(null));

        let s = rig.lock_state();
        assert_eq!(s.engage_calls, 0);
        assert_eq!(s.release_calls, 0);
        drop(s);
        assert_eq!(rig.inner.snapshot().len(), 3);
    }

    #[test]
    fn re_engages_after_a_completed_release() {
        let rig = Rig::new();

        // First batch drains and the grace period passes...
        let t1 = tid();
        rig.start(&t1);
        rig.end(&t1);
        rig.elapse_grace();
        assert!(!rig.engaged());
        // ...then a later turn must re-engage.
        rig.start(&tid());

        let s = rig.lock_state();
        assert!(s.engaged);
        assert_eq!(s.engage_calls, 2, "re-engaged for the second batch");
        assert_eq!(s.release_calls, 1);
    }

    #[test]
    fn thread_timer_runs_the_task_after_the_delay() {
        let (tx, rx) = std::sync::mpsc::channel();
        ThreadTimer.after(
            Duration::from_millis(1),
            Box::new(move || tx.send(()).expect("receiver is waiting")),
        );
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the scheduled task runs");
    }
    /// OS-integration check: the *real* `KeepAwakeInhibitor` must produce a
    /// power assertion visible to `pmset` while engaged and clear it on release.
    /// This exercises the one thing the fake-backed tests can't: that our exact
    /// `keepawake` builder config (`idle` only, no `PreventSystemSleep`) maps to
    /// the assertion macOS actually shows. Ignored by default — it touches real
    /// system power state and is macOS-only.
    ///
    /// `pmset` is system-wide, and `reason` is a fixed string, so another
    /// Switchboard instance (e.g. a running `make dev`) would hold an
    /// identically-named assertion. We therefore match on *our own* PID, not the
    /// name alone — otherwise a concurrent instance's assertion would survive our
    /// release and fail the after-check spuriously.
    #[test]
    #[ignore = "macOS-only; creates a real power assertion — run with: cargo test -p switchboard-app -- --ignored real_inhibitor"]
    fn real_inhibitor_engages_idle_assertion_visible_to_pmset() {
        fn pmset_assertions() -> String {
            let out = std::process::Command::new("pmset")
                .args(["-g", "assertions"])
                .output()
                .expect("pmset should be present on macOS");
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        const NAME: &str = "Switchboard: agent turn in progress";
        // `pmset` attributes each assertion to its owning process as `pid N(name)`.
        let our_pid = format!("pid {}(", std::process::id());
        let our_assertion = |out: &str| -> Option<String> {
            out.lines()
                .find(|l| l.contains(&our_pid) && l.contains(NAME))
                .map(str::to_owned)
        };

        let inhibitor = KeepAwakeInhibitor::new();
        assert!(inhibitor.engage(), "engage should report the lock held");
        let during = pmset_assertions();
        inhibitor.release();
        let after = pmset_assertions();

        // *Our* named assertion is present only while engaged...
        let our_line = our_assertion(&during)
            .unwrap_or_else(|| panic!("expected our assertion while engaged; pmset:\n{during}"));
        assert!(
            our_assertion(&after).is_none(),
            "our assertion must clear on release; pmset:\n{after}"
        );
        // ...and it's the idle assertion, not the stronger system-sleep one.
        assert!(
            our_line.contains("PreventUserIdleSystemSleep"),
            "our assertion should be the idle type; line was:\n{our_line}"
        );
        assert!(
            !our_line.contains("PreventSystemSleep named"),
            "we must not hold a PreventSystemSleep assertion; line was:\n{our_line}"
        );
    }
}
