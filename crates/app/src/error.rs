//! Typed errors for command free-functions. The Tauri `#[tauri::command]`
//! wrapper maps these to `String` at the IPC boundary (Tauri convention).

use switchboard_core::{AgentId, CoreError, ProjectId};
use switchboard_dispatcher::DispatcherError;
use switchboard_harness::DispatchError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] CoreError),

    #[error(transparent)]
    Dispatcher(#[from] DispatcherError),

    /// Pre-dispatch harness check failed (e.g., binary not on PATH).
    /// Distinct from `Dispatcher` because the call site is the adapter's
    /// `probe()` method — not dispatcher business — and the frontend gates
    /// behaviour on it (`check_claude_binary` banner) independently of any
    /// dispatch attempt.
    #[error("harness probe failed: {0}")]
    Probe(DispatchError),

    #[error("no working directory has been initialised — call init_directory first")]
    NoDirectory,

    #[error("no active project — call set_active_project first")]
    NoActiveProject,

    #[error("project {0} is not loaded")]
    ProjectNotLoaded(ProjectId),

    #[error("agent {0} not found in any loaded project")]
    AgentNotFound(AgentId),

    #[error("invalid UUID {value:?}: {source}")]
    InvalidUuid {
        value: String,
        #[source]
        source: uuid::Error,
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
