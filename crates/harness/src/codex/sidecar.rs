//! Per-agent session-link sidecar for Codex agents.
//!
//! Codex assigns its own session id (the `thread_id` from `thread.started`)
//! on first dispatch. Switchboard records the mapping in an append-only
//! JSONL file at `<directory>/.switchboard/projects/<project-id>/sessions/
//! <agent_id>.jsonl`. This is the **M2.3→M2.4 contract**:
//! - `session_id` drives M2.4's session-file filename glob.
//! - `original_start_date_utc` drives M2.4's date-partition path lookup; it
//!   is set on the very first dispatch and copied verbatim on every resume
//!   (NEVER recomputed from `Utc::today()` — Codex appends to the original
//!   spawn-date's session file even on cross-day resumes per M2.1 findings).
//!
//! Each dispatch appends a new record. Latest-line-wins on resume lookups;
//! the full history is retained for debugging. Duplicate records are
//! intended.
//!
//! **Failure semantics.** Sidecar persistence is load-bearing for resume
//! (without it, the second turn would create a new session and lose
//! context) and for M2.4 enrichment (without `original_start_date_utc`, the
//! adapter doesn't know which date-partitioned directory to look in).
//! Silently swallowing a write error would create an unresumable agent, so
//! callers (the producer task in [`crate::codex::mod`]) **immediately
//! terminate the stream** with `TurnEnd(Failed{AdapterFailure})` on append
//! failure — no continued parsing past a missing sidecar record.
//!
//! **Crash-safety note.** Writes use `writeln!` + `flush()`, mirroring
//! `switchboard_core::io::append_jsonl` (`crates/core/src/io.rs`). Neither
//! call site issues `file.sync_data()` or fsyncs the parent directory — a
//! power loss between write and writeback can leave a torn line. M1.5
//! review flagged this gap workspace-wide; M4 owns the fix. **Whoever
//! hardens core's `append_jsonl` must also harden this helper** so the two
//! call sites don't drift apart.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::{AgentId, ProjectId};

/// One row of the per-agent session-link sidecar JSONL.
///
/// **Schema is the M2.3→M2.4 contract.** Renaming or restructuring these
/// fields requires coordinated M2.4 changes — M2.4 reads `session_id` (for
/// the filename glob) and `original_start_date_utc` (for the date-partition
/// directory) directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLinkRecord {
    /// Codex `thread_id` captured from the first stream event of the very
    /// first dispatch (or, for the attach-existing-session flow, parsed
    /// from the existing session file's path).
    pub session_id: String,
    /// The UTC calendar date on which the session was originally spawned.
    /// **Never recomputed.** On the first dispatch this is `Utc::today()`;
    /// on every subsequent resume it is copied verbatim from the prior
    /// record. Codex appends to the original spawn-date's session file
    /// regardless of resume date (per M2.1 findings).
    pub original_start_date_utc: NaiveDate,
    /// Wall-clock time this specific record was written. Distinct from
    /// `original_start_date_utc`: each dispatch gets a fresh `started_at`.
    pub started_at: DateTime<Utc>,
}

/// Errors raised by sidecar I/O. Distinct from `DispatchError` (which is
/// pre-stream) and `AdapterFailure` (post-stream synthesized event) — the
/// adapter glue maps these to the right surface per the call site.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SidecarError {
    #[error("sidecar I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sidecar corrupt at {path} line {line}: {source}")]
    Corrupt {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("sidecar record serialization failed: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
}

/// Compute the canonical sidecar path. Mirrors the plan-pinned layout:
/// `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.jsonl`.
#[must_use]
pub fn sidecar_path(directory: &Path, project_id: ProjectId, agent_id: AgentId) -> PathBuf {
    directory
        .join(".switchboard")
        .join("projects")
        .join(project_id.to_string())
        .join("sessions")
        .join(format!("{agent_id}.jsonl"))
}

/// Read the most-recent record from the sidecar (last non-empty line).
/// Returns `Ok(None)` if the file does not exist (the first-dispatch case —
/// there is no record to resume from). Corruption on any line returns
/// [`SidecarError::Corrupt`] with the line number, mirroring the
/// `CoreError::CorruptJsonl` convention in `crates/core/src/io.rs`.
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
/// missing (`std::fs::create_dir_all`). Parallel `writeln!` + `flush()`
/// pattern to `switchboard_core::io::append_jsonl`; see the crash-safety
/// note at the top of this module.
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
    use uuid::Uuid;

    fn fresh_record() -> SessionLinkRecord {
        SessionLinkRecord {
            session_id: Uuid::now_v7().to_string(),
            original_start_date_utc: Utc::now().date_naive(),
            started_at: Utc::now(),
        }
    }

    #[test]
    fn read_latest_on_nonexistent_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.jsonl");
        assert!(read_latest(&path).unwrap().is_none());
    }

    #[test]
    fn append_creates_parent_directory_chain() {
        // Parent path does NOT exist yet; the sidecar lives several levels
        // deep. append_record must create the entire chain — this guards
        // against the regression where `append_jsonl`-style helpers create
        // the file but not the directories above it.
        let tmp = TempDir::new().unwrap();
        let path = tmp
            .path()
            .join(".switchboard")
            .join("projects")
            .join(Uuid::now_v7().to_string())
            .join("sessions")
            .join(format!("{}.jsonl", Uuid::now_v7()));
        assert!(!path.parent().unwrap().exists());

        let record = fresh_record();
        append_record(&path, &record).expect("append should create parent + write");

        assert!(path.is_file(), "sidecar file created");
        let read = read_latest(&path).unwrap();
        assert_eq!(read.as_ref(), Some(&record));
    }

    #[test]
    fn read_latest_returns_last_appended_record() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sidecar.jsonl");

        let first = fresh_record();
        append_record(&path, &first).unwrap();
        let second = SessionLinkRecord {
            session_id: first.session_id.clone(),
            original_start_date_utc: first.original_start_date_utc,
            started_at: first.started_at + chrono::Duration::seconds(60),
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
        // Force append to fail by creating a *regular file* at the path
        // where `create_dir_all` would need to create a directory. POSIX
        // and Windows both reject this; create_dir_all surfaces an error.
        let tmp = TempDir::new().unwrap();
        let blocking_file = tmp.path().join("blocker");
        std::fs::write(&blocking_file, "i am a file").unwrap();
        // Path "blocker/sessions/agent.jsonl" — blocker is a file, so
        // create_dir_all("blocker/sessions") must fail.
        let target = blocking_file.join("sessions").join("agent.jsonl");

        let err = append_record(&target, &fresh_record())
            .expect_err("append must fail when parent cannot be created");
        match err {
            SidecarError::Io { .. } => {}
            other => panic!("expected SidecarError::Io, got {other:?}"),
        }
    }

    #[test]
    fn read_latest_corrupt_line_returns_corrupt_error_with_line_number() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sidecar.jsonl");
        let valid = fresh_record();
        // Line 1: valid; line 2: garbage. read_latest is supposed to flag
        // the corrupt line with its number — same convention as
        // CoreError::CorruptJsonl.
        let valid_json = serde_json::to_string(&valid).unwrap();
        std::fs::write(&path, format!("{valid_json}\n{{not valid json\n")).unwrap();

        match read_latest(&path) {
            Err(SidecarError::Corrupt { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected SidecarError::Corrupt(line=2), got {other:?}"),
        }
    }

    #[test]
    fn sidecar_path_matches_plan_canonical_layout() {
        let directory = std::path::Path::new("/Users/x/workspace");
        let project_id = Uuid::nil();
        let agent_id = Uuid::nil();
        let path = sidecar_path(directory, project_id, agent_id);
        let expected = std::path::PathBuf::from(format!(
            "/Users/x/workspace/.switchboard/projects/{project_id}/sessions/{agent_id}.jsonl"
        ));
        assert_eq!(path, expected);
    }

    #[test]
    fn session_link_record_wire_shape_is_stable() {
        // The schema is the M2.3→M2.4 contract. Pin the field names so a
        // future rename surfaces here, not as a silent M2.4 lookup failure.
        let record = SessionLinkRecord {
            session_id: "019e2c5f-aaaa-7000-8000-000000000001".to_owned(),
            original_start_date_utc: NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
            started_at: DateTime::parse_from_rfc3339("2026-05-15T12:30:45Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["session_id"], "019e2c5f-aaaa-7000-8000-000000000001");
        assert_eq!(json["original_start_date_utc"], "2026-05-15");
        assert!(
            json["started_at"]
                .as_str()
                .unwrap()
                .starts_with("2026-05-15T12:30:45")
        );
    }
}
