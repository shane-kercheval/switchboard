//! Per-agent session-link sidecar for Antigravity agents.
//!
//! **Legacy / migration-only.** Session identity now lives on the agent's
//! registry record (`AgentRecord.session_locator`); the adapter no longer reads
//! or writes this sidecar, and no new `.antigravity.jsonl` is ever created. This
//! module is retained solely so the one-time migration pass can fold any
//! pre-existing sidecar into the registry, after which it is deleted. Do not
//! wire new code to it.
//!
//! Antigravity assigns the conversation UUID server-side; the adapter
//! captures it post-spawn (by watching for a new
//! `~/.gemini/antigravity-cli/brain/<uuid>/` directory). Subsequent dispatches
//! resume against the captured UUID via `agy -p --conversation <uuid>`.
//!
//! **Schema deliberately does not store the transcript file path.** The
//! transcript path is derivable from `conversation_id` via the constants
//! in [`crate::antigravity::paths`]. Storing a derived value would pin the
//! `.system_generated/` layout at write time; the path-constant approach
//! lets every stored sidecar self-heal if Google relocates the transcript
//! between Antigravity versions — flip one line in `paths.rs`, no
//! migration.
//!
//! Filename convention: `<agent_id>.antigravity.jsonl` under
//! `<directory>/.switchboard/projects/<project-id>/sessions/`. Codex
//! sidecars live at `<agent_id>.jsonl` (no suffix) — legacy convention
//! pre-dating multi-harness sidecars. Any future harness with a sidecar
//! should follow the `.<harness>.jsonl` suffix convention.
//!
//! Mirrors Codex's sidecar (`crate::codex::sidecar`) in:
//! - JSONL append-only format with latest-line-wins on read.
//! - `writeln!` + `flush()` write pattern (no fsync; same workspace-wide
//!   crash-safety gap).
//! - `SidecarError::{Io, Corrupt, Serialize}` shape.
//! - Fail-loud on corruption (Switchboard-owned JSONL invariant from
//!   AGENTS.md).
//!
//! Each dispatch appends a new record; duplicates are intended (debug
//! signal: "which UUIDs has this agent captured across its lifetime").

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::{AgentId, ProjectId};
use uuid::Uuid;

/// One row of the per-agent Antigravity sidecar JSONL.
///
/// **Schema is load-bearing for resume and hydration.** Renaming or
/// restructuring these fields requires coordinated changes downstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLinkRecord {
    /// Server-assigned Antigravity conversation UUID. Captured by watching
    /// for a new `~/.gemini/antigravity-cli/brain/<uuid>/` directory after
    /// `agy -p` spawn.
    pub conversation_id: Uuid,
    /// Wall-clock UTC time this record was written. Each dispatch appends
    /// a fresh record; on resume this is the resume time, not the original
    /// capture time. UI transcript ordering uses event/turn timestamps,
    /// not this field.
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SidecarError {
    #[error("antigravity sidecar I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("antigravity sidecar corrupt at {path} line {line}: {source}")]
    Corrupt {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("antigravity sidecar record serialization failed: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
}

/// Compute the canonical sidecar path:
/// `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.antigravity.jsonl`.
///
/// The `.antigravity` suffix disambiguates from the Codex sidecar at
/// `<agent-id>.jsonl`. An agent is bound to exactly one harness, so a
/// given `agent_id` will only ever have one of the two file shapes — but
/// the explicit suffix keeps schemas harness-owned and avoids touching
/// Codex's code.
#[must_use]
pub fn sidecar_path(directory: &Path, project_id: ProjectId, agent_id: AgentId) -> PathBuf {
    directory
        .join(".switchboard")
        .join("projects")
        .join(project_id.to_string())
        .join("sessions")
        .join(format!("{agent_id}.antigravity.jsonl"))
}

/// Read the most-recent record from the sidecar (last non-empty line).
/// Returns `Ok(None)` if the file does not exist (the not-yet-dispatched
/// case — there is no record to resume from). Mirrors
/// [`crate::codex::sidecar::read_latest`].
pub fn read_latest(path: &Path) -> Result<Option<SessionLinkRecord>, SidecarError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(SidecarError::Io {
                path: path.to_owned(),
                source: e,
            });
        }
    };
    let reader = BufReader::new(file);
    let mut latest: Option<SessionLinkRecord> = None;
    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| SidecarError::Io {
            path: path.to_owned(),
            source: e,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let record: SessionLinkRecord =
            serde_json::from_str(&line).map_err(|e| SidecarError::Corrupt {
                path: path.to_owned(),
                line: idx + 1,
                source: e,
            })?;
        latest = Some(record);
    }
    Ok(latest)
}

/// Append a record to the sidecar. Creates the parent directory chain if
/// missing. Mirrors [`crate::codex::sidecar::append_record`].
pub fn append_record(path: &Path, record: &SessionLinkRecord) -> Result<(), SidecarError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SidecarError::Io {
            path: parent.to_owned(),
            source: e,
        })?;
    }
    let line =
        serde_json::to_string(record).map_err(|source| SidecarError::Serialize { source })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| SidecarError::Io {
            path: path.to_owned(),
            source: e,
        })?;
    writeln!(file, "{line}").map_err(|e| SidecarError::Io {
        path: path.to_owned(),
        source: e,
    })?;
    file.flush().map_err(|e| SidecarError::Io {
        path: path.to_owned(),
        source: e,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_record() -> SessionLinkRecord {
        SessionLinkRecord {
            conversation_id: Uuid::new_v4(),
            captured_at: Utc::now(),
        }
    }

    #[test]
    fn read_latest_on_nonexistent_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.antigravity.jsonl");
        assert!(read_latest(&path).unwrap().is_none());
    }

    #[test]
    fn append_creates_parent_directory_chain() {
        // Sidecar lives several levels deep; append_record must create the
        // full chain. Guards against the regression where a JSONL helper
        // creates the file but not the directories above it.
        let tmp = TempDir::new().unwrap();
        let path = tmp
            .path()
            .join(".switchboard")
            .join("projects")
            .join(Uuid::now_v7().to_string())
            .join("sessions")
            .join(format!("{}.antigravity.jsonl", Uuid::now_v7()));
        assert!(!path.parent().unwrap().exists());

        let record = fresh_record();
        append_record(&path, &record).expect("append should create parent + write");

        assert!(path.is_file());
        let read = read_latest(&path).unwrap();
        assert_eq!(read.as_ref(), Some(&record));
    }

    #[test]
    fn read_latest_returns_last_appended_record() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sidecar.antigravity.jsonl");

        let first = fresh_record();
        append_record(&path, &first).unwrap();
        let second = SessionLinkRecord {
            conversation_id: first.conversation_id,
            captured_at: first.captured_at + chrono::Duration::seconds(60),
        };
        append_record(&path, &second).unwrap();

        let latest = read_latest(&path).unwrap();
        assert_eq!(
            latest.as_ref(),
            Some(&second),
            "latest-line-wins: the second append must be the read result"
        );
    }

    #[test]
    fn append_to_unwritable_parent_returns_io_error() {
        // Force append to fail by creating a regular file where
        // create_dir_all would need to create a directory.
        let tmp = TempDir::new().unwrap();
        let blocking_file = tmp.path().join("blocker");
        std::fs::write(&blocking_file, "i am a file").unwrap();
        let target = blocking_file
            .join("sessions")
            .join("agent.antigravity.jsonl");

        let err = append_record(&target, &fresh_record())
            .expect_err("append must fail when parent cannot be created");
        match err {
            SidecarError::Io { .. } => {}
            other => panic!("expected SidecarError::Io, got {other:?}"),
        }
    }

    #[test]
    fn read_latest_corrupt_line_returns_corrupt_error_with_line_number() {
        // Switchboard-owned JSONL invariant: corruption must surface
        // loud, not degrade silently. Mirrors Codex's sidecar test.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sidecar.antigravity.jsonl");
        let valid = fresh_record();
        let valid_json = serde_json::to_string(&valid).unwrap();
        std::fs::write(&path, format!("{valid_json}\n{{not valid json\n")).unwrap();

        match read_latest(&path) {
            Err(SidecarError::Corrupt { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected SidecarError::Corrupt(line=2), got {other:?}"),
        }
    }

    #[test]
    fn sidecar_path_uses_antigravity_suffix() {
        // Pins the filename convention: <agent_id>.antigravity.jsonl
        // (disambiguating from Codex's <agent_id>.jsonl at the same dir).
        let directory = Path::new("/dir");
        let project_id = Uuid::nil();
        let agent_id = Uuid::nil();
        let path = sidecar_path(directory, project_id, agent_id);
        let expected = PathBuf::from(format!(
            "/dir/.switchboard/projects/{project_id}/sessions/{agent_id}.antigravity.jsonl"
        ));
        assert_eq!(path, expected);
    }

    #[test]
    fn session_link_record_wire_shape_is_stable() {
        // Pin field names so a future rename surfaces here, not as a
        // silent resume / hydration lookup failure.
        let record = SessionLinkRecord {
            conversation_id: Uuid::parse_str("01234567-89ab-4def-8123-456789abcdef").unwrap(),
            captured_at: DateTime::parse_from_rfc3339("2026-05-19T12:30:45Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            json["conversation_id"],
            "01234567-89ab-4def-8123-456789abcdef"
        );
        assert!(
            json["captured_at"]
                .as_str()
                .unwrap()
                .starts_with("2026-05-19T12:30:45")
        );
        // Negative: transcript_path is deliberately absent.
        assert!(
            json.get("transcript_path").is_none(),
            "transcript_path must NOT be in the wire shape — it is recomputed from conversation_id"
        );
    }
}
