//! Subprocess helpers shared between harness adapters.
//!
//! Spawning a CLI subprocess, draining its stderr to a bounded tail buffer,
//! formatting that tail for inclusion in synthesized error events, and
//! force-killing the resulting process group are concerns every harness
//! adapter has. Keeping them in one module means a fix to (say) the UTF-8
//! boundary handling in [`format_stderr_tail`] lands once, not once per
//! adapter; and the `killpg`-vs-plain-`kill` distinction (load-bearing for
//! Codex's two-process tree) is implemented
//! in a single place that any new harness adapter calls without having to
//! re-derive the correct behavior — see [`terminate_then_kill`].
//!
//! **What is NOT here.** `synthesize_truncation_turn_end` stays
//! adapter-local — Claude and Codex construct different diagnostic messages
//! (Codex consumes a parser-buffered `error` event payload that Claude has
//! no equivalent for). Both adapters compose their messages on top of
//! [`format_stderr_tail`] from this module.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::io::AsyncBufReadExt;

use crate::adapter::DispatchError;
use crate::events::TurnId;
use switchboard_core::AgentId;

/// How long to wait for a process group to exit after SIGTERM before
/// escalating to SIGKILL. A ceiling, not a fixed wait — a well-behaved
/// harness exits on SIGTERM in milliseconds, so this is only paid by a
/// SIGTERM-deaf (or slow-to-flush) process. Kept generous enough for Codex's
/// Node-parent-plus-Rust-child tree to flush its session file.
pub const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// Maximum number of stderr lines retained in the per-dispatch tail buffer.
/// Tail-only (FIFO drop of older lines) — we only need the last few lines
/// of stderr when synthesizing a failure message for a truncated stream.
pub const STDERR_TAIL_CAPACITY: usize = 16;

/// Bound the formatted stderr message length so it stays readable in the
/// UI. Truncation happens on char boundaries (see [`format_stderr_tail`]).
pub const STDERR_MESSAGE_MAX_LEN: usize = 800;

/// Sentinel bracketing the PATH value we ask the login shell to print, so we
/// can recover it even when shell startup emits its own banner/MOTD output
/// around our command.
#[cfg(target_os = "macos")]
const PATH_SENTINEL: &str = "__SWITCHBOARD_PATH__";

/// Extract the PATH value bracketed by [`PATH_SENTINEL`] from login-shell
/// output. Returns `None` if the markers are absent or bracket an empty value.
#[cfg(target_os = "macos")]
fn parse_sentinel_path(output: &str) -> Option<String> {
    let start = output.find(PATH_SENTINEL)? + PATH_SENTINEL.len();
    let rest = &output[start..];
    let end = rest.find(PATH_SENTINEL)?;
    let path = rest[..end].trim();
    (!path.is_empty()).then(|| path.to_owned())
}

/// Run the user's login shell once to print its PATH between two sentinels.
/// `-ilc` sources both login (`.zprofile`) and interactive (`.zshrc`) startup
/// files — where nvm/asdf/pyenv and `~/.local/bin` typically extend PATH — so
/// the result matches what the user sees in a terminal. `$SHELL` falls back to
/// zsh (the macOS default). `printf` avoids a trailing newline inside the
/// sentinels.
#[cfg(target_os = "macos")]
fn run_login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let script = format!("printf '%s%s%s' '{PATH_SENTINEL}' \"$PATH\" '{PATH_SENTINEL}'");
    let output = std::process::Command::new(shell)
        .args(["-ilc", &script])
        .output()
        .ok()?;
    parse_sentinel_path(&String::from_utf8_lossy(&output.stdout))
}

/// Capture the login-shell PATH, bounded by a timeout so a misbehaving startup
/// file (one that blocks on input) can't stall app launch. The shell runs on a
/// detached thread; if it doesn't answer within the window we give up and the
/// caller falls back to the process PATH.
#[cfg(target_os = "macos")]
fn capture_login_shell_path() -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run_login_shell_path());
    });
    rx.recv_timeout(Duration::from_secs(5)).ok().flatten()
}

/// The PATH used to locate and run harness CLIs: the user's login-shell PATH,
/// captured once and cached. A GUI launch (Spotlight/Launchpad) inherits only a
/// minimal PATH that omits nvm, `~/.local/bin`, Homebrew, etc.; this recovers
/// the full terminal PATH. Falls back to the process PATH if the shell capture
/// fails — correct for a terminal launch, safe degradation for a GUI launch.
#[cfg(target_os = "macos")]
fn resolved_path() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        capture_login_shell_path().unwrap_or_else(|| std::env::var("PATH").unwrap_or_default())
    })
}

/// Set the resolved PATH on a harness subprocess so the spawned CLI — and the
/// tools *it* shells out to (git, node, ripgrep, an `env`-shebang interpreter,
/// …) — resolve the same way they would in the user's terminal. Without this, a
/// GUI-launched app hands the child its minimal PATH and those lookups fail even
/// when the harness binary itself was found. No-op off macOS, where the
/// inherited PATH is already correct.
pub fn apply_path_env(command: &mut tokio::process::Command) {
    #[cfg(target_os = "macos")]
    command.env("PATH", resolved_path());
    #[cfg(not(target_os = "macos"))]
    let _ = command;
}

/// Resolve a harness binary path to an absolute path. Absolute paths are
/// trusted as-is (spawn will return `NotFound` if the binary is missing and
/// the caller maps that to `BinaryNotFound`). Relative names go through `which`,
/// searching the resolved login-shell PATH on macOS so a GUI launch finds the
/// same binary the terminal would.
pub fn resolve_binary(path: &Path) -> Result<PathBuf, DispatchError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    #[cfg(target_os = "macos")]
    let found = which::which_in(path, Some(resolved_path()), std::path::Path::new("."));
    #[cfg(not(target_os = "macos"))]
    let found = which::which(path);
    found.map_err(|_| DispatchError::BinaryNotFound)
}

/// Check whether a harness binary is present and executable. Absolute paths are
/// checked directly; relative names search `which` over the resolved
/// login-shell PATH on macOS so a GUI launch probes the same locations the
/// terminal would.
pub fn probe_binary(path: &Path) -> Result<(), DispatchError> {
    #[cfg(target_os = "macos")]
    let found = which::which_in(path, Some(resolved_path()), std::path::Path::new("."));
    #[cfg(not(target_os = "macos"))]
    let found = which::which(path);
    found.map(|_| ()).map_err(|_| DispatchError::BinaryNotFound)
}

/// Extract just the version number from a `--version` line, since CLIs pad it
/// differently: `claude` prints `2.1.156 (Claude Code)`, `codex` prints
/// `codex-cli 0.134.0`, others print a bare `0.44.0`. Returns the first
/// whitespace-separated token that looks like a dotted version (optionally
/// `v`-prefixed, which is stripped), or `None` if the line has none — callers
/// then show "Installed" without a number rather than echoing the binary name.
#[must_use]
pub fn parse_cli_version(line: &str) -> Option<String> {
    line.split_whitespace()
        .map(|tok| tok.strip_prefix('v').unwrap_or(tok))
        .find(|tok| {
            let mut segments = tok.split('.');
            let major_numeric = segments
                .next()
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
            // Require at least `<digits>.<digit>…` so "codex-cli" (no dot) and
            // "(Claude" are rejected but "2.1.156" / "0.44.0" match.
            let minor_starts_numeric = segments
                .next()
                .is_some_and(|s| s.bytes().next().is_some_and(|b| b.is_ascii_digit()));
            major_numeric && minor_starts_numeric
        })
        .map(str::to_owned)
}

/// Best-effort version string for a harness CLI: the first line of
/// `<binary> --version`, trimmed. Returns `None` when the binary can't be
/// resolved/invoked or reports nothing — the value is display-only, never
/// load-bearing, so any failure collapses to "unknown" rather than an error.
pub fn fetch_version(binary: &Path) -> Option<String> {
    let resolved = resolve_binary(binary).ok()?;
    let mut command = std::process::Command::new(&resolved);
    // Same PATH augmentation as a dispatched turn: a `--version` that shells
    // out (or is an env-shebang script) must resolve its interpreter/tools the
    // way a real turn would, so the displayed version matches what runs.
    #[cfg(target_os = "macos")]
    command.env("PATH", resolved_path());
    let output = command.arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    (!line.is_empty()).then_some(line)
}

/// Drain a child's stderr stream into a bounded tail buffer.
///
/// Each line is also emitted at `tracing::debug!` with the harness name as
/// context so a `RUST_LOG=debug` run shows the stderr inline with the rest
/// of the trace. `harness_name` is the short identifier ("claude", "codex")
/// used in the log message — passed as a parameter so this function isn't
/// duplicated per adapter just to change the log prefix.
pub async fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    agent_id: AgentId,
    turn_id: TurnId,
    tail: Arc<Mutex<VecDeque<String>>>,
    harness_name: &'static str,
) {
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                tracing::debug!(agent_id = %agent_id, %turn_id, "{harness_name} stderr: {line}");
                if let Ok(mut buf) = tail.lock() {
                    if buf.len() >= STDERR_TAIL_CAPACITY {
                        buf.pop_front();
                    }
                    buf.push_back(line);
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(agent_id = %agent_id, %turn_id, error = %e, "stderr read error");
                break;
            }
        }
    }
}

/// Return a single-line, length-bounded representation of the captured
/// stderr tail buffer. Empty string if no lines were captured.
///
/// Length-bounding is performed on **char boundaries** — slicing a String
/// by byte offsets can land mid-UTF-8 and panic (real risk with non-ASCII
/// paths or error messages in stderr).
pub fn format_stderr_tail(tail: &Mutex<VecDeque<String>>) -> String {
    let Ok(buf) = tail.lock() else {
        return String::new();
    };
    if buf.is_empty() {
        return String::new();
    }
    let joined = buf.iter().cloned().collect::<Vec<_>>().join(" | ");
    if joined.len() > STDERR_MESSAGE_MAX_LEN {
        // Find the lowest char boundary at or after `joined.len() - MAX`.
        // Walk byte positions forward from that target until we hit a
        // valid boundary; result is guaranteed to be `<= MAX` chars worth
        // of suffix (typically fewer if multi-byte chars sit at the edge).
        let target = joined.len() - STDERR_MESSAGE_MAX_LEN;
        let start = (target..=joined.len())
            .find(|&i| joined.is_char_boundary(i))
            .unwrap_or(joined.len());
        let mut truncated = joined[start..].to_owned();
        truncated.insert(0, '…');
        truncated
    } else {
        joined
    }
}

/// Terminate a harness subprocess and **the whole process group** it leads:
/// SIGTERM the group, give the parent up to [`TERMINATE_GRACE`] to flush and
/// exit, then SIGKILL the group unconditionally to sweep any survivor. Reaps
/// the direct child (so callers must not `wait()` again, or must tolerate an
/// already-reaped result).
///
/// **Why the group, not just the PID.** `tokio::process::Child::kill` signals
/// only the spawned PID. For a two-process tree (Codex's Node parent + Rust
/// child), killing only the parent leaves the child holding the write end of
/// the stdout/stderr pipes; a drain task then blocks forever on an EOF that
/// never arrives. `process_group(0)` at spawn (used by every adapter here)
/// makes the spawned child its own process-group leader (`pgid == pid`), so
/// passing its PID to `killpg` signals every process in the group.
///
/// **Why the final SIGKILL is unconditional.** Waiting on the direct child
/// only tells us the *parent* exited — not that the *group* is empty. A
/// descendant that ignores SIGTERM and outlives the parent (still holding our
/// pipes) would otherwise be missed, and the adapter's stderr drain would hang
/// forever. So after the parent's grace window we always `killpg(SIGKILL)`:
/// when the group already exited it's a harmless `ESRCH` no-op; when a survivor
/// remains it's what actually tears it down. We do **not** `wait` the survivor
/// — it's a grandchild, reparented to (and reaped by) init once the parent
/// died; we only reap our direct child.
///
/// **Why SIGTERM-first.** A graceful signal lets the harness flush and leave
/// its session file in a resumable state — load-bearing for both cancellation
/// (the user stopped a healthy turn) and the adapter error paths (a parse /
/// stream-read error means *we* stopped reading, not that the harness is
/// unhealthy; it may be mid-write to its session file). The grace is a ceiling
/// for the parent: a process that exits promptly on SIGTERM adds no latency.
///
/// Cross-platform: non-unix has no process-group concept, so it falls back to
/// `child.kill()` (SIGKILL-equivalent).
pub async fn terminate_then_kill(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        let Some(pid) = child.id() else {
            // No PID → already exited/reaped; nothing to signal.
            let _ = child.wait().await;
            return;
        };
        let group = nix::unistd::Pid::from_raw(pid.cast_signed());
        killpg_signal(group, nix::sys::signal::Signal::SIGTERM);
        // Give the parent the grace window to flush + exit on SIGTERM. We
        // ignore the result: whether it exited or timed out, the unconditional
        // group SIGKILL below is what guarantees teardown (see doc).
        let _ = tokio::time::timeout(TERMINATE_GRACE, child.wait()).await;
        killpg_signal(group, nix::sys::signal::Signal::SIGKILL);
        let _ = child.wait().await;
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
    }
}

/// Signal a process group, ignoring `ESRCH` ("no such process" — the group
/// already exited between the caller's check and this signal, which is a
/// no-op success, not a failure).
#[cfg(unix)]
fn killpg_signal(pgid: nix::unistd::Pid, signal: nix::sys::signal::Signal) {
    let _ = nix::sys::signal::killpg(pgid, signal);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_stderr_tail_returns_empty_string_when_buffer_is_empty() {
        let tail: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
        assert_eq!(format_stderr_tail(&tail), "");
    }

    #[test]
    fn format_stderr_tail_joins_lines_with_pipe_separator() {
        let tail: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
        tail.lock().unwrap().push_back("first".to_owned());
        tail.lock().unwrap().push_back("second".to_owned());
        assert_eq!(format_stderr_tail(&tail), "first | second");
    }

    #[test]
    fn format_stderr_tail_handles_non_ascii_at_truncation_boundary() {
        // Regression: byte-slicing on a String can land mid-UTF-8 and
        // panic with "byte index N is not a char boundary." Stderr from
        // real subprocesses often contains paths or messages with
        // multi-byte characters (e.g., accented usernames, emoji, smart
        // quotes). Truncation must walk to a char boundary.
        let tail: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
        // 600 ASCII chars + 150 "…" (3 bytes each) → 1050 bytes total,
        // well over the 800-byte truncation threshold. The byte at
        // (len - 800) almost certainly lands mid-character.
        let mut payload = "A".repeat(600);
        for _ in 0..150 {
            payload.push('…');
        }
        tail.lock().unwrap().push_back(payload);

        let result = format_stderr_tail(&tail);
        // Critically: NO PANIC. Plus the leading-ellipsis prefix marks
        // the truncation visually.
        assert!(
            result.starts_with('…'),
            "truncated output should be prefixed with …"
        );
        // Total chars bounded by STDERR_MESSAGE_MAX_LEN + a small constant
        // (the prefix and the boundary walk overhead).
        assert!(result.chars().count() < 850);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_sentinel_path_extracts_value_between_markers() {
        let out = format!("{PATH_SENTINEL}/usr/bin:/bin{PATH_SENTINEL}");
        assert_eq!(parse_sentinel_path(&out).as_deref(), Some("/usr/bin:/bin"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_sentinel_path_ignores_surrounding_banner_noise() {
        // A login shell may print MOTD/banner lines around our printf; the
        // sentinels let us recover the value regardless.
        let out = format!("Welcome to zsh\nbanner line\n{PATH_SENTINEL}/a:/b:/c{PATH_SENTINEL}\n");
        assert_eq!(parse_sentinel_path(&out).as_deref(), Some("/a:/b:/c"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_sentinel_path_none_when_missing_or_empty() {
        assert_eq!(parse_sentinel_path("no markers here"), None);
        // Both markers present but bracketing nothing → None.
        assert_eq!(
            parse_sentinel_path(&format!("{PATH_SENTINEL}{PATH_SENTINEL}")),
            None
        );
        // Only the opening marker → can't bracket a value → None.
        assert_eq!(parse_sentinel_path(&format!("{PATH_SENTINEL}/a:/b")), None);
    }

    #[test]
    fn resolve_binary_returns_absolute_path_verbatim() {
        // Absolute path → trusted as-is, no PATH lookup. Even nonexistent
        // absolute paths return Ok; spawn at the call site is what fails
        // with NotFound, mapped to BinaryNotFound by the adapter.
        let path = std::path::Path::new("/nonexistent/absolute/binary");
        assert_eq!(resolve_binary(path).unwrap(), path.to_path_buf());
    }

    #[test]
    fn resolve_binary_relative_name_not_on_path_returns_binary_not_found() {
        let path = std::path::Path::new("definitely-not-a-real-binary-name-xyz123");
        assert!(matches!(
            resolve_binary(path),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[test]
    fn probe_binary_is_stricter_than_resolve_for_absolute_paths() {
        // Intentional divergence: `resolve_binary` trusts an absolute path
        // verbatim (failure deferred to spawn), but `probe_binary` actually
        // checks existence + exec bit, so a missing absolute path fails at
        // probe time. This test locks that contract so a future refactor can't
        // silently collapse the two.
        let path = std::path::Path::new("/nonexistent/absolute/binary");
        assert!(resolve_binary(path).is_ok());
        assert!(matches!(
            probe_binary(path),
            Err(DispatchError::BinaryNotFound)
        ));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn apply_path_env_sets_resolved_path_on_child() {
        // The behavior the fix exists for: a spawned harness subprocess must
        // see the resolved (login-shell) PATH, not the parent's minimal GUI
        // PATH. Run a child that prints its inherited PATH and assert it equals
        // what we resolved.
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "printf %s \"$PATH\""]);
        apply_path_env(&mut command);
        let output = command.output().await.expect("sh should run");
        assert_eq!(String::from_utf8_lossy(&output.stdout), resolved_path());
    }

    #[test]
    fn fetch_version_returns_first_line_for_present_binary() {
        // `cargo` is guaranteed present wherever `cargo test` runs and
        // supports `--version`; it stands in for a harness CLI to prove the
        // first-line extraction without depending on a real harness install.
        let version = fetch_version(std::path::Path::new("cargo"))
            .expect("cargo --version should report a line");
        assert!(
            version.contains("cargo"),
            "unexpected version line: {version}"
        );
        assert!(!version.contains('\n'), "should be a single trimmed line");
    }

    #[test]
    fn fetch_version_none_for_missing_binary() {
        assert_eq!(
            fetch_version(std::path::Path::new("definitely-not-a-real-binary-xyz123")),
            None
        );
    }

    #[test]
    fn parse_cli_version_extracts_number_from_real_formats() {
        // Captured live: each harness pads its --version differently.
        assert_eq!(
            parse_cli_version("2.1.156 (Claude Code)").as_deref(),
            Some("2.1.156")
        );
        assert_eq!(
            parse_cli_version("codex-cli 0.134.0").as_deref(),
            Some("0.134.0")
        );
        assert_eq!(parse_cli_version("0.44.0").as_deref(), Some("0.44.0"));
        assert_eq!(parse_cli_version("1.0.3").as_deref(), Some("1.0.3"));
    }

    #[test]
    fn parse_cli_version_strips_leading_v_and_handles_no_version() {
        assert_eq!(parse_cli_version("v1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(parse_cli_version(""), None);
        assert_eq!(parse_cli_version("no version here"), None);
    }
}
