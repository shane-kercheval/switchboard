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
/// `active_project_id` → `needs_session_meta`. Always acquire in this
/// order. Violating the order can deadlock under concurrent access.
/// Single-lock acquisitions (which most callers do) are unaffected — the
/// convention only matters when nesting. `needs_session_meta` is the
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
            needs_session_meta: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

/// Recover from `Mutex` poisoning rather than panic — none of the holders
/// here can panic with the lock held, so this is defensive only.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
