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

    #[error(transparent)]
    Prompt(#[from] switchboard_prompts::PromptError),

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

    /// A manual forward was requested with no sources — there is nothing to
    /// forward. Rejected at the command boundary.
    #[error("a forward must reference at least one source agent")]
    NoForwardSources,

    /// Pre-dispatch harness check failed (e.g., binary not on PATH).
    /// Distinct because the call site is the adapter's `probe()` method, and
    /// the frontend gates behaviour on it (`check_claude_binary` banner)
    /// independently of any dispatch attempt.
    #[error("harness probe failed: {0}")]
    Probe(DispatchError),

    /// Persisting the user-global workspace registry (`workspace.yaml`) failed.
    /// The registry is convenience state (the cross-directory project list and
    /// its cached snapshot), so callers treat this as best-effort and log
    /// rather than abort — but `save` still surfaces it for the rare caller
    /// that wants to react.
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

    /// The project is mid-lifecycle-operation (a re-point or a delete has
    /// evicted its routable state and is rebuilding it). Distinct from
    /// [`Self::ProjectNotLoaded`]: the project exists and will be usable again
    /// shortly, so the caller should retry rather than treat it as gone.
    #[error("project {0} is being repaired — try again in a moment")]
    ProjectUnderMaintenance(ProjectId),

    /// A turn was prepared against a view of the project that a lifecycle
    /// operation has since invalidated — the working directory moved, or the
    /// project was deleted, between resolving it and starting the turn.
    ///
    /// **Its own variant because both neighbours misdescribe it.**
    /// [`Self::ProjectUnderMaintenance`] says a repair is in progress, which may
    /// have finished; [`Self::ProjectNotLoaded`] says the project is absent, which
    /// it may not be. Both were returned for this condition, so the user was told
    /// one of two untrue things depending on which gate caught it. Collapsing a
    /// distinct condition onto a neighbouring error at the point of use is the
    /// habit this variant exists to stop — do not fold it back.
    ///
    /// The message avoids "resend": a workflow turn has no sender to retry it,
    /// and a deleted project cannot be retried at all.
    #[error(
        "project {0} changed before the turn could start — reload the project and try again if it still exists"
    )]
    ProjectViewStale(ProjectId),

    /// One `AgentId` is claimed by two projects' registries.
    ///
    /// Cannot happen from ordinary use — agent ids are minted fresh and an
    /// agent lives in exactly one registry. It becomes reachable once agents can
    /// be *moved* between projects, where an interrupted move leaves the record
    /// in both the source and the target. Surfacing it here is what makes that
    /// anomaly visible and fixable instead of silent: the id cache used to
    /// insert last-wins, so whichever registry was read second quietly won and
    /// the agent dispatched into a project the user wasn't looking at.
    #[error(
        "agent {agent_id} is claimed by two projects ({existing_project_id} and {incoming_project_id}) — its registry record exists in both"
    )]
    AgentProjectConflict {
        agent_id: AgentId,
        existing_project_id: ProjectId,
        incoming_project_id: ProjectId,
    },

    /// The project's `instance.lock` is held by another Switchboard process.
    /// Inter-process guard: one Switchboard process per project. The
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

    /// The advisory lock on this turn's harness session file is already held.
    ///
    /// **The message names both causes, because the lock cannot tell them
    /// apart.** The one it exists for is another Switchboard instance — a dev
    /// build and the installed app have separate stores, so nothing else stops
    /// them driving one session. But an unmaterialized branch holds its
    /// *parent's* key for its whole first turn, so the same refusal reaches a
    /// user who sent to that parent from this window. `flock` reports contention,
    /// not who holds it, so claiming "another instance" would be a guess that is
    /// sometimes simply false — and the two causes have the same fix anyway.
    ///
    /// Phrased for the user: a refused turn surfaces this string verbatim as the
    /// dispatch failure.
    #[error(
        "this conversation is already in use by a running turn — in another Switchboard window, or by a branch being created from it here. Wait for that turn to finish, then resend."
    )]
    SessionInUse,

    /// Failed to open or `flock` a session lock for a reason other than
    /// contention (e.g. the lock directory is unwritable). Refuses the turn
    /// exactly as contention does — see `session_lock::acquire_session_locks`
    /// for why this is fail-closed rather than warn-and-proceed.
    #[error("failed to acquire the session lock for this turn: {source}")]
    SessionLockIo {
        #[source]
        source: std::io::Error,
    },

    #[error("agent {0} not found in any loaded project")]
    AgentNotFound(AgentId),

    /// A forward source resolved to a project the journal read pass never
    /// covered — the read set and the resolve set diverged. Distinct from
    /// `ProjectNotLoaded` on purpose: the project *is* loaded, so "open it"
    /// is the wrong recovery, and the honest diagnosis is an internal invariant
    /// break in the forward resolver rather than anything the user can act on.
    #[error(
        "internal: forward source resolved to project {0}, which the journal read \
         pass did not cover — the read set and resolve set have diverged"
    )]
    ForwardJournalMissing(ProjectId),

    /// A forward source named a project that does not own the agent. Reachable
    /// from a persisted draft whose agent moved between projects, or was removed
    /// and re-created elsewhere. Refused rather than resolved against whichever
    /// project happens to own it now: the user picked a specific agent in a
    /// specific project, and silently substituting a different context is the
    /// stale-forward class this whole path exists to prevent.
    #[error(
        "forward source {agent_id} no longer belongs to the project it was picked from \
         (declared {declared}, now in {actual}) — remove the chip and pick it again"
    )]
    ForwardSourceMoved {
        agent_id: AgentId,
        declared: ProjectId,
        actual: ProjectId,
    },

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

    /// Interactive resume cannot start while Switchboard still owns work for
    /// the same harness session; two writers can corrupt its transcript.
    #[error(
        "{name} is still active in Switchboard — wait for it to finish or stop it before resuming in a terminal"
    )]
    ResumeAgentBusy { name: String },

    #[error("this agent has no resumable session yet")]
    ResumeUnavailable,

    #[error(
        "unsupported terminal app {terminal_app:?} — choose Terminal or iTerm in Settings → External apps"
    )]
    UnsupportedTerminalApp { terminal_app: String },

    /// Fork: the source agent has no session file to branch from yet — it has
    /// never completed (or even started) a turn, so `--resume` would target a
    /// session that doesn't exist. Distinct from the *capability* rejection in
    /// `CoreError::SessionForkUnsupported`: this harness can fork, this agent
    /// just has nothing to fork from. Deliberately worded as "no session,"
    /// not "no completed turn" — a cancelled first turn also leaves a
    /// resumable file, and branching that is well-defined.
    #[error("{name} has no session to branch from yet — send it a message first")]
    ForkSourceHasNoSession { name: String },

    /// Fork: the source agent is running a turn, so a branch taken now would
    /// inherit its in-flight prompt with a synthesized "No response requested."
    /// placeholder instead of the real answer (harness-behavior §3.5, Probe A).
    /// Refused rather than queued — waiting would deliver the answer to the
    /// very turn the user is branching away from.
    #[error(
        "{name} is working — a branch taken now would not include its current answer; \
         wait for it to finish, or cancel it first"
    )]
    ForkSourceBusy { name: String },

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

    /// Project-file search could not walk the loaded project tree. The
    /// frontend degrades this to "File search unavailable" instead of showing
    /// an empty result set that looks authoritative.
    #[error("failed to search project files under {root}: {source}")]
    ProjectFileSearch {
        root: PathBuf,
        #[source]
        source: ignore::Error,
    },

    /// Git-view "Add Repo": the chosen path isn't inside any git repository, so
    /// it can't be resolved to a repo root to track. The frontend surfaces this
    /// inline ("not a git repository") on the add affordance.
    #[error("{path} is not inside a git repository")]
    NotAGitRepo { path: String },

    /// Persisting the Git-view tracked-repo registry (`git-view.yaml`) failed.
    /// Best-effort like [`WorkspacePersist`](Self::WorkspacePersist) — callers log
    /// rather than abort — but a distinct variant so diagnostics name the right
    /// file instead of mislabeling it as the workspace registry.
    #[error("failed to persist git-view registry at {path}: {source}")]
    GitRegistryPersist {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Persisting user preferences (`config.yaml`) failed. Unlike the registries
    /// this surfaces to the caller (the `set_preferences` command), since a
    /// failed explicit save is something the user just asked for and should hear
    /// about — not silent best-effort state.
    #[error("failed to persist preferences at {path}: {source}")]
    PreferencesPersist {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `git fetch` for a tracked repo failed (no network, no remote, auth, or
    /// `git` not on PATH). Carries git's stderr verbatim. Best-effort at the call
    /// site — the Git view records a "fetch failed" state and degrades to a stale
    /// read rather than treating this as fatal.
    #[error("git fetch failed for {root}: {message}")]
    GitFetch { root: String, message: String },

    /// `fetch_repo` was asked to fetch a path that doesn't resolve to a tracked
    /// repo root — a stale frontend reference, or a fetch racing a remove. Refuse
    /// rather than shell `git fetch` against an arbitrary caller-supplied path:
    /// fetch is the one Git-view command that runs a subprocess, so it only acts
    /// on roots the user has explicitly tracked.
    #[error("{root} is not a tracked repository")]
    RepoNotTracked { root: String },

    /// `git difftool` for a tracked repo/worktree failed. Carries git's stderr
    /// verbatim so the frontend can surface setup problems from the user's Git
    /// difftool configuration.
    #[error("git difftool failed for {root}: {message}")]
    GitDifftool { root: String, message: String },

    /// A genuine mid-read failure reading a worktree's changed files or a file's
    /// diff (corrupt object, I/O fault) — distinct from the non-error empty
    /// results those reads return for a clean or unreadable path. Carries git's
    /// message so the diff panel can surface why it couldn't load.
    #[error("failed to read git changes for {path}: {message}")]
    GitRead { path: String, message: String },

    /// A branch-comparison follow-up no longer matches the repository/HEAD
    /// endpoint that produced its file list. Distinct from an empty file diff.
    #[error("Branch comparison is out of date. Refresh the Git view and try again.")]
    BranchComparisonStale,

    /// Staging a dropped attachment failed — creating the project's
    /// `attachments/` dir, or copying the source file into it (the dropped file
    /// is gone, unreadable, or the metadata dir is unwritable). Carries the
    /// source path so the user can see which drop failed.
    #[error("failed to stage attachment from {source_path}: {source}")]
    AttachmentStage {
        source_path: String,
        #[source]
        source: std::io::Error,
    },

    /// A workflow-language error (parse, invocation-validation, or binding) from
    /// the pure crate, surfaced at the command boundary.
    #[error(transparent)]
    Workflow(#[from] switchboard_workflow::WorkflowError),

    /// No workflow with this name was found (built-in or in the user folder).
    #[error("workflow {name:?} not found")]
    WorkflowNotFound { name: String },

    /// No user-global workflows directory is resolvable (no config dir — an
    /// exotic host with no home).
    #[error("workflows are not available (no config directory)")]
    WorkflowsDirUnavailable,

    /// The workflow uses a step type that is not runnable in this version
    /// (`pause_for_user` / `for_each`) — gated at invoke, not a parse failure.
    #[error("step type not supported in this version")]
    WorkflowStepUnsupported,

    /// No workflow run with this id is active or on disk for the project.
    #[error("workflow run {run_id} not found")]
    WorkflowRunNotFound { run_id: uuid::Uuid },

    /// A workflow is already running in this project. Only one workflow may run
    /// per project at a time — the live progress view and compose lockout assume a
    /// single run. Enforced atomically at registry insertion so concurrent invokes
    /// can't both start.
    #[error("a workflow is already running in this project")]
    WorkflowAlreadyRunning { project_id: ProjectId },

    /// A failed or interrupted workflow run is held in this project, waiting to be
    /// dismissed. A held run occupies the project the same way a live one does (it
    /// replaces the compose box until dismissed), so a new run can't start until it
    /// is abandoned. Distinct from `WorkflowAlreadyRunning` so the UI can prompt
    /// "dismiss the failed workflow first" rather than "wait for the running one".
    #[error("a failed or interrupted workflow run must be dismissed before starting another")]
    WorkflowRunRequiresDismissal { project_id: ProjectId },

    /// Abandon was asked to clear a run that is still live. The interpreter would
    /// just recreate the file on its next record; cancel it first.
    #[error("workflow run {run_id} is still active; cancel it before abandoning")]
    WorkflowRunActive { run_id: uuid::Uuid },

    /// "Copy to my workflows" would overwrite an existing file; the app never
    /// clobbers a user's workflow.
    #[error("a workflow file already exists at {path}")]
    WorkflowCopyExists { path: PathBuf },

    /// A filesystem error reading, writing, or deleting a workflow / run file.
    #[error("workflow file I/O error at {path}: {source}")]
    WorkflowCopyIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The run-record directory (`projects/<id>/runs/`) could not be created
    /// before launching a workflow. Run files are how a failed/interrupted run
    /// survives a restart, so we fail loudly here rather than start a run whose
    /// records would silently fail to persist.
    #[error("could not prepare workflow run directory at {path}: {source}")]
    WorkflowRunSetupIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// "Copy to my prompts" would overwrite an existing file. The app never
    /// clobbers a user's prompt — they rename or remove the existing file first.
    #[error("a prompt file already exists at {path}")]
    PromptCopyExists { path: PathBuf },

    /// Writing the copied built-in prompt into the user's folder failed.
    #[error("failed to copy prompt to {path}: {source}")]
    PromptCopyIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
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
