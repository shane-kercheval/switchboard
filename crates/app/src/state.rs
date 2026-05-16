//! Tauri-side application state. Owns the bound working directory, loaded
//! projects, dispatcher, and harness adapter for the lifetime of the app.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use switchboard_core::{AgentId, Directory, Project, ProjectId};
use switchboard_dispatcher::{Dispatcher, EventEmitter};
use switchboard_harness::HarnessAdapter;

/// The single piece of state managed by Tauri. Multi-project from day 1 (per
/// system-design §3); M1 only loads one project at a time, but the shape
/// supports M4's project switcher without restructuring.
///
/// **Lock-order convention** (when more than one of these mutexes is held
/// at the same time): `registry_write` → `directory` → `projects` →
/// `active_project_id` → `pending_first_dispatch`. Always acquire in this
/// order. Violating the order can deadlock under concurrent access.
/// Single-lock acquisitions (which most callers do) are unaffected — the
/// convention only matters when nesting. `pending_first_dispatch` is the
/// tail because both `attach_agent_impl` (under `registry_write`) and
/// `send_message_impl` (no other locks held) acquire it briefly with no
/// `.await` crossing the guard.
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
    /// One-shot set of `agent_id`s whose next dispatch must run with
    /// `DispatchOptions::is_first_dispatch_after_attach = true`. Populated by
    /// `attach_agent_impl`; drained-and-passed-through by `send_message_impl`
    /// on the next dispatch for the same agent.
    ///
    /// **Purpose.** The Codex attach-existing-session flow pre-writes a
    /// sidecar record at attach time. Without this flag, the Codex adapter
    /// would see `prior.is_some()` on its first post-attach dispatch and
    /// skip `SessionMeta` emission — leaving the sidebar's MCP/skills/model
    /// listing empty until some other code path triggered emission. The
    /// flag tells the adapter "force `SessionMeta` even though the sidecar
    /// is non-empty."
    ///
    /// **Restore-on-Err.** `send_message_impl` drains the flag *before*
    /// awaiting `dispatcher.send_message`. On pre-stream `Err` (binary
    /// missing, spawn failure), the flag is re-inserted so a retry still
    /// forces `SessionMeta`. Mid-stream failures (adapter spawned Ok,
    /// stream aborted before `emit_terminal_with_enrichment`) are **not**
    /// covered — the flag is gone, `SessionMeta` was never emitted, and
    /// the agent's sidebar stays empty for its lifetime. Workaround:
    /// re-attach (pure metadata op, no harness invocation). Revisit if
    /// real users hit this.
    ///
    /// **Rebind clearing.** Cleared by `init_directory_impl` alongside
    /// `projects` and `active_project_id` — a stale `agent_id` from a
    /// previous directory's attach must not leak across rebinds.
    pub pending_first_dispatch: Mutex<HashSet<AgentId>>,
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
            pending_first_dispatch: Mutex::new(HashSet::new()),
        }
    }
}

/// Recover from `Mutex` poisoning rather than panic — none of the holders
/// here can panic with the lock held, so this is defensive only.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
