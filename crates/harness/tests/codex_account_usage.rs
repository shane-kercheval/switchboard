//! Process-level tests for the Codex account usage read: drive
//! `read_account_usage` against the `fake_codex` fixture binary and assert on
//! the reading, the failure modes, and the teardown.
//!
//! **These are hermetic on purpose, and the timeout case especially so.** The
//! live test (`live.rs`, `live_codex_account_usage_*`) proves the *real* CLI
//! still returns the shape we depend on; it cannot prove what happens when the
//! CLI never answers, because a live server that hangs on command is not
//! something a test can arrange. Leaving the kill-on-timeout path to the live
//! suite would mean never testing it at all.

use std::path::{Path, PathBuf};
use std::time::Duration;

use switchboard_harness::{AccountUsageError, read_account_usage};

#[cfg(unix)]
use nix::unistd::{Pid, getpgid};

const FAKE_CODEX: &str = env!("CARGO_BIN_EXE_fake_codex");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex");

/// Write an executable shim that stands in for the `codex` binary.
///
/// `read_account_usage` invokes `<binary> app-server` and passes no fixture
/// path — correctly, since the real CLI takes none — so the fixture has to be
/// supplied from outside the call. The shim appends it, turning the production
/// argv into the `fake_codex app-server <fixture>` form that binary replays.
///
/// `exec` rather than a plain call so the shim is *replaced* by `fake_codex`:
/// the pid and the process group the spawn assigned stay the same, which is
/// what lets the teardown assertions below reason about a single group.
fn codex_shim(dir: &Path, fixture: &Path) -> PathBuf {
    let shim = dir.join("codex");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nexec {FAKE_CODEX} \"$@\" {}\n",
            fixture.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    shim
}

/// [`codex_shim`] over one of the checked-in fixtures, named by stem.
fn recorded_shim(dir: &Path, fixture: &str) -> PathBuf {
    codex_shim(dir, Path::new(&format!("{FIXTURES}/{fixture}.jsonl")))
}

/// Bounds a *successful* read against a fixture, so it only has to outlast a
/// local replay — unrelated to the production bound, which is sized from the
/// real call's measured latency.
const AMPLE: Duration = Duration::from_secs(30);

#[tokio::test]
async fn reads_every_account_bucket_from_the_recorded_response() {
    let dir = tempfile::TempDir::new().unwrap();
    let shim = recorded_shim(dir.path(), "account-rate-limits");

    let usage = read_account_usage(&shim, AMPLE).await.expect("a reading");

    assert_eq!(usage.ordinary_usage_allowed, Some(false));
    // Both buckets survive the trip, including the model-scoped one. Filtering
    // is the frontend's decision; dropping it here would put an interpretation
    // in Rust that M2 then could not change.
    assert_eq!(
        usage.rate_limits_by_limit_id.keys().collect::<Vec<_>>(),
        vec!["base_model_inference", "codex"]
    );
    assert_eq!(
        usage.rate_limits_by_limit_id["codex"]["rateLimitReachedType"],
        "rate_limit_reached"
    );
    assert_eq!(
        usage.rate_limits_by_limit_id["base_model_inference"]["normalModelSlug"],
        "gpt-5.6-luna"
    );
}

#[tokio::test]
async fn the_answer_is_found_behind_unrelated_server_traffic() {
    // The recorded stream carries the `initialize` response and an unsolicited
    // `remoteControl/status/changed` notification ahead of the answer. Reading
    // by line position instead of by id would pick up one of those.
    let dir = tempfile::TempDir::new().unwrap();
    let shim = recorded_shim(dir.path(), "account-rate-limits");

    let usage = read_account_usage(&shim, AMPLE).await.expect("a reading");

    assert!(usage.rate_limits_by_limit_id.contains_key("codex"));
}

#[tokio::test]
async fn a_missing_binary_is_reported_as_missing_not_as_a_spawn_failure() {
    let dir = tempfile::TempDir::new().unwrap();
    let absent = dir.path().join("definitely-not-codex");

    let error = read_account_usage(&absent, AMPLE).await.unwrap_err();

    assert!(
        matches!(error, AccountUsageError::BinaryNotFound),
        "got {error:?}"
    );
}

#[tokio::test]
async fn a_server_that_exits_without_answering_reports_no_response() {
    // `// exit:1` makes the fake exit before emitting anything — the shape of a
    // server that dies during startup. The reading must be absent, not wrong.
    let dir = tempfile::TempDir::new().unwrap();
    let shim = recorded_shim(dir.path(), "account-rate-limits-silent");

    let error = read_account_usage(&shim, AMPLE).await.unwrap_err();

    assert!(
        matches!(error, AccountUsageError::NoResponse { .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn a_server_error_reply_is_reported_as_rpc() {
    let dir = tempfile::TempDir::new().unwrap();
    let shim = recorded_shim(dir.path(), "account-rate-limits-error");

    let error = read_account_usage(&shim, AMPLE).await.unwrap_err();

    assert!(
        matches!(error, AccountUsageError::Rpc(ref detail) if detail.contains("Method not found")),
        "got {error:?}"
    );
}

#[tokio::test]
async fn stderr_from_a_failed_read_reaches_the_error_message() {
    // The only clue a failing spawn leaves. Nulling stderr would be simpler and
    // would make this class of failure undiagnosable from a log.
    let dir = tempfile::TempDir::new().unwrap();
    let shim = recorded_shim(dir.path(), "account-rate-limits-silent");

    let error = read_account_usage(&shim, AMPLE).await.unwrap_err();

    assert!(
        error.to_string().contains("fake_codex"),
        "stderr tail should be carried; got {error}"
    );
}

/// Poll until `path` holds a parseable pgid; bounded so a spawn failure panics
/// with a message rather than hanging the suite.
#[cfg(unix)]
async fn wait_for_pgid(path: &Path) -> Pid {
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Ok(n) = s.trim().parse::<i32>()
        {
            return Pid::from_raw(n);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("fake_codex never wrote its pgid at {}", path.display());
}

/// Poll until the process group's leader is gone (`getpgid` → `ESRCH`).
#[cfg(unix)]
async fn assert_group_reaped(leader: Pid) {
    for _ in 0..200 {
        if getpgid(Some(leader)).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("process group {leader} still alive after the read returned");
}

#[cfg(unix)]
#[tokio::test]
async fn a_hung_server_times_out_and_leaves_no_process_behind() {
    // The failure mode this bound exists for: `codex app-server` holds stdio
    // open and waits for more requests, so a read that gives up without killing
    // it leaks a process per refresh — and refreshes run after every Codex
    // turn.
    let dir = tempfile::TempDir::new().unwrap();
    let pgid_path = dir.path().join("pgid");
    let fixture = dir.path().join("hang.jsonl");
    std::fs::write(
        &fixture,
        format!(
            "// pgid_to:{}\n// stderr:wedged before answering\n// hang\n",
            pgid_path.display()
        ),
    )
    .unwrap();
    let shim = codex_shim(dir.path(), &fixture);

    let timeout = Duration::from_secs(2);
    let read = tokio::spawn(async move { read_account_usage(&shim, timeout).await });
    let leader = wait_for_pgid(&pgid_path).await;

    let error = read.await.unwrap().unwrap_err();
    assert!(
        matches!(error, AccountUsageError::Timeout { bound, .. } if bound == timeout),
        "got {error:?}"
    );
    // A wedged child that printed a reason first is the common shape, and the
    // timeout is the one bounded failure — so it is the one whose diagnosis
    // matters most. Reporting a bare "didn't answer in time" throws it away.
    assert!(
        error.to_string().contains("wedged before answering"),
        "the timeout must carry the server's last words; got {error}"
    );
    assert_group_reaped(leader).await;
}

#[cfg(unix)]
#[tokio::test]
async fn abandoning_the_call_still_tears_the_server_down() {
    // The leak the detached task exists to prevent. `codex app-server` holds
    // stdio open waiting for more requests, so a caller that drops this future
    // — a coalescing refresh superseded by a newer one, a panel that closes —
    // would otherwise strand one server per abandoned call, after every Codex
    // turn. `kill_on_drop` is not a substitute: it signals only the spawned
    // pid, never a descendant.
    let dir = tempfile::TempDir::new().unwrap();
    let pgid_path = dir.path().join("pgid");
    let fixture = dir.path().join("hang.jsonl");
    std::fs::write(
        &fixture,
        format!("// pgid_to:{}\n// hang\n", pgid_path.display()),
    )
    .unwrap();
    let shim = codex_shim(dir.path(), &fixture);

    // A bound far longer than the test, so nothing but the drop can end this.
    let mut call = Box::pin(read_account_usage(&shim, Duration::from_mins(10)));
    // Poll it once so the spawn actually happens, then walk away.
    let started = tokio::time::timeout(Duration::from_millis(200), &mut call).await;
    assert!(started.is_err(), "the fixture must not answer");
    let leader = wait_for_pgid(&pgid_path).await;
    drop(call);

    assert_group_reaped(leader).await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_server_is_killed_after_a_successful_read_too() {
    // Success is the path that leaks in production: the real server answers and
    // then keeps waiting for the next request, so "we got what we came for" is
    // not a reason to stop cleaning up.
    let dir = tempfile::TempDir::new().unwrap();
    let pgid_path = dir.path().join("pgid");
    let answer = std::fs::read_to_string(format!("{FIXTURES}/account-rate-limits.jsonl")).unwrap();
    let fixture = dir.path().join("answer-then-hang.jsonl");
    std::fs::write(
        &fixture,
        format!("// pgid_to:{}\n{answer}\n// hang\n", pgid_path.display()),
    )
    .unwrap();
    let shim = codex_shim(dir.path(), &fixture);

    let usage = read_account_usage(&shim, AMPLE).await.expect("a reading");
    assert!(usage.rate_limits_by_limit_id.contains_key("codex"));

    let leader = wait_for_pgid(&pgid_path).await;
    assert_group_reaped(leader).await;
}
