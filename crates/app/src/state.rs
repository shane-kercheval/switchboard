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
pub struct AppState {
    pub directory: Mutex<Option<Directory>>,
    pub projects: Mutex<HashMap<ProjectId, Project>>,
    pub active_project_id: Mutex<Option<ProjectId>>,
    pub dispatcher: Arc<Dispatcher>,
    pub adapter: Arc<dyn HarnessAdapter>,
    pub emitter: Arc<dyn EventEmitter>,
}

impl AppState {
    pub fn new(adapter: Arc<dyn HarnessAdapter>, emitter: Arc<dyn EventEmitter>) -> Self {
        Self {
            directory: Mutex::new(None),
            projects: Mutex::new(HashMap::new()),
            active_project_id: Mutex::new(None),
            dispatcher: Arc::new(Dispatcher::new()),
            adapter,
            emitter,
        }
    }
}

/// Recover from `Mutex` poisoning rather than panic — none of the holders
/// here can panic with the lock held, so this is defensive only.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
