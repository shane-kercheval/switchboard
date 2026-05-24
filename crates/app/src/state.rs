//! Tauri-side application state. Owns the bound working directory, loaded
//! projects, dispatcher, and harness adapter for the lifetime of the app.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use switchboard_core::{AgentId, AgentRecord, Directory, Project, ProjectId};
use switchboard_dispatcher::{Dispatcher, EventEmitter};
use switchboard_harness::HarnessAdapter;

use crate::workspace::{self, Workspace};

/// The single piece of state managed by Tauri. Multi-project from day 1 (per
/// system-design §3); only one project is loaded at a time today, but the
/// shape supports a future project switcher without restructuring.
///
/// **Lock-order convention** (when more than one of these mutexes is held
/// at the same time): `workspace` → `registry_write` → `directory` →
/// `projects` → `active_project_id` → `needs_session_meta` →
/// `project_locks` → `agents_by_id`. Always acquire in this order. `workspace`
/// is at the head because it is the app-owned user-global registry that sits
/// above any single directory's state; today it is only ever acquired alone
/// (no nesting yet), but placing it first keeps a future nesting compliant.
/// Violating the order can
/// deadlock under concurrent access. Single-lock acquisitions (which most
/// callers do) are unaffected — the convention only matters when nesting.
/// `needs_session_meta` is the tail because both `attach_agent_impl` (under
/// `registry_write`) and `send_message_impl` (no other locks held) acquire
/// it briefly with no `.await` crossing the guard. `project_locks` and
/// `agents_by_id` (M4.1) are leaf maps acquired briefly; they are never
/// nested with *each other*, but they are taken while `directory` is held
/// during a rebind (and while `registry_write` is held during open/create) —
/// both of which precede them in the order, so those nestings are compliant.
/// When nesting them, follow the documented tail order.
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
    pub directory: Mutex<Option<Directory>>,
    pub projects: Mutex<HashMap<ProjectId, Project>>,
    pub active_project_id: Mutex<Option<ProjectId>>,
    /// Acquired around any operation that appends to a JSONL on disk
    /// (`projects.jsonl` or a project's `registry.jsonl`). `std::sync::Mutex`
    /// because the protected work is fully synchronous — no `.await` while
    /// the guard is held.
    pub registry_write: Mutex<()>,
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
    /// **Rebind clearing.** Cleared by `init_directory_impl` alongside
    /// `projects` and `active_project_id` — a stale `agent_id` from a
    /// previous directory's attach must not leak across rebinds.
    pub needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,

    /// Per-project inter-process lock handles (M4.1). One entry per loaded
    /// project, holding an advisory exclusive lock (std `File::try_lock`,
    /// stable since Rust 1.89 — `flock` on unix) on
    /// `<directory>/.switchboard/projects/<id>/instance.lock`. Acquired in
    /// the project-open/create path before the project is inserted into
    /// `projects`; the live `File` *is* the lock, so dropping it (removing
    /// the entry on rebind, or the process exiting/crashing) releases the
    /// lock — no explicit unlock or stale-lock cleanup needed. This is an
    /// inter-process guard only: a second Switchboard process opening the
    /// same project is refused (`AppError::ProjectLocked`); intra-process
    /// re-open returns the already-loaded handle without re-locking.
    pub project_locks: Mutex<HashMap<ProjectId, File>>,

    /// Canonical agent-lookup index (M4.1): `AgentId → AgentRecord`. The
    /// record carries `project_id`, so this single map answers "which
    /// project owns this agent, and what is its record" without scanning
    /// every loaded project's `registry.jsonl` from disk (the prior
    /// `lookup_agent` hot path). Populated on project open, agent
    /// register/attach, and `list_agents`; cleared on directory rebind.
    /// v1 has no agent/project deletion, so invalidation is insert-only
    /// within a directory session plus a full clear on rebind. `AgentRecord`
    /// is immutable after registration, so a cached copy never goes stale.
    pub agents_by_id: Mutex<HashMap<AgentId, AgentRecord>>,

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
            directory: Mutex::new(None),
            projects: Mutex::new(HashMap::new()),
            active_project_id: Mutex::new(None),
            registry_write: Mutex::new(()),
            dispatcher: Arc::new(Dispatcher::new()),
            claude_adapter,
            codex_adapter,
            gemini_adapter,
            antigravity_adapter,
            emitter,
            needs_session_meta: Arc::new(Mutex::new(HashSet::new())),
            project_locks: Mutex::new(HashMap::new()),
            agents_by_id: Mutex::new(HashMap::new()),
            workspace: Mutex::new(Workspace::default()),
            workspace_path: None,
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
}

/// Persist the workspace registry to disk if a `workspace_path` is configured.
/// Best-effort: a `None` path is a no-op (tests), and a save failure is logged
/// rather than propagated — the registry is convenience state, like the cached
/// project snapshot it holds, and must not break the operation that triggered
/// the save.
// Lands ahead of its production callers (the next M4.6 increment calls this
// after directory add/remove). Exercised by this module's tests today.
#[allow(dead_code)]
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
