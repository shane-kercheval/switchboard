//! Tauri host crate. Owns the command surface, the `AppHandleEmitter`
//! bridging the dispatcher to Tauri's event system, and adapter selection
//! at startup. Business logic lives in `commands` and `state`; this file
//! is the Tauri wiring layer.

mod commands;
mod dispatch_context;
mod emitter;
mod error;
mod git_registry;
mod journal;
mod locator_sink;
mod metadata;
mod preferences;
mod prompts_setup;
mod secret_store;
mod state;
mod workspace;

use std::sync::Arc;

use switchboard_dispatcher::EventEmitter;
use switchboard_harness::{
    AntigravityAdapter, ClaudeCodeAdapter, CodexAdapter, GeminiAdapter, HarnessAdapter,
    MockHarnessAdapter,
};
use tauri::{Emitter, Manager, State};

use crate::commands::ProjectConversation;
use crate::commands::{
    AgentSessionInfo, DirectoryInfo, HarnessInstallStatus, ProjectListing, RepoListing,
    WorkspaceDirectories, add_mcp_provider_impl, add_tracked_repo_impl, agent_session_info_impl,
    attach_agent_impl, cancel_agent_impl, cancel_send_impl, cancel_turn_impl, changed_files_impl,
    check_antigravity_auth_impl, check_antigravity_binary_impl, check_claude_auth_impl,
    check_claude_binary_impl, check_codex_auth_impl, check_codex_binary_impl,
    check_gemini_auth_impl, check_gemini_binary_impl, commit_changed_files_impl,
    commit_file_diff_impl, commit_ranges_impl, create_agent_impl, create_project_impl,
    delete_project_impl, editor_open_argv, fetch_repo_impl, file_diff_impl,
    get_harness_install_status_impl, get_preferences_impl, init_directory_impl, list_agents_impl,
    list_mcp_providers_impl, list_projects_impl, list_prompts_impl, list_tracked_repos_from_inputs,
    list_workspace_directories_impl, load_project_conversation_impl, load_transcript_impl,
    open_project_impl, parse_uuid, pick_directory_impl, read_tracked_repo_from_inputs,
    remove_agent_impl, remove_directory_impl, remove_mcp_provider_impl, remove_queued_message_impl,
    remove_tracked_repo_impl, rename_agent_impl, rename_project_impl, render_prompt_impl,
    reveal_in_finder_argv, search_project_files_in_root, search_project_files_root_impl,
    send_message_impl, set_active_project_impl, set_preferences_impl, set_project_archived_impl,
    sync_prompts_and_notify, terminal_open_argv, test_mcp_connection_impl, tracked_repos_inputs,
    tracked_roots, validate_external_url,
};
use crate::preferences::Preferences;
use crate::state::AppState;

use switchboard_core::{AgentRecord, HarnessKind, ProjectSummary};
use switchboard_git::{BranchKind, ChangedFile, CommitChanges, FileDiff, GitCommitRange};
use switchboard_prompts::{McpProviderInfo, Prompt, RenderedPrompt};

#[tauri::command]
async fn check_claude_binary(state: State<'_, AppState>) -> Result<(), String> {
    check_claude_binary_impl(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_codex_binary(state: State<'_, AppState>) -> Result<(), String> {
    check_codex_binary_impl(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_codex_auth() -> Result<(), String> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    check_codex_auth_impl(&home).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_gemini_binary(state: State<'_, AppState>) -> Result<(), String> {
    check_gemini_binary_impl(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_gemini_auth() -> Result<(), String> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    check_gemini_auth_impl(&home).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_antigravity_binary(state: State<'_, AppState>) -> Result<(), String> {
    check_antigravity_binary_impl(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_antigravity_auth() -> Result<(), String> {
    // No `$HOME` forwarding: Antigravity's auth lives in the macOS
    // Keychain, not under `$HOME`. See the impl docstring.
    check_antigravity_auth_impl().map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_claude_auth() -> Result<(), String> {
    // No `$HOME` forwarding: Claude's auth lives in the macOS Keychain.
    check_claude_auth_impl().map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_harness_install_status(
    state: State<'_, AppState>,
    harness: HarnessKind,
) -> Result<HarnessInstallStatus, String> {
    Ok(get_harness_install_status_impl(state.inner(), harness))
}

#[tauri::command]
async fn pick_directory(path: String) -> Result<DirectoryInfo, String> {
    pick_directory_impl(&path).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn init_directory(state: State<'_, AppState>, path: String) -> Result<DirectoryInfo, String> {
    init_directory_impl(state.inner(), &path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_directory(state: State<'_, AppState>, path: String) -> Result<(), String> {
    remove_directory_impl(state.inner(), &path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectListing>, String> {
    list_projects_impl(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_workspace_directories(
    state: State<'_, AppState>,
) -> Result<WorkspaceDirectories, String> {
    Ok(list_workspace_directories_impl(state.inner()))
}

/// List all prompts across configured providers (user-global; no project
/// argument). Reads local providers this milestone; never hits the network.
#[tauri::command]
async fn list_prompts(state: State<'_, AppState>) -> Result<Vec<Prompt>, String> {
    Ok(list_prompts_impl(state.inner()))
}

/// Render `name` from `provider` with `args`, returning the finished text.
/// Serves both preview and send — the caller passes the identical args map.
/// May touch the network (MCP `prompts/get`), hence async.
#[tauri::command]
async fn render_prompt(
    state: State<'_, AppState>,
    provider: String,
    name: String,
    args: std::collections::BTreeMap<String, String>,
) -> Result<RenderedPrompt, String> {
    render_prompt_impl(state.inner(), &provider, &name, &args)
        .await
        .map_err(|e| e.to_string())
}

/// Rebuild the cached prompt list from all configured providers. Wired to the
/// Settings "Sync" button; used to pick up prompts edited on a server mid-session.
#[tauri::command]
async fn sync_prompts(state: State<'_, AppState>) -> Result<(), String> {
    let state = state.inner();
    sync_prompts_and_notify(state.prompts.clone(), Arc::clone(&state.emitter)).await;
    Ok(())
}

/// Configured MCP providers with status, for the Settings list.
#[tauri::command]
async fn list_mcp_providers(state: State<'_, AppState>) -> Result<Vec<McpProviderInfo>, String> {
    Ok(list_mcp_providers_impl(state.inner()))
}

/// Add a generic MCP server (name + URL + optional bearer token).
#[tauri::command]
async fn add_mcp_provider(
    state: State<'_, AppState>,
    name: String,
    url: String,
    bearer: Option<String>,
) -> Result<(), String> {
    add_mcp_provider_impl(state.inner(), &name, &url, bearer.as_deref()).map_err(|e| e.to_string())
}

/// Remove a configured MCP server (deletes its config entry and stored token).
#[tauri::command]
async fn remove_mcp_provider(state: State<'_, AppState>, name: String) -> Result<(), String> {
    remove_mcp_provider_impl(state.inner(), &name).map_err(|e| e.to_string())
}

/// Probe a candidate MCP server (connect + list) before saving; returns the
/// number of prompts it exposes.
#[tauri::command]
async fn test_mcp_connection(
    state: State<'_, AppState>,
    url: String,
    bearer: Option<String>,
) -> Result<usize, String> {
    test_mcp_connection_impl(state.inner(), &url, bearer)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_tracked_repo(state: State<'_, AppState>, path: String) -> Result<(), String> {
    add_tracked_repo_impl(state.inner(), &path).map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_tracked_repo(state: State<'_, AppState>, path: String) -> Result<(), String> {
    // Infallible by design — `Result` matches the idempotent-ack convention of
    // the `cancel_*` commands. Registry persistence is best-effort and logged in
    // `persist_git_registry`, deliberately not surfaced here.
    remove_tracked_repo_impl(state.inner(), &path);
    Ok(())
}

#[tauri::command]
async fn list_tracked_repos(state: State<'_, AppState>) -> Result<Vec<RepoListing>, String> {
    // Snapshot the cheap state-derived inputs on the async thread, then run the
    // synchronous `git2` reads on a blocking worker (decision 8) so they don't
    // stall the async runtime.
    let inputs = tracked_repos_inputs(state.inner());
    tauri::async_runtime::spawn_blocking(move || list_tracked_repos_from_inputs(&inputs))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn read_tracked_repo(
    state: State<'_, AppState>,
    path: String,
) -> Result<RepoListing, String> {
    let inputs = tracked_repos_inputs(state.inner());
    tauri::async_runtime::spawn_blocking(move || read_tracked_repo_from_inputs(&path, &inputs))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn fetch_repo(state: State<'_, AppState>, path: String) -> Result<(), String> {
    fetch_repo_impl(state.inner(), &path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn changed_files(
    state: State<'_, AppState>,
    path: String,
) -> Result<Vec<ChangedFile>, String> {
    // Snapshot tracked roots on the async thread, then run the synchronous `git2`
    // read on a blocking worker — consistent with the other Git-view reads, and
    // gated on the tracked set so an untracked path returns empty, not live data.
    let roots = tracked_roots(state.inner());
    tauri::async_runtime::spawn_blocking(move || changed_files_impl(&roots, &path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn file_diff(
    state: State<'_, AppState>,
    path: String,
    file: String,
) -> Result<FileDiff, String> {
    let roots = tracked_roots(state.inner());
    tauri::async_runtime::spawn_blocking(move || file_diff_impl(&roots, &path, &file))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn branch_commits(
    state: State<'_, AppState>,
    repo_root: String,
    kind: BranchKind,
    name: String,
) -> Result<Vec<GitCommitRange>, String> {
    let roots = tracked_roots(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        commit_ranges_impl(&roots, &repo_root, kind, &name)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn commit_changed_files(
    state: State<'_, AppState>,
    repo_root: String,
    oid: String,
) -> Result<CommitChanges, String> {
    let roots = tracked_roots(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        commit_changed_files_impl(&roots, &repo_root, &oid)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn commit_file_diff(
    state: State<'_, AppState>,
    repo_root: String,
    oid: String,
    file: String,
) -> Result<FileDiff, String> {
    let roots = tracked_roots(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        commit_file_diff_impl(&roots, &repo_root, &oid, &file)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_preferences(state: State<'_, AppState>) -> Result<Preferences, String> {
    Ok(get_preferences_impl(state.inner()))
}

#[tauri::command]
async fn set_preferences(
    state: State<'_, AppState>,
    preferences: Preferences,
) -> Result<(), String> {
    set_preferences_impl(state.inner(), &preferences).map_err(|e| e.to_string())
}

#[tauri::command]
async fn create_project(
    state: State<'_, AppState>,
    name: String,
    directory: String,
) -> Result<ProjectSummary, String> {
    create_project_impl(state.inner(), &name, &directory).map_err(|e| e.to_string())
}

#[tauri::command]
async fn rename_project(
    state: State<'_, AppState>,
    project_id: String,
    new_name: String,
) -> Result<ProjectListing, String> {
    let id = parse_uuid(&project_id).map_err(|e| e.to_string())?;
    rename_project_impl(state.inner(), id, &new_name).map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_project(state: State<'_, AppState>, project_id: String) -> Result<(), String> {
    let id = parse_uuid(&project_id).map_err(|e| e.to_string())?;
    delete_project_impl(state.inner(), id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_project_archived(
    state: State<'_, AppState>,
    project_id: String,
    archived: bool,
) -> Result<(), String> {
    let id = parse_uuid(&project_id).map_err(|e| e.to_string())?;
    set_project_archived_impl(state.inner(), id, archived).map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectSummary, String> {
    let id = parse_uuid(&project_id).map_err(|e| e.to_string())?;
    open_project_impl(state.inner(), id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_active_project(state: State<'_, AppState>, project_id: String) -> Result<(), String> {
    let id = parse_uuid(&project_id).map_err(|e| e.to_string())?;
    set_active_project_impl(state.inner(), id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn create_agent(
    state: State<'_, AppState>,
    name: String,
    harness: HarnessKind,
) -> Result<AgentRecord, String> {
    create_agent_impl(state.inner(), &name, harness).map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_agent(state: State<'_, AppState>, agent_id: String) -> Result<(), String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    remove_agent_impl(state.inner(), id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn rename_agent(
    state: State<'_, AppState>,
    agent_id: String,
    new_name: String,
) -> Result<AgentRecord, String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    rename_agent_impl(state.inner(), id, &new_name).map_err(|e| e.to_string())
}

#[tauri::command]
async fn attach_agent(
    state: State<'_, AppState>,
    name: String,
    harness: HarnessKind,
    existing_session_id: String,
) -> Result<AgentRecord, String> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    attach_agent_impl(state.inner(), &name, harness, &existing_session_id, &home)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_agents(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<AgentRecord>, String> {
    let pid = match project_id {
        Some(s) => Some(parse_uuid(&s).map_err(|e| e.to_string())?),
        None => None,
    };
    list_agents_impl(state.inner(), pid).map_err(|e| e.to_string())
}

#[tauri::command]
async fn search_project_files(
    state: State<'_, AppState>,
    project_id: String,
    query: String,
    limit: usize,
) -> Result<Vec<String>, String> {
    let id = parse_uuid(&project_id).map_err(|e| e.to_string())?;
    let root = search_project_files_root_impl(state.inner(), id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || search_project_files_in_root(&root, &query, limit))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn send_message(
    state: State<'_, AppState>,
    agent_id: String,
    prompt: String,
    send_id: String,
) -> Result<String, String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    // The frontend mints one `send_id` per Send and passes it on every
    // per-recipient call, so a fan-out's turns share it (hydration groups the
    // user's message once).
    let sid = parse_uuid(&send_id).map_err(|e| e.to_string())?;
    // Returns the minted `message_id` immediately (the send is accepted, not
    // necessarily started). The turn's `turn_id` and lifecycle flow over the
    // per-agent event channel; the correlated `TurnStart` carries this
    // `message_id`, and a pre-`TurnStart` failure surfaces as `MessageFailed`.
    let message_id = send_message_impl(state.inner(), id, &prompt, sid)
        .await
        .map_err(|e| e.to_string())?;
    Ok(message_id.to_string())
}

#[tauri::command]
async fn remove_queued_message(
    state: State<'_, AppState>,
    agent_id: String,
    message_id: String,
) -> Result<RemovedQueued, String> {
    let aid = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    let mid = parse_uuid(&message_id).map_err(|e| e.to_string())?;
    let removed = remove_queued_message_impl(state.inner(), aid, mid)
        .await
        .map_err(|e| e.to_string())?;
    Ok(RemovedQueued {
        agent_id: removed.agent_id.to_string(),
        send_id: removed.send_id.to_string(),
        prompt: removed.prompt,
    })
}

/// Wire result of `remove_queued_message` — the removed message's payload, so
/// the compose bar can restore the text the user had queued.
#[derive(serde::Serialize)]
struct RemovedQueued {
    agent_id: String,
    send_id: String,
    prompt: String,
}

#[tauri::command]
async fn cancel_turn(state: State<'_, AppState>, agent_id: String) -> Result<(), String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    // Idempotent stop — the dispatcher no-ops if there's nothing to cancel.
    // The synthesized `Cancelled` terminal + return-to-idle flow back to the
    // frontend over the per-agent event channel, so the command just acks.
    cancel_turn_impl(state.inner(), id);
    Ok(())
}

#[tauri::command]
async fn cancel_agent(state: State<'_, AppState>, agent_id: String) -> Result<(), String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    // Idempotent "stop agent": cancels the in-flight turn + clears the backlog.
    // The synthesized `Cancelled` terminal flows back over the event channel and
    // the dropped queued items are resolved by the frontend's optimistic
    // cleanup, so the command just acks.
    cancel_agent_impl(state.inner(), id);
    Ok(())
}

#[tauri::command]
async fn cancel_send(
    state: State<'_, AppState>,
    send_id: String,
    recipients: Vec<String>,
) -> Result<(), String> {
    let sid = parse_uuid(&send_id).map_err(|e| e.to_string())?;
    let agent_ids = recipients
        .iter()
        .map(|r| parse_uuid(r))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    // Send-scoped, idempotent stop — each recipient's actor decides whether its
    // current turn belongs to this send; the synthesized `Cancelled` terminals
    // flow back over the per-agent event channels, so the command just acks.
    cancel_send_impl(state.inner(), sid, &agent_ids);
    Ok(())
}

#[tauri::command]
async fn load_transcript(
    state: State<'_, AppState>,
    agent_id: String,
) -> Result<switchboard_harness::LoadedTranscript, String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    load_transcript_impl(state.inner(), id, &home).map_err(|e| e.to_string())
}

#[tauri::command]
async fn agent_session_info(
    state: State<'_, AppState>,
    agent_id: String,
) -> Result<AgentSessionInfo, String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    agent_session_info_impl(state.inner(), id, &home).map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_session_file(state: State<'_, AppState>, agent_id: String) -> Result<(), String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let info = agent_session_info_impl(state.inner(), id, &home).map_err(|e| e.to_string())?;
    let Some(path) = info.session_file else {
        return Err("this agent has no session file yet".to_owned());
    };
    // `.jsonl` has no default macOS app handler, so a plain open fails
    // (kLSApplicationNotFoundErr). `open -t` forces the default *text* editor.
    // macOS-specific, which is fine — Switchboard is macOS-only in v1.
    let status = tokio::process::Command::new("open")
        .arg("-t")
        .arg(&path)
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`open -t` failed for {path} (exit {status})"))
    }
}

#[tauri::command]
async fn open_external_url(url: String) -> Result<(), String> {
    validate_external_url(&url)?;
    let status = tokio::process::Command::new("open")
        .arg(&url)
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`open` failed for {url} (exit {status})"))
    }
}

/// Spawn a macOS opener argv (program in `argv[0]`, args in the rest) and map a
/// spawn failure / non-zero exit to a flat error string. Shared by the Git-view
/// open actions, which differ only in how they build the argv.
async fn run_open_argv(argv: Vec<String>) -> Result<(), String> {
    let (program, rest) = argv.split_first().ok_or("empty open command")?;
    let status = tokio::process::Command::new(program)
        .args(rest)
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program}` failed (exit {status})"))
    }
}

#[tauri::command]
async fn open_in_editor(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let editor = get_preferences_impl(state.inner()).editor_command;
    run_open_argv(editor_open_argv(editor.as_deref(), &path)).await
}

#[tauri::command]
async fn open_in_terminal(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let terminal = get_preferences_impl(state.inner()).terminal_app;
    run_open_argv(terminal_open_argv(&terminal, &path)).await
}

#[tauri::command]
async fn reveal_in_finder(path: String) -> Result<(), String> {
    run_open_argv(reveal_in_finder_argv(&path)).await
}

#[tauri::command]
async fn load_project_conversation(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectConversation, String> {
    let id = parse_uuid(&project_id).map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    load_project_conversation_impl(state.inner(), id, &home)
        .await
        .map_err(|e| e.to_string())
}

/// Bridges the dispatcher's `EventEmitter` abstraction onto Tauri's
/// `AppHandle::emit`. Emit failures are logged — Tauri's `emit` returns
/// `Err` when payload serialization fails, which can't happen for our
/// `NormalizedEvent` payloads, so this is defensive.
struct AppHandleEmitter {
    app: tauri::AppHandle,
}

impl EventEmitter for AppHandleEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        if let Err(e) = self.app.emit(name, payload) {
            tracing::warn!(event_name = name, error = %e, "tauri emit failed");
        }
    }
}

/// Reads `SWITCHBOARD_HARNESS` to decide which adapter triple to construct.
/// - Unset or `"claude"` → real adapters for all three harnesses.
/// - `"mock"` → all three adapters = `MockHarnessAdapter`.
/// - Any other value → panic (silent fall-through to default would be a footgun).
///
/// Returns `(claude_adapter, codex_adapter, gemini_adapter, antigravity_adapter)`.
/// All are constructed under "claude"/unset because the `match agent.harness`
/// routing in `send_message_impl` may dispatch to any at runtime; no
/// adapter's constructor performs a binary check, so missing CLIs only
/// surface at `check_*_binary` time, not at app startup.
// A 4-tuple of the same trait object reads as "complex" to clippy, but a
// named struct for a private one-call-site startup helper would be more
// ceremony than it's worth — the tuple is destructured immediately at the
// single call site in `run`.
#[allow(clippy::type_complexity)]
fn build_adapters() -> (
    Arc<dyn HarnessAdapter>,
    Arc<dyn HarnessAdapter>,
    Arc<dyn HarnessAdapter>,
    Arc<dyn HarnessAdapter>,
) {
    match std::env::var("SWITCHBOARD_HARNESS").as_deref() {
        Ok("mock") => {
            tracing::info!("SWITCHBOARD_HARNESS=mock — using MockHarnessAdapter for all harnesses");
            let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
            (
                Arc::clone(&mock),
                Arc::clone(&mock),
                Arc::clone(&mock),
                mock,
            )
        }
        Ok("claude") | Err(_) => (
            Arc::new(ClaudeCodeAdapter::new()),
            Arc::new(CodexAdapter::new()),
            Arc::new(GeminiAdapter::new()),
            Arc::new(AntigravityAdapter::new()),
        ),
        Ok(other) => panic!(
            "invalid SWITCHBOARD_HARNESS={other:?}; expected one of: claude, mock (or unset for default)"
        ),
    }
}

/// Resolve the user-global config directory — the single location that holds
/// `workspace.yaml`, the prompt `config.yaml`, and the default `prompts/` store.
/// `None` only when no home directory is resolvable, in which case user-global
/// persistence (workspace registry, prompt config) is disabled.
///
/// Release builds always resolve the OS config dir for `switchboard`, so the
/// installed app's state lives at one fixed, predictable location no env var can
/// move.
///
/// Debug builds resolve a separate dev directory so dev runs never read or
/// overwrite the installed app's state. `SWITCHBOARD_CONFIG_DIR` (set per port by
/// `make dev`) overrides the location outright; without it the fallback is a
/// shared `switchboard-dev`. The override is debug-only by construction — it
/// cannot relocate the installed app's data.
#[cfg(not(debug_assertions))]
fn config_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("", "", "switchboard")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

#[cfg(debug_assertions)]
fn config_dir() -> Option<std::path::PathBuf> {
    debug_config_dir(std::env::var_os("SWITCHBOARD_CONFIG_DIR"))
}

/// Pure decision behind the debug arm of [`config_dir`], split out so the
/// override mapping is testable without mutating process-global env (which is
/// `unsafe` and racy under edition 2024). An explicit override is used verbatim;
/// otherwise the shared `switchboard-dev` directory is the fallback.
#[cfg(debug_assertions)]
fn debug_config_dir(override_dir: Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    if let Some(dir) = override_dir {
        return Some(std::path::PathBuf::from(dir));
    }
    directories::ProjectDirs::from("", "", "switchboard-dev")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

/// Path to the user-global `workspace.yaml` (the cross-directory project list).
fn workspace_config_path() -> Option<std::path::PathBuf> {
    config_dir().map(|dir| dir.join("workspace.yaml"))
}

/// The Git-view tracked-repo registry (`git-view.yaml`) — a sibling of
/// `workspace.yaml` in the same user-global config dir, so both move together
/// (the debug `SWITCHBOARD_CONFIG_DIR` override relocates both at once).
fn git_registry_config_path() -> Option<std::path::PathBuf> {
    config_dir().map(|dir| dir.join("git-view.yaml"))
}

/// Personal preferences live in `config.yaml` — the **shared** personal-config
/// file that also holds the prompt providers. Each subsystem round-trips the
/// others' keys on write (see `preferences::save`), so they coexist in one file.
fn preferences_config_path() -> Option<std::path::PathBuf> {
    config_dir().map(|dir| dir.join("config.yaml"))
}

/// Build the prompt service from the user-global config dir, seeding the example
/// prompts on first run. The pure `crates/prompts` never touches `directories`;
/// the app resolves and injects the config path, default prompts dir, home, and
/// the secret store.
///
/// The injected secret store is the OS keychain (`KeyringSecretStore`), namespaced
/// by build so debug tokens stay separate from a release install's.
fn build_prompt_service() -> switchboard_prompts::PromptService {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if let Some(dir) = config_dir() {
        let prompts_dir = dir.join("prompts");
        crate::prompts_setup::seed_example_prompts(&prompts_dir);
        let secrets = build_secret_store(&dir);
        switchboard_prompts::PromptService::new(dir.join("config.yaml"), prompts_dir, home, secrets)
    } else {
        tracing::warn!("no home directory resolved — prompt providers disabled");
        switchboard_prompts::PromptService::disabled()
    }
}

/// Release builds use the OS keychain. **Debug builds use a plaintext file store**
/// in the dev config dir instead — an unsigned dev binary's signature changes on
/// every compile, so the macOS Keychain re-prompts on every credential read; the
/// file store sidesteps that. Dev-only tradeoff: the token sits in plaintext on
/// the developer's own machine.
#[cfg(not(debug_assertions))]
fn build_secret_store(_dir: &std::path::Path) -> Arc<dyn switchboard_prompts::SecretStore> {
    Arc::new(crate::secret_store::KeyringSecretStore::new(
        keyring_service(),
    ))
}

#[cfg(debug_assertions)]
fn build_secret_store(dir: &std::path::Path) -> Arc<dyn switchboard_prompts::SecretStore> {
    Arc::new(crate::secret_store::FileSecretStore::new(
        dir.join("mcp-secrets.json"),
    ))
}

/// Keychain service name for stored provider bearers (release only — debug uses
/// the file store, so this isn't compiled there).
#[cfg(not(debug_assertions))]
fn keyring_service() -> &'static str {
    "switchboard"
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Attach the user-global persistence locations to a fresh `AppState`. Each is
/// independently optional: if a location can't be resolved (exotic host with no
/// home dir), that registry/preferences set stays in-memory only and its persist
/// is a no-op — safe because all three are convenience state, never load-bearing.
fn with_persistence_paths(state: AppState) -> AppState {
    // `workspace.yaml` — the cross-directory project registry.
    let state = if let Some(path) = workspace_config_path() {
        state.with_workspace(path)
    } else {
        tracing::warn!("no home directory resolved — workspace registry persistence disabled");
        state
    };
    // `git-view.yaml` — the Git-view tracked-repo registry.
    let state = if let Some(path) = git_registry_config_path() {
        state.with_git_registry(path)
    } else {
        state
    };
    // `config.yaml` — personal preferences.
    if let Some(path) = preferences_config_path() {
        state.with_preferences(path)
    } else {
        state
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "length is dominated by the flat `generate_handler!` command registry, which reads better as one list than split across helpers"
)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let (claude_adapter, codex_adapter, gemini_adapter, antigravity_adapter) = build_adapters();

    // In release builds, enforce single-instance: a second launch focuses the
    // existing window instead of spawning a rival process, keeping one
    // `workspace.yaml` writer and no cross-process clobber. Disabled in debug so
    // multiple dev instances (two worktrees, or `make dev` on two ports) can run
    // at once. Their isolation is launcher-provided, not structural: `make dev`
    // sets `SWITCHBOARD_CONFIG_DIR` per port so each resolves its own config dir
    // (see `workspace_config_path`). A debug build launched without it (bare
    // `cargo run`, IDE run button) falls back to the shared `switchboard-dev`
    // registry, so two such instances can last-writer-wins each other — accepted,
    // since it's atomic-write dev convenience state, never the installed app's data.
    let builder = tauri::Builder::default();
    #[cfg(not(debug_assertions))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }));
    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(move |app| {
            let emitter: Arc<dyn EventEmitter> = Arc::new(AppHandleEmitter {
                app: app.handle().clone(),
            });
            let state = AppState::new(
                Arc::clone(&claude_adapter),
                Arc::clone(&codex_adapter),
                Arc::clone(&gemini_adapter),
                Arc::clone(&antigravity_adapter),
                emitter,
            );
            // Attach all user-global persistence locations (workspace.yaml,
            // git-view.yaml, config.yaml) — see `with_persistence_paths`.
            let state = with_persistence_paths(state);
            // Resolve and inject the user-global prompt config + default prompts
            // store (seeding the example prompts on first run).
            let prompts = build_prompt_service();
            // Warm the prompt cache in the background so a slow/cold MCP server
            // never blocks startup. `PromptService` is cheaply cloneable and
            // shares its cache `Arc`, so the clone the task syncs is the same
            // cache the managed state reads.
            tauri::async_runtime::spawn(sync_prompts_and_notify(
                prompts.clone(),
                Arc::clone(&state.emitter),
            ));
            let state = state.with_prompts(prompts);
            // Cold start: open a `Directory` handle for every workspace entry so
            // restored directories report `available: true` and participate in
            // the cross-harness session-id collision scan. Unopenable
            // directories (unmounted/moved) are skipped and stay unavailable.
            crate::state::eager_load_directories(&state);
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_claude_binary,
            check_codex_binary,
            check_codex_auth,
            check_gemini_binary,
            check_gemini_auth,
            check_antigravity_binary,
            check_antigravity_auth,
            check_claude_auth,
            get_harness_install_status,
            pick_directory,
            init_directory,
            remove_directory,
            list_projects,
            list_workspace_directories,
            add_tracked_repo,
            remove_tracked_repo,
            list_tracked_repos,
            read_tracked_repo,
            fetch_repo,
            changed_files,
            file_diff,
            branch_commits,
            commit_changed_files,
            commit_file_diff,
            get_preferences,
            set_preferences,
            list_prompts,
            render_prompt,
            sync_prompts,
            list_mcp_providers,
            add_mcp_provider,
            remove_mcp_provider,
            test_mcp_connection,
            create_project,
            rename_project,
            delete_project,
            set_project_archived,
            open_project,
            set_active_project,
            create_agent,
            remove_agent,
            rename_agent,
            attach_agent,
            list_agents,
            search_project_files,
            send_message,
            remove_queued_message,
            cancel_turn,
            cancel_agent,
            cancel_send,
            agent_session_info,
            open_session_file,
            open_external_url,
            open_in_editor,
            open_in_terminal,
            reveal_in_finder,
            load_transcript,
            load_project_conversation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// Gated on `debug_assertions` because `debug_config_dir` exists only in debug
// builds; `cargo test --release` turns those off and the symbol away.
#[cfg(all(test, debug_assertions))]
mod tests {
    use super::debug_config_dir;
    use std::path::PathBuf;

    #[test]
    fn override_dir_is_used_verbatim() {
        let path = debug_config_dir(Some("/tmp/switchboard-test".into()))
            .expect("an explicit override always yields a path");
        assert_eq!(path, PathBuf::from("/tmp/switchboard-test"));
    }

    #[test]
    fn no_override_falls_back_to_switchboard_dev() {
        // Routes through `ProjectDirs`, which is `None` only on a host with no
        // resolvable home (not a dev machine or normal CI). Skip rather than
        // unwrap so an exotic host degrades quietly instead of panicking.
        if let Some(path) = debug_config_dir(None) {
            assert!(path.ends_with("switchboard-dev"));
        }
    }
}
