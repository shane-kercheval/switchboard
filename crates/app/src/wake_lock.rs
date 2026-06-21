//! Global emitter decorator that keeps the machine awake while any agent turn
//! is in flight, and releases the OS wake lock once the last turn ends.
//!
//! **Why an emitter decorator.** Turn liveness is already broadcast as wire
//! events: the dispatcher emits exactly one `turn_start` and exactly one
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
//! **Why a trait for the OS call.** `KeepAwakeInhibitor` is the only part that
//! touches real power-management APIs (via the `keepawake` crate); everything
//! else — the set bookkeeping, the engage-on-first / release-on-last edges — is
//! pure and unit-tested against a fake `SleepInhibitor`. Best-effort by design:
//! a failed `engage` logs and leaves the machine able to sleep rather than
//! blocking a turn.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use switchboard_dispatcher::EventEmitter;

use crate::state::lock;

/// Acquires and releases an OS-level "stay awake" lock. Idempotent on both
/// sides: repeated `engage`/`release` calls collapse to the underlying lock's
/// presence or absence. Abstracted so the counting decorator can be tested
/// without touching real power assertions.
pub trait SleepInhibitor: Send + Sync {
    /// Ensure the machine is held awake. A no-op if already engaged.
    fn engage(&self);
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
    fn engage(&self) {
        let mut held = lock(&self.guard);
        if held.is_some() {
            return;
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
            Ok(awake) => *held = Some(awake),
            Err(e) => {
                tracing::warn!(error = %e, "failed to acquire wake lock; system may sleep mid-turn");
            }
        }
    }

    fn release(&self) {
        // Dropping the guard releases the OS lock.
        *lock(&self.guard) = None;
    }
}

/// Wraps an `EventEmitter`, tracking the set of in-flight turns across all
/// agents and driving a [`SleepInhibitor`] on the empty→non-empty and
/// non-empty→empty edges. Forwards every event to the inner emitter unchanged.
///
/// **Why a set of `turn_id`s, not a counter.** `turn_id` is a dispatcher-minted,
/// process-wide-unique `UUIDv7`, so insert/remove are idempotent: a duplicate
/// `turn_start` can't double-hold the lock and a duplicate `turn_end` can't
/// release it while another turn is live. The wake lock is a global OS side
/// effect, so it stays robust to a bad adapter/parser event rather than
/// amplifying it. A `turn_end` whose `turn_id` is absent or never-tracked is a
/// no-op — failing safe toward staying awake rather than sleeping mid-turn.
pub struct WakeLockEmitter<I: SleepInhibitor> {
    inner: Arc<dyn EventEmitter>,
    inhibitor: I,
    active_turns: Mutex<HashSet<String>>,
}

impl<I: SleepInhibitor> WakeLockEmitter<I> {
    pub fn new(inner: Arc<dyn EventEmitter>, inhibitor: I) -> Self {
        Self {
            inner,
            inhibitor,
            active_turns: Mutex::new(HashSet::new()),
        }
    }
}

impl<I: SleepInhibitor> EventEmitter for WakeLockEmitter<I> {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        // The inhibitor call stays *inside* the lock so the side effect is
        // serialized with the set transition — dropping the lock first would
        // let a release race ahead of a re-engage and leave the machine
        // sleepable while a turn is live.
        match payload.get("type").and_then(serde_json::Value::as_str) {
            Some("turn_start") => {
                if let Some(turn_id) = turn_id_of(&payload) {
                    let mut active = lock(&self.active_turns);
                    let was_empty = active.is_empty();
                    if active.insert(turn_id.to_owned()) && was_empty {
                        self.inhibitor.engage();
                    }
                }
            }
            Some("turn_end") => {
                if let Some(turn_id) = turn_id_of(&payload) {
                    let mut active = lock(&self.active_turns);
                    if active.remove(turn_id) && active.is_empty() {
                        self.inhibitor.release();
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use switchboard_dispatcher::RecordingEmitter;
    use uuid::Uuid;

    /// Records engage/release calls and tracks current engaged state so tests
    /// can assert both the edge transitions and the call counts.
    #[derive(Default)]
    struct FakeInhibitor {
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        engaged: bool,
        engage_calls: usize,
        release_calls: usize,
    }

    impl SleepInhibitor for &FakeInhibitor {
        fn engage(&self) {
            let mut s = lock(&self.state);
            s.engaged = true;
            s.engage_calls += 1;
        }
        fn release(&self) {
            let mut s = lock(&self.state);
            s.engaged = false;
            s.release_calls += 1;
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

    fn build(
        inhibitor: &FakeInhibitor,
    ) -> (WakeLockEmitter<&FakeInhibitor>, Arc<RecordingEmitter>) {
        let inner = Arc::new(RecordingEmitter::new());
        let emitter = WakeLockEmitter::new(Arc::clone(&inner) as Arc<dyn EventEmitter>, inhibitor);
        (emitter, inner)
    }

    #[test]
    fn first_turn_start_engages_and_forwards() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, inner) = build(&inhibitor);

        let payload = turn_start(&tid());
        emitter.emit("agent:x", payload.clone());

        assert!(lock(&inhibitor.state).engaged, "first turn must engage");
        let recorded = inner.snapshot();
        assert_eq!(recorded.len(), 1, "event must still be forwarded");
        assert_eq!(recorded[0].1, payload);
    }

    #[test]
    fn single_turn_releases_on_end() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, _inner) = build(&inhibitor);

        let t = tid();
        emitter.emit("agent:x", turn_start(&t));
        emitter.emit("agent:x", turn_end(&t, &completed()));

        let s = lock(&inhibitor.state);
        assert!(!s.engaged, "machine may sleep once the only turn ends");
        assert_eq!(s.engage_calls, 1);
        assert_eq!(s.release_calls, 1);
    }

    #[test]
    fn overlapping_turns_engage_once_release_only_on_last() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, _inner) = build(&inhibitor);

        // Two agents' turns overlap: start, start, end, end.
        let (a, b) = (tid(), tid());
        emitter.emit("agent:a", turn_start(&a));
        emitter.emit("agent:b", turn_start(&b));
        assert!(lock(&inhibitor.state).engaged);

        emitter.emit("agent:a", turn_end(&a, &completed()));
        assert!(
            lock(&inhibitor.state).engaged,
            "still engaged while the second turn runs"
        );

        emitter.emit("agent:b", turn_end(&b, &completed()));
        let s = lock(&inhibitor.state);
        assert!(!s.engaged, "released only after the last turn ends");
        assert_eq!(s.engage_calls, 1, "engaged exactly once across overlap");
        assert_eq!(s.release_calls, 1, "released exactly once across overlap");
    }

    #[test]
    fn failed_terminal_releases_like_completed() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, _inner) = build(&inhibitor);

        let t = tid();
        emitter.emit("agent:x", turn_start(&t));
        emitter.emit(
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

        assert!(
            !lock(&inhibitor.state).engaged,
            "a failed terminal clears its turn identically to a completed one"
        );
    }

    #[test]
    fn cancelled_terminal_releases_like_completed() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, _inner) = build(&inhibitor);

        let t = tid();
        emitter.emit("agent:x", turn_start(&t));
        emitter.emit(
            "agent:x",
            turn_end(&t, &json!({"status": "cancelled", "source": "user"})),
        );

        assert!(
            !lock(&inhibitor.state).engaged,
            "a cancelled terminal clears its turn identically to a completed one"
        );
    }

    #[test]
    fn duplicate_turn_start_does_not_double_track() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, _inner) = build(&inhibitor);

        // Same turn's start arrives twice (e.g. a buggy adapter/parser).
        let t = tid();
        emitter.emit("agent:x", turn_start(&t));
        emitter.emit("agent:x", turn_start(&t));
        assert_eq!(
            lock(&inhibitor.state).engage_calls,
            1,
            "a duplicate start must not re-engage"
        );

        // A single matching end still fully drains and releases.
        emitter.emit("agent:x", turn_end(&t, &completed()));
        let s = lock(&inhibitor.state);
        assert!(!s.engaged, "one end clears the (idempotently) tracked turn");
        assert_eq!(s.release_calls, 1);
    }

    #[test]
    fn duplicate_turn_end_does_not_release_while_another_turn_is_live() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, _inner) = build(&inhibitor);

        // Two turns live; the first ends, then its terminal is delivered a
        // second time. The duplicate must NOT release while turn `b` runs —
        // the precise failure a bare counter would allow (2→1→0).
        let (a, b) = (tid(), tid());
        emitter.emit("agent:a", turn_start(&a));
        emitter.emit("agent:b", turn_start(&b));
        emitter.emit("agent:a", turn_end(&a, &completed()));
        emitter.emit("agent:a", turn_end(&a, &completed())); // duplicate

        assert!(
            lock(&inhibitor.state).engaged,
            "a duplicate end for an already-cleared turn must not release while `b` is live"
        );
        assert_eq!(
            lock(&inhibitor.state).release_calls,
            0,
            "no release yet — `b` is still running"
        );

        emitter.emit("agent:b", turn_end(&b, &completed()));
        let s = lock(&inhibitor.state);
        assert!(!s.engaged, "released only once the last live turn ends");
        assert_eq!(s.release_calls, 1);
    }

    #[test]
    fn unpaired_turn_end_is_a_safe_no_op() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, inner) = build(&inhibitor);

        // A stray terminal with no live turn: must be a no-op for the lock.
        emitter.emit("agent:x", turn_end(&tid(), &completed()));

        let s = lock(&inhibitor.state);
        assert!(!s.engaged);
        assert_eq!(s.engage_calls, 0);
        assert_eq!(s.release_calls, 0, "no release without a prior engage");
        drop(s);
        assert_eq!(inner.snapshot().len(), 1, "event still forwarded");
    }

    #[test]
    fn turn_end_missing_turn_id_fails_safe_and_does_not_release() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, _inner) = build(&inhibitor);

        // A live turn, then a malformed terminal with no `turn_id`: it can't be
        // matched, so it must not release (fail safe toward staying awake).
        let t = tid();
        emitter.emit("agent:x", turn_start(&t));
        emitter.emit(
            "agent:x",
            json!({"type": "turn_end", "outcome": completed(), "ended_at": "now"}),
        );

        assert!(
            lock(&inhibitor.state).engaged,
            "an unmatched terminal must not release a live turn's lock"
        );
        assert_eq!(lock(&inhibitor.state).release_calls, 0);
    }

    #[test]
    fn non_lifecycle_events_do_not_touch_the_lock_but_forward() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, inner) = build(&inhibitor);

        for payload in [
            json!({"type": "content_chunk", "turn_id": "t", "kind": "text", "text": "hi"}),
            json!({"type": "session_meta", "agent_id": Uuid::now_v7().to_string()}),
            json!({"type": "agent_idle", "agent_id": Uuid::now_v7().to_string()}),
            json!({"type": "rate_limit_event", "agent_id": Uuid::now_v7().to_string(), "info": {}}),
        ] {
            emitter.emit("agent:x", payload);
        }

        let s = lock(&inhibitor.state);
        assert_eq!(s.engage_calls, 0);
        assert_eq!(s.release_calls, 0);
        drop(s);
        assert_eq!(inner.snapshot().len(), 4, "all events forwarded");
    }

    #[test]
    fn malformed_payload_is_forwarded_without_panic() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, inner) = build(&inhibitor);

        emitter.emit("agent:x", json!({"type": 42}));
        emitter.emit("agent:x", json!("not-an-object"));
        emitter.emit("agent:x", json!(null));

        let s = lock(&inhibitor.state);
        assert_eq!(s.engage_calls, 0);
        assert_eq!(s.release_calls, 0);
        drop(s);
        assert_eq!(inner.snapshot().len(), 3);
    }

    #[test]
    fn re_engages_after_full_drain() {
        let inhibitor = FakeInhibitor::default();
        let (emitter, _inner) = build(&inhibitor);

        // First batch drains fully...
        let t1 = tid();
        emitter.emit("agent:x", turn_start(&t1));
        emitter.emit("agent:x", turn_end(&t1, &completed()));
        // ...then a later turn must re-engage.
        emitter.emit("agent:x", turn_start(&tid()));
        assert!(lock(&inhibitor.state).engaged);

        let s = lock(&inhibitor.state);
        assert_eq!(s.engage_calls, 2, "re-engaged for the second batch");
        assert_eq!(s.release_calls, 1);
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
        inhibitor.engage();
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
