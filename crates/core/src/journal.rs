//! Conversation journal — Switchboard's user-side conversation persistence.
//!
//! This refines the "Switchboard stores no transcript" posture into a split
//! (see system-design §3): the journal owns the **user's** side (each *send*,
//! and an *outcome* marker for every non-completed turn), while harness
//! session files own the **agents'** side (completed-turn content). The two
//! partition cleanly — a completed turn's content comes from the harness file;
//! a failed or cancelled turn's marker comes from here — so no correlation or
//! de-dup between the two sources is needed.
//!
//! One `journal.jsonl` lives per project (`projects/<id>/journal.jsonl`).
//! Records are append-only and durable per-record (the fsync in
//! [`crate::io::append_jsonl`]); they land at human-paced turn boundaries
//! (one at turn-start, one at a non-completed terminal), so the fsync pressure
//! is negligible.
//!
//! **Partial content of an aborted turn:** never persisted *here* — the journal
//! holds only the outcome marker (failure reason / cancel source, no agent
//! content). The live UI shows partial output from the dispatcher's in-memory
//! stream buffer until the next turn or a restart. After restart, any partial
//! shown comes solely from the harness session file, and Switchboard adds none
//! of its own — so the post-restart partial is whatever that harness chose to
//! keep (Claude Code and Codex keep nothing for an aborted turn → marker only;
//! a harness that persists partial would have it rendered automatically). See
//! system-design §3 and §7 "Unified history after restart".

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::AgentId;
use crate::attachment::Attachment;
use crate::error::Result;
use crate::io::{append_jsonl, read_jsonl};

/// Groups the recipients of one fan-out send so the user's message renders
/// once across N turns. Minted per Send action; shared by every per-recipient
/// record of that send.
pub type SendId = Uuid;

/// One line in a project's `journal.jsonl`. Tagged `type` to match the wire
/// convention used elsewhere in Switchboard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum JournalRecord {
    /// Written when a recipient's turn *starts* — specifically **immediately
    /// before the harness subprocess is spawned**, fail-closed (if this write
    /// fails the turn does not start). One record per recipient; carries the
    /// user's prompt — the user's side of the conversation, which Switchboard
    /// legitimately owns. A message removed from the queue before it starts is
    /// never journaled (correctly absent after restart). Note this is written
    /// *before* (and independently of) any `TurnStart` wire event: a turn that
    /// fails to start at all carries this `Send` plus a `Failed` `Outcome`, and
    /// no `TurnStart` was ever emitted — so a `Send` does not imply an observed
    /// `TurnStart`.
    Send {
        send_id: SendId,
        turn_id: Uuid,
        agent_id: AgentId,
        prompt: String,
        /// Files attached to this send (empty for a plain text send). Defaults to
        /// empty on deserialize so journals written before attachments existed —
        /// `Send` lines with no `attachments` key — still parse.
        #[serde(default)]
        attachments: Vec<Attachment>,
        at: DateTime<Utc>,
    },
    /// Written on a **non-completed** terminal (failed or cancelled) — never
    /// for a completed turn. Carries no agent content; `outcome` is the
    /// terminal outcome's wire shape (e.g. `{"status":"cancelled","source":"user"}`
    /// or `{"status":"failed","kind":"harness_error","message":"…"}`), stored
    /// opaquely so core need not depend on the harness outcome type.
    Outcome {
        send_id: SendId,
        turn_id: Uuid,
        agent_id: AgentId,
        outcome: serde_json::Value,
        started_at: DateTime<Utc>,
        ended_at: DateTime<Utc>,
    },
}

/// Append one record to a project's `journal.jsonl`.
///
/// Multiple agents in a project append concurrently; a single-line `O_APPEND`
/// write is atomic on POSIX, so no per-journal lock is needed and records are
/// read back ordered by timestamp at merge time, not by file offset.
pub fn append_record(path: &Path, record: &JournalRecord) -> Result<()> {
    append_jsonl(path, record)
}

/// Read every record from a project's `journal.jsonl` (empty if the file does
/// not exist yet). Fail-loud on a corrupt line (per the Switchboard-owned-JSONL
/// invariant) — same as the other append-only logs.
pub fn read_records(path: &Path) -> Result<Vec<JournalRecord>> {
    read_jsonl(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn send(send_id: SendId, agent_id: AgentId, prompt: &str) -> JournalRecord {
        JournalRecord::Send {
            send_id,
            turn_id: Uuid::now_v7(),
            agent_id,
            prompt: prompt.to_owned(),
            attachments: Vec::new(),
            at: Utc::now(),
        }
    }

    #[test]
    fn append_then_read_round_trips_records_in_order() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let s1 = Uuid::now_v7();
        let a = Uuid::now_v7();
        let r1 = send(s1, a, "first");
        let r2 = JournalRecord::Outcome {
            send_id: s1,
            turn_id: Uuid::now_v7(),
            agent_id: a,
            outcome: json!({"status": "cancelled", "source": "user"}),
            started_at: Utc::now(),
            ended_at: Utc::now(),
        };
        append_record(&path, &r1).unwrap();
        append_record(&path, &r2).unwrap();

        let read = read_records(&path).unwrap();
        assert_eq!(read, vec![r1, r2]);
    }

    #[test]
    fn read_missing_journal_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        assert!(read_records(&path).unwrap().is_empty());
    }

    #[test]
    fn records_grouped_by_send_id_for_a_fan_out() {
        // Two recipients of one Send share a send_id → hydration groups them
        // into a single user message.
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let shared = Uuid::now_v7();
        let other = Uuid::now_v7();
        append_record(&path, &send(shared, Uuid::now_v7(), "fan-out")).unwrap();
        append_record(&path, &send(shared, Uuid::now_v7(), "fan-out")).unwrap();
        append_record(&path, &send(other, Uuid::now_v7(), "separate")).unwrap();

        let read = read_records(&path).unwrap();
        let shared_count = read
            .iter()
            .filter(|r| matches!(r, JournalRecord::Send { send_id, .. } if *send_id == shared))
            .count();
        assert_eq!(shared_count, 2, "both fan-out recipients share one send_id");
    }

    #[test]
    fn send_with_attachments_round_trips() {
        use crate::attachment::{Attachment, AttachmentKind};

        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let record = JournalRecord::Send {
            send_id: Uuid::now_v7(),
            turn_id: Uuid::now_v7(),
            agent_id: Uuid::now_v7(),
            prompt: "read this".to_owned(),
            attachments: vec![Attachment {
                label: "image-1".to_owned(),
                kind: AttachmentKind::Image,
                path: "/p/.switchboard/projects/x/attachments/u__diagram.png".to_owned(),
                original_name: "diagram.png".to_owned(),
            }],
            at: Utc::now(),
        };
        append_record(&path, &record).unwrap();
        assert_eq!(read_records(&path).unwrap(), vec![record]);
    }

    #[test]
    fn old_send_without_attachments_field_defaults_to_empty() {
        // A journal written before attachments existed: a Send line with no
        // `attachments` key must still parse (to an empty list), not fail loud.
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let line = format!(
            "{}\n",
            json!({
                "type": "send",
                "send_id": Uuid::now_v7(),
                "turn_id": Uuid::now_v7(),
                "agent_id": Uuid::now_v7(),
                "prompt": "legacy",
                "at": "2026-05-14T04:43:19Z",
            })
        );
        std::fs::write(&path, line).unwrap();

        let read = read_records(&path).unwrap();
        match read.as_slice() {
            [
                JournalRecord::Send {
                    prompt,
                    attachments,
                    ..
                },
            ] => {
                assert_eq!(prompt, "legacy");
                assert!(attachments.is_empty());
            }
            other => panic!("expected one Send, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_line_surfaces_loud() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        append_record(&path, &send(Uuid::now_v7(), Uuid::now_v7(), "ok")).unwrap();
        std::fs::write(&path, "{not json}\n").unwrap();
        assert!(read_records(&path).is_err());
    }
}
