//! App-side conversation-journal sink.
//!
//! Implements the dispatcher's [`ConversationJournal`] trait by writing to a
//! specific project's `journal.jsonl` (core owns the file format; the
//! dispatcher owns *when* records are written; this binds the two to a project
//! path + `send_id`). Constructed per dispatch in `send_message_impl`,
//! mirroring how `SessionMetaObservingEmitter` is constructed there.
//!
//! **Asymmetric failure handling**, matching the [`ConversationJournal`]
//! contract: `record_send` is **fail-closed** — it returns the write error so
//! the dispatcher can refuse to start a turn whose send it couldn't persist
//! (a lost send record would orphan the assistant's reply after restart).
//! `record_outcome` is **best-effort** — its turn already ran, so it logs and
//! swallows (worst case is a mislabel, not data loss).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use switchboard_core::{AgentId, Attachment, JournalRecord, SendId};
use switchboard_dispatcher::{ConversationJournal, JournalError};
use switchboard_harness::{TurnId, TurnOutcome};

/// Writes one project's conversation journal. Bound to the project's
/// `journal.jsonl` path and the `send_id` of the originating Send action (one
/// per Send; shared across a fan-out's recipients so hydration groups them).
pub struct ProjectJournal {
    journal_path: PathBuf,
    send_id: SendId,
}

impl ProjectJournal {
    pub fn new(journal_path: PathBuf, send_id: SendId) -> Self {
        Self {
            journal_path,
            send_id,
        }
    }
}

impl ConversationJournal for ProjectJournal {
    fn record_send(
        &self,
        turn_id: TurnId,
        agent_id: AgentId,
        prompt: &str,
        attachments: &[Attachment],
        at: DateTime<Utc>,
    ) -> Result<(), JournalError> {
        let record = JournalRecord::Send {
            send_id: self.send_id,
            turn_id,
            agent_id,
            prompt: prompt.to_owned(),
            attachments: attachments.to_vec(),
            at,
        };
        // Fail-closed: propagate the error so the dispatcher refuses to start
        // the turn. The error text reaches the user via the command's Result.
        switchboard_core::journal::append_record(&self.journal_path, &record)
            .map_err(|e| JournalError(Box::new(e)))
    }

    fn record_outcome(
        &self,
        turn_id: TurnId,
        agent_id: AgentId,
        outcome: &TurnOutcome,
        started_at: DateTime<Utc>,
        ended_at: DateTime<Utc>,
    ) {
        // Store the outcome in its wire shape so core stays free of the
        // harness outcome type. Serialization of a well-formed outcome is
        // infallible; log-and-skip on the cosmic edge rather than panic.
        let outcome = match serde_json::to_value(outcome) {
            Ok(value) => value,
            Err(e) => {
                tracing::error!(
                    %agent_id,
                    error = %e,
                    "failed to serialize turn outcome for journal — skipping (should be unreachable)"
                );
                return;
            }
        };
        let record = JournalRecord::Outcome {
            send_id: self.send_id,
            turn_id,
            agent_id,
            outcome,
            started_at,
            ended_at,
        };
        if let Err(e) = switchboard_core::journal::append_record(&self.journal_path, &record) {
            tracing::warn!(
                %agent_id,
                error = %e,
                "failed to journal turn outcome — turn proceeds; marker may be absent after restart"
            );
        }
    }

    fn record_link(
        &self,
        turn_id: TurnId,
        agent_id: AgentId,
        hydration_key: &str,
        at: DateTime<Utc>,
    ) {
        let record = JournalRecord::TurnLink {
            send_id: self.send_id,
            turn_id,
            agent_id,
            hydration_key: hydration_key.to_owned(),
            at,
        };
        // Best-effort: a lost link just drops this turn to positional correlation
        // in the merge; the turn already produced content, so never fail it.
        if let Err(e) = switchboard_core::journal::append_record(&self.journal_path, &record) {
            tracing::warn!(
                %agent_id,
                error = %e,
                "failed to journal turn link — turn proceeds; merge falls back to positional correlation for it"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Best-effort contract: a failing link write must not panic or propagate —
    /// the turn already produced content, so it just drops to positional. The
    /// path's parent doesn't exist, so `append_record` errors; `record_link`
    /// must swallow it. (`record_link` returns `()`, so the type already forbids
    /// failing the turn; this guards the impl against a future `unwrap`.)
    #[test]
    fn record_link_swallows_write_errors() {
        let journal = ProjectJournal::new(
            PathBuf::from("/nonexistent-dir-xyz/journal.jsonl"),
            Uuid::now_v7(),
        );
        journal.record_link(Uuid::now_v7(), Uuid::now_v7(), "msg_first", Utc::now());
        // Reaching here (no panic) is the assertion.
    }
}
