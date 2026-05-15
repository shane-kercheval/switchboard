//! Tauri-side application state. Owns the bound working directory, loaded
//! projects, dispatcher, and harness adapter for the lifetime of the app.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use switchboard_core::{Directory, Project, ProjectId};
use switchboard_dispatcher::{Dispatcher, EventEmitter};
use switchboard_harness::HarnessAdapter;

/// The single piece of state managed by Tauri. Multi-project from day 1 (per
/// system-design §3); M1 only loads one project at a time, but the shape
/// supports M4's project switcher without restructuring.
///
/// **Lock-order convention** (when more than one of these mutexes is held
/// at the same time): `registry_write` → `directory` → `projects` →
/// `active_project_id`. Always acquire in this order. Violating the order
/// can deadlock under concurrent access. Single-lock acquisitions (which
/// most callers do) are unaffected — the convention only matters when
/// nesting.
///
/// `registry_write` serializes append-only-log mutations
/// (`create_project`, `register_agent`, `init_directory`).
/// `Directory::create_project` and `Project::register_agent` have a TOCTOU
/// window between their internal "is this name unique?" read and the
/// subsequent append; two concurrent IPC calls could otherwise both pass
/// the uniqueness check and append colliding records. The mutex closes
/// that window inside one process; cross-process serialization is M4's
/// `instance.lock`.
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
    /// Adapter for `HarnessKind::ClaudeCode` agents. M2.3+: named fields per
    /// harness replace M1/M2's single `adapter` field so the routing rule
    /// (`send_message_impl` matches on `agent.harness`) is type-supported.
    pub claude_adapter: Arc<dyn HarnessAdapter>,
    /// Adapter for `HarnessKind::Codex` agents.
    pub codex_adapter: Arc<dyn HarnessAdapter>,
    pub emitter: Arc<dyn EventEmitter>,
}

impl AppState {
    pub fn new(
        claude_adapter: Arc<dyn HarnessAdapter>,
        codex_adapter: Arc<dyn HarnessAdapter>,
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
            emitter,
        }
    }
}

/// Recover from `Mutex` poisoning rather than panic — none of the holders
/// here can panic with the lock held, so this is defensive only.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
