//! Typed errors for command free-functions. The Tauri `#[tauri::command]`
//! wrapper maps these to `String` at the IPC boundary (Tauri convention).

use std::path::PathBuf;

use switchboard_core::{AgentId, CoreError, HarnessKind, ProjectId, SendId};
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

    /// A single-recipient cancel named a send the agent tools did not make and
    /// no workflow started through them made: a message the user typed, or a
    /// wrong id.
    #[error(
        "send {send_id} was not made through these tools or by a workflow started through them (it may be one the user typed, or the id is wrong): to stop a reply the user started, tell the user"
    )]
    SendNotCancellable { send_id: SendId },

    /// A cancel named a send made by a workflow the user started from the
    /// window.
    #[error(
        "send {send_id} belongs to a workflow the user started from Switchboard's window: tell the user rather than cancelling it"
    )]
    ReplyInUserWorkflow { send_id: SendId },

    /// A forward named a source's reply by `send_id`, and the source did not
    /// receive that send in its project: the id is wrong, or belongs to
    /// another agent or another project. Nothing was sent.
    #[error(
        "{name} has no reply to send {send_id} in its project, so nothing was sent: check the send_id, and the source's project_id if the send was made in another project"
    )]
    ForwardReplyUnknown { name: String, send_id: SendId },

    /// A forward named a source's reply by `send_id` while that reply is
    /// still waiting in line or running. A named reply is never waited for, so
    /// nothing was sent.
    #[error(
        "{name}'s reply to send {send_id} is still {state}, so nothing was sent: wait for {name} with wait_agent, then forward again"
    )]
    ForwardReplyUnfinished {
        name: String,
        send_id: SendId,
        state: &'static str,
    },

    /// A forward named a source's reply by `send_id` and that reply did not
    /// complete: it failed, was cancelled, never reached the agent, or could
    /// not be confirmed after a restart. Nothing was sent.
    #[error(
        "{name}'s reply to send {send_id} {state}, so nothing was sent: forward a completed reply, or leave out send_id to forward {name}'s latest answer"
    )]
    ForwardReplyNotCompleted {
        name: String,
        send_id: SendId,
        state: &'static str,
    },

    /// An agent-access cancel named a workflow run the user started from the
    /// window.
    #[error(
        "workflow run {run_id} was started by the user from Switchboard's window: tell the user rather than cancelling it"
    )]
    RunStartedByUser { run_id: uuid::Uuid },

    /// An agent-access call named an agent the project does not hold: a typo,
    /// an id from another project, or an agent since removed.
    #[error("no agent {agent_id} in project {project_id}: list_agents shows the project's agents")]
    AgentNotInProject {
        project_id: ProjectId,
        agent_id: AgentId,
    },

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
        "unsupported terminal app {terminal_app:?} — choose Terminal or iTerm in Settings → General → External Apps"
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

    /// Compaction: Switchboard cannot mechanically drive this harness's
    /// compaction. Like [`switchboard_core::CoreError::SessionForkUnsupported`]
    /// this is a statement about **Switchboard's support**, not about what the
    /// CLI can do — Codex compacts through a protocol Switchboard does not speak
    /// — and saying otherwise would be false. Defense in depth: the frontend
    /// never offers the action for these harnesses, so reaching this means a
    /// caller bypassed the capability predicate.
    #[error(
        "Switchboard cannot compact {harness} conversations \
         — it has no way to ask this harness to do it"
    )]
    CompactionUnsupported { harness: HarnessKind },

    /// Compaction: the agent has no session file, so there is no conversation to
    /// summarize. Worded as "no session" for the same reason
    /// [`Self::ForkSourceHasNoSession`] is — a cancelled first turn still leaves a
    /// resumable file, and compacting that is well-defined.
    #[error("{name} has no conversation to compact yet — send it a message first")]
    CompactionSourceHasNoSession { name: String },

    /// Compaction: the agent carries fork provenance but has no session of its
    /// own, so the *next ordinary send* is the dispatch that will copy its
    /// parent's session. A compaction must not be that dispatch: it would create
    /// the branch as a side effect of a maintenance action, and the fork is a
    /// turn that needs a prompt. Normally unreachable — this state is the residue
    /// of a fork whose first send failed to launch.
    #[error("{name} hasn't branched from its source yet — send it a message first, then compact")]
    CompactionForkNotMaterialized { name: String },

    /// Context breakdown: Switchboard cannot ask this harness what is in its
    /// context window. Same "statement about Switchboard's support" framing as
    /// [`Self::CompactionUnsupported`], and the same defense-in-depth role — the
    /// frontend shows no chevron and no menu item for these harnesses, so
    /// reaching this means a caller bypassed the capability predicate.
    #[error(
        "Switchboard cannot read {harness}'s context breakdown \
         — it has no way to ask this harness what is in its context window"
    )]
    ContextReportUnsupported { harness: HarnessKind },

    /// Context breakdown: the agent has no session file, so there is no context
    /// to measure. Same wording rationale as
    /// [`Self::CompactionSourceHasNoSession`].
    #[error("{name} has no conversation to analyze yet — send it a message first")]
    ContextReportSourceHasNoSession { name: String },

    /// Context breakdown: the agent carries fork provenance but has no session
    /// of its own, so the next ordinary send is the dispatch that will copy its
    /// parent's session. A report must not be that dispatch — it would create
    /// the branch as a side effect of a *read*. Same residue-of-a-failed-fork
    /// state as [`Self::CompactionForkNotMaterialized`].
    #[error(
        "{name} hasn't branched from its source yet \
         — send it a message first, then analyze its context"
    )]
    ContextReportForkNotMaterialized { name: String },

    /// Fork: the source agent has a turn in flight, so the copy the branch takes
    /// would be of a session file still being written. For a send that means the
    /// branch inherits a synthesized "No response requested." placeholder instead
    /// of the real answer (harness-behavior §3.5, Probe A); for a compaction it
    /// means copying a half-rewritten history. Refused rather than queued —
    /// waiting would deliver the answer to the very turn the user is branching
    /// away from.
    ///
    /// The message is deliberately **not** kind-aware. Three of its four raise
    /// sites reach their answer through `busy_fork_source`'s bool predicate, so
    /// naming the kind would mean widening that predicate for a wording change —
    /// and "its conversation must stop changing" is already true of both, where
    /// "its current answer" was true only of a send.
    #[error(
        "{name} is busy — its conversation must stop changing before you can branch. \
         Wait for it to finish, or cancel it first"
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

    /// Fork: Codex couldn't create the fork. Nothing was registered; the
    /// message says why, in Codex's words where it gave any.
    #[error(transparent)]
    CodexFork(#[from] switchboard_harness::CodexForkError),

    /// Fork: this process runs with mock harnesses and spawns no CLIs, and a
    /// Codex fork has to be created by the real Codex CLI. Said plainly rather
    /// than reported as a missing session, which would send a developer
    /// looking for the wrong problem.
    #[error("Codex forks need the real Codex CLI, and Switchboard is running with mock harnesses")]
    CodexForkNeedsRealHarness,

    /// Transcript hydration failed at the lookup-mechanism level (I/O on a
    /// file that exists, registry lookup failure). Per-line parse damage
    /// degrades silently to warnings inside `LoadedTranscript` — it does
    /// not surface here.
    #[error(transparent)]
    LoadTranscript(#[from] switchboard_harness::LoadTranscriptError),

    /// "Open session file" could not read a session file it opens as a copy
    /// (one Codex has compressed, or one the user asked to open as a copy):
    /// the file is gone, unreadable, or does not decode.
    #[error("reading {}: {source}", path.display())]
    SessionFileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

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

    /// Persisting the user-global harness quota snapshots (`usage.yaml`) failed.
    /// Best-effort like the registries above: losing a snapshot costs an empty
    /// usage section until the next turn reports a reading, so callers log rather
    /// than abort.
    #[error("failed to persist harness usage at {path}: {source}")]
    HarnessUsagePersist {
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

    /// A workflow start in a project where an agent is being removed, or was
    /// removed while the start read the roster. Momentary: starting again reads
    /// the roster as it is now.
    #[error("an agent is being removed from this project; start the workflow again in a moment")]
    WorkflowBlockedByAgentRemoval { project_id: ProjectId },

    /// The agent's record is gone, but saving the change failed afterwards
    /// (the folder could not be synced), so a crash could bring it back.
    #[error(
        "the agent was removed, but saving the change failed, so it may reappear after a crash: {source}"
    )]
    AgentRemovalNotDurable {
        #[source]
        source: switchboard_core::CoreError,
    },

    /// Saving the removal failed and the registry could not be read back, so
    /// whether the record is gone is unknown. The agent refuses messages until
    /// the app restarts, when the registry decides.
    #[error(
        "could not confirm the agent was removed ({source}); it refuses messages until Switchboard restarts"
    )]
    AgentRemovalUnconfirmed {
        #[source]
        source: switchboard_core::CoreError,
    },

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

    /// A send or forward named a recipient that is not in the project's
    /// roster. Spelled as the remote-control plan spells it (`unknown_recipient`
    /// on the wire), because both clients validate recipients the same way.
    #[error("agent {agent_id} is not in project {project_id}'s roster")]
    UnknownRecipient {
        project_id: ProjectId,
        agent_id: AgentId,
    },

    /// A turn cannot start because the project's working directory is gone:
    /// the harness is spawned with it as cwd. Refused before anything is
    /// journaled, so a caller sees the cause instead of a failed turn.
    #[error("project {project_id}'s working directory does not exist: {}", path.display())]
    DirectoryMissing {
        project_id: ProjectId,
        path: PathBuf,
    },

    /// The desktop window reported that it cannot listen to this agent's
    /// events, so a send to it would run invisibly. Refused per recipient.
    #[error(
        "the desktop window is not listening to agent {agent_id}; nothing was sent to it: try again, and if it repeats, ask the user to reopen the project in Switchboard"
    )]
    DesktopUnavailable { agent_id: AgentId },

    /// The desktop window did not acknowledge loading the project within the
    /// sync deadline. Nothing was dispatched; the load keeps running, so a
    /// retry usually succeeds.
    #[error(
        "the desktop window did not finish loading project {0} in time; nothing was sent: try again"
    )]
    ProjectLoadTimeout(ProjectId),

    /// The desktop window could not load the project (its open or roster read
    /// failed), so a turn started now would run where the window cannot show
    /// it. Nothing was dispatched.
    #[error("the desktop window could not load project {project_id}: {message}")]
    DesktopSyncFailed {
        project_id: ProjectId,
        message: String,
    },

    /// A forward's source is still mid-turn past the caller's wait budget.
    /// The compose was cancelled and nothing was dispatched.
    #[error(
        "{name} is still responding, so nothing was sent — wait for it with wait_agent, then forward again; the retry sends to every recipient it names, including any you cancelled"
    )]
    SourceBusy { name: String },

    /// A caller-supplied argument is malformed or inconsistent, including a
    /// retry key reused for a different request.
    #[error("{0}")]
    InvalidArgument(String),

    /// The composed message is larger than a harness CLI can take. Checked up
    /// front on the final body so the whole send is refused before any
    /// recipient is dispatched; the dispatcher's own check stays as the
    /// backstop for other callers.
    #[error("{0}")]
    PromptTooLarge(DispatchError),

    /// No in-memory record of this send: it was never made, it was pruned, or
    /// the app restarted since. Waits fall back to the journal; cancels cannot.
    #[error("send {0} is not known to this Switchboard process")]
    SendUnknown(SendId),

    /// A single result item cannot fit the reply budget even with every text
    /// field emptied. `read_with` names the call that reads it in pages, when
    /// the result type has one; a result with none says only that it is too
    /// large, because advice the caller cannot follow is worse than none.
    #[error("{}", too_large_message(*read_with))]
    ResultTooLarge { read_with: Option<&'static str> },

    /// An error shared between the request that raised it and any retry that
    /// joined it with the same key. Every `send` and `forward` error arrives in
    /// this wrapper, always — whether a retry is holding a copy is a matter of
    /// timing, and an error whose shape depended on timing would be a trap. Use
    /// [`AppError::root`] to match on the underlying error.
    #[error("{0}")]
    Shared(std::sync::Arc<AppError>),

    /// A retry joined an in-flight request with the same key, and its own wait
    /// budget ran out before the original settled. The original keeps running
    /// and its key stays claimed: call again with the same key.
    #[error("request {send_id} is still in progress — call again with the same idempotency_key")]
    RequestInProgress { send_id: SendId },

    /// A blocking phase was ended by the server's own cancellation (shutdown)
    /// before it produced a result. Nothing was dispatched.
    #[error("the operation was interrupted by the server shutting down")]
    Interrupted,

    /// An invariant the code relies on did not hold. Never caused by the
    /// caller's input.
    #[error("internal error: {0}")]
    Internal(String),

    /// A step before dispatch did not finish within the call's wait budget —
    /// a slow prompt provider, or opening a forward's source projects. Nothing
    /// was dispatched; trying again is the right response, not changing the
    /// request.
    #[error("{what} did not finish within the wait budget; nothing was sent — try again")]
    TimedOut { what: &'static str },

    /// A fan-out under `refuse` found these recipients busy up front, so the
    /// whole send was refused and nothing was dispatched. Names rather than
    /// ids because the message is what the caller reads; the ids are in the
    /// request it just made.
    #[error(
        "busy: {}; nothing was sent: wait for them with wait_agent, or send again with on_busy queue to line it up behind their turns",
        names.join(", ")
    )]
    RecipientsBusy { names: Vec<String> },

    /// A caller other than the window asked to remove an agent with a turn
    /// running or queued. The window's own remove cancels that work; a caller
    /// that cannot see the agent's pane must stop it on purpose first.
    #[error(
        "{name} has a turn running or queued: wait for it with wait_agent, or cancel a reply you sent with cancel_send and agent_id, then remove it; if a reply the user started keeps it busy, ask the user"
    )]
    RemoveAgentBusy { name: String },

    /// A caller other than the window asked to remove an agent while a
    /// workflow runs in its project, which may send to the agent in a later
    /// step.
    #[error(
        "a workflow is running in this project and may send to {name}: wait for it with wait_workflow, or cancel it, then remove the agent"
    )]
    RemoveAgentWorkflowRunning { name: String },

    /// Writing the copied built-in prompt into the user's folder failed.
    #[error("failed to copy prompt to {path}: {source}")]
    PromptCopyIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn too_large_message(read_with: Option<&'static str>) -> String {
    match read_with {
        Some(call) => {
            format!("the result is too large to return whole; read it in pages with {call}")
        }
        None => "the result is too large to return".to_owned(),
    }
}

impl AppError {
    /// The underlying error, with any [`AppError::Shared`] wrappers removed.
    /// The way to match on an `ops` error's kind.
    #[must_use]
    pub fn root(&self) -> &AppError {
        match self {
            AppError::Shared(inner) => inner.root(),
            other => other,
        }
    }

    pub fn invalid_uuid(value: impl Into<String>, source: uuid::Error) -> Self {
        Self::InvalidUuid {
            value: value.into(),
            source,
        }
    }
}
