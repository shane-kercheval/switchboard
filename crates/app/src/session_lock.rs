//! Cross-**process** advisory locks on harness session files.
//!
//! A dev build and the installed app resolve different config dirs, so they
//! never share a store — but they do share the harness's own session files
//! (`~/.claude/projects/…`, `~/.codex/sessions/…`), because those belong to the
//! harness, not to Switchboard. Two Switchboard instances driving the same
//! session concurrently is therefore possible and unguarded by anything in the
//! store. `docs/harness-behavior.md` §3.5 documents what that costs: forking a
//! session while another writer is mid-turn yields a semantically incomplete
//! snapshot — the parent's in-flight answer replaced by a synthesized
//! `"No response requested."` stub, which the branch then permanently lacks —
//! plus a merge-misattribution risk. That section closes by naming the residual
//! it accepts, that the in-process mid-turn fork gate is "a look, not a lock, so
//! a concurrent automation or a bare-CLI writer on the same session can still
//! slip through." A second Switchboard instance is exactly that writer, and this
//! module is what closes it.
//!
//! **Lock files are created on first use and never removed.** One per distinct
//! `(harness, session, cwd)` ever dispatched, zero bytes each, under the release
//! config dir — growth is bounded by how many conversations the install has ever
//! had. That is deliberate, and the tidy-up it invites is unsafe in a way that
//! produces no error: **unlinking a lock file does not release the lock anyone
//! currently holds on it**, and the next `create` lands on a fresh inode, so two
//! processes end up holding what each believes is the same lock, on two
//! different files. Locking the file before unlinking does not fix it either — a
//! third process can open the old inode, lose the lock race, and acquire it
//! after the unlink while a fourth creates and locks the replacement path,
//! splitting the namespace the same way. There is no cheap safe sweep, and the
//! files cost nothing, so the contract is simply: never unlink them.
//!
//! **The lock root is not the config dir.** Every other user-global path
//! resolves through `config_dir()`, which a debug build deliberately redirects
//! so dev runs never touch installed state. That isolation is the opposite of
//! what is wanted here: dev and release must contend on the *same* file or the
//! lock guards nothing. See `crate::session_lock_root`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::path::Path;

use sha2::{Digest, Sha256};
use switchboard_core::{HarnessKind, SessionLocator};
use switchboard_dispatcher::TurnPermit;

use crate::error::AppError;

/// Subdirectory of the lock root holding one file per locked session.
const LOCKS_DIR: &str = "locks";

/// Bumped if the key's input tuple or encoding ever changes.
///
/// **Bumping this is a protocol break, not a version stamp.** Two builds that
/// disagree about the schema compute different filenames and therefore stop
/// protecting each other entirely — and the common case is exactly that, since a
/// development build is usually newer than the installed one. Folding the
/// constant into the hash makes the break total and honest rather than partial
/// and silent, but that is a choice about *how* it fails, not a reason it is
/// free.
///
/// **So any bump must ship a transition that acquires both keys, old and new,
/// until every build in circulation computes the new one.** That is the rule, not
/// a suggestion: a bump without it silently removes the protection this module
/// exists to provide. The golden vectors in `tests` are what make an accidental
/// bump fail loudly.
const KEY_SCHEMA: &[u8] = b"switchboard-session-lock-v1";

/// Retry schedule for a transient `WouldBlock`, in milliseconds.
///
/// **Insurance against a spurious `WouldBlock`, not a claim about kernel
/// behaviour.** A handle released microseconds ago — the previous turn's permit,
/// or another process exiting — can lose a race with an acquisition starting at
/// the same instant; `acquire_project_lock_retries_a_transiently_released_lock`
/// exercises exactly that shape. An earlier version of this comment explained it
/// as the kernel finalizing `flock` release asynchronously. That was never
/// probed, and `close(2)` is specified to release synchronously, so the symptom
/// is stated here rather than an invented cause. The retry is cheap enough not
/// to need the stronger claim.
///
/// A genuinely live holder keeps the lock through the whole window, so real
/// contention still surfaces (just ~155 ms later); only the false positive is
/// absorbed. The uncontended path locks on the first try with no delay.
const RETRY_BACKOFF_MS: [u64; 5] = [5, 10, 20, 40, 80];

/// `try_lock` with the backoff above. `Err(())` means a live holder; I/O errors
/// surface to the caller, which knows what the file means.
///
/// Shared with `commands::acquire_project_lock` rather than copied: the schedule
/// and the reason for it are one decision, and two copies drift.
pub(crate) fn try_lock_with_backoff(file: &File) -> Result<(), std::io::Error> {
    let mut attempt = 0usize;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                let Some(&backoff) = RETRY_BACKOFF_MS.get(attempt) else {
                    return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
                };
                std::thread::sleep(std::time::Duration::from_millis(backoff));
                attempt += 1;
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// The lock-file stem for one harness session: a hex SHA-256 of the
/// `(schema, harness, locator, cwd-namespace)` tuple.
///
/// **Hashed, never the raw locator.** A Codex `thread_id` is an arbitrary
/// `String` (its own doc says so) — putting one in a filename is a path-validity
/// and length hazard, and a Claude cwd is a full absolute path that cannot be a
/// filename at all. A fixed-length hex digest is a valid filename for every
/// input by construction.
///
/// **Each component is length-prefixed**, so the encoding is injective by
/// construction rather than by argument: no byte inside a component can be read
/// as the boundary between two. Today's component types make an actual collision
/// unreachable — the one caller-controlled variable-length field, a Codex
/// `thread_id`, is followed by a fixed-format date — so this is structural
/// insurance for the next component someone adds, not a defence against a
/// demonstrated case. It costs one `u64` per component and is stated here
/// precisely so nobody has to re-derive whether a plain concatenation would have
/// been fine.
///
/// **`cwd` is part of the identity for Claude only.** Claude session ids are
/// namespaced by working directory — the same uuid under two different cwds
/// names two different files — so the id alone under-specifies. Codex thread ids
/// and Antigravity conversation uuids are globally unique, so including cwd
/// there would over-lock in the useless direction: the same session reached from
/// two directories would take two different locks and contend on neither. This
/// is the same per-harness scoping rationale the session-uniqueness scans use.
///
/// **Both enums are `#[non_exhaustive]`, and the unknown arms refuse.** A tag
/// invented for an unrecognized variant would be a guess about that harness's
/// namespacing, and a wrong guess here is silent under-locking — the failure the
/// whole module exists to prevent, reintroduced in the one place nobody would
/// look. `UnsupportedHarness` propagates to a refused turn, which is loud,
/// immediate, and exactly what should happen while a new harness is half-wired.
pub(crate) fn session_lock_key(
    harness: HarnessKind,
    locator: &SessionLocator,
    cwd: &Path,
) -> Result<String, AppError> {
    let mut hasher = Sha256::new();
    let mut component = |bytes: &[u8]| {
        hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(bytes);
    };
    component(KEY_SCHEMA);
    let (harness_tag, cwd_namespaced) = match harness {
        HarnessKind::ClaudeCode => (&b"claude-code"[..], true),
        HarnessKind::Codex => (&b"codex"[..], false),
        HarnessKind::Antigravity => (&b"antigravity"[..], false),
        _ => return Err(AppError::UnsupportedHarness),
    };
    component(harness_tag);
    match locator {
        SessionLocator::Uuid(id) => {
            component(b"uuid");
            component(id.as_bytes());
        }
        SessionLocator::Codex {
            thread_id,
            partition_date,
        } => {
            component(b"codex");
            component(thread_id.as_bytes());
            component(partition_date.to_string().as_bytes());
        }
        _ => return Err(AppError::UnsupportedHarness),
    }
    // Raw OS bytes, not a lossy UTF-8 conversion: two paths differing only in
    // bytes lossy conversion replaces must not collide onto one lock. Empty for
    // the harnesses that do not namespace by cwd — unambiguous because the
    // harness tag is already in the hash above.
    #[cfg(unix)]
    let cwd_bytes: &[u8] = {
        use std::os::unix::ffi::OsStrExt;
        cwd.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let cwd_bytes: &[u8] = cwd.as_os_str().as_encoded_bytes();
    component(if cwd_namespaced { cwd_bytes } else { b"" });
    // Hex by hand: `sha2` 0.11 returns a `hybrid-array` `Array`, which has no
    // `LowerHex` impl, and one fold is cheaper than a dependency on `hex`.
    Ok(hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut acc, byte| {
            // Writing to a `String` cannot fail.
            let _ = write!(acc, "{byte:02x}");
            acc
        }))
}

/// Take an exclusive advisory lock on every key, returning the permit that owns
/// them for the turn.
///
/// **Deadlock is impossible here, and sorted order is not what makes it so.**
/// `held` drops on any failure, so nothing is ever held while waiting for
/// something else — no hold-and-wait, no cycle, whatever the order. What the
/// `BTreeSet` buys is smaller and still real: a materializing fork takes two
/// locks (its own and its parent's) while a plain turn on the parent takes one,
/// and a total order identical in every process stops two of them refusing each
/// other on an overlapping pair they could have taken in sequence.
///
/// **Fail-closed.** Contention *and* I/O failure both refuse the turn. Do not
/// soften either to warn-and-proceed: this lock is what stands between two live
/// writers and a corrupted session snapshot, and the codebase already accepts
/// "can't persist ⇒ refuse the turn" for the send journal. A refusal costs the
/// user one retry; a proceed costs them a branch that permanently lacks the
/// parent's answer.
pub(crate) fn acquire_session_locks(
    lock_root: &Path,
    keys: &BTreeSet<String>,
) -> Result<TurnPermit, AppError> {
    if keys.is_empty() {
        return Ok(TurnPermit::none());
    }
    let dir = lock_root.join(LOCKS_DIR);
    std::fs::create_dir_all(&dir).map_err(|source| AppError::SessionLockIo { source })?;
    let mut held = Vec::with_capacity(keys.len());
    for key in keys {
        let path = dir.join(format!("{key}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            // A pure lock token — nothing is ever written to it, so neither
            // truncate nor preserve matters; pick the non-destructive one.
            .truncate(false)
            .open(&path)
            .map_err(|source| AppError::SessionLockIo { source })?;
        match try_lock_with_backoff(&file) {
            Ok(()) => held.push(file),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // `held` drops here, releasing anything already taken — a
                // partial acquisition must never outlive the failed attempt or
                // the next try contends with itself.
                return Err(AppError::SessionInUse);
            }
            Err(source) => return Err(AppError::SessionLockIo { source }),
        }
    }
    Ok(TurnPermit::holding(held))
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use chrono::NaiveDate;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{acquire_session_locks, session_lock_key};
    use crate::error::AppError;
    use switchboard_core::{HarnessKind, SessionLocator};

    fn uuid_locator() -> SessionLocator {
        SessionLocator::Uuid(
            Uuid::parse_str("11111111-2222-3333-4444-555555555555").expect("fixed uuid"),
        )
    }

    fn codex_locator(thread_id: &str, day: u32) -> SessionLocator {
        SessionLocator::Codex {
            thread_id: thread_id.to_owned(),
            partition_date: NaiveDate::from_ymd_opt(2026, 1, day).expect("valid date"),
        }
    }

    fn key(harness: HarnessKind, locator: &SessionLocator, cwd: &str) -> String {
        session_lock_key(harness, locator, std::path::Path::new(cwd)).expect("wired harness")
    }

    #[test]
    fn a_claude_session_is_namespaced_by_its_working_directory() {
        // Claude session ids are cwd-namespaced: the same uuid under two
        // directories names two different files on disk, so they must not
        // contend. Dropping `cwd` from the Claude arm makes these equal.
        let locator = uuid_locator();
        assert_ne!(
            key(HarnessKind::ClaudeCode, &locator, "/work/alpha"),
            key(HarnessKind::ClaudeCode, &locator, "/work/beta"),
        );
    }

    #[test]
    fn a_codex_session_is_the_same_lock_from_any_working_directory() {
        // The mirror of the case above, and the reason `cwd_namespaced` is
        // per-harness rather than always-on. A Codex thread id is globally
        // unique, so the same session reached from two directories is one file —
        // including cwd here would hand them two locks and protect neither.
        let locator = codex_locator("thread-abc", 2);
        assert_eq!(
            key(HarnessKind::Codex, &locator, "/work/alpha"),
            key(HarnessKind::Codex, &locator, "/work/beta"),
        );
    }

    #[test]
    fn the_same_uuid_under_two_harnesses_is_two_locks() {
        // Claude and Antigravity share the `Uuid` locator shape but not a
        // session-file namespace, so the harness tag has to be in the hash.
        let locator = uuid_locator();
        assert_ne!(
            key(HarnessKind::ClaudeCode, &locator, "/work/alpha"),
            key(HarnessKind::Antigravity, &locator, "/work/alpha"),
        );
    }

    #[test]
    fn a_codex_thread_is_identified_by_its_partition_date_too() {
        // The rollout file lives under `<year>/<month>/<day>/`, so the date is
        // part of *which file* the id names, not decoration on it.
        assert_ne!(
            key(HarnessKind::Codex, &codex_locator("thread-abc", 2), "/w"),
            key(HarnessKind::Codex, &codex_locator("thread-abc", 3), "/w"),
        );
    }

    #[test]
    fn a_key_is_a_valid_fixed_length_filename() {
        // The reason for hashing at all: a Codex thread id is an arbitrary
        // string and a cwd is an absolute path, neither of which is a filename.
        let key = key(
            HarnessKind::Codex,
            &codex_locator("../../etc/passwd\0 and a very long tail", 2),
            "/work/alpha",
        );
        assert_eq!(key.len(), 64, "sha-256 hex is fixed width");
        assert!(
            key.chars().all(|c| c.is_ascii_hexdigit()),
            "must contain nothing a path could interpret: {key}"
        );
    }

    /// Fixed expected digests for fixed inputs.
    ///
    /// **This is a wire contract between separately-installed builds, not a test
    /// fixture.** Two copies of Switchboard protect each other only by computing
    /// the *same filename* from the same conversation, and every other test here
    /// asserts keys only relative to one another — so a refactor of the component
    /// order, the length prefixes, the harness tags, or the schema constant could
    /// change every key while the whole suite stayed green, and the two builds
    /// would silently stop contending with no error anywhere.
    ///
    /// If a change makes these fail, the answer is almost never to update them.
    /// It is to revert the change, or to ship the dual-key transition described
    /// on `KEY_SCHEMA`.
    #[test]
    fn key_digests_are_pinned_across_builds() {
        // A change here is a cross-build protocol break, not a stale
        // expectation. Revert the change, or ship the dual-key transition
        // described on `KEY_SCHEMA`.
        assert_eq!(
            key(HarnessKind::ClaudeCode, &uuid_locator(), "/work/alpha"),
            "4a6e3180ee2a1c1ebf66c1cb6923440bd9d6a4e7d2929323c2c78e49acf039cb",
            "claude key changed"
        );
        assert_eq!(
            key(HarnessKind::Antigravity, &uuid_locator(), "/work/alpha"),
            "f72c7466d6a588b7b4670ca9cb0fd362b91baec9e8940d67cdea3d7a50b8b1db",
            "antigravity key changed"
        );
        assert_eq!(
            key(
                HarnessKind::Codex,
                &codex_locator("thread-abc", 2),
                "/work/alpha"
            ),
            "24ecf734db079a04988243f45e3322dac2e625d4fbfcfe7fdbbffd47a9218d2e",
            "codex key changed"
        );
    }

    #[test]
    fn a_held_key_refuses_a_second_holder() {
        let root = TempDir::new().expect("temp lock root");
        let keys = [key(HarnessKind::ClaudeCode, &uuid_locator(), "/w")]
            .into_iter()
            .collect();
        let _held = acquire_session_locks(root.path(), &keys).expect("first holder acquires");
        assert!(
            matches!(
                acquire_session_locks(root.path(), &keys),
                Err(AppError::SessionInUse)
            ),
            "a second holder of the same session must be refused"
        );
    }

    #[test]
    fn dropping_the_permit_frees_the_key() {
        // The whole point of returning an RAII permit: nothing calls "release,"
        // so a turn that ends any way at all leaves the session usable.
        let root = TempDir::new().expect("temp lock root");
        let keys: std::collections::BTreeSet<String> =
            [key(HarnessKind::ClaudeCode, &uuid_locator(), "/w")]
                .into_iter()
                .collect();
        drop(acquire_session_locks(root.path(), &keys).expect("first holder acquires"));
        acquire_session_locks(root.path(), &keys).expect("the next turn must be able to acquire");
    }

    #[test]
    fn a_refused_multi_key_acquisition_releases_what_it_already_took() {
        // The fork case: two keys, one contended. Keeping the uncontended half
        // would leave a lock nobody owns held for the process's lifetime — the
        // next attempt would then contend with its own leftovers forever.
        //
        // **The contended key is chosen as the one acquired *second*, derived
        // from the set's own order rather than assumed.** Written the obvious way
        // — hold a fixed key and hope it sorts last — the attempt fails on its
        // first key, takes nothing, and has nothing to give back, so the test
        // passes against an implementation that leaks every partial acquisition.
        // It did: this assertion survived `mem::forget`-ing the held handles
        // until the order was made explicit.
        let root = TempDir::new().expect("temp lock root");
        let both: std::collections::BTreeSet<String> = [
            key(HarnessKind::ClaudeCode, &uuid_locator(), "/w"),
            key(
                HarnessKind::ClaudeCode,
                &SessionLocator::Uuid(
                    Uuid::parse_str("99999999-8888-7777-6666-555555555555").expect("fixed uuid"),
                ),
                "/w",
            ),
        ]
        .into_iter()
        .collect();
        let mut ordered = both.iter();
        let taken_first = ordered.next().expect("two keys").clone();
        let taken_second = ordered.next().expect("two keys").clone();

        // Someone else already holds the second one, so the attempt below gets
        // the first, then refuses.
        let contended = [taken_second].into_iter().collect();
        let _held = acquire_session_locks(root.path(), &contended).expect("contended key acquired");

        assert!(matches!(
            acquire_session_locks(root.path(), &both),
            Err(AppError::SessionInUse)
        ));

        let released = [taken_first].into_iter().collect();
        acquire_session_locks(root.path(), &released)
            .expect("the key the refused attempt took must have been released with it");
    }

    #[test]
    fn no_keys_touches_no_files() {
        // An agent with no locator yet (a Codex or Antigravity first turn) has
        // no session to contend on. It must not be refused, and must not create
        // lock state for a session that does not exist.
        let root = TempDir::new().expect("temp lock root");
        acquire_session_locks(root.path(), &std::collections::BTreeSet::new())
            .expect("nothing to lock is not a refusal");
        assert!(
            !root.path().join(super::LOCKS_DIR).exists(),
            "an empty key set must not create the lock directory"
        );
    }

    #[test]
    fn an_unwritable_lock_root_refuses_the_turn() {
        // Fail-closed on I/O, not just on contention: an unwritable lock root
        // means we cannot prove the session is ours, which is the same state as
        // knowing it is not.
        let root = TempDir::new().expect("temp lock root");
        // A *file* where the locks directory must be, so `create_dir_all` fails.
        File::create(root.path().join(super::LOCKS_DIR)).expect("occupy the path");
        let keys = [key(HarnessKind::ClaudeCode, &uuid_locator(), "/w")]
            .into_iter()
            .collect();
        assert!(
            matches!(
                acquire_session_locks(root.path(), &keys),
                Err(AppError::SessionLockIo { .. })
            ),
            "an I/O failure must refuse rather than proceed unlocked"
        );
    }
}
