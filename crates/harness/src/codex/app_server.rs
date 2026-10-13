//! A short-lived, bare `codex app-server`, spoken to over its stdio pipes.
//!
//! Two features need Codex's app-server, and neither needs it to keep running:
//! the account quota read ([`super::account_usage`]) and manual compaction
//! ([`super::compaction`]). Each starts a server, writes its requests, reads
//! until it has its answer, and kills the server. This module owns that
//! lifecycle, so the two can't drift apart. Any later app-server feature should
//! use it rather than start its own server.
//!
//! **Never the shared daemon.** A bare stdio `codex app-server` is a process of
//! its own, owned by the caller. Switchboard must not use `codex app-server
//! proxy`, the control socket, or `--managed-daemon`: those reach the user's
//! long-lived Codex daemon, which other Codex clients share.
//!
//! **Accepted exposure.** `codex app-server` is marked `[experimental]` in the
//! CLI's own help, and its protocol is not in the published Codex docs. The
//! authority is the schema the installed binary emits through `codex app-server
//! generate-json-schema`. The live tests (`live_codex_account_usage_*`,
//! `live_codex_compact_*`) are what notice when it moves.
//!
//! **The caller always tears the server down.** The server answers and then
//! waits for more requests, so it never exits on its own while its stdin is
//! open. Every caller runs it on a task it owns and calls
//! [`AppServer::shut_down`] on every path, success included.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// How long to wait for the stderr drain to finish once the server is dead.
/// Reached only on failure paths, and only to make an error message complete.
const STDERR_SETTLE: Duration = Duration::from_millis(500);

/// One message the server wrote on stdout.
#[derive(Debug)]
pub(crate) enum Message {
    /// The answer to one of the caller's requests, matched by its JSON-RPC
    /// id. A well-formed reply carries exactly one of `result` and `error`;
    /// the caller decides what a reply with neither means.
    Reply {
        id: i64,
        result: Option<Value>,
        error: Option<Value>,
    },
    /// Something the server announced on its own. Notifications interleave
    /// with replies (a `remoteControl/status/changed` was observed between two
    /// replies, and a compaction's turn can arrive before the reply to the
    /// request that started it), so they are passed on rather than discarded.
    Notification { method: String, params: Value },
}

/// Read one line of the server's stdout as a [`Message`].
///
/// `None` for anything that is neither a reply nor a notification: non-JSON
/// (stdout is shared with anything the binary or a shim around it prints), and
/// a request from the server to the client (an object with both a `method` and
/// an `id`). Switchboard answers no server requests: the quota read starts no
/// thread, and a compaction runs with approval policy `never`, so Codex sends
/// it none. Matching on `id` alone would mistake such a request for the reply
/// to one of ours.
pub(crate) fn parse_line(line: &str) -> Option<Message> {
    let message: Value = serde_json::from_str(line).ok()?;
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        if message.get("id").is_some() {
            return None;
        }
        return Some(Message::Notification {
            method: method.to_owned(),
            params: message.get("params").cloned().unwrap_or(Value::Null),
        });
    }
    let id = message.get("id")?.as_i64()?;
    Some(Message::Reply {
        id,
        result: message.get("result").cloned(),
        error: message.get("error").cloned(),
    })
}

/// The `initialize` handshake. Required before any other method.
///
/// **The `initialized` notification that a full client would send next is
/// deliberately omitted.** Probed: the quota read and a compaction both
/// succeed without it.
pub(crate) fn initialize_request(id: i64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "switchboard",
                "title": "Switchboard",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }
    })
    .to_string()
}

/// A running `codex app-server` and the pipes to it.
pub(crate) struct AppServer {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: tokio::io::Split<tokio::io::BufReader<tokio::process::ChildStdout>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    draining: Option<tokio::task::JoinHandle<()>>,
}

impl AppServer {
    /// Start `<binary> app-server` from an already-resolved binary path.
    ///
    /// `cwd` is the process's working directory, or `None` to inherit the
    /// app's. The quota read belongs to no project and passes `None`; a
    /// compaction runs in the agent's working directory, as its turns do.
    ///
    /// # Errors
    ///
    /// The spawn's own error. Callers map it, because what a `NotFound` means
    /// depends on whether they set a working directory
    /// (`subprocess::map_spawn_error`).
    pub(crate) fn spawn(binary: &Path, cwd: Option<&Path>) -> std::io::Result<Self> {
        let mut command = tokio::process::Command::new(binary);
        command
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        crate::subprocess::apply_path_env(&mut command);
        // Own process group, like a dispatched turn, so teardown can signal the
        // whole tree rather than just the process we spawned. The quota probe
        // saw no descendants, but "saw none once" is not a guarantee for a
        // server process, and the cost of being wrong is an orphan holding a
        // pipe.
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(std::io::Error::other(
                "codex app-server exposed no stdio pipes",
            ));
        };

        let stderr_tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        let draining = child.stderr.take().map(|stderr| {
            let tail = Arc::clone(&stderr_tail);
            // Drained rather than nulled so a server that fails after exec (a
            // panicking binary, a shim that errors) leaves a reason in the
            // error. Drained rather than left unread so a chatty build cannot
            // fill the pipe and wedge the server mid-answer.
            tokio::spawn(async move {
                if let Err(e) = crate::subprocess::drain_stderr_into_tail(stderr, tail, |line| {
                    tracing::debug!("codex app-server stderr: {line}");
                })
                .await
                {
                    tracing::warn!(error = %e, "codex app-server stderr read error");
                }
            })
        });

        Ok(Self {
            child,
            stdin: Some(stdin),
            // Byte-oriented, then lossy. `lines()` aborts the whole read on
            // invalid UTF-8, which would contradict `parse_line`'s tolerance of
            // arbitrary non-JSON on this stream. Tolerating garbage text but
            // not garbage bytes has no principle behind it, and the strict
            // version loses a valid answer that arrives after the noise.
            stdout: tokio::io::BufReader::new(stdout).split(b'\n'),
            stderr_tail,
            draining,
        })
    }

    /// Write `requests`, one JSON object each, as newline-delimited JSON in a
    /// single write.
    ///
    /// A write failure means the server is already gone. It is ignored here so
    /// the caller keeps reading stdout, and whatever the server managed to say
    /// still reaches its error.
    ///
    /// **stdin stays open until [`Self::shut_down`]**, and this is load-bearing.
    /// Closing it after the write, the obvious move when there is nothing more
    /// to send, makes the server shut down before it answers: on 0.154.0, 20
    /// quota reads that closed stdin produced no answer and 20 that left it open
    /// answered every time, and 0.155.1 repeated it (0 of 20 closed, 25 of 25
    /// open). For a compaction, closing stdin aborts the work: at 0.162.1 the
    /// server recorded an interrupted turn and exited within 0.13s.
    ///
    /// The same behavior is the app's shutdown guarantee: when Switchboard
    /// exits, its end of this pipe closes, and that makes a server it never got
    /// to signal exit on its own.
    pub(crate) async fn send(&mut self, requests: &[String]) {
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        let mut payload = requests.join("\n");
        payload.push('\n');
        let _ = stdin.write_all(payload.as_bytes()).await;
        let _ = stdin.flush().await;
    }

    /// The next reply or notification on stdout, skipping lines that are
    /// neither (see [`parse_line`]). `Ok(None)` when stdout reaches EOF.
    ///
    /// Cancellation-safe: dropping the future between lines loses nothing.
    ///
    /// # Errors
    ///
    /// The read error that ended the stream.
    pub(crate) async fn next_message(&mut self) -> std::io::Result<Option<Message>> {
        while let Some(chunk) = self.stdout.next_segment().await? {
            if let Some(message) = parse_line(&String::from_utf8_lossy(&chunk)) {
                return Ok(Some(message));
            }
        }
        Ok(None)
    }

    /// Close stdin and tear down the server's whole process group.
    ///
    /// Not bounded by any caller's deadline, on purpose:
    /// `terminate_then_kill` carries its own grace window and an unconditional
    /// `SIGKILL`, and interrupting it is what would leak the process.
    pub(crate) async fn shut_down(&mut self) {
        self.stdin = None;
        crate::subprocess::terminate_then_kill(&mut self.child).await;
    }

    /// Wait briefly for the stderr drain to reach EOF, so a failure's message
    /// carries the server's last words. Call after [`Self::shut_down`].
    ///
    /// **The kill has to come first**, for two measured reasons:
    ///
    /// - A hung server still holds its error pipe open, so the drain can't
    ///   reach EOF while it lives. Settling before the kill always used the
    ///   full settle budget and learned nothing: 2.51s observed against a 2.00s
    ///   bound, three identical runs.
    /// - A line reader yields nothing for a final line with no trailing newline
    ///   until EOF. That is the usual shape of a wedged process, a half-written
    ///   diagnostic and then a stall, and before this ordering its message was
    ///   unreachable entirely. `SIGKILL` closes the pipe and the fragment
    ///   arrives.
    ///
    /// Still bounded: a descendant holding the pipe would otherwise park this
    /// forever.
    pub(crate) async fn settle_stderr(&mut self) {
        if let Some(draining) = self.draining.take() {
            let _ = tokio::time::timeout(STDERR_SETTLE, draining).await;
        }
    }

    /// The server's last stderr lines as one capped line, or `""` when it
    /// printed nothing.
    pub(crate) fn stderr_tail(&self) -> String {
        crate::subprocess::format_stderr_tail(&self.stderr_tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_matched_by_its_id_with_either_outcome() {
        let Some(Message::Reply { id, result, error }) = parse_line(r#"{"id":2,"result":{}}"#)
        else {
            panic!("a reply");
        };
        assert_eq!((id, result, error), (2, Some(serde_json::json!({})), None));

        let Some(Message::Reply { id, result, error }) =
            parse_line(r#"{"id":1,"error":{"code":-32600,"message":"thread not found: x"}}"#)
        else {
            panic!("a reply");
        };
        assert_eq!(id, 1);
        assert!(result.is_none());
        assert_eq!(error.unwrap()["message"], "thread not found: x");
    }

    #[test]
    fn a_notification_carries_its_method_and_params() {
        let Some(Message::Notification { method, params }) =
            parse_line(r#"{"method":"turn/started","params":{"threadId":"t"},"emittedAtMs":1}"#)
        else {
            panic!("a notification");
        };
        assert_eq!(method, "turn/started");
        assert_eq!(params["threadId"], "t");
    }

    #[test]
    fn a_server_request_is_never_taken_for_a_reply() {
        // It carries an `id`, possibly one of ours, but it asks rather than
        // answers.
        assert!(
            parse_line(r#"{"id":2,"method":"item/commandExecution/requestApproval","params":{}}"#)
                .is_none()
        );
    }

    #[test]
    fn noise_is_skipped() {
        assert!(parse_line("not json").is_none());
        assert!(parse_line(r#"{"jsonrpc":"2.0"}"#).is_none());
        assert!(parse_line(r#"{"id":"text-id","result":{}}"#).is_none());
    }
}
