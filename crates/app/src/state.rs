//! Tauri-side application state. Owns the bound working directory, loaded
//! projects, dispatcher, and harness adapter for the lifetime of the app.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use switchboard_core::{AgentId, AgentRecord, Directory, Project, ProjectId};
use switchboard_dispatcher::{Dispatcher, EventEmitter};
use switchboard_harness::HarnessAdapter;

use crate::git_registry::{self, GitRegistry};
use crate::preferences::{self, Preferences};
use crate::workspace::{self, Workspace};

/// The single piece of state managed by Tauri. Multi-project and
/// multi-directory (per system-design §3): the app holds N working directories
/// concurrently, each hosting N projects. `directories` keys every loaded
/// `Directory` handle by its canonical path.
///
/// **Lock-order convention** (when more than one of these mutexes is held
/// at the same time): `workspace` → `registry_write` → `git_registry` →
/// `directories` → `projects` → `active_project_id` → `needs_session_meta` →
/// `project_locks` → `agents_by_id`. Always acquire in this order. `workspace`
/// is at the head because it is the app-owned user-global registry that sits
/// above any single directory's state; in practice it is taken either standalone
/// (`list_projects`, the workspace switcher) or nested *under* `registry_write`
/// in `init_directory` (which holds `registry_write` for its whole body) — never
/// the inverse. `git_registry` (the Git-view tracked-repo list) follows the same
/// shape: standalone for the Git-view read/add/remove commands, and nested under
/// `registry_write` during `init_directory`'s auto-sync hook — so it sorts after
/// `registry_write` here. **No path may acquire `registry_write` while holding
/// `git_registry`** (the inverse order is the deadlock this convention forbids).
/// `directories` holds every loaded directory keyed by canonical path; it sits
/// below the registries and above the per-project maps. Violating the order can
/// deadlock under concurrent access. Single-lock acquisitions (which most
/// callers do) are unaffected — the convention only matters when nesting.
/// `needs_session_meta` is the tail because both `attach_agent_impl` (under
/// `registry_write`) and `send_message_impl` (no other locks held) acquire
/// it briefly with no `.await` crossing the guard. `project_locks` and
/// `agents_by_id` (M4.1) are leaf maps acquired briefly; they are taken while
/// `registry_write` is held during open/create/remove — which precedes them in
/// the order, so those nestings are compliant.
/// When nesting them, follow the documented tail order.
///
/// `preferences` is a **standalone leaf**: it is only ever acquired by
/// `get_preferences` / `set_preferences`, never nested with another state lock.
/// `set_preferences_impl` deliberately holds it **across its `config.yaml`
/// write** to serialize saves (the temp file is a fixed path, so concurrent
/// unserialized writes would corrupt it). Because it's a leaf taken alone, no
/// other lock may be acquired while holding it — keep it that way.
///
/// `registry_write` serializes append-only-log mutations
/// (`create_project`, `register_agent`, `init_directory`).
/// `Directory::create_project` and `Project::register_agent` have a TOCTOU
/// window between their internal "is this name unique?" read and the
/// subsequent append; two concurrent IPC calls could otherwise both pass
/// the uniqueness check and append colliding records. The mutex closes
/// that window inside one process; cross-process serialization is future
/// work (an `instance.lock` per directory).
pub struct AppState {
    /// Every loaded working directory, keyed by its **canonical** path
    /// (`Directory::at` canonicalizes, so `Directory.path` is the key). The
    /// app holds N directories concurrently; commands resolve the directory
    /// that owns a project from the project's own `directory` field, not a
    /// single bound handle.
    ///
    /// **Session-id uniqueness contract.** Cross-harness session-id uniqueness
    /// (the same-session-parallel-invocation guard) is enforced across all
    /// *loaded/available* directories — those present in this map. At startup
    /// `eager_load_directories` opens a handle for every available workspace
    /// directory, so "loaded == available" and the Codex/Antigravity collision
    /// scan is workspace-wide. An unavailable directory (absent from this map)
    /// cannot collide because it cannot be dispatched into while unavailable.
    pub directories: Mutex<HashMap<PathBuf, Directory>>,
    pub projects: Mutex<HashMap<ProjectId, Project>>,
    pub active_project_id: Mutex<Option<ProjectId>>,
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
    /// Adapter for `HarnessKind::Gemini` agents.
    pub gemini_adapter: Arc<dyn HarnessAdapter>,
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
    /// **Removal clearing.** `remove_directory_impl` drops the entries for the
    /// removed directory's agents alongside the matching `projects` and
    /// `agents_by_id` entries — a stale `agent_id` from a removed directory's
    /// attach must not leak forward.
    pub needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,

    /// Per-project inter-process lock handles (M4.1). One entry per loaded
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

    /// Canonical agent-lookup index (M4.1): `AgentId → AgentRecord`. The
    /// record carries `project_id`, so this single map answers "which
    /// project owns this agent, and what is its record" without scanning
    /// every loaded project's `registry.jsonl` from disk (the prior
    /// `lookup_agent` hot path). Populated on project open, agent
    /// register/attach, and `list_agents`; the removed directory's entries are
    /// dropped on `remove_directory`. v1 has no agent/project deletion, so
    /// invalidation is insert-only within a session plus a targeted prune when
    /// a directory is removed. An `AgentRecord` is otherwise immutable after
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
    pub preferences: Mutex<Preferences>,

    /// Resolved path of `config.yaml`, or `None` when no global location was
    /// resolved (tests, exotic host). `set_preferences` errors-as-noop persist
    /// while this is `None`, so tests never touch user-global state.
    pub preferences_path: Option<PathBuf>,
}

impl AppState {
    pub fn new(
        claude_adapter: Arc<dyn HarnessAdapter>,
        codex_adapter: Arc<dyn HarnessAdapter>,
        gemini_adapter: Arc<dyn HarnessAdapter>,
        antigravity_adapter: Arc<dyn HarnessAdapter>,
        emitter: Arc<dyn EventEmitter>,
    ) -> Self {
        Self {
            directories: Mutex::new(HashMap::new()),
            projects: Mutex::new(HashMap::new()),
            active_project_id: Mutex::new(None),
            registry_write: Arc::new(Mutex::new(())),
            dispatcher: Arc::new(Dispatcher::new()),
            claude_adapter,
            codex_adapter,
            gemini_adapter,
            antigravity_adapter,
            emitter,
            needs_session_meta: Arc::new(Mutex::new(HashSet::new())),
            project_locks: Mutex::new(HashMap::new()),
            agents_by_id: Arc::new(Mutex::new(HashMap::new())),
            workspace: Mutex::new(Workspace::default()),
            workspace_path: None,
            git_registry: Mutex::new(GitRegistry::default()),
            git_registry_path: None,
            preferences: Mutex::new(Preferences::default()),
            preferences_path: None,
        }
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
        self.preferences = Mutex::new(preferences::load(&path));
        self.preferences_path = Some(path);
        self
    }
}

/// Eager-load every workspace directory's `Directory` handle into
/// `state.directories` at startup. `with_workspace` only loads the workspace
/// registry (paths + cached snapshots); without this, nothing populates
/// `directories` on a cold start (its only other writer is `init_directory`), so
/// every directory would report `available: false` until re-initialized and the
/// cross-harness session-id collision scan would cover no directories.
///
/// For each workspace entry we attempt `Directory::at` — a pure on-disk read
/// that canonicalizes and validates the path is a directory. **No per-project
/// `instance.lock` is taken here**: locks stay lazy, acquired on project
/// activation (open/create), not at startup. A directory that fails to open
/// (unmounted, moved, permissions) is skipped with a `warn` and stays absent
/// from `directories` → it naturally reports `available: false` and cannot be
/// dispatched into. Startup never aborts on a bad directory.
///
/// Contract this establishes: after eager-load, "loaded == every available
/// workspace directory," which is what makes the Codex/Antigravity session-id
/// collision scan (`enumerate_all_projects` over `state.directories`)
/// workspace-wide for all available directories.
pub(crate) fn eager_load_directories(state: &AppState) {
    let paths: Vec<PathBuf> = lock(&state.workspace)
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    let mut directories = lock(&state.directories);
    for path in paths {
        match Directory::at(&path) {
            Ok(directory) => {
                directories.insert(directory.path.clone(), directory);
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "workspace directory could not be opened at startup — marking unavailable"
                );
            }
        }
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
mod tests {
    use switchboard_dispatcher::RecordingEmitter;
    use switchboard_harness::MockHarnessAdapter;
    use tempfile::tempdir;

    use super::*;

    fn mock_state() -> AppState {
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter: Arc<dyn EventEmitter> = Arc::new(RecordingEmitter::new());
        AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            mock,
            emitter,
        )
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
    fn eager_load_opens_every_available_workspace_directory() {
        // Given a workspace with two real directories, after eager-load both are
        // present in `state.directories` (keyed by their canonical paths).
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let state = mock_state();
        {
            let mut ws = lock(&state.workspace);
            ws.add(dir_a.path().to_path_buf());
            ws.add(dir_b.path().to_path_buf());
        }

        eager_load_directories(&state);

        let dirs = lock(&state.directories);
        assert_eq!(dirs.len(), 2, "both available directories loaded");
        assert!(dirs.contains_key(&dir_a.path().canonicalize().unwrap()));
        assert!(dirs.contains_key(&dir_b.path().canonicalize().unwrap()));
    }

    #[test]
    fn eager_load_skips_unavailable_directory() {
        // A workspace entry whose path no longer exists is skipped (stays
        // absent → available:false), and does not abort the load of the rest.
        let dir_a = tempdir().unwrap();
        let missing = dir_a.path().join("gone");
        let state = mock_state();
        {
            let mut ws = lock(&state.workspace);
            ws.add(dir_a.path().to_path_buf());
            ws.add(missing);
        }

        eager_load_directories(&state);

        let dirs = lock(&state.directories);
        assert_eq!(dirs.len(), 1, "only the available directory loaded");
        assert!(dirs.contains_key(&dir_a.path().canonicalize().unwrap()));
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
