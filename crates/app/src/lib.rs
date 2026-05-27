//! Tauri host crate. Owns the command surface, the `AppHandleEmitter`
//! bridging the dispatcher to Tauri's event system, and adapter selection
//! at startup. Business logic lives in `commands` and `state`; this file
//! is the Tauri wiring layer.

mod commands;
mod dispatch_context;
mod emitter;
mod error;
mod journal;
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
    AgentSessionInfo, DirectoryInfo, ProjectListing, WorkspaceDirectories, agent_session_info_impl,
    attach_agent_impl, cancel_agent_impl, cancel_send_impl, cancel_turn_impl,
    check_antigravity_auth_impl, check_antigravity_binary_impl, check_claude_binary_impl,
    check_codex_auth_impl, check_codex_binary_impl, check_gemini_auth_impl,
    check_gemini_binary_impl, create_agent_impl, create_project_impl, init_directory_impl,
    list_agents_impl, list_projects_impl, list_workspace_directories_impl,
    load_project_conversation_impl, load_transcript_impl, open_project_impl, parse_uuid,
    pick_directory_impl, remove_directory_impl, remove_queued_message_impl, send_message_impl,
    set_active_project_impl,
};
use crate::state::AppState;

use switchboard_core::{AgentRecord, HarnessKind, ProjectSummary};

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

#[tauri::command]
async fn create_project(
    state: State<'_, AppState>,
    name: String,
    directory: String,
) -> Result<ProjectSummary, String> {
    create_project_impl(state.inner(), &name, &directory).map_err(|e| e.to_string())
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let (claude_adapter, codex_adapter, gemini_adapter, antigravity_adapter) = build_adapters();

    tauri::Builder::default()
        // Must be the first plugin registered. A second launch of the app
        // hands its argv/cwd to the already-running instance via this callback
        // (instead of starting a rival process); we surface the existing window
        // rather than spawn a duplicate. Single-instance keeps one
        // `workspace.yaml` writer, so there is no cross-process clobber.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
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
            // Resolve the user-global `workspace.yaml` location. If no home
            // directory is resolvable (exotic host), skip workspace persistence
            // entirely — the registry stays empty and `persist_workspace` is a
            // no-op, which is safe since the registry is convenience state.
            let state = if let Some(dirs) = directories::ProjectDirs::from("", "", "switchboard") {
                let path = dirs.config_dir().join("workspace.yaml");
                state.with_workspace(path)
            } else {
                tracing::warn!(
                    "no home directory resolved — workspace registry persistence disabled"
                );
                state
            };
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
            pick_directory,
            init_directory,
            remove_directory,
            list_projects,
            list_workspace_directories,
            create_project,
            open_project,
            set_active_project,
            create_agent,
            attach_agent,
            list_agents,
            send_message,
            remove_queued_message,
            cancel_turn,
            cancel_agent,
            cancel_send,
            agent_session_info,
            open_session_file,
            load_transcript,
            load_project_conversation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
