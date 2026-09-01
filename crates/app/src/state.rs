//! Tauri-side application state. Owns the bound working directory, loaded
//! projects, dispatcher, and harness adapter for the lifetime of the app.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use switchboard_core::{AgentId, AgentRecord, Project, ProjectId, Store};
use switchboard_dispatcher::{Dispatcher, EventEmitter};
use switchboard_harness::HarnessAdapter;
use switchboard_prompts::PromptService;
use switchboard_workflow::WorkflowStepInfo;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::git_registry::{self, GitRegistry};
use crate::notification::{Notifier, NullNotifier};
use crate::preferences::{self, Preferences};
use crate::workspace::{self, Workspace};

/// Why a project is blocked by the move machinery, and the intent file
/// involved. Distinguished because the two ask different things of the user:
/// a failure means the store needs repairing; a deferral means their *other*
/// Switchboard process has the project open and closing it plus a relaunch is
/// the whole fix.
#[derive(Debug, Clone)]
pub struct MoveBlock {
    pub intent: PathBuf,
    pub deferred: bool,
}

/// A live workflow run's in-memory handle. The on-disk `runs/<run-id>.jsonl` is
/// the durable record (and the only thing that survives a crash); this registry
/// is the live mirror that lets the app cancel a run, list active runs, and
/// report each one's current step without re-reading disk. The owning background
/// task removes the entry when the run reaches a terminal status.
pub struct ActiveRun {
    /// Fires a workflow-level cancel; the interpreter observes it and finishes
    /// `cancelled`. Cloned out under the registry lock (never fired while holding
    /// it) by cancel and by directory/project teardown.
    pub cancel: CancellationToken,
    /// The project the run belongs to — used to scope teardown and the
    /// `workflow:<project-id>` progress channel.
    pub project_id: ProjectId,
    /// The workflow's name, for the run indicator label.
    pub workflow: String,
    /// Latest step progress, updated by the progress sink as the run advances, so
    /// `list_workflow_runs` reports a live run's step without reading disk.
    pub snapshot: RunSnapshot,
    /// Per-step display info with recipients **resolved** to concrete agent names
    /// (from the invocation's bindings), so the live progress view names the actual
    /// agents. Disk-sourced runs instead reconstruct *declared* steps from the run
    /// file; this resolved copy exists only for the in-flight run.
    pub steps: Vec<WorkflowStepInfo>,
    /// Notified once when the run reaches a terminal status. Teardown collects a
    /// clone **before** firing cancel, then awaits it — so the wait can't be
    /// stranded by the owning task removing its own registry entry. `notify_one`
    /// stores a permit if the terminal beats the waiter, so the wakeup is never
    /// lost regardless of ordering.
    pub done: Arc<tokio::sync::Notify>,
}

/// A live run's step progress. A run is only in the registry while running, so
/// there is no terminal status here — terminal removes the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSnapshot {
    /// Total top-level steps (from the run-start event).
    pub total_steps: usize,
    /// Zero-based index of the step currently in progress.
    pub current_step: usize,
}

/// What the user is looking at, as last reported by the frontend.
///
/// `seq` is the frontend's monotonically increasing navigation counter. Writes
/// carrying a stale `seq` are dropped, so a slow IPC call issued before a rapid
/// Settings → project → Git sequence cannot land after — and overwrite — a newer
/// view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisibleProject {
    pub project_id: Option<ProjectId>,
    pub seq: u64,
}

impl VisibleProject {
    /// Apply an update if it is newer than what we hold. Returns whether it was
    /// applied — `false` means a newer view already won.
    pub fn apply(&mut self, project_id: Option<ProjectId>, seq: u64) -> bool {
        if seq < self.seq {
            return false;
        }
        *self = Self { project_id, seq };
        true
    }
}

/// Two-way handshake for the post-eviction pause point.
///
/// **`entered` is not decoration.** A one-way barrier can be deleted from the
/// operation and the test still passes, having silently stopped testing the
/// ordering — which is exactly how the first two attempts at that test failed.
/// With the handshake, the test blocks on `entered`, so removing the pause makes
/// it time out rather than pass.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct MaintenanceBarrier {
    pub entered: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}

/// The single piece of state managed by Tauri. Multi-project and
/// multi-directory (per system-design §3): the app holds N working directories
/// concurrently, each hosting N projects. `directories` keys every loaded
/// `Directory` handle by its canonical path.
///
/// **Lock-order convention** (when more than one of these mutexes is held
/// at the same time): `workspace` → `registry_write` → `git_registry` →
/// `maintenance` → `project_generation` → `projects` → `active_project_id` →
/// `needs_session_meta` → `project_locks` → `agents_by_id`. Always acquire in
/// this order.
///
/// `maintenance` and `project_generation` sit directly under `registry_write`
/// because `begin_maintenance` takes all three in that order — the write lock
/// first (so a claim's membership is derived and marked atomically against
/// `create_project`), then the marks, then the generations. `workspace`
/// is at the head because it is the app-owned user-global registry that sits
/// above any single directory's state; in practice it is taken either standalone
/// (`list_projects`, the workspace switcher) or nested *under* `registry_write`
/// in `init_directory` (which holds `registry_write` for its whole body) — never
/// the inverse. `git_registry` (the Git-view tracked-repo list) follows the same
/// shape: standalone for the Git-view read/add/remove commands, and nested under
/// `registry_write` during `init_directory`'s auto-sync hook — so it sorts after
/// `registry_write` here. **No path may acquire `registry_write` while holding
/// `git_registry`** (the inverse order is the deadlock this convention forbids).
/// Violating the order can deadlock under concurrent access. Single-lock acquisitions (which most
/// callers do) are unaffected — the convention only matters when nesting.
/// `needs_session_meta` is the tail because both `attach_agent_impl` (under
/// `registry_write`) and `send_message_impl` (no other locks held) acquire
/// it briefly with no `.await` crossing the guard. `project_locks` and
/// `agents_by_id` are leaf maps acquired briefly; they are taken while
/// `registry_write` is held during open/create/remove — which precedes them in
/// the order, so those nestings are compliant.
/// When nesting them, follow the documented tail order.
///
/// `forwards` and `workflow_runs` are **tail-leaf** maps (acquired alone, last,
/// briefly, never across an `.await`): they sort after `agents_by_id`. Teardown
/// (`delete_project`) collects the affected runs' cancel
/// tokens under the `workflow_runs` lock, releases it, then fires them and awaits
/// the runs reaching terminal **before** draining agents — so no cancel token is
/// fired while the lock is held, and the order keeps a run resolving `cancelled`
/// (not `failed`) and stops it dispatching against unloaded state.
///
/// `preferences` is a **standalone leaf**: acquired by `get_preferences` /
/// `set_preferences`, and by `ProjectDispatchContextFactory::build`, which
/// samples the browser-tools preference live for every dispatch. Never nested
/// with another state lock, and never held across I/O — `set_preferences_impl`
/// releases it before writing `config.yaml`, precisely so a starting turn can't
/// park behind a save. The write is serialized by `switchboard_core`'s
/// `YAML_EDIT_LOCK`, not by this mutex (which cannot serialize it: the prompt
/// service co-owns `config.yaml` from another crate and never takes this lock).
/// Because it's a leaf taken alone, no other lock may be acquired while holding
/// it — keep it that way.
///
/// `registry_write` serializes project-registry mutations and small mutable
/// project sidecars such as message pins.
/// `Directory::create_project` and `Project::register_agent` have a TOCTOU
/// window between their internal "is this name unique?" read and the
/// subsequent append; two concurrent IPC calls could otherwise both pass
/// the uniqueness check and append colliding records. The mutex closes
/// that window inside one process; cross-process serialization is future
/// work (an `instance.lock` per directory).
pub struct AppState {
    pub projects: Mutex<HashMap<ProjectId, Project>>,
    pub active_project_id: Mutex<Option<ProjectId>>,
    /// The project the user is to be treated as **looking at right now**, or
    /// `None` when there is none (Settings, the Git view, a project still
    /// loading, or a project whose reading mode is on — the user watching a run
    /// asked to be notified about it as though they weren't there).
    ///
    /// Deliberately separate from [`Self::active_project_id`], which answers a
    /// different question — "which project would a backend action target" — and
    /// stays set while the user is in Settings or the Git view. Only the
    /// notification gate reads this, to stay quiet about the one outcome the user
    /// can already see. Merging the two would silently reintroduce that bug.
    ///
    /// Carries the frontend's monotonic sequence number alongside the value so a
    /// slow write cannot clobber a newer view during rapid navigation.
    pub visible_project: Mutex<VisibleProject>,
    /// Acquired around any operation that appends to a JSONL on disk
    /// (`projects.jsonl` or a project's `registry.jsonl`). `std::sync::Mutex`
    /// because the protected work is fully synchronous — no `.await` while
    /// the guard is held. `Arc` so the per-dispatch session-locator sink (which
    /// outlives any single command, living on the dispatcher's `'static` actor
    /// task) can hold a handle and serialize its registry write here.
    pub registry_write: Arc<Mutex<()>>,
    pub dispatcher: Arc<Dispatcher>,
    /// Adapter for `HarnessKind::ClaudeCode` agents. Named fields per harness
    /// (one per supported `HarnessKind`) make the routing rule
    /// (`send_message_impl` matches on `agent.harness`) type-supported —
    /// adding a new harness forces a compiler-checked update here.
    pub claude_adapter: Arc<dyn HarnessAdapter>,
    /// Adapter for `HarnessKind::Codex` agents.
    pub codex_adapter: Arc<dyn HarnessAdapter>,
    /// Adapter for `HarnessKind::Antigravity` agents.
    pub antigravity_adapter: Arc<dyn HarnessAdapter>,
    pub emitter: Arc<dyn EventEmitter>,
    /// Set of `agent_id`s whose next dispatch must run with
    /// `DispatchOptions::is_first_dispatch_after_attach = true`. Populated by
    /// `attach_agent_impl` (Codex-only — see below); read (not drained) by
    /// `send_message_impl`; cleared by the per-dispatch emitter decorator
    /// when a `session_meta` event for the matching agent is observed.
    ///
    /// **Purpose.** The Codex attach-existing-session flow pre-writes a
    /// sidecar record at attach time. Without this flag, the Codex adapter
    /// would see `prior.is_some()` on its first post-attach dispatch and
    /// skip `SessionMeta` emission — leaving the sidebar's MCP/skills/model
    /// listing empty until some other code path triggered emission. The
    /// flag tells the adapter "force `SessionMeta` even though the sidecar
    /// is non-empty."
    ///
    /// **Codex-only.** Claude Code emits `SessionMeta` from its `system/init`
    /// stream event on every dispatch (see `crates/harness/src/claude_code.rs`),
    /// so the override has nothing to do for Claude attaches. The insert in
    /// `attach_agent_impl` is gated on `HarnessKind::Codex`.
    ///
    /// **Read-don't-drain.** `send_message_impl` reads with `contains`, not
    /// `remove`. The clear happens in a per-dispatch emitter decorator
    /// (`crate::emitter::SessionMetaObservingEmitter`) that intercepts
    /// `session_meta` events on the per-agent channel and removes the
    /// `agent_id` only when emission is genuinely observed. This means:
    /// - Successive dispatches that fail mid-stream pre-`SessionMeta` each
    ///   continue to see `is_first_dispatch_after_attach: true` — the flag
    ///   persists until the override actually does its job.
    /// - Once `SessionMeta` flows through, the decorator drops the flag and
    ///   subsequent dispatches use the default `false`.
    ///
    /// **Wrapped in `Arc<Mutex<…>>`** so the emitter decorator can hold a
    /// clone for the lifetime of the dispatcher's `'static` drain task.
    ///
    /// **Deletion clearing.** `delete_project_impl` drops the entries for the
    /// deleted project's agents alongside the matching `projects` and
    /// `agents_by_id` entries — a stale `agent_id` from a deleted project's
    /// attach must not leak forward.
    pub needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,

    /// Per-project inter-process lock handles. One entry per loaded
    /// project, holding an advisory exclusive lock (std `File::try_lock`,
    /// stable since Rust 1.89 — `flock` on unix) on
    /// `<directory>/.switchboard/projects/<id>/instance.lock`. Acquired in
    /// the project-open/create path before the project is inserted into
    /// `projects`; the live `File` *is* the lock, so dropping it (removing
    /// the entry on directory removal, or the process exiting/crashing)
    /// releases the lock — no explicit unlock or stale-lock cleanup needed. This is an
    /// inter-process guard only: a second Switchboard process opening the
    /// same project is refused (`AppError::ProjectLocked`); intra-process
    /// re-open returns the already-loaded handle without re-locking.
    pub project_locks: Mutex<HashMap<ProjectId, File>>,

    /// Canonical agent-lookup index: `AgentId → AgentRecord`. The
    /// record carries `project_id`, so this single map answers "which
    /// project owns this agent, and what is its record" without scanning
    /// every loaded project's `registry.jsonl` from disk (the prior
    /// `lookup_agent` hot path). Populated on project open, agent
    /// register/attach, and `list_agents`; the removed directory's entries are
    /// dropped by `delete_project_impl`, and a removed agent's by
    /// `remove_agent_impl`, so invalidation is insert-only within a session
    /// plus those targeted prunes. An `AgentRecord` is otherwise immutable after
    /// registration, with one exception: `rename_agent_impl` and
    /// `set_agent_session_locator_impl` (the runtime session-locator capture)
    /// mutate a record in place and re-insert the updated copy here in the same
    /// `registry_write` critical section, so the cache never lags the registry.
    /// `Arc` so the dispatch-context factory and its per-dispatch
    /// session-locator sink (both `'static` on the actor task) share this one
    /// map: the sink writes the captured locator here, and the factory
    /// live-reads the agent record from it at the next turn's start.
    pub agents_by_id: Arc<Mutex<HashMap<AgentId, AgentRecord>>>,

    /// User-global workspace registry — the set of working directories the app
    /// knows about plus a cached snapshot of each directory's projects (see
    /// `crate::workspace`). Convenience state, not load-bearing: it backs the
    /// flat cross-directory project list. Defaults to empty; production
    /// hydrates it from `workspace.yaml` via [`AppState::with_workspace`].
    pub workspace: Mutex<Workspace>,

    /// Resolved path of `workspace.yaml`, or `None` when no global location was
    /// resolved (tests, or an exotic host with no home dir). `persist_workspace`
    /// is a no-op while this is `None`, so tests never touch user-global state.
    pub workspace_path: Option<PathBuf>,

    /// Per-project counter, bumped by every lifecycle operation.
    ///
    /// **Closes the admitted-then-suspended send.** The maintenance mark stops
    /// *new* work entering the window, but a send that resolved its `Project` a
    /// moment earlier already holds a snapshot of where the project used to
    /// live, and nothing bounds how long it can sit between that lookup and the
    /// dispatcher enqueue — `ensure_materializing_fork_may_dispatch` awaits a
    /// reply from an actor that may itself be mid-turn. Re-draining after the
    /// write does not help: a send suspended past the drain still enqueues its
    /// stale factory.
    ///
    /// So the send captures the generation at lookup and re-verifies it
    /// immediately before handing off. Anything whose view predates a lifecycle
    /// operation is refused, however long it waited.
    ///
    /// **The interval past that check is covered too**, by the same counter read
    /// a second time. `ProjectDispatchContextFactory` captures this value when it
    /// is built and re-compares it in `preflight`, which the dispatcher runs at
    /// the instant the turn actually starts. That is what makes the factory's
    /// frozen `Project` — and the working directory every dispatch takes from it
    /// — safe to hold for the actor's whole lifetime, and it is also the only
    /// check reached by a send that queued behind another turn and popped long
    /// after every command-boundary gate had passed.
    ///
    /// `Arc` for exactly that: the factory holds a clone. The factory is *given*
    /// its comparison value rather than reading one — see
    /// `ProjectDispatchContextFactory::generation_at_capture` for why sampling it
    /// at construction pairs a stale project with a fresh counter.
    ///
    /// The span this still does not cover — admission to spawn — is described at
    /// [`crate::commands::reject_if_generation_changed`], including why delete
    /// bounds it and re-point would not.
    pub project_generation: Arc<Mutex<HashMap<ProjectId, u64>>>,

    /// Root under which cross-**process** harness session locks live
    /// (`<lock_root>/locks/<key>.lock`).
    ///
    /// **Required, and deliberately not `config_dir()`.** Every other
    /// user-global path is dev-isolated so a debug build never touches installed
    /// state; this one must be the *opposite*, because its entire purpose is to
    /// make a dev build and the installed app contend on one file. Injected
    /// rather than resolved here so tests take real locks under a `TempDir`
    /// instead of the developer's live lock directory, where they would contend
    /// with a running app. See `crate::session_lock_root`.
    pub lock_root: PathBuf,

    /// Test-only pause point, awaited by the lifecycle operations immediately
    /// after they evict and before they drain.
    ///
    /// The window between those two steps is what finding-1's race lives in, and
    /// it closes too fast to observe by polling — the drain completes before a
    /// test can look. Without a way to hold it open, the ordering could only be
    /// argued, and this milestone has already shipped one test that looked like
    /// coverage and wasn't. A barrier is the smaller price.
    #[cfg(test)]
    pub maintenance_barrier: Mutex<Option<Arc<MaintenanceBarrier>>>,

    /// Two-way handshake inside `capture_dispatch_snapshot`, taken **while it
    /// holds `registry_write`**.
    ///
    /// **The second such seam, added deliberately rather than by momentum.** The
    /// property it exists for — that a dispatch's project, agent, roster, and
    /// lifecycle generation come from one instant — is enforced by holding a
    /// single lock, and a test cannot observe "one lock" from outside. Every
    /// version that tried timed out to be indistinguishable from the broken form:
    /// `maintenance_barrier` pauses *after* `begin_maintenance` has returned and
    /// released its guard, so it cannot stage the interleaving at all. Pausing
    /// mid-capture is the only way a test can prove maintenance is excluded.
    ///
    /// A third seam should prompt generalizing this into one mechanism rather
    /// than a third field.
    #[cfg(test)]
    pub capture_barrier: Mutex<Option<Arc<MaintenanceBarrier>>>,

    /// Projects currently mid-lifecycle-operation — a re-point or a delete has
    /// evicted their routable state and has not finished rebuilding it.
    ///
    /// **A closed window, not a swept one.** These operations must evict before
    /// they drain (or a send lands in the gap and spawns a fresh actor on the
    /// old working directory), which leaves an interval where the project is
    /// absent from every map but still exists. Without a gate, a user clicking a
    /// *different* project in the same directory during that interval opens it
    /// against the stale path, and a second drain would only catch that by
    /// convergence argument. Refusing entry is the version that stays true when
    /// the code around it changes.
    ///
    /// Read by `open_project_impl`, `create_project_impl`, and the dispatch
    /// resolution path; set and cleared under `registry_write`, which is what
    /// makes "evict and mark" atomic against those callers.
    pub maintenance: Arc<Mutex<HashSet<ProjectId>>>,

    /// The user-global project store — every project's index, catalog, and
    /// metadata root.
    ///
    /// **Required, unlike the `Option<PathBuf>` paths above.** Those guard
    /// user-global *convenience* state that degrades to a no-op when
    /// unresolvable; project persistence cannot. A `None` store would mean
    /// creating a project silently succeeds and vanishes, so the root is
    /// injected at construction and its absence is a startup failure, not a
    /// degraded mode.
    ///
    /// Holds no cached state (see [`Store`]), so it needs no lock: every method
    /// reads or rewrites files, serialized by [`Self::registry_write`] where
    /// mutation requires it.
    pub store: Store,

    /// Keeps a test's temp store root alive for the state's lifetime. See
    /// [`Self::new_for_test`].
    #[cfg(test)]
    store_tmp: Option<tempfile::TempDir>,

    /// Keeps a test's temp lock root alive, for the same reason as `store_tmp`:
    /// the root is required, and a test taking real locks under the developer's
    /// live lock directory would contend with a running app.
    #[cfg(test)]
    lock_tmp: Option<tempfile::TempDir>,

    /// User-global Git-view tracked-repo registry — the ordered set of repo roots
    /// the Git view shows (see `crate::git_registry`). A superset of the
    /// directories that host projects: stores paths only, never git state.
    /// Defaults to empty; production hydrates it from `git-view.yaml` via
    /// [`AppState::with_git_registry`].
    pub git_registry: Mutex<GitRegistry>,

    /// Resolved path of `git-view.yaml`, or `None` when unresolved (tests, exotic
    /// host) or when the existing file couldn't be read this session.
    /// `persist_git_registry` is a no-op while this is `None`.
    pub git_registry_path: Option<PathBuf>,

    /// User-global personal preferences (see `crate::preferences`). Backend-owned
    /// `config.yaml`; the first backend-persisted settings (theme stays
    /// frontend-only). Defaults until hydrated via [`AppState::with_preferences`].
    /// Shared so `ProjectDispatchContextFactory` can read it live per dispatch
    /// (browser-tools toggle), rather than freezing the value at enqueue.
    pub preferences: Arc<Mutex<Preferences>>,

    /// Resolved path of `config.yaml`, or `None` when no global location was
    /// resolved (tests, exotic host). `set_preferences` errors-as-noop persist
    /// while this is `None`, so tests never touch user-global state.
    pub preferences_path: Option<PathBuf>,

    /// User-global prompt resolution (local file providers; MCP later). Read-only
    /// after construction — the command shims call `list`/`render` through it.
    /// Defaults to an inert (disabled) service; production injects the configured
    /// one via [`AppState::with_prompts`], and tests that exercise prompts do the
    /// same with temp paths.
    pub prompts: PromptService,

    /// In-flight manual forwards, keyed by `forward_id`. Each entry is the
    /// [`CancellationToken`] for one held cross-agent forward: while
    /// `forward_message_impl` awaits its sources, `cancel_forward_impl` fires the
    /// matching token to release the hold without dispatching (the user
    /// cancelling a "waiting for …" send). The command inserts on entry and
    /// removes on every exit, so a finished/cancelled forward leaves no entry.
    /// Frontend-owned `forward_id` (minted per held send), so an entry never
    /// outlives the one command that owns it. Not load-bearing across restart —
    /// a held forward is live-UI-only until it dispatches (system-design §7).
    pub forwards: Mutex<HashMap<Uuid, CancellationToken>>,

    /// In-flight workflow runs, keyed by `run_id`. The live mirror of the on-disk
    /// `runs/<run-id>.jsonl` records: lets cancel/list act on a running workflow
    /// and report its current step without touching disk. The run's background
    /// task inserts on spawn and removes on terminal; directory/project teardown
    /// fires the entry's cancel **before** draining agents. A **tail-leaf** mutex
    /// (peer of `forwards`): acquired briefly, last in the lock order, and never
    /// held across an `.await` — cancel tokens are cloned out under the lock and
    /// fired after release. Not load-bearing across restart (an interrupted run is
    /// recovered from its file, not this map). `Arc` so the run's background task
    /// and its progress sink (both `'static`) share this one map — the sink
    /// updates a run's step snapshot, the task removes the entry on terminal.
    pub workflow_runs: Arc<Mutex<HashMap<Uuid, ActiveRun>>>,

    /// Serializes agent moves within this process. Moves are rare and
    /// user-initiated, so a second attempt while one runs is refused rather
    /// than queued. Cross-process exclusion is not this mutex's job: the store's
    /// files are covered by the two projects' `instance.lock`s and the harness
    /// session by the session lock.
    pub move_mutex: tokio::sync::Mutex<()>,
    /// Projects blocked by a move that could not complete, mapped to what
    /// blocks them. Entries here are also in `maintenance` (which is what
    /// actually refuses work); this map only upgrades the refusal to the right
    /// story — genuine damage needing repair, or a healthy deferral because
    /// another Switchboard process holds the project. Never cleared in process;
    /// the next launch retries.
    pub move_repairs: Mutex<HashMap<ProjectId, MoveBlock>>,
    /// Set when the move-recovery directory could not be read at startup,
    /// holding the reason. While set, **every** project operation refuses: this
    /// is the one failure where we cannot prove any project is safe, because we
    /// cannot prove no move is pending. Distinct from `move_repairs`, which
    /// blocks the specific projects a known-bad intent implicates.
    pub move_recovery_unavailable: Mutex<Option<String>>,

    /// Fires OS notifications on a workflow run's completion/failure (suppressed
    /// when the window is focused). Defaults to a no-op; production injects the
    /// gated notifier via [`AppState::with_notifier`].
    pub notifier: Arc<dyn Notifier>,
    /// Shared with the notification delivery path so the permission prompt is
    /// requested once and any pending request is awaited before a notification is
    /// posted — see [`crate::notification::AuthorizationGate`].
    pub notification_gate: Arc<crate::notification::AuthorizationGate>,

    /// The **user-global** workflows directory (`<config-dir>/workflows`) — the
    /// single store of workflow definitions, shared across every project (unlike
    /// runs, which stay per-project). `None` on a host with no resolvable config
    /// dir; production injects it via [`AppState::with_workflows_dir`].
    pub workflows_dir: Option<PathBuf>,
}

impl AppState {
    pub fn new(
        claude_adapter: Arc<dyn HarnessAdapter>,
        codex_adapter: Arc<dyn HarnessAdapter>,
        antigravity_adapter: Arc<dyn HarnessAdapter>,
        emitter: Arc<dyn EventEmitter>,
        store: Store,
        lock_root: PathBuf,
    ) -> Self {
        Self {
            store,
            maintenance: Arc::new(Mutex::new(HashSet::new())),
            project_generation: Arc::new(Mutex::new(HashMap::new())),
            lock_root,
            #[cfg(test)]
            maintenance_barrier: Mutex::new(None),
            #[cfg(test)]
            capture_barrier: Mutex::new(None),
            #[cfg(test)]
            store_tmp: None,
            #[cfg(test)]
            lock_tmp: None,
            projects: Mutex::new(HashMap::new()),
            active_project_id: Mutex::new(None),
            visible_project: Mutex::new(VisibleProject::default()),
            registry_write: Arc::new(Mutex::new(())),
            dispatcher: Arc::new(Dispatcher::new()),
            claude_adapter,
            codex_adapter,
            antigravity_adapter,
            emitter,
            needs_session_meta: Arc::new(Mutex::new(HashSet::new())),
            project_locks: Mutex::new(HashMap::new()),
            agents_by_id: Arc::new(Mutex::new(HashMap::new())),
            workspace: Mutex::new(Workspace::default()),
            workspace_path: None,
            git_registry: Mutex::new(GitRegistry::default()),
            git_registry_path: None,
            preferences: Arc::new(Mutex::new(Preferences::default())),
            preferences_path: None,
            prompts: PromptService::disabled(),
            forwards: Mutex::new(HashMap::new()),
            workflow_runs: Arc::new(Mutex::new(HashMap::new())),
            move_mutex: tokio::sync::Mutex::new(()),
            move_repairs: Mutex::new(HashMap::new()),
            move_recovery_unavailable: Mutex::new(None),
            notifier: Arc::new(NullNotifier),
            notification_gate: Arc::new(crate::notification::AuthorizationGate::new(Arc::new(
                crate::notification::OsAuthorizationRequester,
            ))),
            workflows_dir: None,
        }
    }

    /// Construct a state whose store lives in a temp directory the state itself
    /// owns.
    ///
    /// The store root is required, so without this every one of the ~125 test
    /// fixtures would have to create a second `TempDir` and thread it through
    /// its return tuple purely to keep the root alive — churn that obscures what
    /// each test is actually about. Owning the `TempDir` here ties the store's
    /// lifetime to the state's, which is exactly the intended scope. Reach for
    /// `state.store.root()` when a test needs to inspect on-disk layout.
    #[cfg(test)]
    pub fn new_for_test(
        claude_adapter: Arc<dyn HarnessAdapter>,
        codex_adapter: Arc<dyn HarnessAdapter>,
        antigravity_adapter: Arc<dyn HarnessAdapter>,
        emitter: Arc<dyn EventEmitter>,
    ) -> Self {
        let root = tempfile::TempDir::new().expect("temp store root");
        let locks = tempfile::TempDir::new().expect("temp lock root");
        let store = Store::open(root.path()).expect("open temp store");
        let mut state = Self::new(
            claude_adapter,
            codex_adapter,
            antigravity_adapter,
            emitter,
            store,
            locks.path().to_path_buf(),
        );
        state.store_tmp = Some(root);
        state.lock_tmp = Some(locks);
        state
    }

    /// Construct a test state over an **existing** store root, for tests that
    /// need two `AppState`s to see the same store — a restart, or a second
    /// process's view. The caller owns the root and must keep it alive.
    #[cfg(test)]
    pub fn new_for_test_at(
        root: &std::path::Path,
        claude_adapter: Arc<dyn HarnessAdapter>,
        codex_adapter: Arc<dyn HarnessAdapter>,
        antigravity_adapter: Arc<dyn HarnessAdapter>,
        emitter: Arc<dyn EventEmitter>,
    ) -> Self {
        let store = Store::open(root).expect("open temp store");
        let locks = tempfile::TempDir::new().expect("temp lock root");
        let mut state = Self::new(
            claude_adapter,
            codex_adapter,
            antigravity_adapter,
            emitter,
            store,
            locks.path().to_path_buf(),
        );
        state.lock_tmp = Some(locks);
        state
    }

    /// Builder step that injects the production notifier. Production calls this
    /// after `new`; tests that assert on notifications pass a recorder.
    #[must_use]
    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = notifier;
        self
    }

    /// Builder step that injects the user-global workflows directory. Production
    /// calls this after `new`; tests that exercise workflows pass a temp dir.
    #[must_use]
    pub fn with_workflows_dir(mut self, dir: PathBuf) -> Self {
        self.workflows_dir = Some(dir);
        self
    }

    /// Builder step that injects the configured prompt service. Production calls
    /// this after `new`; tests that exercise prompts pass a service over temp
    /// paths, while others keep the disabled default.
    #[must_use]
    pub fn with_prompts(mut self, prompts: PromptService) -> Self {
        self.prompts = prompts;
        self
    }

    /// Builder step that loads the workspace registry from `path` and records
    /// the path for later persistence. Production calls this after `new`; tests
    /// skip it so `workspace_path` stays `None` and the registry stays empty.
    #[must_use]
    pub fn with_workspace(mut self, path: PathBuf) -> Self {
        let outcome = workspace::load(&path);
        self.workspace = Mutex::new(outcome.workspace);
        // Only enable persistence when the read was trustworthy. If the file
        // existed but couldn't be read, `persistable` is false and we leave
        // `workspace_path` None so a later save never overwrites a registry we
        // failed to load (see `workspace::LoadOutcome`).
        self.workspace_path = outcome.persistable.then_some(path);
        self
    }

    /// Builder step that loads the Git-view tracked-repo registry from `path`.
    /// Same persistability contract as [`with_workspace`](Self::with_workspace):
    /// an unreadable existing file disables persistence so it's never clobbered.
    #[must_use]
    pub fn with_git_registry(mut self, path: PathBuf) -> Self {
        let outcome = git_registry::load(&path);
        self.git_registry = Mutex::new(outcome.registry);
        self.git_registry_path = outcome.persistable.then_some(path);
        self
    }

    /// Builder step that loads personal preferences from `path` and records the
    /// path for later saves. Unlike the registries there is no persistability
    /// gate: preferences are written only on explicit user save, so a corrupt
    /// file simply yields defaults this session and the next save replaces it.
    #[must_use]
    pub fn with_preferences(mut self, path: PathBuf) -> Self {
        self.preferences = Arc::new(Mutex::new(preferences::load(&path)));
        self.preferences_path = Some(path);
        self
    }
}

/// Persist the workspace registry to disk if a `workspace_path` is configured.
/// Best-effort: a `None` path is a no-op (tests), and a save failure is logged
/// rather than propagated — the registry is convenience state, like the cached
/// project snapshot it holds, and must not break the operation that triggered
/// the save.
pub(crate) fn persist_workspace(state: &AppState) {
    let Some(path) = state.workspace_path.as_ref() else {
        return;
    };
    // Snapshot under the lock, then release it before touching disk — never
    // hold a state mutex across filesystem I/O (single-writer app, so the next
    // mutation's persist captures anything that lands after this clone).
    let snapshot = lock(&state.workspace).clone();
    if let Err(e) = workspace::save(path, &snapshot) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "failed to persist workspace registry"
        );
    }
}

/// Persist the Git-view tracked-repo registry to disk if a `git_registry_path`
/// is configured. Best-effort, same as [`persist_workspace`]: a `None` path is a
/// no-op (tests), and a save failure is logged rather than propagated.
pub(crate) fn persist_git_registry(state: &AppState) {
    let Some(path) = state.git_registry_path.as_ref() else {
        return;
    };
    let snapshot = lock(&state.git_registry).clone();
    if let Err(e) = git_registry::save(path, &snapshot) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "failed to persist git-view registry"
        );
    }
}

/// Recover from `Mutex` poisoning rather than panic — none of the holders
/// here can panic with the lock held, so this is defensive only.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod visible_project_tests {
    use super::*;

    #[test]
    fn a_stale_sequence_number_is_dropped() {
        // Rapid Settings → project → Git navigation issues overlapping writes.
        // Without this guard, the slowest one wins and the gate reasons about a
        // view the user has already left.
        let mut v = VisibleProject::default();
        let newer = ProjectId::from_u128(1);
        assert!(v.apply(Some(newer), 5));
        assert!(
            !v.apply(Some(ProjectId::from_u128(2)), 3),
            "older write loses"
        );
        assert_eq!(v.project_id, Some(newer));
    }

    #[test]
    fn an_equal_or_newer_sequence_number_applies() {
        let mut v = VisibleProject::default();
        assert!(v.apply(Some(ProjectId::from_u128(1)), 1));
        assert!(
            v.apply(None, 1),
            "a re-send of the current view still applies"
        );
        assert_eq!(v.project_id, None);
        assert!(v.apply(Some(ProjectId::from_u128(2)), 2));
        assert_eq!(v.project_id, Some(ProjectId::from_u128(2)));
    }
}

#[cfg(test)]
mod tests {
    use switchboard_dispatcher::RecordingEmitter;
    use switchboard_harness::MockHarnessAdapter;
    use tempfile::tempdir;

    use super::*;

    fn mock_state() -> AppState {
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter: Arc<dyn EventEmitter> = Arc::new(RecordingEmitter::new());
        AppState::new_for_test(Arc::clone(&mock), Arc::clone(&mock), mock, emitter)
    }

    #[test]
    fn persist_workspace_with_no_path_writes_nothing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");

        let state = mock_state();
        lock(&state.workspace).add(path.clone());
        persist_workspace(&state);

        assert!(!path.exists());
    }

    #[test]
    fn persist_workspace_with_path_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");

        let state = mock_state().with_workspace(path.clone());
        lock(&state.workspace).add(PathBuf::from("/some/dir"));
        persist_workspace(&state);

        let loaded = workspace::load(&path).workspace;
        assert_eq!(&loaded, &*lock(&state.workspace));
    }
}
