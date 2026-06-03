//! Per-agent **metadata sidecar** — a small, harness-agnostic cache of
//! stream-only metadata so it survives an app restart.
//!
//! Some harness metadata lives *only* on the live event stream and has no
//! on-disk equivalent in the harness's own session file — "class C" in
//! `docs/research/harness-behavior.md` §3.1. Today that's Claude's
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
//! whole file with the latest snapshot. Correlating snapshots to individual
//! messages (per-`send_id`) was considered and deferred — if ever needed it
//! lands additively in the journal at turn-start, not here.
//!
//! **Best-effort, not load-bearing.** A missing file reads as empty; a
//! corrupt file logs and reads as empty (never fails hydration). Writes that
//! fail are logged and dropped by the caller — the sidecar is a UX
//! improvement (restart continuity), not a correctness dependency, so unlike
//! the registry-resident session locator it never fails a turn.

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

/// The metadata sidecar file contents. Fields are optional so the schema can
/// grow additively (a future class-C field is a new `Option` field, not a
/// breaking change). Today only `rate_limit` is persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaSidecar {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitSnapshot>,
}

impl Default for MetaSidecar {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            rate_limit: None,
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

/// Persist the latest rate-limit snapshot (last-write-wins). Reads the
/// existing sidecar first (to preserve other fields when the schema grows),
/// replaces `rate_limit`, and writes the whole file **atomically** via a
/// sibling `.tmp` + `rename` so a crash mid-write can't leave a torn file.
///
/// The `.tmp` is created in the same `sessions/` directory as the target so
/// the `rename` is a same-filesystem atomic replace (a cross-filesystem
/// rename is not atomic and can fail).
pub fn write_rate_limit(
    path: &Path,
    payload: serde_json::Value,
    captured_at: DateTime<Utc>,
) -> Result<(), MetaSidecarError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MetaSidecarError::Io {
            path: parent.to_owned(),
            source: e,
        })?;
    }
    // Preserve any other fields a future schema adds; today there are none,
    // but reading-then-replacing keeps the write forward-compatible.
    let mut sidecar = read(path).unwrap_or_default();
    sidecar.schema_version = SCHEMA_VERSION;
    sidecar.rate_limit = Some(RateLimitSnapshot {
        payload,
        captured_at,
    });

    let json = serde_json::to_vec_pretty(&sidecar)
        .map_err(|source| MetaSidecarError::Serialize { source })?;

    // Atomic replace: write a sibling temp file, then rename over the target.
    // `<target>.tmp` sits in the same directory, guaranteeing a same-filesystem
    // rename.
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
