//! Workflow-run record types — the format of a run's `runs/<run-id>.jsonl`.
//! Defined here (with the language) per architecture decision #1; the interpreter
//! in `crates/app` writes them and the app owns the path.
//!
//! **Progress bookkeeping, not replay state.** Resume/retry is deferred beyond v1
//! (a crashed or failed run is *abandoned*, not resumed — see the v1 plan and
//! `docs/workflow-spec.md` §"Failure handling"), so these records carry only what
//! the app needs to *surface* a run and let the user abandon it: the run's workflow
//! name + step count, a marker per completed step, and a terminal marker. They
//! carry **no agent output text** — the system-design §3 "Switchboard stores no
//! agent content" invariant stands unmodified. The live per-run output scope the
//! template helpers read ([`crate::OutputScope`]) is in-memory only and never
//! reaches disk.
//!
//! **Interrupted is inferred, never written.** A run whose file ends without a
//! [`RunRecord::Terminal`] died mid-run; the app reads that as `interrupted`. That is
//! why the written terminal carries [`TerminalStatus`] (no `Interrupted`), while
//! the reader-facing [`RunStatus`] includes it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The terminal status of a run as the reader / UI sees it. `Interrupted` is
/// never written — it is inferred from a run file with no [`RunRecord::Terminal`]
/// (the process died mid-run). The writable subset is [`TerminalStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunStatus {
    Complete,
    Cancelled,
    Failed,
    Interrupted,
}

/// The statuses a [`RunRecord::Terminal`] record may carry — the **controlled**
/// terminals only. Excludes `Interrupted` (inferred from the absence of a
/// terminal record, never written) so a file can't both claim "interrupted" and
/// trigger the absence-inference. (Same make-invalid-states-unrepresentable
/// discipline as `core`'s `SessionLocator`.)
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

/// One line in `runs/<run-id>.jsonl`. Tagged `type` to match the JSONL wire
/// convention used elsewhere (the conversation journal, the agent registry).
/// `#[non_exhaustive]` so a later variant (e.g. an iteration-progress marker when
/// `for_each` lands, or a persisted snapshot if resume is ever added) is an
/// additive, non-breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunRecord {
    /// First line of the file: the run's workflow name and step count, so the app
    /// can label it ("interrupted at step N of M, workflow X") without re-reading the
    /// workflow file. Carries the *name* only — not the program or inputs (that
    /// snapshot is the deferred-resume concern, not written here).
    Started {
        workflow: String,
        total_steps: usize,
        at: DateTime<Utc>,
    },
    /// Written when a top-level step finishes. The highest `step_index` seen,
    /// with no following `Terminal`, is the run's interrupted point.
    StepCompleted {
        step_index: usize,
        at: DateTime<Utc>,
    },
    /// Written when the run reaches a controlled terminal. Its absence marks the
    /// run `interrupted` on restart.
    ///
    /// For a `Failed` terminal, `failed_step` and `reason` carry the durable
    /// "what/where" so a failed run surfaced after a restart can explain itself —
    /// `reason` is the interpreter's operational error message (a render error,
    /// contention refusal, unresolvable prompt id, harness failure), **never agent
    /// output** (§3 stands). They are `None` for `complete` / `cancelled` (and
    /// omitted from the wire), and the interpreter is the only layer that holds the
    /// reason at failure time — hence captured here rather than reconstructed later.
    Terminal {
        status: TerminalStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed_step: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
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

    fn roundtrip(record: &RunRecord) -> RunRecord {
        serde_json::from_str(&serde_json::to_string(record).unwrap()).unwrap()
    }

    #[test]
    fn records_round_trip() {
        for record in [
            RunRecord::Started {
                workflow: "review-and-aggregate".to_owned(),
                total_steps: 3,
                at: fixed_time(),
            },
            RunRecord::StepCompleted {
                step_index: 1,
                at: fixed_time(),
            },
            RunRecord::Terminal {
                status: TerminalStatus::Complete,
                failed_step: None,
                reason: None,
                at: fixed_time(),
            },
            RunRecord::Terminal {
                status: TerminalStatus::Failed,
                failed_step: Some(2),
                reason: Some("agent reviewer-1 is busy".to_owned()),
                at: fixed_time(),
            },
        ] {
            assert_eq!(roundtrip(&record), record);
        }
    }

    #[test]
    fn terminal_round_trips_for_each_writable_status() {
        // Only the controlled terminals are writable; `Interrupted` is not a
        // `TerminalStatus` variant, so it cannot be written as a record at all.
        for status in [
            TerminalStatus::Complete,
            TerminalStatus::Cancelled,
            TerminalStatus::Failed,
        ] {
            let record = RunRecord::Terminal {
                status,
                failed_step: None,
                reason: None,
                at: fixed_time(),
            };
            assert_eq!(roundtrip(&record), record);
        }
    }

    #[test]
    fn non_failed_terminal_omits_failure_fields_on_the_wire() {
        // `complete` / `cancelled` terminals stay clean — no null failure fields.
        let value = serde_json::to_value(RunRecord::Terminal {
            status: TerminalStatus::Complete,
            failed_step: None,
            reason: None,
            at: fixed_time(),
        })
        .unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("failed_step"));
        assert!(!obj.contains_key("reason"));
    }

    #[test]
    fn failed_terminal_carries_operational_reason() {
        let value = serde_json::to_value(RunRecord::Terminal {
            status: TerminalStatus::Failed,
            failed_step: Some(2),
            reason: Some("prompt id local:nope does not resolve".to_owned()),
            at: fixed_time(),
        })
        .unwrap();
        assert_eq!(value["status"], "failed");
        assert_eq!(value["failed_step"], 2);
        assert_eq!(value["reason"], "prompt id local:nope does not resolve");
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
        let json: serde_json::Value = serde_json::from_str(
            &serde_json::to_string(&RunRecord::Terminal {
                status: TerminalStatus::Complete,
                failed_step: None,
                reason: None,
                at: fixed_time(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(json["type"], "terminal");
        assert_eq!(json["status"], "complete");
    }

    #[test]
    fn started_record_holds_only_name_count_and_time() {
        // Guards the §3 invariant at the record level: the run-start record
        // carries no field that could hold agent output — just a name, a count,
        // and a timestamp.
        let value = serde_json::to_value(RunRecord::Started {
            workflow: "w".to_owned(),
            total_steps: 2,
            at: fixed_time(),
        })
        .unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["at", "total_steps", "type", "workflow"]);
    }
}
