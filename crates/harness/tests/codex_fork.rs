//! Process-level tests for creating a Codex fork without a turn: drive
//! `fork_codex_session` against the `fake_codex` fixture binary and assert on
//! the locator, each failure mode, and the teardown.
//!
//! The live test (`live.rs`, `live_codex_fork_without_a_prompt_*`) proves the
//! real CLI still forks this way. It cannot make Codex exit non-zero, skip
//! `thread.started`, forget its rollout or hang on command, so those paths are
//! tested here or not at all.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;
use switchboard_core::AgentSelection;
use switchboard_harness::{CodexForkError, CodexLocator, fork_codex_session};

#[cfg(unix)]
mod common;
#[cfg(unix)]
use common::{assert_group_reaped, wait_for_pgid};

const FAKE_CODEX: &str = env!("CARGO_BIN_EXE_fake_codex");

const PARENT: &str = "01a12d13-1b40-7232-8895-631605068ea7";
const FORK: &str = "01a12d13-2870-7f61-93dc-f2b7256018a6";

/// Bounds a fork that is expected to finish: it only has to outlast a local
/// replay.
const AMPLE: Duration = Duration::from_secs(30);

/// The bound for the test that wants a timeout. Five seconds rather than less
/// for the reason `codex_account_usage.rs` gives: the fixture must reach its
/// `pgid_to` directive before the bound expires, and a cold first spawn after a
/// link has been seen to take more than two.
const WANTS_TIMEOUT: Duration = Duration::from_secs(5);

/// One test's world: a working folder, a fake home whose `.codex/sessions` the
/// rollout lookup scans, and a shim standing in for `codex`.
struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_owned();
        let home = root.join("home");
        let cwd = root.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        Self {
            _dir: dir,
            root,
            home,
            cwd,
        }
    }

    /// Where Codex would write a rollout for `id` in the `date` folder.
    fn rollout_path(&self, date: &str, id: &str) -> PathBuf {
        let folder = date.replace('-', "/");
        self.home
            .join(".codex/sessions")
            .join(folder)
            .join(format!("rollout-{date}T23-59-59-{id}.jsonl"))
    }

    /// Write `fixture` and a shim that runs `fake_codex` with the real argv
    /// plus the fixture path appended. The real fork passes no prompt, so the
    /// fixture has to come from outside the call (see `fake_codex`'s doc).
    /// `exec` keeps the shim's pid and process group, which the teardown
    /// assertions rely on.
    fn shim(&self, fixture: &str) -> PathBuf {
        let fixture_path = self.root.join("fixture.jsonl");
        std::fs::write(&fixture_path, fixture).unwrap();
        let shim = self.root.join("codex");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nexec {FAKE_CODEX} \"$@\" {}\n",
                fixture_path.display()
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

    async fn fork(&self, binary: &Path, timeout: Duration) -> Result<CodexLocator, CodexForkError> {
        fork_codex_session(
            binary,
            &self.home,
            &self.cwd,
            PARENT,
            &AgentSelection::default(),
            timeout,
        )
        .await
    }
}

fn thread_started(id: &str) -> String {
    format!(r#"{{"type":"thread.started","thread_id":"{id}"}}"#)
}

#[tokio::test]
async fn a_fork_returns_the_new_session_dated_by_the_folder_codex_wrote_it_in() {
    // A date that is not today: the partition date must come from where the
    // rollout actually is, so a creation that crosses midnight still finds it.
    // `// read_stdin` returns at once only if stdin is closed, as probed; an
    // open stdin would hang the fake until the bound.
    let setup = Setup::new();
    let rollout = setup.rollout_path("2026-10-10", FORK);
    let shim = setup.shim(&format!(
        "// read_stdin\n// write_file:{}\n{}\n",
        rollout.display(),
        thread_started(FORK)
    ));

    let locator = setup.fork(&shim, AMPLE).await.expect("a fork");

    assert_eq!(
        locator,
        CodexLocator {
            thread_id: FORK.to_owned(),
            partition_date: NaiveDate::from_ymd_opt(2026, 10, 10).unwrap(),
        }
    );
}

#[tokio::test]
async fn the_id_is_found_behind_lines_that_are_not_events() {
    let setup = Setup::new();
    let rollout = setup.rollout_path("2026-10-11", FORK);
    let shim = setup.shim(&format!(
        "// write_file:{}\nnot json at all\n{{\"type\":\"thread.started\"}}\n{}\n",
        rollout.display(),
        thread_started(FORK)
    ));

    let locator = setup.fork(&shim, AMPLE).await.expect("a fork");

    assert_eq!(locator.thread_id, FORK);
}

#[tokio::test]
async fn a_non_zero_exit_is_reported_with_codexs_own_words() {
    // The shape (exit 1, nothing on stdout, one line on stderr) is what Codex
    // produced when the probe resumed a fork whose parent was gone. The error
    // text itself is invented: the probe resumed, it did not fork, and printed
    // `missing source rollout`.
    let setup = Setup::new();
    let shim = setup.shim("// stderr:Error: thread/fork failed: no rollout found\n// exit:1\n");

    let error = setup.fork(&shim, AMPLE).await.unwrap_err();

    assert!(
        matches!(&error, CodexForkError::Exit { code: Some(1), stderr } if stderr.contains("no rollout found")),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("no rollout found"),
        "the message must carry Codex's reason; got {error}"
    );
}

#[tokio::test]
async fn a_non_zero_exit_wins_over_a_reported_id() {
    // Codex printed an id and then failed: whatever it wrote is not a fork to
    // register.
    let setup = Setup::new();
    let rollout = setup.rollout_path("2026-10-11", FORK);
    let shim = setup.shim(&format!(
        "// write_file:{}\n{}\n// exit:2\n",
        rollout.display(),
        thread_started(FORK)
    ));

    let error = setup.fork(&shim, AMPLE).await.unwrap_err();

    assert!(
        matches!(error, CodexForkError::Exit { code: Some(2), .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn exiting_cleanly_without_thread_started_is_refused() {
    let setup = Setup::new();
    let shim = setup.shim("// stderr:nothing to report\n{\"type\":\"turn.started\"}\n");

    let error = setup.fork(&shim, AMPLE).await.unwrap_err();

    assert!(
        matches!(&error, CodexForkError::NoThreadStarted { stderr } if stderr.contains("nothing to report")),
        "got {error:?}"
    );
}

#[tokio::test]
async fn a_reported_id_with_no_rollout_on_disk_is_refused() {
    // Switchboard must never register a fork whose session Codex did not write.
    let setup = Setup::new();
    let shim = setup.shim(&format!("{}\n", thread_started(FORK)));

    let error = setup.fork(&shim, AMPLE).await.unwrap_err();

    assert!(
        matches!(&error, CodexForkError::RolloutNotFound { thread_id } if thread_id == FORK),
        "got {error:?}"
    );
}

#[tokio::test]
async fn a_missing_binary_is_reported_as_missing() {
    let setup = Setup::new();
    let absent = setup.root.join("definitely-not-codex");

    let error = setup.fork(&absent, AMPLE).await.unwrap_err();

    assert!(
        matches!(error, CodexForkError::BinaryNotFound),
        "got {error:?}"
    );
}

#[tokio::test]
async fn a_missing_working_folder_is_not_reported_as_a_missing_binary() {
    let setup = Setup::new();
    let shim = setup.shim(&format!("{}\n", thread_started(FORK)));
    let gone = setup.root.join("deleted-project");

    let error = fork_codex_session(
        &shim,
        &setup.home,
        &gone,
        PARENT,
        &AgentSelection::default(),
        AMPLE,
    )
    .await
    .unwrap_err();

    assert!(
        matches!(&error, CodexForkError::WorkingDirectoryMissing(path) if *path == gone),
        "got {error:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_hung_fork_times_out_and_leaves_no_process_behind() {
    // The case the bound exists for: while this runs the caller holds the
    // parent's session lock, so a hang must end, and must not leave Codex (or a
    // child it started, which `spawn_child_holding_stderr` models) running.
    let setup = Setup::new();
    let pgid_path = setup.root.join("pgid");
    let shim = setup.shim(&format!(
        "// pgid_to:{}\n// spawn_child_holding_stderr\n// stderr:stuck creating the fork\n// hang\n",
        pgid_path.display()
    ));

    let error = setup.fork(&shim, WANTS_TIMEOUT).await.unwrap_err();

    assert!(
        matches!(&error, CodexForkError::Timeout { bound, stderr } if *bound == WANTS_TIMEOUT && stderr.contains("stuck creating the fork")),
        "got {error:?}"
    );
    assert_group_reaped(wait_for_pgid(&pgid_path).await).await;
}

#[cfg(unix)]
#[tokio::test]
async fn abandoning_the_fork_still_tears_codex_down() {
    // A bound far longer than the test, so only the drop can end the call.
    let setup = Setup::new();
    let pgid_path = setup.root.join("pgid");
    let shim = setup.shim(&format!(
        "// pgid_to:{}\n// spawn_child_holding_stderr\n// hang\n",
        pgid_path.display()
    ));

    let mut call = Box::pin(setup.fork(&shim, Duration::from_mins(10)));
    let started = tokio::time::timeout(Duration::from_millis(200), &mut call).await;
    assert!(started.is_err(), "the fixture must not finish");
    let leader = wait_for_pgid(&pgid_path).await;
    drop(call);

    assert_group_reaped(leader).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_helper_codex_leaves_running_is_killed_after_a_successful_fork() {
    // Codex exits on its own and its stdout closes, so nothing times out; the
    // child keeps only stderr open. A normal exit must still sweep the group.
    let setup = Setup::new();
    let pgid_path = setup.root.join("pgid");
    let rollout = setup.rollout_path("2026-10-11", FORK);
    let shim = setup.shim(&format!(
        "// pgid_to:{}\n// write_file:{}\n// spawn_child_holding_only_stderr\n{}\n",
        pgid_path.display(),
        rollout.display(),
        thread_started(FORK)
    ));

    let locator = setup.fork(&shim, AMPLE).await.expect("a fork");

    assert_eq!(locator.thread_id, FORK);
    assert_group_reaped(wait_for_pgid(&pgid_path).await).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_helper_codex_leaves_running_is_killed_after_a_failed_fork() {
    let setup = Setup::new();
    let pgid_path = setup.root.join("pgid");
    let shim = setup.shim(&format!(
        "// pgid_to:{}\n// spawn_child_holding_only_stderr\n// stderr:could not fork\n// exit:1\n",
        pgid_path.display()
    ));

    let error = setup.fork(&shim, AMPLE).await.unwrap_err();

    assert!(
        matches!(&error, CodexForkError::Exit { code: Some(1), stderr } if stderr.contains("could not fork")),
        "got {error:?}"
    );
    assert_group_reaped(wait_for_pgid(&pgid_path).await).await;
}
