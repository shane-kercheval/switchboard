//! App-side stream-only-metadata cache sink.
//!
//! Implements the dispatcher's [`MetadataCache`] trait by writing to a
//! specific agent's per-agent metadata sidecar
//! (`<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.meta.json`).
//! The harness crate owns the file format + atomic write (`meta_sidecar`);
//! the dispatcher owns *when* a snapshot is recorded; this binds the two to a
//! resolved sidecar path. Constructed per dispatch in the dispatch-context
//! factory, mirroring how [`crate::journal::ProjectJournal`] is built there.
//!
//! **Best-effort**, matching the [`MetadataCache`] contract: a failed write
//! logs at `warn` and is dropped. The sidecar is a restart-continuity UX
//! improvement, not load-bearing — a lost write degrades to "the agent bar
//! shows no quota state until the next event," which is exactly the
//! pre-cache behavior.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use switchboard_core::AgentId;
use switchboard_dispatcher::MetadataCache;

/// Writes one agent's metadata sidecar. Bound to the resolved sidecar path
/// (computed once from `directory` + `project_id` + `agent_id`) and the
/// `agent_id` it was built for — the latter is used only to assert the
/// dispatcher hands back the expected agent (the cache is per-agent, so a
/// mismatch means a factory/context wiring bug).
pub struct ProjectMetadataCache {
    agent_id: AgentId,
    sidecar_path: PathBuf,
    turnmeta_path: PathBuf,
}

impl ProjectMetadataCache {
    pub fn new(agent_id: AgentId, sidecar_path: PathBuf, turnmeta_path: PathBuf) -> Self {
        Self {
            agent_id,
            sidecar_path,
            turnmeta_path,
        }
    }
}

impl MetadataCache for ProjectMetadataCache {
    fn record_rate_limit(
        &self,
        agent_id: AgentId,
        info: serde_json::Value,
        captured_at: DateTime<Utc>,
    ) {
        // The cache is per-agent (one instance per dispatch context); the
        // path was resolved for `self.agent_id`. The trait still passes the
        // event's `agent_id`, so assert they agree — a mismatch is a wiring
        // bug (a context built for one agent handling another's event).
        // Debug-assert to catch it in tests; in release, **skip the write**
        // rather than panic OR write the wrong data: `self.sidecar_path` is
        // this agent's file but `info` would be another agent's payload, so
        // writing it would surface B's quota state under A after restart.
        // Skipping degrades to "A's sidecar unchanged" — the same best-effort
        // outcome as a failed write.
        debug_assert_eq!(
            agent_id, self.agent_id,
            "metadata cache built for {} received event for {agent_id}",
            self.agent_id
        );
        if agent_id != self.agent_id {
            tracing::warn!(
                expected = %self.agent_id,
                got = %agent_id,
                "metadata cache agent_id mismatch — skipping write to avoid persisting another agent's data"
            );
            return;
        }
        if let Err(e) = switchboard_harness::meta_sidecar::write_rate_limit(
            &self.sidecar_path,
            info,
            captured_at,
        ) {
            tracing::warn!(
                agent_id = %self.agent_id,
                error = %e,
                "failed to persist rate-limit snapshot to metadata sidecar — restart continuity degraded; turn unaffected"
            );
        }
    }

    fn record_context_window(
        &self,
        agent_id: AgentId,
        context_window: u32,
        captured_at: DateTime<Utc>,
    ) {
        // Same per-agent wiring guard as `record_rate_limit`: the path was
        // resolved for `self.agent_id`, so a mismatched event id is a wiring
        // bug. Skip rather than write another agent's window under this one.
        debug_assert_eq!(
            agent_id, self.agent_id,
            "metadata cache built for {} received event for {agent_id}",
            self.agent_id
        );
        if agent_id != self.agent_id {
            tracing::warn!(
                expected = %self.agent_id,
                got = %agent_id,
                "metadata cache agent_id mismatch — skipping context-window write to avoid persisting another agent's data"
            );
            return;
        }
        if let Err(e) = switchboard_harness::meta_sidecar::write_context_window(
            &self.sidecar_path,
            context_window,
            captured_at,
        ) {
            tracing::warn!(
                agent_id = %self.agent_id,
                error = %e,
                "failed to persist context-window snapshot to metadata sidecar — restart continuity degraded; turn unaffected"
            );
        }
    }

    fn record_turn_spend(
        &self,
        agent_id: AgentId,
        message_id: String,
        total_cost_usd: Option<f64>,
        spend: switchboard_harness::TurnSpend,
        captured_at: DateTime<Utc>,
    ) {
        // Same per-agent wiring guard as the snapshot writers: the turnmeta
        // path was resolved for `self.agent_id`, so a mismatched event id is a
        // wiring bug. Skip rather than append another agent's turn under this
        // one's log.
        debug_assert_eq!(
            agent_id, self.agent_id,
            "metadata cache built for {} received event for {agent_id}",
            self.agent_id
        );
        if agent_id != self.agent_id {
            tracing::warn!(
                expected = %self.agent_id,
                got = %agent_id,
                "metadata cache agent_id mismatch — skipping turn-spend append to avoid persisting another agent's data"
            );
            return;
        }
        let record = switchboard_harness::turnmeta_sidecar::TurnMetaRecord {
            message_id,
            total_cost_usd,
            spend,
            captured_at,
        };
        if let Err(e) = switchboard_harness::turnmeta_sidecar::append(&self.turnmeta_path, &record)
        {
            tracing::warn!(
                agent_id = %self.agent_id,
                error = %e,
                "failed to append turn-spend record to turn-metadata sidecar — reopen cost/overage degraded; turn unaffected"
            );
        }
    }
}
