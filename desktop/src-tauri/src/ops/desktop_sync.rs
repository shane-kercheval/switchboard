//! Keeping the window in step with what another client does.
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
//! Changes that start no turn (an agent created or renamed, a project
//! created, a run dismissed) only notify the window. Correctness does not
//! depend on those notifications: the blocking sync before the next turn
//! registers any new agent anyway. They exist so the window shows the change
//! without waiting for a turn.
//!
//! [`WindowSync`] is the production implementation; [`NoopDesktopSync`] and
//! the tests' double stand in where no window exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use switchboard_core::{AgentId, ProjectId};
use switchboard_dispatcher::EventEmitter;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::lock;

/// Which agents' events the window is listening to.
///
/// **A list of the agents heard, not of the ones missed.** A turn for an agent
/// the window is not listening to runs invisibly, and Tauri does not replay
/// events, so it stays invisible. Naming the heard agents refuses every other
/// one by construction, including an agent created after the window read its
/// roster: a workflow binds its steps' agents by name against a roster read
/// after the window answered, so such an agent can be a recipient.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Hearing {
    /// There is no window to miss anything: a state built without one, or a
    /// test that is not about the window.
    #[default]
    Everyone,
    /// Exactly these agents.
    Only(Vec<AgentId>),
}

impl Hearing {
    #[must_use]
    pub fn hears(&self, agent_id: AgentId) -> bool {
        match self {
            Self::Everyone => true,
            Self::Only(agents) => agents.contains(&agent_id),
        }
    }

    /// The agents among `agents` the window is not listening to.
    #[must_use]
    pub fn unheard(&self, agents: impl IntoIterator<Item = AgentId>) -> Vec<AgentId> {
        agents.into_iter().filter(|a| !self.hears(*a)).collect()
    }
}

/// What the window reports once a project is ready for a turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyReport {
    /// A send to an agent the window does not hear is refused per recipient
    /// (`desktop_unavailable`) rather than run invisibly, matching the
    /// window's own refusal to dispatch into a fork whose subscription failed;
    /// a workflow whose steps reach one is refused whole.
    pub hearing: Hearing,
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

/// Reports ready immediately and tells the window nothing. For states built
/// without a window, and for tests that are not about the window.
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

/// The window's answer to a `desktop_sync_project` request, as its
/// acknowledging command carries it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SyncAck {
    /// Nothing is loading and every subscription has settled; these are the
    /// agents whose events the window is listening to.
    Ready { listening_agents: Vec<AgentId> },
    /// The window's open or roster read failed.
    Failed { message: String },
}

/// How long the backend waits for the window's acknowledgement. The window's
/// load keeps running past it, so a retry usually succeeds.
pub const SYNC_DEADLINE: Duration = Duration::from_secs(10);

/// One request waiting for the window.
struct Waiting {
    project_id: ProjectId,
    answer: oneshot::Sender<SyncAck>,
}

type Pending = Mutex<HashMap<Uuid, Waiting>>;

/// A request still waiting for the window's answer, as the window fetches it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingSync {
    pub request_id: Uuid,
    pub project_id: ProjectId,
}

/// The production round trip: emits `desktop_sync_project { request_id,
/// project_id }` to the window and waits for the matching
/// [`WindowSync::acknowledge`], which the window's `desktop_project_synced`
/// command calls.
///
/// A request stays pending only while someone waits for it. When the waiting
/// future is dropped (the send was cancelled mid-round-trip, or a deadline
/// passed), its request is removed, and an acknowledgement that arrives
/// afterwards is ignored.
///
/// **The window can also fetch what is waiting** ([`WindowSync::pending`]).
/// The request is an event, and Tauri does not replay events, so a request
/// emitted before the window is listening (its first seconds after launch,
/// or a reload) would otherwise be lost and the send would time out. The
/// window fetches the waiting requests once it is listening and answers them
/// like the event; a request answered twice is answered once, since the
/// second answer finds nothing waiting.
pub struct WindowSync {
    window: Arc<dyn EventEmitter>,
    deadline: Duration,
    pending: Pending,
}

impl WindowSync {
    #[must_use]
    pub fn new(window: Arc<dyn EventEmitter>) -> Self {
        Self::with_deadline(window, SYNC_DEADLINE)
    }

    #[must_use]
    pub fn with_deadline(window: Arc<dyn EventEmitter>, deadline: Duration) -> Self {
        Self {
            window,
            deadline,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Deliver the window's answer to a pending request. Returns whether a
    /// request was waiting for it; one that is no longer pending is ignored.
    pub fn acknowledge(&self, request_id: Uuid, ack: SyncAck) -> bool {
        let waiter = lock(&self.pending).remove(&request_id);
        // The waiter may also have been dropped between the removal and this
        // send; that is the same late acknowledgement, and also ignored.
        if let Some(waiting) = waiter
            && waiting.answer.send(ack).is_ok()
        {
            return true;
        }
        tracing::debug!(%request_id, "ignoring a window sync acknowledgement nobody is waiting for");
        false
    }

    /// The requests waiting for the window's answer.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingSync> {
        lock(&self.pending)
            .iter()
            .map(|(request_id, waiting)| PendingSync {
                request_id: *request_id,
                project_id: waiting.project_id,
            })
            .collect()
    }

    /// How many requests are waiting for the window.
    #[must_use]
    pub fn pending_requests(&self) -> usize {
        lock(&self.pending).len()
    }
}

/// Removes its request when the waiting future ends, however it ends.
struct PendingGuard<'a> {
    pending: &'a Pending,
    request_id: Uuid,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        lock(self.pending).remove(&self.request_id);
    }
}

#[async_trait]
impl DesktopSync for WindowSync {
    async fn ensure_ready(&self, project_id: ProjectId) -> Result<ReadyReport, AppError> {
        let request_id = Uuid::now_v7();
        let (answer, rx) = oneshot::channel();
        lock(&self.pending).insert(request_id, Waiting { project_id, answer });
        let _guard = PendingGuard {
            pending: &self.pending,
            request_id,
        };
        let started = Instant::now();
        self.window.emit(
            "desktop_sync_project",
            serde_json::json!({ "request_id": request_id, "project_id": project_id }),
        );
        let answer = tokio::time::timeout(self.deadline, rx).await;
        let elapsed_ms = started.elapsed().as_millis();
        match answer {
            Ok(Ok(SyncAck::Ready { listening_agents })) => {
                tracing::info!(%project_id, elapsed_ms, listening = listening_agents.len(), "window ready for a turn");
                Ok(ReadyReport {
                    hearing: Hearing::Only(listening_agents),
                })
            }
            Ok(Ok(SyncAck::Failed { message })) => {
                tracing::info!(%project_id, elapsed_ms, "window could not load the project");
                Err(AppError::DesktopSyncFailed {
                    project_id,
                    message,
                })
            }
            // Only `acknowledge` removes a request while it is awaited, and it
            // sends before dropping the sender.
            Ok(Err(_)) => Err(AppError::Internal(
                "the window sync request was dropped unanswered".to_owned(),
            )),
            Err(_) => {
                tracing::info!(%project_id, elapsed_ms, "window did not answer the sync in time");
                Err(AppError::ProjectLoadTimeout(project_id))
            }
        }
    }

    fn roster_changed(&self, project_id: ProjectId) {
        self.window.emit(
            "desktop_roster_changed",
            serde_json::json!({ "project_id": project_id }),
        );
    }

    fn projects_changed(&self) {
        self.window
            .emit("projects_changed", serde_json::Value::Null);
    }

    fn runs_changed(&self, project_id: ProjectId) {
        self.window.emit(
            "desktop_runs_changed",
            serde_json::json!({ "project_id": project_id }),
        );
    }
}
