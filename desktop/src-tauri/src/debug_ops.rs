//! Drives `ops` in a debug build the way an MCP client will, so the window's
//! side of it can be checked by hand: a send to a project the window has not
//! opened, an agent created behind its back, a message queued behind a
//! running turn, the round trip with the window closed. Refused in release
//! builds.
//!
//! Two ways in. From the devtools console (right-click → Inspect):
//!
//! ```js
//! window.__TAURI_INTERNALS__.invoke("debug_ops", { request: { op: "create_project", name: "a", directory: "/tmp/a" } })
//! ```
//!
//! Or, when the app was started with `SWITCHBOARD_DEBUG_SOCKET=<path>`, from a
//! terminal: one JSON request per line on that Unix socket, one JSON reply
//! per line back (`{"ok": …}` or `{"error": …}`):
//!
//! ```sh
//! echo '{"op":"hide_window"}' | nc -U /tmp/switchboard-debug.sock
//! ```
//!
//! A send with `delay_seconds` above zero returns at once and runs later,
//! logging its receipt; that is how the round trip is checked with the
//! window closed. The log line `window ready for a turn` carries its time.

use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use switchboard_core::{HarnessKind, ProjectId, SendId};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::ops::handle::AppStateHandle;
use crate::ops::send::{SendRequest, cancel_send, send, wait_send};
use crate::ops::wait::WaitBudget;
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DebugOp {
    CreateProject {
        name: String,
        directory: String,
    },
    CreateAgent {
        project_id: ProjectId,
        name: String,
        harness: HarnessKind,
        #[serde(default)]
        model: Option<String>,
    },
    Send {
        request: SendRequest,
        #[serde(default)]
        delay_seconds: u64,
    },
    WaitSend {
        project_id: ProjectId,
        send_id: SendId,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },
    CancelSend {
        project_id: ProjectId,
        send_id: SendId,
    },
    /// What ⌘W does: the window hides and the app keeps running.
    HideWindow,
    ShowWindow,
    /// Run a script in the window, e.g. to press one of its buttons or read
    /// what it shows. The script hands a value back by calling `report`.
    Eval {
        js: String,
    },
    /// Called from a script in the window: hold a value for `take_report`.
    Report {
        value: serde_json::Value,
    },
    /// The value the window last reported, if any, cleared as it is read.
    TakeReport,
}

/// The value a script in the window last reported.
static REPORT: std::sync::Mutex<Option<serde_json::Value>> = std::sync::Mutex::new(None);

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn main_window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    app.get_webview_window("main")
        .ok_or_else(|| "no main window".to_owned())
}

pub async fn run(app: AppHandle, op: DebugOp) -> Result<serde_json::Value, String> {
    if !cfg!(debug_assertions) {
        return Err("debug_ops exists only in debug builds".to_owned());
    }
    let to_json =
        |value: Result<serde_json::Value, serde_json::Error>| value.map_err(|e| e.to_string());
    let state = app.state::<AppState>();
    match op {
        DebugOp::CreateProject { name, directory } => {
            let summary = crate::ops::change::create_project(state.inner(), &name, &directory)
                .map_err(|e| e.to_string())?;
            to_json(serde_json::to_value(summary))
        }
        DebugOp::CreateAgent {
            project_id,
            name,
            harness,
            model,
        } => {
            let record = crate::ops::change::create_agent(
                state.inner(),
                project_id,
                &name,
                harness,
                model,
                None,
            )
            .map_err(|e| e.to_string())?;
            to_json(serde_json::to_value(record))
        }
        DebugOp::Send {
            request,
            delay_seconds,
        } => {
            let handle: Arc<dyn AppStateHandle> = Arc::new(app.clone());
            if delay_seconds == 0 {
                let receipt = send(handle, home_dir(), request, WaitBudget::default_wait())
                    .await
                    .map_err(|e| e.to_string())?;
                return to_json(serde_json::to_value(receipt));
            }
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(delay_seconds)).await;
                match send(handle, home_dir(), request, WaitBudget::default_wait()).await {
                    Ok(receipt) => tracing::info!(?receipt, "debug_ops: delayed send dispatched"),
                    Err(error) => tracing::info!(%error, "debug_ops: delayed send refused"),
                }
            });
            Ok(serde_json::json!({ "scheduled_in_seconds": delay_seconds }))
        }
        DebugOp::WaitSend {
            project_id,
            send_id,
            timeout_seconds,
        } => {
            let budget = WaitBudget::new(timeout_seconds, false);
            let reply = wait_send(state.inner(), project_id, send_id, &budget, &home_dir())
                .await
                .map_err(|e| e.to_string())?;
            to_json(serde_json::to_value(reply))
        }
        DebugOp::CancelSend {
            project_id,
            send_id,
        } => {
            let cancelled =
                cancel_send(state.inner(), project_id, send_id).map_err(|e| e.to_string())?;
            to_json(serde_json::to_value(cancelled))
        }
        DebugOp::HideWindow => {
            main_window(&app)?.hide().map_err(|e| e.to_string())?;
            Ok(serde_json::Value::Null)
        }
        DebugOp::ShowWindow => {
            let window = main_window(&app)?;
            window.show().map_err(|e| e.to_string())?;
            window.set_focus().map_err(|e| e.to_string())?;
            Ok(serde_json::Value::Null)
        }
        DebugOp::Eval { js } => {
            main_window(&app)?.eval(js).map_err(|e| e.to_string())?;
            Ok(serde_json::Value::Null)
        }
        DebugOp::Report { value } => {
            *crate::state::lock(&REPORT) = Some(value);
            Ok(serde_json::Value::Null)
        }
        DebugOp::TakeReport => Ok(crate::state::lock(&REPORT)
            .take()
            .unwrap_or(serde_json::Value::Null)),
    }
}

/// Serve [`run`] on the Unix socket named by `SWITCHBOARD_DEBUG_SOCKET`, in a
/// debug build that was started with it. A Unix socket rather than a TCP
/// port, so no web page can reach it.
pub fn serve_socket(app: &AppHandle) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(path) = std::env::var_os("SWITCHBOARD_DEBUG_SOCKET") else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let path = PathBuf::from(path);
        if let Err(reason) = clear_socket_path(&path) {
            tracing::warn!(path = %path.display(), reason, "debug_ops: not serving the debug socket");
            return;
        }
        let listener = match tokio::net::UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, "debug_ops: could not bind the debug socket");
                return;
            }
        };
        // Owner-only, so no other account on the Mac can drive the app.
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        {
            tracing::warn!(%error, "debug_ops: could not restrict the debug socket");
            return;
        }
        tracing::info!(path = %path.display(), "debug_ops: listening");
        while let Ok((stream, _)) = listener.accept().await {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let reply = match serde_json::from_str::<DebugOp>(&line) {
                        Ok(op) => match run(app.clone(), op).await {
                            Ok(value) => serde_json::json!({ "ok": value }),
                            Err(error) => serde_json::json!({ "error": error }),
                        },
                        Err(error) => serde_json::json!({ "error": error.to_string() }),
                    };
                    if write
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
}

/// Make `path` free for a new socket: nothing there, or a leftover socket,
/// which is removed. Anything else is refused rather than deleted, so a
/// mistyped path cannot cost a real file. Read without following a link.
fn clear_socket_path(path: &std::path::Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
        Ok(metadata) if metadata.file_type().is_socket() => {
            std::fs::remove_file(path).map_err(|e| e.to_string())
        }
        Ok(_) => Err("something other than a socket is at that path".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::clear_socket_path;

    #[test]
    fn a_regular_file_at_the_socket_path_is_left_alone() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "keep me").unwrap();
        assert!(clear_socket_path(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
    }

    #[test]
    fn a_leftover_socket_is_removed_and_a_missing_path_is_fine() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("debug.sock");
        assert!(clear_socket_path(&path).is_ok());
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(clear_socket_path(&path).is_ok());
        assert!(!path.exists());
    }
}
