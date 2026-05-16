//! Typed errors for command free-functions. The Tauri `#[tauri::command]`
//! wrapper maps these to `String` at the IPC boundary (Tauri convention).

use std::path::PathBuf;

use switchboard_core::{AgentId, CoreError, HarnessKind, ProjectId};
use switchboard_dispatcher::DispatcherError;
use switchboard_harness::DispatchError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] CoreError),

    #[error(transparent)]
    Dispatcher(#[from] DispatcherError),

    /// Pre-dispatch harness check failed (e.g., binary not on PATH).
    /// Distinct from `Dispatcher` because the call site is the adapter's
    /// `probe()` method — not dispatcher business — and the frontend gates
    /// behaviour on it (`check_claude_binary` banner) independently of any
    /// dispatch attempt.
    #[error("harness probe failed: {0}")]
    Probe(DispatchError),

    #[error("no working directory has been initialised — call init_directory first")]
    NoDirectory,

    #[error("no active project — call set_active_project first")]
    NoActiveProject,

    #[error("project {0} is not loaded")]
    ProjectNotLoaded(ProjectId),

    #[error("agent {0} not found in any loaded project")]
    AgentNotFound(AgentId),

    #[error("invalid UUID {value:?}: {source}")]
    InvalidUuid {
        value: String,
        #[source]
        source: uuid::Error,
    },

    /// `HarnessKind` is `#[non_exhaustive]`. If a future variant lands but
    /// `AppState` hasn't been extended with an adapter for it, `send_message`
    /// surfaces this rather than silently dispatching to the wrong adapter
    /// or panicking. Should be unreachable in well-maintained code; tracks
    /// "did we forget to wire a new harness?" as a typed error.
    #[error("unsupported harness kind: app has no adapter wired for this variant")]
    UnsupportedHarness,

    /// Attach-flow: the user supplied a `session_id` that has no
    /// corresponding session file on disk under the appropriate harness
    /// directory. Surfaces the expected path so the user can verify the
    /// session-id and the encoded-cwd against what the harness actually
    /// created.
    #[error("{harness:?} session file for session_id not found; expected at {expected_path}")]
    SessionFileNotFound {
        harness: HarnessKind,
        expected_path: String,
    },

    /// Attach-flow: a different Switchboard agent (possibly in a different
    /// project under the same directory) is already bound to the supplied
    /// `session_id`. Two `AgentRecord`s pointing at the same harness session
    /// could each pass the dispatcher's per-agent contention check and
    /// dispatch concurrently — corrupting the harness session per
    /// `docs/research/same-session-parallel-invocation.md`. Reject at
    /// registration time.
    #[error(
        "session is already attached to agent {existing_agent_name:?} in project {existing_project_name:?}"
    )]
    SessionAlreadyAttached {
        existing_agent_id: AgentId,
        existing_agent_name: String,
        existing_project_id: ProjectId,
        existing_project_name: String,
    },

    /// Attach-flow (Codex only): the date-partition scan turned up more
    /// than one `rollout-*-<session_id>.jsonl` file. Codex session UUIDs
    /// are unique by construction, so this implies an external anomaly
    /// (manual copy, FS corruption). Surfacing the candidate list lets
    /// the user investigate rather than binding arbitrarily.
    #[error(
        "ambiguous Codex session file for session_id {session_id}: {} candidates",
        paths.len()
    )]
    AmbiguousSessionFile {
        session_id: String,
        paths: Vec<PathBuf>,
    },

    /// Attach-flow (Codex only): registering the `AgentRecord` succeeded but
    /// writing the per-agent session-link sidecar failed. The
    /// `AgentRecord` is already in the registry; until the user retries the
    /// attach (or the sidecar appears via some other path), this Codex
    /// agent will look like a fresh-spawn to the adapter.
    #[error(transparent)]
    Sidecar(#[from] switchboard_harness::codex::sidecar::SidecarError),

    /// Attach-flow: the cross-project collision scan tripped over an
    /// unrelated agent's corrupt sidecar. Per the Switchboard-owned-JSONL
    /// loud-fail invariant in `AGENTS.md`, corruption must surface rather
    /// than be skipped (which could let a duplicate attach through and
    /// violate the same-session-uniqueness contract). Wrapped instead of
    /// re-using `Sidecar` so the user-facing message can call out that
    /// the failure is about a *different* agent's state, not the session
    /// they're trying to attach.
    #[error(
        "cannot scan for session collisions: sidecar at {path} is corrupt — repair or remove before retrying attach"
    )]
    AttachBlockedByCorruption {
        path: PathBuf,
        #[source]
        source: switchboard_harness::codex::sidecar::SidecarError,
    },

    /// Attach-flow: catch-all for future `AttachLookupError` variants we
    /// don't yet know how to surface specifically. The Codex date-scan
    /// helper is `#[non_exhaustive]`; this lands a useful message rather
    /// than silently miscategorizing a new variant as one of the existing
    /// `SessionFileNotFound` / `AmbiguousSessionFile` shapes.
    #[error("attach session lookup failed: {message}")]
    AttachLookupFailed { message: String },
}

impl AppError {
    pub fn invalid_uuid(value: impl Into<String>, source: uuid::Error) -> Self {
        Self::InvalidUuid {
            value: value.into(),
            source,
        }
    }
}
