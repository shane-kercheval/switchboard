//! The window half of the sync a turn-starting operation runs before it
//! dispatches.
//!
//! **Why a round trip at all.** The window learns about projects, agents, and
//! messages only through its own actions, and Tauri does not replay events: a
//! turn dispatched for a project the window has not loaded is invisible until
//! reload, and the window's first history load assumes no turn is running
//! while it reads. So before any turn starts, the backend asks the window to
//! load or refresh the project and waits until its roster, history load, and
//! event subscriptions are all ready. Reads, waits, and cancels never go
//! through this, so they keep working when the window is slow or hung.
//!
//! This module holds the seam. The production implementation (the Tauri
//! event and its acknowledging command) is a separate piece of work; until it
//! lands, [`NoopDesktopSync`] reports ready at once, which is safe only because
//! nothing reachable from the window calls the turn tier of `ops`.

use async_trait::async_trait;
use switchboard_core::{AgentId, ProjectId};

use crate::error::AppError;

/// What the window reports once a project is ready for a turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyReport {
    /// Agents whose event subscription failed in the window. A send to one of
    /// them is refused per recipient (`desktop_unavailable`) rather than run
    /// invisibly, matching the window's own refusal to dispatch into a fork
    /// whose subscription failed.
    pub unsubscribed_agents: Vec<AgentId>,
}

#[async_trait]
pub trait DesktopSync: Send + Sync {
    /// Ask the window to load or refresh `project_id` and wait until it is
    /// ready for a turn. The turn tier calls this after opening the project in
    /// Rust and before dispatching; an error means nothing is dispatched.
    async fn ensure_ready(&self, project_id: ProjectId) -> Result<ReadyReport, AppError>;

    /// Tell the window a project's roster changed. Not awaited: correctness
    /// does not depend on it, because the blocking sync before the next turn
    /// registers any new agent anyway.
    fn roster_changed(&self, project_id: ProjectId);

    /// Tell the window the project list changed. Not awaited, for the same
    /// reason as [`Self::roster_changed`].
    fn projects_changed(&self);

    /// Tell the window a project's workflow runs changed (a run was dismissed
    /// over MCP), so it stops showing the dismissed run as blocking. Not
    /// awaited.
    fn runs_changed(&self, project_id: ProjectId);
}

/// Reports ready immediately and tells the window nothing. The default until
/// the production round trip exists, and the double for tests that are not
/// about the window.
pub struct NoopDesktopSync;

#[async_trait]
impl DesktopSync for NoopDesktopSync {
    async fn ensure_ready(&self, _project_id: ProjectId) -> Result<ReadyReport, AppError> {
        Ok(ReadyReport::default())
    }

    fn roster_changed(&self, _project_id: ProjectId) {}

    fn projects_changed(&self) {}

    fn runs_changed(&self, _project_id: ProjectId) {}
}
