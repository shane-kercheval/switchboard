//! Tauri host crate. Owns the command surface, the `AppHandleEmitter`
//! bridging the dispatcher to Tauri's event system, and adapter selection
//! at startup. Business logic lives in `commands` and `state`; this file
//! is the Tauri wiring layer.

mod commands;
mod error;
mod state;

use std::sync::Arc;

use switchboard_dispatcher::EventEmitter;
use switchboard_harness::{ClaudeCodeAdapter, CodexAdapter, HarnessAdapter, MockHarnessAdapter};
use tauri::{Emitter, Manager, State};

use crate::commands::{
    DirectoryInfo, attach_agent_impl, check_claude_binary_impl, check_codex_binary_impl,
    create_agent_impl, create_project_impl, init_directory_impl, list_agents_impl,
    list_projects_impl, open_project_impl, parse_uuid, pick_directory_impl, send_message_impl,
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
async fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectSummary>, String> {
    list_projects_impl(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn create_project(
    state: State<'_, AppState>,
    name: String,
) -> Result<ProjectSummary, String> {
    create_project_impl(state.inner(), &name).map_err(|e| e.to_string())
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
) -> Result<String, String> {
    let id = parse_uuid(&agent_id).map_err(|e| e.to_string())?;
    // Returns as soon as TurnStart has been emitted; the JoinHandle drops
    // here and the drain task detaches, continuing in the background. The
    // load-bearing ordering invariant (TurnId returned synchronously,
    // TurnStart already on the wire) is preserved.
    let handle = send_message_impl(state.inner(), id, &prompt)
        .await
        .map_err(|e| e.to_string())?;
    Ok(handle.turn_id.to_string())
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

/// Reads `SWITCHBOARD_HARNESS` to decide which adapter pair to construct.
/// - Unset or `"claude"` → `claude_adapter = ClaudeCodeAdapter`, `codex_adapter = CodexAdapter`.
/// - `"mock"` → both adapters = `MockHarnessAdapter`.
/// - Any other value → panic (silent fall-through to default would be a footgun).
///
/// Returns `(claude_adapter, codex_adapter)`. Both are constructed under
/// "claude"/unset because the `match agent.harness` routing in
/// `send_message_impl` may dispatch to either at runtime; neither adapter's
/// constructor performs a binary check, so missing CLIs only surface at
/// `check_*_binary` time, not at app startup.
fn build_adapters() -> (Arc<dyn HarnessAdapter>, Arc<dyn HarnessAdapter>) {
    match std::env::var("SWITCHBOARD_HARNESS").as_deref() {
        Ok("mock") => {
            tracing::info!(
                "SWITCHBOARD_HARNESS=mock — using MockHarnessAdapter for both harnesses"
            );
            let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
            (Arc::clone(&mock), mock)
        }
        Ok("claude") | Err(_) => (
            Arc::new(ClaudeCodeAdapter::new()),
            Arc::new(CodexAdapter::new()),
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

    let (claude_adapter, codex_adapter) = build_adapters();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let emitter: Arc<dyn EventEmitter> = Arc::new(AppHandleEmitter {
                app: app.handle().clone(),
            });
            app.manage(AppState::new(
                Arc::clone(&claude_adapter),
                Arc::clone(&codex_adapter),
                emitter,
            ));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_claude_binary,
            check_codex_binary,
            pick_directory,
            init_directory,
            list_projects,
            create_project,
            open_project,
            set_active_project,
            create_agent,
            attach_agent,
            list_agents,
            send_message,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
