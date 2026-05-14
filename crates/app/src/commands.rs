//! Free-function implementations behind each Tauri command. The
//! `#[tauri::command]` wrappers in `lib.rs` are thin shims that adapt these
//! to Tauri's `State<'_, AppState>` / `String` conventions; the free
//! functions are what the unit tests target.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use switchboard_core::{
    AgentId, AgentRecord, Directory, HarnessKind, Project, ProjectId, ProjectSummary,
};
use switchboard_dispatcher::{DispatchHandle, EventEmitter};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::{AppState, lock};

/// Returned by `init_directory_impl` — gives the caller everything it needs
/// to render the directory header (path) and project list in one round trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryInfo {
    pub path: String,
    pub has_switchboard: bool,
    pub projects: Vec<ProjectSummary>,
}

/// Idempotent for the same path: creates `.switchboard/` if missing and
/// binds the directory in `AppState`. Re-binding to a *different* canonical
/// path clears the loaded-project cache and the active project, so the
/// frontend can't subsequently dispatch to an agent from the previous
/// directory (which would resolve to a now-stale `project.root`).
///
/// In-flight dispatches on agents from the prior directory keep running
/// (their `AgentIdleGuard` and event channels are dispatcher-owned and
/// agent-scoped) — graceful cleanup of those is M4 work.
pub async fn init_directory_impl(state: &AppState, path: &str) -> Result<DirectoryInfo, AppError> {
    let directory = Directory::at(Path::new(path))?;
    directory.init()?;
    let projects = directory.list_projects()?;
    let info = DirectoryInfo {
        path: directory.path.to_string_lossy().into_owned(),
        has_switchboard: directory.has_switchboard(),
        projects,
    };

    // Atomic-ish: if the new path differs from the current one, clear the
    // project caches before swapping in the new directory binding so the
    // loaded-projects/active-project state never references a directory we
    // are no longer bound to.
    {
        let mut current = lock(&state.directory);
        let rebinding = matches!(current.as_ref(), Some(d) if d.path != directory.path);
        if rebinding {
            lock(&state.projects).clear();
            *lock(&state.active_project_id) = None;
        }
        *current = Some(directory);
    }
    Ok(info)
}

pub fn list_projects_impl(state: &AppState) -> Result<Vec<ProjectSummary>, AppError> {
    let directory = bound_directory(state)?;
    Ok(directory.list_projects()?)
}

pub fn create_project_impl(state: &AppState, name: &str) -> Result<ProjectSummary, AppError> {
    let directory = bound_directory(state)?;
    let project = directory.create_project(name)?;
    let summary = ProjectSummary {
        id: project.id,
        name: project.config.name.clone(),
        created_at: project.config.created_at,
    };
    lock(&state.projects).insert(project.id, project);
    Ok(summary)
}

pub fn open_project_impl(
    state: &AppState,
    project_id: ProjectId,
) -> Result<ProjectSummary, AppError> {
    if let Some(loaded) = lock(&state.projects).get(&project_id) {
        return Ok(ProjectSummary {
            id: loaded.id,
            name: loaded.config.name.clone(),
            created_at: loaded.config.created_at,
        });
    }
    let directory = bound_directory(state)?;
    let project = directory.open_project(project_id)?;
    let summary = ProjectSummary {
        id: project.id,
        name: project.config.name.clone(),
        created_at: project.config.created_at,
    };
    lock(&state.projects).insert(project.id, project);
    Ok(summary)
}

pub fn set_active_project_impl(state: &AppState, project_id: ProjectId) -> Result<(), AppError> {
    if !lock(&state.projects).contains_key(&project_id) {
        return Err(AppError::ProjectNotLoaded(project_id));
    }
    *lock(&state.active_project_id) = Some(project_id);
    Ok(())
}

pub fn create_agent_impl(state: &AppState, name: &str) -> Result<AgentRecord, AppError> {
    let active = lock(&state.active_project_id).ok_or(AppError::NoActiveProject)?;
    let project = lock(&state.projects)
        .get(&active)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(active))?;
    Ok(project.register_agent(name, HarnessKind::ClaudeCode)?)
}

pub fn list_agents_impl(
    state: &AppState,
    project_id: Option<ProjectId>,
) -> Result<Vec<AgentRecord>, AppError> {
    let pid = match project_id {
        Some(p) => p,
        None => lock(&state.active_project_id).ok_or(AppError::NoActiveProject)?,
    };
    let project = lock(&state.projects)
        .get(&pid)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(pid))?;
    Ok(project.list_agents()?)
}

/// Resolves the agent (across all loaded projects) and dispatches the turn
/// through the dispatcher. Returns the `DispatchHandle` (`turn_id` + drain
/// `JoinHandle`) as soon as the adapter's `dispatch()` returns and
/// `TurnStart` has been emitted — the drain task continues in the
/// background.
///
/// The Tauri command shim discards the `JoinHandle`, returning just the
/// `TurnId` to the frontend; tests `.await` the handle for deterministic
/// drain completion (instead of polling agent status).
pub async fn send_message_impl(
    state: &AppState,
    agent_id: AgentId,
    prompt: &str,
) -> Result<DispatchHandle, AppError> {
    let (project, agent) = lookup_agent(state, agent_id)?;
    let handle = state
        .dispatcher
        .send_message(
            &agent,
            &project.root,
            prompt,
            state.adapter.as_ref(),
            Arc::clone(&state.emitter) as Arc<dyn EventEmitter>,
        )
        .await?;
    Ok(handle)
}

pub fn check_claude_binary_impl(state: &AppState) -> Result<(), AppError> {
    state.adapter.probe().map_err(AppError::Probe)
}

fn bound_directory(state: &AppState) -> Result<Directory, AppError> {
    lock(&state.directory)
        .as_ref()
        .cloned()
        .ok_or(AppError::NoDirectory)
}

fn lookup_agent(state: &AppState, agent_id: AgentId) -> Result<(Project, AgentRecord), AppError> {
    // Clone the project handles out so we don't hold the projects lock
    // while doing disk I/O via `list_agents`.
    let candidates: Vec<Project> = lock(&state.projects).values().cloned().collect();
    for project in candidates {
        let agents = project.list_agents()?;
        if let Some(agent) = agents.into_iter().find(|a| a.id == agent_id) {
            return Ok((project, agent));
        }
    }
    Err(AppError::AgentNotFound(agent_id))
}

pub(crate) fn parse_uuid(value: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|e| AppError::invalid_uuid(value, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use switchboard_dispatcher::RecordingEmitter;
    use switchboard_harness::{ClaudeCodeAdapter, HarnessAdapter, MockHarnessAdapter};
    use tempfile::TempDir;

    fn fresh_state_with_mock() -> (TempDir, AppState, Arc<RecordingEmitter>) {
        let tmp = TempDir::new().unwrap();
        let adapter: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(adapter, emitter.clone() as Arc<dyn EventEmitter>);
        (tmp, state, emitter)
    }

    #[tokio::test]
    async fn init_directory_creates_switchboard_layout() {
        let (tmp, state, _) = fresh_state_with_mock();
        let info = init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(info.has_switchboard);
        assert!(info.projects.is_empty());
        assert!(tmp.path().join(".switchboard").is_dir());
        assert!(tmp.path().join(".switchboard/config.yaml").is_file());
    }

    #[tokio::test]
    async fn init_directory_to_different_path_clears_projects_and_unbinds_old_agents() {
        // First directory: create a project + agent.
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        let adapter: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(adapter, emitter as Arc<dyn EventEmitter>);

        init_directory_impl(&state, tmp_a.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "assistant").unwrap();

        // Rebind to a different directory.
        let info_b = init_directory_impl(&state, tmp_b.path().to_str().unwrap())
            .await
            .unwrap();

        // Loaded-project state was cleared, active project unset.
        assert_eq!(info_b.projects.len(), 0);
        assert!(lock(&state.projects).is_empty());
        assert!(lock(&state.active_project_id).is_none());

        // The actual user-visible bug guard: sending to the old agent ID
        // now returns AgentNotFound (not a silent dispatch against the old
        // project.root).
        let err = send_message_impl(&state, agent.id, "should fail")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn init_directory_is_idempotent() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        // Second call must succeed and preserve any created projects.
        create_project_impl(&state, "alpha").unwrap();
        let info = init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "alpha");
    }

    #[test]
    fn list_projects_without_init_errors() {
        let (_tmp, state, _) = fresh_state_with_mock();
        let err = list_projects_impl(&state).unwrap_err();
        assert!(matches!(err, AppError::NoDirectory));
    }

    #[tokio::test]
    async fn create_open_set_active_round_trip() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let summary = create_project_impl(&state, "alpha").unwrap();
        // open_project on an already-loaded project is a no-op equivalent.
        let reopened = open_project_impl(&state, summary.id).unwrap();
        assert_eq!(reopened.id, summary.id);
        set_active_project_impl(&state, summary.id).unwrap();
        assert_eq!(
            *lock(&state.active_project_id),
            Some(summary.id),
            "active project set"
        );
    }

    #[tokio::test]
    async fn set_active_project_rejects_unloaded() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let unknown = Uuid::now_v7();
        let err = set_active_project_impl(&state, unknown).unwrap_err();
        assert!(matches!(err, AppError::ProjectNotLoaded(_)));
    }

    #[tokio::test]
    async fn create_agent_without_active_project_errors() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let err = create_agent_impl(&state, "assistant").unwrap_err();
        assert!(matches!(err, AppError::NoActiveProject));
    }

    #[tokio::test]
    async fn send_message_dispatches_and_emits_events() {
        let (tmp, state, emitter) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, project.id).unwrap();
        let agent = create_agent_impl(&state, "assistant").unwrap();

        let DispatchHandle { turn_id, join } =
            send_message_impl(&state, agent.id, "hello").await.unwrap();
        join.await.unwrap();

        let events = emitter.snapshot();
        assert!(!events.is_empty(), "expected events to be emitted");
        let channel = format!("agent:{}", agent.id);
        for (name, _) in &events {
            assert_eq!(name, &channel);
        }
        assert_eq!(events[0].1["type"], "turn_start");
        assert_eq!(events[0].1["turn_id"], turn_id.to_string());
        assert_eq!(
            state.dispatcher.agent_status(agent.id),
            Some(switchboard_dispatcher::AgentStatus::Idle)
        );
    }

    #[tokio::test]
    async fn send_message_unknown_agent_errors() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, project.id).unwrap();
        let err = send_message_impl(&state, Uuid::now_v7(), "hi")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::AgentNotFound(_)));
    }

    #[test]
    fn check_claude_binary_with_mock_adapter_returns_ok() {
        let (_tmp, state, _) = fresh_state_with_mock();
        assert!(check_claude_binary_impl(&state).is_ok());
    }

    #[test]
    fn check_claude_binary_with_missing_binary_returns_error() {
        let adapter: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::with_binary_path(
            "/nonexistent/claude-xyz",
        ));
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(adapter, emitter as Arc<dyn EventEmitter>);
        let err = check_claude_binary_impl(&state).unwrap_err();
        assert!(matches!(err, AppError::Probe(_)));
    }

    #[tokio::test]
    async fn list_agents_defaults_to_active_project() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj_a = create_project_impl(&state, "alpha").unwrap();
        let proj_b = create_project_impl(&state, "beta").unwrap();
        set_active_project_impl(&state, proj_a.id).unwrap();
        create_agent_impl(&state, "a-agent").unwrap();
        set_active_project_impl(&state, proj_b.id).unwrap();
        create_agent_impl(&state, "b-agent").unwrap();

        // Default = active project (beta).
        let agents = list_agents_impl(&state, None).unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "b-agent");

        // Explicit project id returns that project's agents.
        let agents_a = list_agents_impl(&state, Some(proj_a.id)).unwrap();
        assert_eq!(agents_a.len(), 1);
        assert_eq!(agents_a[0].name, "a-agent");
    }

    #[tokio::test]
    async fn cross_project_concurrent_send_no_cross_talk() {
        let (tmp, state, emitter) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj_a = create_project_impl(&state, "alpha").unwrap();
        let proj_b = create_project_impl(&state, "beta").unwrap();

        // Two projects in same directory; same agent name in each is fine.
        set_active_project_impl(&state, proj_a.id).unwrap();
        let agent_a = create_agent_impl(&state, "assistant").unwrap();
        set_active_project_impl(&state, proj_b.id).unwrap();
        let agent_b = create_agent_impl(&state, "assistant").unwrap();

        let (handle_a, handle_b) = tokio::join!(
            send_message_impl(&state, agent_a.id, "A's prompt"),
            send_message_impl(&state, agent_b.id, "B's prompt"),
        );
        let handle_a = handle_a.unwrap();
        let handle_b = handle_b.unwrap();
        handle_a.join.await.unwrap();
        handle_b.join.await.unwrap();

        let events = emitter.snapshot();
        let ch_a = format!("agent:{}", agent_a.id);
        let ch_b = format!("agent:{}", agent_b.id);
        let a_count = events.iter().filter(|(n, _)| n == &ch_a).count();
        let b_count = events.iter().filter(|(n, _)| n == &ch_b).count();
        assert_eq!(a_count, 5, "agent A's channel got the wrong event count");
        assert_eq!(b_count, 5, "agent B's channel got the wrong event count");
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        let err = parse_uuid("not-a-uuid").unwrap_err();
        assert!(matches!(err, AppError::InvalidUuid { .. }));
    }
}
