//! Run-status and step-checkpoint record types — the format of a workflow run's
//! `runs/<run-id>.jsonl`. Defined here (with the language) per architecture
//! decision #1; the interpreter in `crates/app` (M4) writes them and the app
//! owns the path.
//!
//! **Resolved-text output scope (the scoped §3 exception).** [`OutputScope`]
//! persists each participating agent's *resolved completed-turn text*, not a
//! turn-id to be re-joined from a harness session file. The dispatcher `turn_id`
//! is not joinable to a harness file's own turn ids, and one harness has no
//! per-turn id at all, so the text is captured from the live stream at
//! completion and persisted directly. This is the narrow, intentional exception
//! to system-design §3's "Switchboard stores no agent content" — text-only, the
//! same filter as forwarding (no thinking, no tool output) — that lets a
//! crash-recovered run re-feed an earlier step's output without the impossible
//! disk join.
//!
//! **Iteration fields exist from the start.** [`IterationState`] is unused by the
//! M6 interpreter (no `for_each` execution until M7) but is defined now so M7's
//! retry-from-inside-iteration needs no schema migration — the same reason the
//! record format lives with the language rather than being grown later.
//!
//! **Resume/retry deferred beyond v1 (2026-06-18).** These types were shaped for
//! crash-recovery *resume* (re-feeding earlier-step output after a restart). That
//! feature was subsequently deferred: v1 runs a workflow start→finish and
//! *abandons* an interrupted/failed run rather than resuming it. As a result the
//! replay-oriented parts here — the persisted resolved-text [`OutputScope`] and
//! [`IterationState`] — are **not written or read by v1**, and the §3
//! "no agent content" exception they would have required is **not taken** (no
//! agent text is persisted). The per-run output scope still exists, but only
//! in-memory for a live run (read by the template helpers). The interpreter
//! milestone (M4) is expected to trim these types to the minimal progress/terminal
//! bookkeeping v1 actually needs; they are retained as-is for now because the
//! exact minimal shape is settled alongside M5's run-surfacing UI. See the v1
//! plan's architecture decisions #6/#8 and `docs/workflow-spec.md`
//! §"Failure handling".

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Per-run map of agent → most-recent resolved completed-turn **text** observed
/// by this run. Keyed by the canonical (hyphen→underscore, lowercased) agent
/// name, matching the project's agent-name uniqueness rule and how the template
/// helpers (`responses_from`, `last_output`, …) look agents up. The runtime
/// updates it on each awaited terminal; the helpers read through it.
///
/// `BTreeMap` (not a hash map) so checkpoint serialization is deterministic.
/// Iteration is **sorted by key** — callers that need declared/agent-list order
/// (the helper functions) must iterate their own ordered argument list and look
/// up here, never iterate this map.
pub type OutputScope = BTreeMap<String, String>;

/// The full terminal status of a workflow run, as the reader / UI sees it (per
/// the spec's status table). This is a *read* type: `Interrupted` is never
/// written — it is **inferred** on restart from a run file whose last record is a
/// [`CheckpointRecord::StepCompleted`] with no following
/// [`CheckpointRecord::RunTerminal`] (the process died mid-run). The set of
/// statuses that can be *written* is the strictly smaller [`TerminalStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunStatus {
    Complete,
    Cancelled,
    Failed,
    Interrupted,
}

/// The statuses a [`CheckpointRecord::RunTerminal`] record may carry — the
/// **controlled** terminals only. Deliberately excludes `Interrupted`: that state
/// is inferred from the *absence* of a terminal record, never written, so making
/// it unrepresentable in the written record removes the contradiction of a file
/// that both says "interrupted" and triggers the absence-inference. (Same
/// make-invalid-states-unrepresentable discipline as `core`'s `SessionLocator`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TerminalStatus {
    Complete,
    Cancelled,
    Failed,
}

impl From<TerminalStatus> for RunStatus {
    fn from(status: TerminalStatus) -> Self {
        match status {
            TerminalStatus::Complete => RunStatus::Complete,
            TerminalStatus::Cancelled => RunStatus::Cancelled,
            TerminalStatus::Failed => RunStatus::Failed,
        }
    }
}

/// The per-iteration state captured in a checkpoint when execution is inside a
/// `for_each` body, so retry can rebind the iteration variable and resume at the
/// failed step *within* that iteration. `item_value` is a `String` because v1
/// `for_each` iterates `[text]` / `[agent]` lists, whose items are strings (and
/// the iteration variable binds into the template as a string); structured items
/// are not a v1/v2 feature, so a richer type would be speculative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IterationState {
    pub index: usize,
    pub item_var: String,
    pub item_value: String,
}

/// One line in `runs/<run-id>.jsonl`. Tagged `type` to match the JSONL wire
/// convention used elsewhere in Switchboard (the conversation journal, the agent
/// registry). `#[non_exhaustive]` so a future variant (e.g. a persisted
/// invocation snapshot) is an additive, non-breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckpointRecord {
    /// Written at a step boundary. Carries the index of the step just completed,
    /// the **accumulated** per-run output scope (all prior steps' resolved
    /// outputs, not just this step's — what a resumed run re-feeds to later
    /// `forward_from`/helpers), the current-scope `user_input` (so a retry of a
    /// step after a completed `pause_for_user` renders correctly), and — when
    /// inside a `for_each` — the iteration state.
    StepCompleted {
        step_index: usize,
        #[serde(default)]
        iteration: Option<IterationState>,
        output_scope: OutputScope,
        #[serde(default)]
        user_input: Option<String>,
        at: DateTime<Utc>,
    },
    /// Written when the run reaches a controlled terminal. Its absence after a
    /// `StepCompleted` is what marks a run `interrupted` on restart — which is why
    /// the field is [`TerminalStatus`] (no `Interrupted`), not [`RunStatus`].
    RunTerminal {
        status: TerminalStatus,
        at: DateTime<Utc>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_time() -> DateTime<Utc> {
        // Deterministic — no wall-clock in tests.
        "2026-06-18T12:00:00Z".parse().unwrap()
    }

    fn roundtrip(record: &CheckpointRecord) -> CheckpointRecord {
        let line = serde_json::to_string(record).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn step_completed_with_resolved_text_scope_and_iteration_round_trips() {
        let mut output_scope = OutputScope::new();
        output_scope.insert("reviewer_1".to_owned(), "the full review text".to_owned());
        output_scope.insert("reviewer_2".to_owned(), "another review".to_owned());
        let record = CheckpointRecord::StepCompleted {
            step_index: 3,
            iteration: Some(IterationState {
                index: 1,
                item_var: "milestone".to_owned(),
                item_value: "M2".to_owned(),
            }),
            output_scope,
            user_input: Some("approve".to_owned()),
            at: fixed_time(),
        };
        assert_eq!(roundtrip(&record), record);
    }

    #[test]
    fn step_completed_without_iteration_or_user_input_round_trips() {
        let record = CheckpointRecord::StepCompleted {
            step_index: 0,
            iteration: None,
            output_scope: OutputScope::new(),
            user_input: None,
            at: fixed_time(),
        };
        assert_eq!(roundtrip(&record), record);
    }

    #[test]
    fn run_terminal_round_trips_for_each_writable_status() {
        // Only the controlled terminals are writable; `Interrupted` is not a
        // `TerminalStatus` variant, so it cannot be written as a record at all.
        for status in [
            TerminalStatus::Complete,
            TerminalStatus::Cancelled,
            TerminalStatus::Failed,
        ] {
            let record = CheckpointRecord::RunTerminal {
                status,
                at: fixed_time(),
            };
            assert_eq!(roundtrip(&record), record);
        }
    }

    #[test]
    fn terminal_status_maps_to_run_status() {
        assert_eq!(
            RunStatus::from(TerminalStatus::Complete),
            RunStatus::Complete
        );
        assert_eq!(
            RunStatus::from(TerminalStatus::Cancelled),
            RunStatus::Cancelled
        );
        assert_eq!(RunStatus::from(TerminalStatus::Failed), RunStatus::Failed);
    }

    #[test]
    fn wire_shape_is_type_tagged_snake_case() {
        let record = CheckpointRecord::RunTerminal {
            status: TerminalStatus::Complete,
            at: fixed_time(),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
        assert_eq!(json["type"], "run_terminal");
        assert_eq!(json["status"], "complete");
    }

    #[test]
    fn missing_optional_fields_default_on_read() {
        // A StepCompleted line written without `iteration` / `user_input` keys
        // still parses (forward/backward compatibility for the optional fields).
        let line = r#"{"type":"step_completed","step_index":2,"output_scope":{"a":"x"},"at":"2026-06-18T12:00:00Z"}"#;
        let record: CheckpointRecord = serde_json::from_str(line).unwrap();
        match record {
            CheckpointRecord::StepCompleted {
                iteration,
                user_input,
                output_scope,
                ..
            } => {
                assert!(iteration.is_none());
                assert!(user_input.is_none());
                assert_eq!(output_scope.get("a").map(String::as_str), Some("x"));
            }
            other => panic!("expected StepCompleted, got {other:?}"),
        }
    }
}
