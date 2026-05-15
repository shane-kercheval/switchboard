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
use switchboard_harness::HarnessAdapter;
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

/// Read-only inspection. Canonicalizes the path, checks whether
/// `.switchboard/` already exists, and lists projects if it does. **Does
/// not** create directories, write files, or modify `AppState` — the
/// frontend uses this to show the appropriate post-folder-picker CTA
/// (init / create-project / select-project) before committing.
pub async fn pick_directory_impl(path: &str) -> Result<DirectoryInfo, AppError> {
    let directory = Directory::at(Path::new(path))?;
    let has_switchboard = directory.has_switchboard();
    let projects = if has_switchboard {
        // Reject incompatible directory config versions before listing
        // projects. The version field exists explicitly so a future v2
        // schema can't be silently accepted by a v1 build.
        directory.config()?;
        directory.list_projects()?
    } else {
        Vec::new()
    };
    Ok(DirectoryInfo {
        path: directory.path.to_string_lossy().into_owned(),
        has_switchboard,
        projects,
    })
}

/// Idempotent for the same path: creates `.switchboard/` if missing and
/// binds the directory in `AppState`. Re-binding to a *different* canonical
/// path clears the loaded-project cache and the active project, so the
/// frontend can't subsequently dispatch to an agent from the previous
/// directory (which would resolve to a now-stale `project.directory`).
///
/// In-flight dispatches on agents from the prior directory keep running
/// (their `AgentIdleGuard` and event channels are dispatcher-owned and
/// agent-scoped) — graceful cleanup of those is M4 work.
pub async fn init_directory_impl(state: &AppState, path: &str) -> Result<DirectoryInfo, AppError> {
    // Serialize against concurrent registry writes (create_project,
    // register_agent). init_directory creates `.switchboard/` structure
    // and writes the directory's config.yaml — both modify the registry's
    // on-disk shape.
    let _write = lock(&state.registry_write);
    let directory = Directory::at(Path::new(path))?;
    directory.init()?;
    // Validate the directory's config version after init (init creates a
    // fresh v1 config if missing; this catches the case where the user
    // points at a directory with an incompatible existing config).
    directory.config()?;
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
    // Serialize the uniqueness check + JSONL append against concurrent
    // `create_project` / `register_agent` / `init_directory` calls. Without
    // this, two concurrent IPC calls could both pass the canonical-name
    // uniqueness check (which reads disk) and then both append colliding
    // records (which write disk).
    let _write = lock(&state.registry_write);
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

pub fn create_agent_impl(
    state: &AppState,
    name: &str,
    harness: HarnessKind,
) -> Result<AgentRecord, AppError> {
    // Same TOCTOU protection as create_project_impl — register_agent has
    // an internal read-check-then-append window that two concurrent IPC
    // calls could race through.
    let _write = lock(&state.registry_write);
    let active = lock(&state.active_project_id).ok_or(AppError::NoActiveProject)?;
    let project = lock(&state.projects)
        .get(&active)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(active))?;
    Ok(project.register_agent(name, harness)?)
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
    // Claude is spawned with cwd = the user's bound working directory (the
    // folder they opened), NOT the per-project metadata directory inside
    // `.switchboard/projects/<uuid>/`. The working directory is what
    // contains the user's actual code that claude needs to see via its
    // Read/Glob/Bash tools — the metadata directory is just where
    // Switchboard stores its own state. Multiple projects in the same
    // working directory share the same cwd; their per-agent sessions are
    // distinguished by session UUID, which is unique per agent.
    // M2.3 routing: select the adapter by agent.harness. The dispatcher is
    // harness-agnostic (keyed by AgentId); the match here is the substantive
    // failure surface — a regression that routes Codex through the Claude
    // adapter would silently spawn the wrong binary. App routing test in
    // the test module below pins this against regression.
    let adapter: &dyn HarnessAdapter = match agent.harness {
        HarnessKind::ClaudeCode => state.claude_adapter.as_ref(),
        HarnessKind::Codex => state.codex_adapter.as_ref(),
        _ => return Err(AppError::UnsupportedHarness),
    };
    let handle = state
        .dispatcher
        .send_message(
            &agent,
            &project.directory,
            prompt,
            adapter,
            Arc::clone(&state.emitter) as Arc<dyn EventEmitter>,
        )
        .await?;
    Ok(handle)
}

pub fn check_claude_binary_impl(state: &AppState) -> Result<(), AppError> {
    state.claude_adapter.probe().map_err(AppError::Probe)
}

pub fn check_codex_binary_impl(state: &AppState) -> Result<(), AppError> {
    state.codex_adapter.probe().map_err(AppError::Probe)
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

    use async_trait::async_trait;
    use switchboard_core::CoreError;
    use switchboard_dispatcher::RecordingEmitter;
    use switchboard_harness::{ClaudeCodeAdapter, HarnessAdapter, MockHarnessAdapter};
    use tempfile::TempDir;

    fn fresh_state_with_mock() -> (TempDir, AppState, Arc<RecordingEmitter>) {
        let tmp = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter.clone() as Arc<dyn EventEmitter>,
        );
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
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        );

        init_directory_impl(&state, tmp_a.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "assistant", HarnessKind::ClaudeCode).unwrap();

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
        // project's cwd).
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
        let err = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
        )
        .unwrap_err();
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
        let agent = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
        )
        .unwrap();

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
    async fn pick_directory_rejects_incompatible_config_version() {
        // Set up a directory with a v99 config — `Directory::config()`
        // returns UnsupportedConfigVersion which we want propagated up
        // through pick_directory so the user can't proceed against a
        // future-schema directory with an older Switchboard build.
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        std::fs::write(tmp.path().join(".switchboard/config.yaml"), "version: 99\n").unwrap();

        let err = pick_directory_impl(tmp.path().to_str().unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                AppError::Core(CoreError::UnsupportedConfigVersion { found: 99, .. })
            ),
            "expected UnsupportedConfigVersion(99), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_create_project_same_name_serializes_via_registry_write_lock() {
        // TOCTOU regression: two concurrent IPC calls for create_project
        // with the same name must not both succeed. Without the
        // registry_write mutex, both could pass the uniqueness check
        // before either writes the index. With the mutex, exactly one
        // succeeds and one returns DuplicateProjectName.
        let tmp = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = Arc::new(AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        ));
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let state_a = Arc::clone(&state);
        let state_b = Arc::clone(&state);
        // Run on real threads so the mutex contention is real (not
        // single-threaded cooperative scheduling). The work inside
        // create_project_impl is synchronous once it enters the locked
        // section.
        let a = tokio::task::spawn_blocking(move || create_project_impl(&state_a, "shared-name"));
        let b = tokio::task::spawn_blocking(move || create_project_impl(&state_b, "shared-name"));
        let results = [a.await.unwrap(), b.await.unwrap()];

        let successes = results.iter().filter(|r| r.is_ok()).count();
        let dup_errors = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Err(AppError::Core(CoreError::DuplicateProjectName { .. }))
                )
            })
            .count();
        assert_eq!(successes, 1, "exactly one create must succeed: {results:?}");
        assert_eq!(
            dup_errors, 1,
            "the other must return DuplicateProjectName: {results:?}"
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
        let claude: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::with_binary_path(
            "/nonexistent/claude-xyz",
        ));
        let codex: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(claude, codex, emitter as Arc<dyn EventEmitter>);
        let err = check_claude_binary_impl(&state).unwrap_err();
        assert!(matches!(err, AppError::Probe(_)));
    }

    #[test]
    fn check_codex_binary_with_mock_adapter_returns_ok() {
        let (_tmp, state, _) = fresh_state_with_mock();
        assert!(check_codex_binary_impl(&state).is_ok());
    }

    #[test]
    fn check_codex_binary_with_missing_binary_returns_error() {
        use switchboard_harness::CodexAdapter;
        let claude: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let codex: Arc<dyn HarnessAdapter> =
            Arc::new(CodexAdapter::with_binary_path("/nonexistent/codex-xyz"));
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(claude, codex, emitter as Arc<dyn EventEmitter>);
        let err = check_codex_binary_impl(&state).unwrap_err();
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
        create_agent_impl(&state, "a-agent", switchboard_core::HarnessKind::ClaudeCode).unwrap();
        set_active_project_impl(&state, proj_b.id).unwrap();
        create_agent_impl(&state, "b-agent", switchboard_core::HarnessKind::ClaudeCode).unwrap();

        // Default = active project (beta).
        let agents = list_agents_impl(&state, None).unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "b-agent");

        // Explicit project id returns that project's agents.
        let agents_a = list_agents_impl(&state, Some(proj_a.id)).unwrap();
        assert_eq!(agents_a.len(), 1);
        assert_eq!(agents_a[0].name, "a-agent");
    }

    /// Test-only adapter that emits a `ContentChunk` containing a known tag
    /// and counts how many times it has been dispatched to. Used by the
    /// app routing test below to prove that `send_message_impl` selects
    /// the right adapter based on `agent.harness`.
    struct TaggedMockAdapter {
        tag: &'static str,
        dispatch_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl HarnessAdapter for TaggedMockAdapter {
        fn probe(&self) -> Result<(), switchboard_harness::DispatchError> {
            Ok(())
        }

        async fn dispatch(
            &self,
            _agent: &AgentRecord,
            _cwd: &Path,
            _prompt: &str,
            turn_id: switchboard_harness::TurnId,
        ) -> Result<switchboard_harness::EventStream, switchboard_harness::DispatchError> {
            self.dispatch_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let tag = self.tag.to_owned();
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                let _ = tx.send(switchboard_harness::AdapterEvent::ContentChunk {
                    turn_id,
                    kind: switchboard_harness::ContentKind::Text,
                    text: tag,
                });
                let _ = tx.send(switchboard_harness::AdapterEvent::TurnEnd {
                    turn_id,
                    outcome: switchboard_harness::TurnOutcome::Completed,
                    ended_at: chrono::Utc::now(),
                    usage: None,
                });
            });
            Ok(Box::pin(
                tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
            ))
        }
    }

    /// App routing test (M2.3). The dispatcher is harness-agnostic (keyed
    /// by `AgentId` alone), so adapter cross-talk is structurally impossible
    /// there. The substantive failure mode is at the App layer:
    /// `send_message_impl` selects an adapter via `match agent.harness`,
    /// and a regression that hard-codes one adapter would silently spawn
    /// the wrong binary. This test pins that routing against regression
    /// using two distinguishable adapters tagged "claude" / "codex".
    #[tokio::test]
    async fn send_message_routes_to_adapter_matching_agent_harness() {
        let claude_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let codex_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let claude: Arc<dyn HarnessAdapter> = Arc::new(TaggedMockAdapter {
            tag: "from-claude-adapter",
            dispatch_count: claude_count.clone(),
        });
        let codex: Arc<dyn HarnessAdapter> = Arc::new(TaggedMockAdapter {
            tag: "from-codex-adapter",
            dispatch_count: codex_count.clone(),
        });
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(claude, codex, emitter.clone() as Arc<dyn EventEmitter>);
        let tmp = TempDir::new().unwrap();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let claude_agent = create_agent_impl(&state, "c1", HarnessKind::ClaudeCode).unwrap();
        let codex_agent = create_agent_impl(&state, "x1", HarnessKind::Codex).unwrap();

        let claude_handle = send_message_impl(&state, claude_agent.id, "hi")
            .await
            .unwrap();
        claude_handle.join.await.unwrap();
        let codex_handle = send_message_impl(&state, codex_agent.id, "hi")
            .await
            .unwrap();
        codex_handle.join.await.unwrap();

        assert_eq!(
            claude_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "ClaudeCode agent dispatch must hit the Claude adapter exactly once"
        );
        assert_eq!(
            codex_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Codex agent dispatch must hit the Codex adapter exactly once"
        );

        // Secondary check: the emitted ContentChunk tags match the
        // adapter-of-origin per agent_id. Catches mis-routing where dispatch
        // counts are still 1/1 but the wrong adapter served each.
        let events = emitter.snapshot();
        let claude_channel = format!("agent:{}", claude_agent.id);
        let codex_channel = format!("agent:{}", codex_agent.id);
        let claude_text = events
            .iter()
            .find(|(name, payload)| name == &claude_channel && payload["type"] == "content_chunk")
            .expect("content_chunk on claude channel");
        let codex_text = events
            .iter()
            .find(|(name, payload)| name == &codex_channel && payload["type"] == "content_chunk")
            .expect("content_chunk on codex channel");
        assert_eq!(claude_text.1["text"], "from-claude-adapter");
        assert_eq!(codex_text.1["text"], "from-codex-adapter");
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
        let agent_a = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
        )
        .unwrap();
        set_active_project_impl(&state, proj_b.id).unwrap();
        let agent_b = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
        )
        .unwrap();

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

    #[tokio::test]
    async fn pick_directory_does_not_create_switchboard_dir() {
        let tmp = TempDir::new().unwrap();
        let info = pick_directory_impl(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(!info.has_switchboard);
        assert!(info.projects.is_empty());
        assert!(
            !tmp.path().join(".switchboard").exists(),
            "pick_directory must not write to disk"
        );
    }

    #[tokio::test]
    async fn pick_directory_lists_projects_when_switchboard_exists() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        create_project_impl(&state, "alpha").unwrap();

        // Use a fresh state with no directory bound — pick_directory is
        // stateless, it just inspects the path.
        let info = pick_directory_impl(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(info.has_switchboard);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "alpha");
    }

    #[tokio::test]
    async fn pick_directory_rejects_missing_path() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let err = pick_directory_impl(missing.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Core(_)));
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        let err = parse_uuid("not-a-uuid").unwrap_err();
        assert!(matches!(err, AppError::InvalidUuid { .. }));
    }
}
