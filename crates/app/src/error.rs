//! Typed errors for command free-functions. The Tauri `#[tauri::command]`
//! wrapper maps these to `String` at the IPC boundary (Tauri convention).

use std::path::PathBuf;

use switchboard_core::{AgentId, CoreError, HarnessKind, ProjectId};
use switchboard_harness::{DispatchError, MessageId};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] CoreError),

    /// The agent was busy and the caller requested fail-fast (workflow §7).
    /// Not reachable on the compose-bar `Enqueue` path (which queues); kept for
    /// the future workflow-step dispatch path.
    #[error("agent is busy")]
    AgentBusy,

    /// `remove_queued_message` targeted an id that is no longer enqueued —
    /// already dequeued/started, already removed, or never existed. The
    /// frontend uses this to avoid fake-restoring composer text for a message
    /// that is already running.
    #[error("queued message {0} not found (already started or removed)")]
    QueuedMessageNotFound(MessageId),

    /// Pre-dispatch harness check failed (e.g., binary not on PATH).
    /// Distinct because the call site is the adapter's `probe()` method, and
    /// the frontend gates behaviour on it (`check_claude_binary` banner)
    /// independently of any dispatch attempt.
    #[error("harness probe failed: {0}")]
    Probe(DispatchError),

    #[error("no working directory has been initialised — call init_directory first")]
    NoDirectory,

    /// Persisting the user-global workspace registry (`workspace.yaml`) failed.
    /// The registry is convenience state (the cross-directory project list and
    /// its cached snapshot), so callers treat this as best-effort and log
    /// rather than abort — but `save` still surfaces it for the rare caller
    /// that wants to react.
    // Constructed only by `workspace::save`, whose production callers land in
    // the next M4.6 increment; tests exercise it today.
    #[allow(dead_code)]
    #[error("failed to persist workspace registry at {path}: {source}")]
    WorkspacePersist {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no active project — call set_active_project first")]
    NoActiveProject,

    #[error("project {0} is not loaded")]
    ProjectNotLoaded(ProjectId),

    /// The project's `instance.lock` is held by another Switchboard process.
    /// Inter-process guard (M4.1): one Switchboard process per project. The
    /// frontend surfaces this as "This project is already open in another
    /// Switchboard window."
    #[error("project {0} is already open in another Switchboard process")]
    ProjectLocked(ProjectId),

    /// Failed to open or `flock` a project's `instance.lock` for a reason
    /// other than contention (e.g. the metadata directory is unwritable).
    /// Distinct from `ProjectLocked`, which means "another process holds it."
    #[error("failed to acquire instance lock for project {project_id}: {source}")]
    ProjectLockIo {
        project_id: ProjectId,
        #[source]
        source: std::io::Error,
    },

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
    #[error("{harness} session file for session_id not found; expected at {expected_path}")]
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
        "ambiguous {harness} session file for session_id {session_id}: {} candidates",
        paths.len()
    )]
    AmbiguousSessionFile {
        harness: HarnessKind,
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

    /// Attach-flow (Antigravity): the per-agent session-link sidecar write
    /// failed during attach. Same consequence as the Codex case — the
    /// attached agent would look like a fresh-spawn (new server conversation)
    /// on its first dispatch instead of resuming the attached one.
    #[error(transparent)]
    AntigravitySidecar(#[from] switchboard_harness::antigravity::sidecar::SidecarError),

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
        // Boxed because the collision scan reads Codex *or* Antigravity
        // sidecars, whose error types differ.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// Attach-flow: catch-all for future `AttachLookupError` variants we
    /// don't yet know how to surface specifically. The Codex date-scan
    /// helper is `#[non_exhaustive]`; this lands a useful message rather
    /// than silently miscategorizing a new variant as one of the existing
    /// `SessionFileNotFound` / `AmbiguousSessionFile` shapes.
    #[error("attach session lookup failed: {message}")]
    AttachLookupFailed { message: String },

    /// Subscription auth not detected for the harness. Best-effort
    /// file-presence signal — see `check_codex_auth_impl` docstring for
    /// the known false-positive case (stale `auth.json` with API-key-only
    /// runtime config). Banner copy is actionable ("run `codex login`")
    /// rather than diagnostic.
    #[error("{harness} subscription auth not detected; expected at {expected_path}")]
    AuthNotConfigured {
        harness: HarnessKind,
        expected_path: String,
    },

    /// Transcript hydration failed at the lookup-mechanism level (I/O on a
    /// file that exists, registry lookup failure). Per-line parse damage
    /// degrades silently to warnings inside `LoadedTranscript` — it does
    /// not surface here.
    #[error(transparent)]
    LoadTranscript(#[from] switchboard_harness::LoadTranscriptError),

    /// Transcript hydration tripped over a corrupt per-agent sidecar (Codex or
    /// Antigravity — both store the harness session link as Switchboard-owned
    /// JSONL). Parallel to [`AppError::AttachBlockedByCorruption`] but specific
    /// to the hydration call path. Per the AGENTS.md fail-loud invariant on our
    /// own files, corruption must surface rather than degrade to "agent has no
    /// history." Source is boxed because the two harnesses' sidecar error types
    /// are distinct.
    #[error(
        "cannot hydrate transcript: sidecar at {path} is corrupt — repair or remove before retrying"
    )]
    HydrationBlockedByCorruption {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

impl AppError {
    pub fn invalid_uuid(value: impl Into<String>, source: uuid::Error) -> Self {
        Self::InvalidUuid {
            value: value.into(),
            source,
        }
    }
}
