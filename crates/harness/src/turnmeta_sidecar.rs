//! Per-agent **turn-metadata sidecar** — a small, harness-agnostic append-log
//! of stream-only *per-turn* telemetry (cost + overage) so it survives an app
//! restart and re-attaches to the right message on reopen.
//!
//! Some per-turn metadata lives *only* on the live event stream and has no
//! on-disk equivalent in the harness's own session file — Claude's
//! `total_cost_usd` and overage snapshot arrive on the stream-only `result`
//! record, which is never written to the session file. The live UI shows them
//! the moment a turn ends; without this log they vanish on reopen. This log
//! persists one record per real-spend turn and the app rejoins them onto the
//! hydrated turns by message id.
//!
//! **Append-log, not a snapshot.** Unlike [`crate::meta_sidecar`] (one
//! last-write-wins value per field), this holds *many* records per agent — one
//! per completed real-spend turn — so it's an append-JSONL file, not a
//! read-modify-write blob. Each line is one [`TurnMetaRecord`].
//!
//! **Harness-agnostic, keyed on `AgentId` + message id.** The file is keyed on
//! the Switchboard-owned `AgentId` (like the other per-agent sidecars); each
//! record is keyed on the harness's per-message id (`message_id`), the join key
//! that exists on both the live stream and the on-disk session file. Cost is
//! Claude-only *data* in v1, so only the Claude adapter produces these records,
//! but nothing here is Claude-specific — a future harness reporting cost plugs
//! in its own per-message key without reshaping the store. It lives at
//! `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.turnmeta.jsonl`.
//!
//! **Best-effort, not load-bearing.** A missing file reads as empty; a corrupt
//! *line* is skipped (the rest of the log still loads); a write that fails is
//! logged and dropped by the caller. The log is a UX improvement (restart
//! continuity), not a correctness dependency, so a damaged one never fails
//! hydration or a turn.
//!
//! **No backfill.** Turns that completed before this feature shipped have no
//! record and render no cost/overage on reopen — expected and documented, not a
//! bug.
//!
//! **Growth.** The log grows one line per real-spend turn and [`read`] loads it
//! whole (building the join map) on each reopen. There is no compaction in v1:
//! real-spend (overage) turns are rare enough that the file stays small in
//! practice, so the simplicity is worth more than a pruning mechanism.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::{AgentId, ProjectId};

use crate::events::TurnSpend;

/// One persisted per-turn telemetry record, keyed on the harness per-message id.
///
/// `message_id` is the anchor — the final non-subagent assistant message's id
/// for the turn, the same value the hydrated `Turn::Agent.stable_message_id`
/// carries — so the app can rejoin cost/overage onto the right message on
/// reopen. `total_cost_usd` and `spend` are the stream-only figures that arrive
/// on Claude's `result` record. `captured_at` is the wall-clock write time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnMetaRecord {
    /// The turn's join key: the final non-subagent assistant message's id.
    pub message_id: String,
    /// The turn's notional cost (`result.total_cost_usd`), when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    /// The overage/real-spend snapshot stamped onto the live turn.
    pub spend: TurnSpend,
    /// Wall-clock time this record was written (ISO-8601 UTC on disk).
    pub captured_at: DateTime<Utc>,
}

/// Canonical turn-metadata-sidecar path:
/// `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.turnmeta.jsonl`.
/// Shares the per-agent `sessions/` directory with the metadata snapshot
/// ([`crate::meta_sidecar`]) and any session-link sidecar, but is a distinct
/// filename and concern (an append-log of per-turn telemetry).
#[must_use]
pub fn turnmeta_sidecar_path(
    directory: &Path,
    project_id: ProjectId,
    agent_id: AgentId,
) -> PathBuf {
    directory
        .join(".switchboard")
        .join("projects")
        .join(project_id.to_string())
        .join("sessions")
        .join(format!("{agent_id}.turnmeta.jsonl"))
}

/// Read every record from the turn-metadata sidecar. **Best-effort**: a missing
/// file returns an empty `Vec`; an unreadable file logs at `warn` and returns
/// empty; a corrupt *line* is skipped (logged at `warn`) so the rest of the log
/// still loads. Never errors — the log is a UX improvement, not a correctness
/// dependency, so a damaged one must not block hydration.
#[must_use]
pub fn read(path: &Path) -> Vec<TurnMetaRecord> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "turn-metadata sidecar unreadable — treating as absent");
            return Vec::new();
        }
    };
    let mut records = Vec::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TurnMetaRecord>(line) {
            Ok(record) => records.push(record),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "turn-metadata sidecar line corrupt — skipping");
            }
        }
    }
    records
}

/// Errors raised by a turn-metadata-sidecar append. Surfaced to the caller,
/// which logs and drops them (the log is not load-bearing).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TurnMetaSidecarError {
    #[error("turn-metadata sidecar I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("turn-metadata sidecar serialization failed: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
}

/// Append one record to the turn-metadata sidecar as a single JSONL line,
/// creating the parent directory chain if needed. Each record is independent,
/// so a plain append (not the read-modify-write the snapshot sidecar uses) is
/// correct and avoids rewriting the whole log per turn. Within one app instance
/// an agent's writes are serialized by its single dispatcher actor, so there is
/// no intra-instance interleave.
pub fn append(path: &Path, record: &TurnMetaRecord) -> Result<(), TurnMetaSidecarError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TurnMetaSidecarError::Io {
            path: parent.to_owned(),
            source: e,
        })?;
    }

    let mut line = serde_json::to_string(record)
        .map_err(|source| TurnMetaSidecarError::Serialize { source })?;
    line.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| TurnMetaSidecarError::Io {
            path: path.to_owned(),
            source: e,
        })?;
    file.write_all(line.as_bytes())
        .map_err(|e| TurnMetaSidecarError::Io {
            path: path.to_owned(),
            source: e,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn record(message_id: &str, cost: Option<f64>, is_overage: bool) -> TurnMetaRecord {
        TurnMetaRecord {
            message_id: message_id.to_owned(),
            total_cost_usd: cost,
            spend: TurnSpend {
                real_spend: is_overage,
                is_overage,
                overage_resets_at: None,
            },
            captured_at: ts("2026-05-31T18:42:11Z"),
        }
    }

    #[test]
    fn turnmeta_sidecar_path_matches_canonical_layout() {
        let directory = Path::new("/Users/x/workspace");
        let project_id = Uuid::nil();
        let agent_id = Uuid::nil();
        let path = turnmeta_sidecar_path(directory, project_id, agent_id);
        let expected = PathBuf::from(format!(
            "/Users/x/workspace/.switchboard/projects/{project_id}/sessions/{agent_id}.turnmeta.jsonl"
        ));
        assert_eq!(path, expected);
    }

    #[test]
    fn read_missing_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(read(&tmp.path().join("absent.turnmeta.jsonl")).is_empty());
    }

    #[test]
    fn append_then_read_round_trips_records_in_order() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.turnmeta.jsonl");
        append(&path, &record("msg_a", Some(0.0125), true)).unwrap();
        append(&path, &record("msg_b", Some(0.5), false)).unwrap();

        let back = read(&path);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].message_id, "msg_a");
        assert_eq!(back[0].total_cost_usd, Some(0.0125));
        assert!(back[0].spend.is_overage);
        assert_eq!(back[1].message_id, "msg_b");
        assert!(!back[1].spend.is_overage);
    }

    #[test]
    fn append_creates_parent_directory_chain() {
        let tmp = TempDir::new().unwrap();
        let path = tmp
            .path()
            .join(".switchboard")
            .join("projects")
            .join(Uuid::now_v7().to_string())
            .join("sessions")
            .join(format!("{}.turnmeta.jsonl", Uuid::now_v7()));
        assert!(!path.parent().unwrap().exists());
        append(&path, &record("msg_a", Some(0.01), true)).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn corrupt_line_is_skipped_rest_still_loads() {
        // A torn line (partial write / manual edit) must not lose the healthy
        // records around it.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.turnmeta.jsonl");
        append(&path, &record("msg_a", Some(0.01), true)).unwrap();
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{ not valid json\n").unwrap();
        }
        append(&path, &record("msg_c", Some(0.03), true)).unwrap();

        let back = read(&path);
        assert_eq!(back.len(), 2, "the two healthy lines survive the torn one");
        assert_eq!(back[0].message_id, "msg_a");
        assert_eq!(back[1].message_id, "msg_c");
    }

    #[test]
    fn blank_lines_are_ignored() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.turnmeta.jsonl");
        std::fs::write(&path, "\n\n").unwrap();
        assert!(read(&path).is_empty());
    }
}
