//! Per-agent **metadata sidecar** — a small, harness-agnostic cache of
//! stream-only metadata so it survives an app restart.
//!
//! Some harness metadata lives *only* on the live event stream and has no
//! on-disk equivalent in the harness's own session file — "class C" in
//! `docs/harness-behavior.md` §3.1. Today that's Claude's
//! `rate_limit_event` payload (used %, `isUsingOverage`, reset times). The
//! TUI never loses it because it doesn't restart mid-session; Switchboard
//! does, so without this cache the agent bar shows nothing quota-related
//! until the next event. This sidecar persists the latest snapshot on each
//! event and re-reads it on project open.
//!
//! **Harness-agnostic, keyed on `AgentId`.** Unlike a harness's session
//! locator (`AgentRecord.session_locator`), this is not tied to any harness's
//! session conventions — it's keyed on the Switchboard-owned `AgentId`, so
//! harness session-id reassignment (e.g. Antigravity's expired-conversation
//! reheal) is irrelevant. It lives at
//! `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.meta.json`.
//!
//! **Last-write-wins per field, not an append-log.** Each write replaces the
//! field's latest snapshot. The context-window snapshot carries its source
//! message id only to validate which hydrated turn owns that one value; it
//! does not retain per-message history.
//!
//! **Best-effort, not load-bearing.** A missing file reads as empty; a
//! corrupt file logs and reads as empty (never fails hydration). Writes that
//! fail are logged and dropped by the caller — the sidecar is a UX
//! improvement (restart continuity), not a correctness dependency, so unlike
//! the registry-resident session locator it never fails a turn.
//!
//! **Concurrency.** Within one app instance, an agent's writes are serialized
//! by its single dispatcher actor, so there is no intra-instance race. Two app
//! instances pointed at the *same* working directory (an unsupported setup —
//! supported multi-instance uses `DEV_PORT`-isolated directories) could race on
//! the read-modify-write cycle and lose an update (last writer wins). That is
//! accepted for a best-effort cache: the lost field self-heals on the next
//! event. No file locking — its cross-platform fragility and deadlock risk
//! would be a worse failure mode than the self-healing miss it prevents.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::{AgentId, ProjectId};

/// Current on-disk schema version. Bumped only on a breaking shape change;
/// an unrecognized version reads as empty (best-effort, forward-compatible).
const SCHEMA_VERSION: u32 = 1;

/// One persisted snapshot of a stream-only field, with the wall-clock time it
/// was captured. The `captured_at` drives the UI's "as of …" staleness
/// qualifier so a rehydrated snapshot isn't presented as live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitSnapshot {
    /// Opaque payload, exactly as received from the harness's
    /// `rate_limit_event` (Claude's `isUsingOverage` / `resetsAt` / etc.).
    pub payload: serde_json::Value,
    /// Wall-clock time this snapshot was captured (ISO-8601 UTC on disk).
    pub captured_at: DateTime<Utc>,
}

/// One persisted snapshot of the model's context-window size. Stream-only for
/// Claude (`result.modelUsage.<model>.contextWindow`), absent from the session
/// file, so it must be cached to let the context bar render on reopen.
///
/// `captured_at` is recorded for parity with `RateLimitSnapshot`. The snapshot
/// is reattached only to the exact assistant message that produced it: the same
/// model can have different effective capacity across capability/beta state,
/// so model identity alone is not a durable turn join.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextWindowSnapshot {
    /// The model's total context-window size in tokens.
    pub context_window: u32,
    /// Exact resolved `modelUsage` key that supplied the window. Legacy
    /// snapshots have no provenance and are deliberately not overlaid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Final non-subagent assistant message id for the turn that reported this
    /// window. Legacy snapshots have no join key and are deliberately not
    /// overlaid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Wall-clock time this snapshot was captured (ISO-8601 UTC on disk).
    pub captured_at: DateTime<Utc>,
}

/// The metadata sidecar file contents. Fields are optional so the schema can
/// grow additively (a new class-C field is a new `Option` field, not a
/// breaking change — `schema_version` is bumped only on a breaking shape
/// change, never for an additive field).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaSidecar {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<ContextWindowSnapshot>,
}

impl Default for MetaSidecar {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            rate_limit: None,
            context_window: None,
        }
    }
}

/// Canonical metadata-sidecar path:
/// `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.meta.json`.
/// Shares the per-agent `sessions/` directory with the registry's metadata
/// cache, but is a distinct filename and concern.
#[must_use]
pub fn meta_sidecar_path(directory: &Path, project_id: ProjectId, agent_id: AgentId) -> PathBuf {
    directory
        .join(".switchboard")
        .join("projects")
        .join(project_id.to_string())
        .join("sessions")
        .join(format!("{agent_id}.meta.json"))
}

/// Read the metadata sidecar. **Best-effort**: a missing file returns
/// `None`; a corrupt / unreadable / unrecognized-version file logs at `warn`
/// and returns `None`. Never errors — the sidecar is a UX improvement, not a
/// correctness dependency, so a damaged one must not block hydration.
#[must_use]
pub fn read(path: &Path) -> Option<MetaSidecar> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "metadata sidecar unreadable — treating as absent");
            return None;
        }
    };
    match serde_json::from_slice::<MetaSidecar>(&bytes) {
        Ok(sidecar) if sidecar.schema_version == SCHEMA_VERSION => Some(sidecar),
        Ok(sidecar) => {
            tracing::warn!(
                path = %path.display(),
                found = sidecar.schema_version,
                expected = SCHEMA_VERSION,
                "metadata sidecar schema version mismatch — treating as absent"
            );
            None
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "metadata sidecar corrupt — treating as absent");
            None
        }
    }
}

/// Errors raised by a metadata-sidecar write. Surfaced to the caller, which
/// logs and drops them (the cache is not load-bearing).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MetaSidecarError {
    #[error("metadata sidecar I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("metadata sidecar serialization failed: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
}

/// Persist the latest rate-limit snapshot (last-write-wins for that field).
/// Other fields are preserved (read-then-replace), so writing one field never
/// clobbers another's snapshot.
pub fn write_rate_limit(
    path: &Path,
    payload: serde_json::Value,
    captured_at: DateTime<Utc>,
) -> Result<(), MetaSidecarError> {
    let mut sidecar = read(path).unwrap_or_default();
    sidecar.schema_version = SCHEMA_VERSION;
    sidecar.rate_limit = Some(RateLimitSnapshot {
        payload,
        captured_at,
    });
    persist(path, &sidecar)
}

/// Persist the latest context-window snapshot (last-write-wins for that field).
/// Preserves the rate-limit snapshot alongside it — the two stream-only fields
/// are written from different events, so each writer must keep the other's
/// value.
pub fn write_context_window(
    path: &Path,
    context_window: u32,
    model: String,
    message_id: String,
    captured_at: DateTime<Utc>,
) -> Result<(), MetaSidecarError> {
    let mut sidecar = read(path).unwrap_or_default();
    sidecar.schema_version = SCHEMA_VERSION;
    sidecar.context_window = Some(ContextWindowSnapshot {
        context_window,
        model: Some(model),
        message_id: Some(message_id),
        captured_at,
    });
    persist(path, &sidecar)
}

/// Write the whole sidecar **atomically** via a sibling `.tmp` + `rename` so a
/// crash mid-write can't leave a torn file. The `.tmp` sits in the same
/// directory as the target, guaranteeing a same-filesystem (atomic) rename — a
/// cross-filesystem rename is not atomic and can fail. The fixed `.tmp` name is
/// deliberately self-limiting: at most one stale temp per sidecar, overwritten
/// on the next write and cleaned on a failed rename (a unique-per-write name
/// would instead leak an unbounded orphan on every crash between write and
/// rename, with nothing to reclaim it).
fn persist(path: &Path, sidecar: &MetaSidecar) -> Result<(), MetaSidecarError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MetaSidecarError::Io {
            path: parent.to_owned(),
            source: e,
        })?;
    }

    let json = serde_json::to_vec_pretty(sidecar)
        .map_err(|source| MetaSidecarError::Serialize { source })?;

    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json).map_err(|e| MetaSidecarError::Io {
        path: tmp_path.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        // Best-effort cleanup of the temp file so a failed rename doesn't leave
        // litter; ignore the cleanup result.
        let _ = std::fs::remove_file(&tmp_path);
        MetaSidecarError::Io {
            path: path.to_owned(),
            source: e,
        }
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

    #[test]
    fn meta_sidecar_path_matches_canonical_layout() {
        let directory = Path::new("/Users/x/workspace");
        let project_id = Uuid::nil();
        let agent_id = Uuid::nil();
        let path = meta_sidecar_path(directory, project_id, agent_id);
        let expected = PathBuf::from(format!(
            "/Users/x/workspace/.switchboard/projects/{project_id}/sessions/{agent_id}.meta.json"
        ));
        assert_eq!(path, expected);
    }

    #[test]
    fn read_missing_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(read(&tmp.path().join("absent.meta.json")).is_none());
    }

    #[test]
    fn read_corrupt_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        assert!(read(&path).is_none());
    }

    #[test]
    fn read_unrecognized_schema_version_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        std::fs::write(&path, r#"{"schema_version":999,"rate_limit":null}"#).unwrap();
        assert!(read(&path).is_none());
    }

    #[test]
    fn write_then_read_round_trips_the_snapshot() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        let payload = serde_json::json!({"isUsingOverage": true, "resetsAt": 1_778_701_800u64});
        let captured = ts("2026-05-27T18:42:11Z");
        write_rate_limit(&path, payload.clone(), captured).unwrap();

        let read_back = read(&path).expect("sidecar present after write");
        let rl = read_back.rate_limit.expect("rate_limit populated");
        assert_eq!(rl.payload, payload);
        assert_eq!(rl.captured_at, captured);
        assert_eq!(read_back.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn write_creates_parent_directory_chain() {
        // Deep path whose parents don't exist — write must create them.
        let tmp = TempDir::new().unwrap();
        let path = tmp
            .path()
            .join(".switchboard")
            .join("projects")
            .join(Uuid::now_v7().to_string())
            .join("sessions")
            .join(format!("{}.meta.json", Uuid::now_v7()));
        assert!(!path.parent().unwrap().exists());
        write_rate_limit(
            &path,
            serde_json::json!({"a": 1}),
            ts("2026-05-27T00:00:00Z"),
        )
        .unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn subsequent_writes_overwrite_last_write_wins() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        write_rate_limit(
            &path,
            serde_json::json!({"v": 1}),
            ts("2026-05-27T00:00:00Z"),
        )
        .unwrap();
        write_rate_limit(
            &path,
            serde_json::json!({"v": 2}),
            ts("2026-05-27T01:00:00Z"),
        )
        .unwrap();

        let rl = read(&path).unwrap().rate_limit.unwrap();
        assert_eq!(rl.payload, serde_json::json!({"v": 2}));
        assert_eq!(rl.captured_at, ts("2026-05-27T01:00:00Z"));
    }

    #[test]
    fn write_context_window_round_trips() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        let captured = ts("2026-05-31T12:00:00Z");
        write_context_window(
            &path,
            200_000,
            "claude-sonnet-5".to_owned(),
            "msg-final".to_owned(),
            captured,
        )
        .unwrap();

        let read_back = read(&path).expect("sidecar present after write");
        let cw = read_back.context_window.expect("context_window populated");
        assert_eq!(cw.context_window, 200_000);
        assert_eq!(cw.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(cw.message_id.as_deref(), Some("msg-final"));
        assert_eq!(cw.captured_at, captured);
        assert_eq!(read_back.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn writing_one_field_preserves_the_other() {
        // The two stream-only fields are written from different events; each
        // writer reads-then-replaces, so neither clobbers the other.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        write_rate_limit(
            &path,
            serde_json::json!({"isUsingOverage": true}),
            ts("2026-05-31T12:00:00Z"),
        )
        .unwrap();
        write_context_window(
            &path,
            1_000_000,
            "claude-sonnet-5".to_owned(),
            "msg-final".to_owned(),
            ts("2026-05-31T12:05:00Z"),
        )
        .unwrap();

        let read_back = read(&path).expect("sidecar present");
        assert_eq!(
            read_back.rate_limit.unwrap().payload,
            serde_json::json!({"isUsingOverage": true}),
            "writing context_window must preserve the rate-limit snapshot"
        );
        assert_eq!(read_back.context_window.unwrap().context_window, 1_000_000);
    }

    #[test]
    fn pre_existing_sidecar_without_context_window_reads_with_none() {
        // Backwards compatibility: a sidecar written before the
        // `context_window` field existed (rate_limit only) must still read —
        // the new field is additive and defaults to None, no schema bump, no
        // migration.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"rate_limit":{"payload":{"isUsingOverage":false},"captured_at":"2026-05-31T12:00:00Z"}}"#,
        )
        .unwrap();

        let read_back = read(&path).expect("legacy sidecar must still read");
        assert!(read_back.rate_limit.is_some(), "rate_limit preserved");
        assert!(
            read_back.context_window.is_none(),
            "absent context_window reads as None, not an error"
        );
    }

    #[test]
    fn legacy_context_window_without_model_provenance_still_reads() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"context_window":{"context_window":200000,"captured_at":"2026-05-31T12:00:00Z"}}"#,
        )
        .unwrap();

        let snapshot = read(&path)
            .expect("legacy sidecar remains parseable")
            .context_window
            .expect("legacy context window remains represented");
        assert_eq!(snapshot.context_window, 200_000);
        assert_eq!(snapshot.model, None);
        assert_eq!(snapshot.message_id, None);
    }

    #[test]
    fn a_pre_existing_tmp_file_does_not_corrupt_the_read() {
        // Simulate a crash that left a stale `.tmp` behind: the real file is
        // intact and a leftover `.tmp` must not be read in its place.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.meta.json");
        write_rate_limit(
            &path,
            serde_json::json!({"good": true}),
            ts("2026-05-27T00:00:00Z"),
        )
        .unwrap();
        // Stray temp file with garbage — must be ignored by `read`.
        std::fs::write(path.with_extension("json.tmp"), "torn{{{").unwrap();

        let rl = read(&path).expect("real file still readable despite stray .tmp");
        assert_eq!(
            rl.rate_limit.unwrap().payload,
            serde_json::json!({"good": true})
        );
    }
}
