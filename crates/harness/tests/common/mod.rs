//! Helpers shared by the process-level adapter tests: wait for a fake CLI to
//! report its process group, and prove the whole group is gone afterwards.
//!
//! One copy on purpose. Each test file used to keep its own, and three of the
//! four checked only the group's leader, so a teardown that killed the spawned
//! process and left its children running still passed there.

use std::path::Path;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::killpg;
use nix::unistd::Pid;

/// Poll until `path` holds the process-group id a fake CLI wrote with its
/// `// pgid_to:` directive. Bounded, so a spawn failure panics with a message
/// rather than hanging the suite.
pub async fn wait_for_pgid(path: &Path) -> Pid {
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Ok(n) = s.trim().parse::<i32>()
        {
            return Pid::from_raw(n);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the fake CLI never wrote its pgid at {}", path.display());
}

/// Poll until **every** process in the group is gone.
///
/// Signal 0 to the group fails with `ESRCH` only when no member is left.
/// Checking the leader alone (`getpgid(leader)`) is not enough: a teardown that
/// kills only the spawned process passes that check while a child it started
/// keeps running, which is the leak `terminate_then_kill` exists to prevent.
/// Any other error (a permission error, say) is not evidence the group is
/// gone, so it fails the test instead of passing it.
///
/// This proves something only when the fixture started a child (one of the
/// fakes' `// spawn_child…` directives). With the leader alone in the group,
/// killing just the leader also empties it.
pub async fn assert_group_reaped(leader: Pid) {
    for _ in 0..200 {
        match killpg(leader, None) {
            Err(Errno::ESRCH) => return,
            Ok(()) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(other) => panic!("couldn't check process group {leader}: {other}"),
        }
    }
    panic!("process group {leader} still has a live process");
}
