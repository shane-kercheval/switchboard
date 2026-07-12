//! Integration tests for the git read layer, built against fixture repos
//! constructed by shelling the real `git` CLI into a `tempfile` tempdir. Shelling
//! `git` (rather than building with `git2`) keeps the fixtures faithful to the
//! on-disk shapes real repos have — including the `origin/HEAD` symbolic ref,
//! worktree records, and upstream config that the read layer keys on.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use switchboard_git::{
    BranchKind, ChangeKind, CommitRangeKind, DiffLineKind, SyncState, WorktreeWarning,
    changed_files, commit_changed_files, commit_file_diff, commit_ranges, file_diff, read_repo,
    resolve_repo_root,
};
use tempfile::TempDir;

/// Run `git` with `args` in `dir`, asserting success. Returns stdout trimmed.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed in {}:\nstdout: {}\nstderr: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Deterministic identity + no signing / no global config bleed, so tests are
/// hermetic regardless of the developer's git config.
fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).unwrap();
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// Run a git command with explicit author + committer dates. Commit times have
/// 1-second resolution, so commits created in the same wall-clock second tie
/// under the `TOPOLOGICAL | TIME` revwalk and sibling order becomes
/// non-deterministic — a fixed epoch makes order-sensitive tests stable.
fn git_dated(dir: &Path, epoch: i64, args: &[&str]) -> String {
    let date = format!("@{epoch} +0000");
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed in {}:\nstdout: {}\nstderr: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit_all_at(dir: &Path, message: &str, epoch: i64) {
    git(dir, &["add", "-A"]);
    git_dated(dir, epoch, &["commit", "-q", "-m", message]);
}

/// A repo with one commit on `main`. Returns the tempdir (kept alive by caller).
fn repo_with_main() -> TempDir {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    write(dir.path(), "README.md", "hello\n");
    commit_all(dir.path(), "initial");
    dir
}

/// A "remote" bare repo + a clone of it that has a real `origin/HEAD`, matching
/// what a cloned working repo looks like. Returns `(remote_dir, clone_dir)`.
fn cloned_repo() -> (TempDir, TempDir) {
    let origin_src = repo_with_main();
    let bare = TempDir::new().unwrap();
    git(
        bare.path(),
        &[
            "clone",
            "-q",
            "--bare",
            origin_src.path().to_str().unwrap(),
            ".",
        ],
    );
    let clone = TempDir::new().unwrap();
    git(
        clone.path(),
        &["clone", "-q", bare.path().to_str().unwrap(), "."],
    );
    git(clone.path(), &["config", "user.email", "test@example.com"]);
    git(clone.path(), &["config", "user.name", "Test"]);
    git(clone.path(), &["config", "commit.gpgsign", "false"]);
    (bare, clone)
}

fn branch_view<'a>(
    view: &'a switchboard_git::RepoView,
    name: &str,
) -> &'a switchboard_git::BranchView {
    view.local_branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("branch {name:?} not found in {:?}", view.local_branches))
}

// --- repo discovery & availability ----------------------------------------

#[test]
fn unavailable_path_yields_marked_view_not_error() {
    let dir = TempDir::new().unwrap();
    let not_a_repo = dir.path().join("nope");
    let view = read_repo(&not_a_repo).expect("must not error on a non-repo path");
    assert!(!view.available);
    assert!(view.local_branches.is_empty());
    assert!(view.remote_branches.is_empty());
    assert!(view.default_branch.is_none());
}

#[test]
fn resolves_repo_root_from_subdirectory_and_worktree() {
    let repo = repo_with_main();
    let root = repo.path().canonicalize().unwrap();

    // From a subdirectory.
    let sub = repo.path().join("src/inner");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(
        resolve_repo_root(&sub).unwrap().canonicalize().unwrap(),
        root
    );

    // From a linked worktree (shares the same git db → same root).
    git(repo.path(), &["branch", "feature"]);
    let wt = TempDir::new().unwrap();
    let wt_path = wt.path().join("wt");
    git(
        repo.path(),
        &["worktree", "add", wt_path.to_str().unwrap(), "feature"],
    );
    assert_eq!(
        resolve_repo_root(&wt_path).unwrap().canonicalize().unwrap(),
        root
    );

    // A non-repo path resolves to None.
    let outside = TempDir::new().unwrap();
    assert!(resolve_repo_root(outside.path()).is_none());
}

// --- default-branch detection (decision 4) --------------------------------

#[test]
fn default_branch_resolves_via_origin_head_when_cloned() {
    let (_bare, clone) = cloned_repo();
    let view = read_repo(clone.path()).unwrap();
    assert_eq!(view.default_branch.as_deref(), Some("main"));
}

#[test]
fn default_branch_falls_back_to_local_main_without_origin_head() {
    // A `git init` repo has no origin/HEAD — the local-main fallback is the
    // common path in practice, so it must be covered explicitly.
    let repo = repo_with_main();
    let view = read_repo(repo.path()).unwrap();
    assert_eq!(view.default_branch.as_deref(), Some("main"));
    // merged is resolvable against the local default.
    assert_eq!(branch_view(&view, "main").merged, Some(true));
}

#[test]
fn unresolvable_default_branch_yields_none_signals() {
    // A repo whose only branch is neither main nor master and has no
    // origin/HEAD: the default can't be resolved, so merged/behind_base are None.
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    git(dir.path(), &["checkout", "-q", "-b", "trunk"]);
    write(dir.path(), "f", "x\n");
    commit_all(dir.path(), "c1");

    let view = read_repo(dir.path()).unwrap();
    assert_eq!(view.default_branch, None);
    let trunk = branch_view(&view, "trunk");
    assert_eq!(trunk.merged, None);
    assert_eq!(trunk.behind_base, None);
}

// --- sync state (decision 3) ----------------------------------------------

#[test]
fn clean_branch_in_sync_with_upstream() {
    let (_bare, clone) = cloned_repo();
    let view = read_repo(clone.path()).unwrap();
    let main = branch_view(&view, "main");
    assert_eq!(main.sync, SyncState::InSync);
    assert!(main.upstream.is_some());
    assert!(!main.dangling);
}

#[test]
fn local_only_branch_has_no_upstream_and_is_not_unknown() {
    // A `git init` repo's branch has no upstream configured → LocalOnly, the
    // load-bearing "not pushed, but fine" state — distinct from Unknown.
    let repo = repo_with_main();
    let view = read_repo(repo.path()).unwrap();
    let main = branch_view(&view, "main");
    assert_eq!(main.sync, SyncState::LocalOnly);
    assert_eq!(main.upstream, None);
    assert!(!main.dangling);
}

#[test]
fn ahead_behind_and_diverged_against_upstream() {
    let (bare, clone) = cloned_repo();

    // Ahead: one local commit not pushed.
    write(clone.path(), "a.txt", "a\n");
    commit_all(clone.path(), "local ahead");
    let view = read_repo(clone.path()).unwrap();
    assert_eq!(
        branch_view(&view, "main").sync,
        SyncState::Ahead { commits: 1 }
    );

    // Make the remote advance by two commits via a second clone that pushes.
    let pusher = TempDir::new().unwrap();
    git(
        pusher.path(),
        &["clone", "-q", bare.path().to_str().unwrap(), "."],
    );
    git(pusher.path(), &["config", "user.email", "t@e.com"]);
    git(pusher.path(), &["config", "user.name", "T"]);
    git(pusher.path(), &["config", "commit.gpgsign", "false"]);
    write(pusher.path(), "b.txt", "b\n");
    commit_all(pusher.path(), "remote 1");
    write(pusher.path(), "c.txt", "c\n");
    commit_all(pusher.path(), "remote 2");
    git(pusher.path(), &["push", "-q", "origin", "main"]);

    // Our clone fetches the new remote refs (the read layer never fetches).
    git(clone.path(), &["fetch", "-q", "origin"]);

    // Now local is 1 ahead, 2 behind → diverged.
    let view = read_repo(clone.path()).unwrap();
    assert_eq!(
        branch_view(&view, "main").sync,
        SyncState::Diverged {
            ahead: 1,
            behind: 2
        }
    );
}

#[test]
fn dangling_branch_when_upstream_deleted() {
    let (bare, clone) = cloned_repo();
    // Create + push a feature branch, set upstream, then delete it on the remote.
    git(clone.path(), &["checkout", "-q", "-b", "feature"]);
    write(clone.path(), "f.txt", "f\n");
    commit_all(clone.path(), "feature work");
    git(clone.path(), &["push", "-q", "-u", "origin", "feature"]);
    // Delete the remote branch directly in the bare repo, then prune our remotes.
    git(bare.path(), &["branch", "-D", "feature"]);
    git(clone.path(), &["fetch", "-q", "-p", "origin"]);

    let view = read_repo(clone.path()).unwrap();
    let feature = branch_view(&view, "feature");
    assert!(
        feature.dangling,
        "feature should be dangling after its upstream was deleted"
    );
}

// --- merged / behind_base (decisions 4, 10/10a) ---------------------------

#[test]
fn merged_and_unmerged_branches() {
    let repo = repo_with_main();
    // Merged branch: branch off, then merge back into main (so its tip is an
    // ancestor of main).
    git(repo.path(), &["checkout", "-q", "-b", "done"]);
    write(repo.path(), "done.txt", "d\n");
    commit_all(repo.path(), "done work");
    git(repo.path(), &["checkout", "-q", "main"]);
    git(
        repo.path(),
        &["merge", "-q", "--no-ff", "done", "-m", "merge done"],
    );

    // Unmerged branch: diverges from main and is never merged.
    git(repo.path(), &["checkout", "-q", "-b", "wip"]);
    write(repo.path(), "wip.txt", "w\n");
    commit_all(repo.path(), "wip work");
    git(repo.path(), &["checkout", "-q", "main"]);

    let view = read_repo(repo.path()).unwrap();
    assert_eq!(branch_view(&view, "done").merged, Some(true));
    assert_eq!(branch_view(&view, "wip").merged, Some(false));
    // wip is 0 behind base (it has everything main has; it's *ahead*), done is 0.
    assert_eq!(branch_view(&view, "wip").behind_base, Some(0));
}

#[test]
fn behind_base_is_independent_of_sync() {
    // A branch behind the default branch but in sync with its own upstream
    // reports behind_base > 0 AND a non-Behind sync state — the two are
    // independent signals.
    let (bare, clone) = cloned_repo();

    // Advance main on the remote by 2 and fetch (updates origin/main, the base).
    let pusher = TempDir::new().unwrap();
    git(
        pusher.path(),
        &["clone", "-q", bare.path().to_str().unwrap(), "."],
    );
    git(pusher.path(), &["config", "user.email", "t@e.com"]);
    git(pusher.path(), &["config", "user.name", "T"]);
    git(pusher.path(), &["config", "commit.gpgsign", "false"]);
    write(pusher.path(), "m1.txt", "1\n");
    commit_all(pusher.path(), "main 1");
    write(pusher.path(), "m2.txt", "2\n");
    commit_all(pusher.path(), "main 2");
    git(pusher.path(), &["push", "-q", "origin", "main"]);

    // Branch off the OLD main, push it + set upstream, so it's in sync with its
    // own upstream but behind the (now-advanced) base.
    git(clone.path(), &["checkout", "-q", "-b", "side"]);
    git(clone.path(), &["push", "-q", "-u", "origin", "side"]);
    git(clone.path(), &["fetch", "-q", "origin"]);

    let view = read_repo(clone.path()).unwrap();
    let side = branch_view(&view, "side");
    assert_eq!(
        side.sync,
        SyncState::InSync,
        "in sync with its own upstream"
    );
    assert_eq!(
        side.behind_base,
        Some(2),
        "but two commits behind the advanced base"
    );
}

#[test]
fn remote_branches_carry_merged_and_behind_base_only() {
    let (_bare, clone) = cloned_repo();
    // Push a feature branch to the remote so it shows as a remote branch.
    git(clone.path(), &["checkout", "-q", "-b", "feature"]);
    write(clone.path(), "f.txt", "f\n");
    commit_all(clone.path(), "feature");
    git(clone.path(), &["push", "-q", "-u", "origin", "feature"]);
    git(clone.path(), &["fetch", "-q", "origin"]);
    // Bare-repo default stays main; feature is ahead of base, not merged.

    let view = read_repo(clone.path()).unwrap();
    let remote_feature = view
        .remote_branches
        .iter()
        .find(|b| b.name == "origin/feature")
        .expect("origin/feature should be listed");
    assert_eq!(remote_feature.merged, Some(false));
    assert_eq!(remote_feature.behind_base, Some(0));
    // The symbolic origin/HEAD pointer is not listed as a branch.
    assert!(
        !view
            .remote_branches
            .iter()
            .any(|b| b.name.ends_with("/HEAD")),
        "origin/HEAD must be filtered out"
    );
}

// --- worktrees: dirty/untracked, detached, orphaned, prunable -------------

#[test]
fn worktree_dirty_and_untracked_are_separate_signals() {
    let repo = repo_with_main();

    // Dirty (tracked change), no untracked.
    write(repo.path(), "README.md", "changed\n");
    let view = read_repo(repo.path()).unwrap();
    let wt = branch_view(&view, "main").worktree.as_ref().unwrap();
    assert!(wt.dirty);
    assert!(!wt.untracked);

    // Commit that, then add an untracked-only file.
    commit_all(repo.path(), "commit change");
    write(repo.path(), "new.txt", "u\n");
    let view = read_repo(repo.path()).unwrap();
    let wt = branch_view(&view, "main").worktree.as_ref().unwrap();
    assert!(!wt.dirty, "no tracked changes");
    assert!(wt.untracked, "an untracked file is present");
}

#[test]
fn linked_branch_worktree_is_attached_to_its_branch() {
    let repo = repo_with_main();
    git(repo.path(), &["branch", "feature"]);
    let wt = TempDir::new().unwrap();
    let wt_path = wt.path().join("feature-wt");
    git(
        repo.path(),
        &["worktree", "add", wt_path.to_str().unwrap(), "feature"],
    );

    let view = read_repo(repo.path()).unwrap();
    let feature = branch_view(&view, "feature");
    let feature_wt = feature
        .worktree
        .as_ref()
        .expect("feature should carry its linked worktree");
    assert_eq!(
        feature_wt.path.canonicalize().unwrap(),
        wt_path.canonicalize().unwrap()
    );
    assert!(feature_wt.warning.is_none());
}

#[test]
fn detached_head_worktree_grouped_separately_with_hash() {
    let repo = repo_with_main();
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let wt = TempDir::new().unwrap();
    let wt_path = wt.path().join("detached-wt");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "--detach",
            wt_path.to_str().unwrap(),
            &head,
        ],
    );

    let view = read_repo(repo.path()).unwrap();
    assert_eq!(view.detached_worktrees.len(), 1);
    let detached = &view.detached_worktrees[0];
    assert!(detached.detached_hash.is_some());
    assert!(head.starts_with(detached.detached_hash.as_deref().unwrap()));
}

#[test]
fn orphaned_worktree_when_branch_deleted() {
    let repo = repo_with_main();
    git(repo.path(), &["branch", "doomed"]);
    let wt = TempDir::new().unwrap();
    let wt_path = wt.path().join("doomed-wt");
    git(
        repo.path(),
        &["worktree", "add", wt_path.to_str().unwrap(), "doomed"],
    );
    // Delete the branch ref out from under the worktree → orphaned. `branch -D`
    // safety-refuses a checked-out branch, so delete the ref directly (which is
    // exactly the on-disk state a real orphaned worktree is in).
    git(repo.path(), &["update-ref", "-d", "refs/heads/doomed"]);

    let view = read_repo(repo.path()).unwrap();
    let orphaned = view
        .detached_worktrees
        .iter()
        .find(|w| w.warning == Some(WorktreeWarning::Orphaned))
        .expect("an orphaned worktree should be flagged");
    assert_eq!(
        orphaned.path.canonicalize().unwrap(),
        wt_path.canonicalize().unwrap()
    );
    // The deleted branch must not leak into the local-branch list.
    assert!(
        !view.local_branches.iter().any(|b| b.name == "doomed"),
        "a deleted branch should not appear as a local branch"
    );
}

#[test]
fn prunable_worktree_attaches_to_its_branch() {
    let repo = repo_with_main();
    git(repo.path(), &["branch", "gone"]);
    let wt = TempDir::new().unwrap();
    let wt_path = wt.path().join("gone-wt");
    git(
        repo.path(),
        &["worktree", "add", wt_path.to_str().unwrap(), "gone"],
    );
    // Remove the directory but keep git's worktree record → prunable. Git still
    // considers `gone` checked out here, so the warning must ride on that branch.
    std::fs::remove_dir_all(&wt_path).unwrap();

    let view = read_repo(repo.path()).unwrap();
    let gone = branch_view(&view, "gone");
    let gone_wt = gone
        .worktree
        .as_ref()
        .expect("a prunable worktree must stay attached to its branch");
    assert_eq!(gone_wt.warning, Some(WorktreeWarning::Prunable));
    // It must NOT appear as a branch-less detached row (that's the bug this guards).
    assert!(
        !view
            .detached_worktrees
            .iter()
            .any(|w| w.warning == Some(WorktreeWarning::Prunable)),
        "a prunable record with a recoverable branch should not be a detached row"
    );
}

#[test]
fn prunable_detached_worktree_falls_back_to_detached_row() {
    let repo = repo_with_main();
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let wt = TempDir::new().unwrap();
    let wt_path = wt.path().join("detached-gone-wt");
    // A detached-HEAD worktree has no branch to attach to.
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "--detach",
            wt_path.to_str().unwrap(),
            &head,
        ],
    );
    std::fs::remove_dir_all(&wt_path).unwrap();

    let view = read_repo(repo.path()).unwrap();
    assert!(
        view.detached_worktrees
            .iter()
            .any(|w| w.warning == Some(WorktreeWarning::Prunable)),
        "a prunable record with no branch falls back to a detached row: {:?}",
        view.detached_worktrees
    );
}

// --- bare repo (graceful) --------------------------------------------------

#[test]
fn bare_repo_lists_branches_and_is_marked_bare() {
    let origin_src = repo_with_main();
    git(origin_src.path(), &["branch", "feature"]);
    let bare = TempDir::new().unwrap();
    git(
        bare.path(),
        &[
            "clone",
            "-q",
            "--bare",
            origin_src.path().to_str().unwrap(),
            ".",
        ],
    );

    let view = read_repo(bare.path()).unwrap();
    assert!(view.available, "a bare repo is available, not unavailable");
    assert!(view.is_bare);
    // Its branches still list (main + feature).
    assert!(view.local_branches.iter().any(|b| b.name == "main"));
    assert!(view.local_branches.iter().any(|b| b.name == "feature"));
}

#[test]
fn bare_repo_root_resolves_correctly_via_a_linked_worktree() {
    // The `--bare` + `worktree add` layout, read from one of the worktrees.
    // Bareness reflects how a handle was opened (a linked worktree opens
    // non-bare), so the root must be derived authoritatively — both the dedup
    // key (`resolve_repo_root`) and the rendered model (`read_repo`) must
    // point at the bare repo itself, not its parent directory.
    let origin_src = repo_with_main();
    git(origin_src.path(), &["branch", "feature"]);
    let bare_dir = TempDir::new().unwrap();
    let bare_path = bare_dir.path().join("repo.git");
    git(
        bare_dir.path(),
        &[
            "clone",
            "-q",
            "--bare",
            origin_src.path().to_str().unwrap(),
            bare_path.to_str().unwrap(),
        ],
    );
    let wt = TempDir::new().unwrap();
    let wt_path = wt.path().join("feature-wt");
    git(
        &bare_path,
        &["worktree", "add", wt_path.to_str().unwrap(), "feature"],
    );

    let expected = bare_path.canonicalize().unwrap();
    assert_eq!(
        resolve_repo_root(&wt_path).unwrap().canonicalize().unwrap(),
        expected,
        "dedup key (resolve_repo_root) must be the bare repo, not its parent"
    );
    let view = read_repo(&wt_path).unwrap();
    assert_eq!(
        view.root.canonicalize().unwrap(),
        expected,
        "rendered model root must be the bare repo"
    );
    assert!(
        view.is_bare,
        "read via a linked worktree must still see bare"
    );
}

// --- changed files + diff text -------------------------------------------

#[test]
fn changed_files_covers_staged_unstaged_untracked() {
    let repo = repo_with_main();
    write(repo.path(), "tracked.txt", "v1\n");
    commit_all(repo.path(), "add tracked");

    // Staged modification.
    write(repo.path(), "tracked.txt", "v2\n");
    git(repo.path(), &["add", "tracked.txt"]);
    // Unstaged new file staged then modified-in-worktree, plus a pure untracked.
    write(repo.path(), "staged-add.txt", "s\n");
    git(repo.path(), &["add", "staged-add.txt"]);
    write(repo.path(), "untracked.txt", "u\n");

    let files = changed_files(repo.path()).unwrap();
    let by_name = |n: &str| files.iter().find(|f| f.path == n).map(|f| f.change);
    assert_eq!(by_name("tracked.txt"), Some(ChangeKind::Modified));
    assert_eq!(by_name("staged-add.txt"), Some(ChangeKind::Added));
    assert_eq!(by_name("untracked.txt"), Some(ChangeKind::Untracked));
}

#[test]
fn changed_files_carries_line_counts_per_kind() {
    let repo = repo_with_main();
    write(repo.path(), "tracked.txt", "one\ntwo\nthree\n");
    commit_all(repo.path(), "add tracked");

    // One line replaced (1+/1−), one untracked (all additions), one staged add.
    write(repo.path(), "tracked.txt", "one\nCHANGED\nthree\n");
    write(repo.path(), "untracked.txt", "a\nb\n");
    write(repo.path(), "staged-add.txt", "s\n");
    git(repo.path(), &["add", "staged-add.txt"]);

    let files = changed_files(repo.path()).unwrap();
    let counts = |n: &str| {
        files
            .iter()
            .find(|f| f.path == n)
            .map(|f| (f.additions, f.deletions))
    };
    assert_eq!(counts("tracked.txt"), Some((Some(1), Some(1))));
    assert_eq!(counts("untracked.txt"), Some((Some(2), Some(0))));
    assert_eq!(counts("staged-add.txt"), Some((Some(1), Some(0))));
}

#[test]
fn changed_files_reports_no_counts_for_binary_content() {
    let repo = repo_with_main();
    std::fs::write(repo.path().join("blob.bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();

    let files = changed_files(repo.path()).unwrap();
    let binary = files.iter().find(|f| f.path == "blob.bin").unwrap();
    assert_eq!(binary.change, ChangeKind::Untracked);
    assert_eq!(
        (binary.additions, binary.deletions),
        (None, None),
        "binary counts must be absent, not zero — the UI renders them differently"
    );
}

#[test]
fn changed_files_counts_key_a_rename_by_its_new_path() {
    let repo = repo_with_main();
    write(repo.path(), "old-name.txt", "one\ntwo\nthree\nfour\n");
    commit_all(repo.path(), "add file");
    git(repo.path(), &["mv", "old-name.txt", "new-name.txt"]);
    write(repo.path(), "new-name.txt", "one\ntwo\nthree\nEDITED\n");
    git(repo.path(), &["add", "new-name.txt"]);

    let files = changed_files(repo.path()).unwrap();
    let renamed = files.iter().find(|f| f.path == "new-name.txt").unwrap();
    assert_eq!(renamed.change, ChangeKind::Renamed);
    // The rename pairs old→new, so counts reflect the real edit — not the
    // whole file re-added.
    assert_eq!((renamed.additions, renamed.deletions), (Some(1), Some(1)));
}

#[test]
fn changed_files_counts_survive_a_heavily_edited_rename() {
    // The listing (status walk) and the counts (parallel tree diff) each run
    // their own rename detection; the counts join only lands if both classify
    // the file as the same rename. A heavy — but clearly above-threshold —
    // edit pins that agreement. Deliberately NOT a borderline-similarity case:
    // that would test libgit2's heuristic, not this crate's contract.
    let repo = repo_with_main();
    let original: String = (0..10).fold(String::new(), |mut s, i| {
        use std::fmt::Write as _;
        let _ = writeln!(s, "line {i}");
        s
    });
    write(repo.path(), "old-name.txt", &original);
    commit_all(repo.path(), "add file");

    git(repo.path(), &["mv", "old-name.txt", "new-name.txt"]);
    // Rewrite 3 of 10 lines (~70% similar — well above the ~50% default).
    let edited = original
        .replace("line 1\n", "rewritten 1\n")
        .replace("line 4\n", "rewritten 4\n")
        .replace("line 8\n", "rewritten 8\n");
    write(repo.path(), "new-name.txt", &edited);
    git(repo.path(), &["add", "-A"]);

    let files = changed_files(repo.path()).unwrap();
    let renamed = files.iter().find(|f| f.path == "new-name.txt").unwrap();
    assert_eq!(renamed.change, ChangeKind::Renamed);
    assert_eq!(
        (renamed.additions, renamed.deletions),
        (Some(3), Some(3)),
        "both rename detectors must agree so the counts join lands"
    );
}

#[test]
fn file_diff_returns_structured_hunks_with_line_numbers() {
    let repo = repo_with_main();
    write(repo.path(), "code.txt", "line one\nline two\nline three\n");
    commit_all(repo.path(), "add code");
    write(
        repo.path(),
        "code.txt",
        "line one\nline CHANGED\nline three\n",
    );

    let diff = file_diff(repo.path(), "code.txt").unwrap();
    assert!(!diff.binary);
    assert!(!diff.truncated);
    assert_eq!(diff.hunks.len(), 1, "one contiguous change → one hunk");

    let lines = &diff.hunks[0].lines;
    let removed = lines
        .iter()
        .find(|l| l.origin == DiffLineKind::Removed)
        .expect("a removed line");
    assert_eq!(removed.content, "line two");
    // A removed line exists only on the old side.
    assert!(removed.old_lineno.is_some() && removed.new_lineno.is_none());

    let added = lines
        .iter()
        .find(|l| l.origin == DiffLineKind::Added)
        .expect("an added line");
    assert_eq!(added.content, "line CHANGED");
    assert!(added.new_lineno.is_some() && added.old_lineno.is_none());

    // Surrounding context carries both line numbers and keeps its text.
    let context = lines
        .iter()
        .find(|l| l.origin == DiffLineKind::Context && l.content == "line one")
        .expect("the context line");
    assert!(context.old_lineno.is_some() && context.new_lineno.is_some());
}

#[test]
fn file_diff_includes_untracked_file_content_as_additions() {
    let repo = repo_with_main();
    write(repo.path(), "brand-new.txt", "fresh content\n");

    let diff = file_diff(repo.path(), "brand-new.txt").unwrap();
    let added: Vec<&str> = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.origin == DiffLineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(added, vec!["fresh content"]);
}

#[test]
fn file_diff_is_empty_for_a_clean_file() {
    let repo = repo_with_main();
    write(repo.path(), "code.txt", "stable\n");
    commit_all(repo.path(), "add code");

    let diff = file_diff(repo.path(), "code.txt").unwrap();
    assert!(diff.hunks.is_empty() && !diff.binary && !diff.truncated);
}

#[test]
fn file_diff_caps_a_huge_diff_and_flags_truncation() {
    // The cap protects the panel from giant generated files. An untracked file
    // larger than the cap should come back flagged `truncated`, with no more than
    // the cap's worth of lines and no empty trailing hunks left after the cutoff.
    let repo = repo_with_main();
    let mut huge = String::new();
    for i in 0..6_000 {
        use std::fmt::Write as _;
        let _ = writeln!(huge, "line {i}");
    }
    write(repo.path(), "huge.txt", &huge);

    let diff = file_diff(repo.path(), "huge.txt").unwrap();
    assert!(
        diff.truncated,
        "a >5000-line diff must be flagged truncated"
    );
    let collected: usize = diff.hunks.iter().map(|h| h.lines.len()).sum();
    assert!(
        collected <= 5_000,
        "collected {collected} lines, expected <= cap"
    );
    assert!(
        diff.hunks.iter().all(|h| !h.lines.is_empty()),
        "no empty hunks should be appended after the cap"
    );
}

// Backend size limits, mirrored from `read.rs` (private consts aren't visible to an
// integration test). The existing huge-diff test already hardcodes the 5000-line
// cap the same way.
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_DIFF_LINE_BYTES: usize = 10_000;
const TOO_LARGE_TO_DIFF_BYTES: usize = 10 * 1024 * 1024;

fn collected_lines(diff: &switchboard_git::FileDiff) -> Vec<&switchboard_git::DiffLine> {
    diff.hunks.iter().flat_map(|h| &h.lines).collect()
}

#[test]
fn file_diff_byte_cap_stops_a_file_of_many_mid_size_lines() {
    // Isolates the *byte* cap from the per-line clamp: every line is 9 KB — under the
    // per-line clamp (10 KB) — so no line is individually cut, and the file is ~2.3 MB,
    // under the too-large limit (10 MB). The only thing that can stop collection is
    // the 2 MB byte budget. This also proves the tier boundary: a file between the
    // byte cap and the too-large limit yields a real truncated preview, not the
    // too-large placeholder.
    let repo = repo_with_main();
    let line_len = 9 * 1024;
    let line = "x".repeat(line_len);
    let mut content = String::new();
    for _ in 0..250 {
        content.push_str(&line); // 9 KB/line × 250 ≈ 2.3 MB total
        content.push('\n');
    }
    write(repo.path(), "data.csv", &content);

    let diff = file_diff(repo.path(), "data.csv").unwrap();
    assert!(
        !diff.too_large,
        "a ~2.3 MB file is under the too-large limit"
    );
    assert!(diff.truncated, "but over the byte budget, so truncated");
    let lines = collected_lines(&diff);
    assert!(
        !lines.is_empty(),
        "a real preview must be shown, not nothing"
    );
    let longest = lines.iter().map(|l| l.content.len()).max().unwrap_or(0);
    assert_eq!(
        longest, line_len,
        "no line should be per-line-clamped; the byte cap is what stopped collection"
    );
    let total: usize = lines.iter().map(|l| l.content.len()).sum();
    assert!(
        total > MAX_DIFF_BYTES && total <= MAX_DIFF_BYTES + line_len,
        "retained {total} bytes, expected just past the {MAX_DIFF_BYTES}-byte budget"
    );
}

#[test]
fn file_diff_clamps_a_single_giant_line() {
    // One line (no newline) a little over the per-line clamp: under the too-large
    // limit and under the line cap, so only the per-line clamp can bound it. Sized
    // just past the clamp — no need for megabytes to exercise the cut.
    let repo = repo_with_main();
    let giant = "a".repeat(MAX_DIFF_LINE_BYTES * 2);
    write(repo.path(), "oneline.txt", &giant);

    let diff = file_diff(repo.path(), "oneline.txt").unwrap();
    assert!(!diff.too_large, "5 MB is under the too-large limit");
    assert!(diff.truncated, "the giant line must flag truncation");
    let longest = collected_lines(&diff)
        .iter()
        .map(|l| l.content.len())
        .max()
        .unwrap_or(0);
    assert!(
        longest <= MAX_DIFF_LINE_BYTES,
        "the line must be clamped to the {MAX_DIFF_LINE_BYTES}-byte cap, got {longest}"
    );
}

#[test]
fn file_diff_clamps_a_giant_invalid_utf8_line_without_widening_it_whole() {
    // Invalid-UTF-8 "text" (high bytes, no NUL → libgit2 treats it as text, not
    // binary). The clamp must cut the raw bytes *before* `from_utf8_lossy`, so the
    // retained content stays bounded instead of widening the whole line into
    // replacement chars. `U+FFFD` is 3 bytes, so the clamped line can widen to at
    // most 3× the clamp — still bounded. Sized just past the clamp.
    let repo = repo_with_main();
    let giant = vec![0xFFu8; MAX_DIFF_LINE_BYTES * 2];
    std::fs::write(repo.path().join("blob.dat"), &giant).unwrap();

    let diff = file_diff(repo.path(), "blob.dat").unwrap();
    assert!(!diff.binary, "no NUL bytes, so libgit2 renders it as text");
    assert!(diff.truncated, "the giant line must flag truncation");
    let longest = collected_lines(&diff)
        .iter()
        .map(|l| l.content.len())
        .max()
        .unwrap_or(0);
    assert!(
        longest <= MAX_DIFF_LINE_BYTES * 3,
        "retained {longest} bytes; the raw line must be cut before widening, not after"
    );
}

#[test]
fn file_diff_marks_a_file_over_the_inline_limit_too_large() {
    // A new file past the too-large limit is never rendered: it short-circuits to
    // `too_large` with its size and no hunks, so libgit2 never loads the blob.
    let repo = repo_with_main();
    let size = TOO_LARGE_TO_DIFF_BYTES + 1024 * 1024; // ~11 MB
    write(repo.path(), "recording.json", &"a".repeat(size));

    let diff = file_diff(repo.path(), "recording.json").unwrap();
    assert!(
        diff.too_large,
        "a file past the limit must be flagged too_large"
    );
    assert!(!diff.truncated, "too_large is distinct from truncated");
    assert!(
        diff.hunks.is_empty(),
        "no content is rendered for a too-large file"
    );
    assert_eq!(
        diff.too_large_bytes,
        Some(size as u64),
        "the file size must be carried for the UI message"
    );
}

#[test]
fn file_diff_clean_large_tracked_file_is_empty_not_too_large() {
    // A clean (committed, unmodified) large file has no delta, so it must come back
    // empty — never flagged too_large. Guards the contract the removed filesystem
    // fast path used to violate.
    let repo = repo_with_main();
    let size = TOO_LARGE_TO_DIFF_BYTES + 1024 * 1024; // ~11 MB
    write(repo.path(), "big.bin", &"a".repeat(size));
    commit_all(repo.path(), "add big file");

    let diff = file_diff(repo.path(), "big.bin").unwrap();
    assert!(
        !diff.too_large,
        "a clean file has no change to be too large"
    );
    assert!(diff.hunks.is_empty(), "a clean file has no hunks");
}

#[test]
fn file_diff_modified_large_tracked_file_is_too_large() {
    // A *modified* large tracked file routes through the tracked-delta path, not the
    // untracked-content path the other too_large test covers — pin it separately.
    let repo = repo_with_main();
    let size = TOO_LARGE_TO_DIFF_BYTES + 1024 * 1024; // ~11 MB
    write(repo.path(), "big.bin", &"a".repeat(size));
    commit_all(repo.path(), "add big file");
    write(repo.path(), "big.bin", &"b".repeat(size)); // changed content, still huge

    let diff = file_diff(repo.path(), "big.bin").unwrap();
    assert!(diff.too_large, "a modified huge file must be too_large");
    assert!(diff.hunks.is_empty());
}

#[test]
fn file_diff_worktree_mode_only_change_of_large_file_is_reported_too_large() {
    // Documents a known limitation: a worktree mode-only change (chmod, no content
    // change) on a huge file is reported `too_large` even though there's nothing to
    // render. libgit2 leaves the worktree side's blob id unset, so we can't confirm
    // the content is unchanged without hashing the file — and a content change with a
    // coincidentally-equal size would be indistinguishable. The label is imperfect,
    // but the file is still gated by header size with no content load, and chmod-ing
    // a 10 MB+ tracked file is vanishingly rare. (A *committed* pure rename, where
    // both blob ids are real, is correctly not too_large — see the commit-path test.)
    let repo = repo_with_main();
    git(repo.path(), &["config", "core.fileMode", "true"]);
    let size = TOO_LARGE_TO_DIFF_BYTES + 1024 * 1024; // ~11 MB
    write(repo.path(), "big.sh", &"a".repeat(size));
    commit_all(repo.path(), "add big file");
    std::fs::set_permissions(
        repo.path().join("big.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let diff = file_diff(repo.path(), "big.sh").unwrap();
    assert!(
        diff.too_large,
        "documents the worktree mode-only limitation"
    );
    assert!(diff.hunks.is_empty());
}

#[test]
fn file_diff_worktree_rename_of_large_file_stays_too_large() {
    // A worktree rename isn't paired (no find_similar on the workdir path): it's a
    // large deletion plus a large untracked add, each real content. Both sides must
    // stay too_large — the identical-blob skip must not absorb them.
    let repo = repo_with_main();
    let size = TOO_LARGE_TO_DIFF_BYTES + 1024 * 1024; // ~11 MB
    write(repo.path(), "old.bin", &"a".repeat(size));
    commit_all(repo.path(), "add big file");
    std::fs::rename(repo.path().join("old.bin"), repo.path().join("new.bin")).unwrap();

    assert!(
        file_diff(repo.path(), "new.bin").unwrap().too_large,
        "the untracked add side stays too_large"
    );
    assert!(
        file_diff(repo.path(), "old.bin").unwrap().too_large,
        "the deletion side stays too_large"
    );
}

#[test]
fn file_diff_flags_binary_content_with_no_body() {
    // The binary flag rests on libgit2 emitting a binary marker under our diff
    // options; pin that end-to-end rather than trusting the assumption.
    let repo = repo_with_main();
    std::fs::write(repo.path().join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    commit_all(repo.path(), "add binary");
    std::fs::write(repo.path().join("blob.bin"), [0u8, 1, 2, 3, 0, 255, 254]).unwrap();

    let diff = file_diff(repo.path(), "blob.bin").unwrap();
    assert!(diff.binary, "binary content must set the binary flag");
    assert!(diff.hunks.is_empty(), "binary diffs carry no rendered body");
}

#[test]
fn file_diff_matches_the_path_literally_not_as_a_glob() {
    // A real filename with pathspec metacharacters must match only itself — not a
    // sibling its glob form would match — and the result must not merge files.
    let repo = repo_with_main();
    write(repo.path(), "a[1].txt", "base\n");
    write(repo.path(), "a1.txt", "base\n"); // what `a[1].txt` would glob-match
    commit_all(repo.path(), "add both");
    write(repo.path(), "a[1].txt", "LITERAL CHANGE\n");
    write(repo.path(), "a1.txt", "GLOB CHANGE\n");

    let diff = file_diff(repo.path(), "a[1].txt").unwrap();
    let added: Vec<&str> = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.origin == DiffLineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(
        added,
        vec!["LITERAL CHANGE"],
        "only the literal file's change"
    );
    assert!(
        !added.contains(&"GLOB CHANGE"),
        "the glob-matched sibling must not bleed in"
    );
}

#[test]
fn clean_worktree_has_no_changed_files() {
    let repo = repo_with_main();
    assert!(changed_files(repo.path()).unwrap().is_empty());
}

// --- mid-read failures surface as errors, never as a false "clean" ---------

/// A garbage `.git/index` makes libgit2's status read fail. The read layer must
/// surface that as `GitError`, not report the worktree as clean — `dirty: false`
/// is the "safe to delete/prune" signal, so a false negative here is dangerous.
fn corrupt_index(dir: &Path) {
    std::fs::write(dir.join(".git/index"), b"GARBAGE NOT AN INDEX").unwrap();
}

#[test]
fn read_repo_errors_on_unreadable_status_rather_than_reporting_clean() {
    let repo = repo_with_main();
    corrupt_index(repo.path());
    assert!(
        read_repo(repo.path()).is_err(),
        "a corrupt index must surface as an error, not a clean worktree"
    );
}

#[test]
fn changed_files_errors_on_unreadable_status() {
    let repo = repo_with_main();
    corrupt_index(repo.path());
    assert!(changed_files(repo.path()).is_err());
}

// --- commit ranges ---------------------------------------------------------

/// A fresh clone of `bare` with hermetic identity — used to push commits/branches
/// to the shared remote so a sibling clone sees them as incoming / remote-only.
fn fresh_clone(bare: &Path) -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["clone", "-q", bare.to_str().unwrap(), "."]);
    git(dir.path(), &["config", "user.email", "t@e.com"]);
    git(dir.path(), &["config", "user.name", "T"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    dir
}

fn subjects(range: &switchboard_git::GitCommitRange) -> Vec<&str> {
    range.commits.iter().map(|c| c.subject.as_str()).collect()
}

#[test]
fn commit_ranges_marks_unpushed_within_full_local_history() {
    let (_bare, clone) = cloned_repo();
    write(clone.path(), "a.txt", "a\n");
    commit_all(clone.path(), "local 1");
    write(clone.path(), "b.txt", "b\n");
    commit_all(clone.path(), "local 2");

    let ranges = commit_ranges(clone.path(), BranchKind::Local, "main").unwrap();
    // One list with the FULL local history — the already-pushed "initial" stays
    // visible alongside the unpushed work (it used to vanish until you pushed).
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].kind, CommitRangeKind::Recent);
    assert_eq!(subjects(&ranges[0]), vec!["local 2", "local 1", "initial"]);
    let unpushed: Vec<_> = ranges[0].commits.iter().map(|c| c.unpushed).collect();
    assert_eq!(unpushed, vec![true, true, false]);
    // Unpushed commits on the local default branch are default-branch work, not
    // feature-branch work — so none are marked branch work.
    assert!(ranges[0].commits.iter().all(|c| !c.branch_work));
    assert!(!ranges[0].truncated);
}

#[test]
fn commit_ranges_incoming_for_behind_branch() {
    let (bare, clone) = cloned_repo();
    let pusher = fresh_clone(bare.path());
    write(pusher.path(), "r.txt", "r\n");
    commit_all(pusher.path(), "remote 1");
    git(pusher.path(), &["push", "-q", "origin", "main"]);
    // The read layer never fetches; the test brings the remote ref local.
    git(clone.path(), &["fetch", "-q", "origin"]);

    let ranges = commit_ranges(clone.path(), BranchKind::Local, "main").unwrap();
    // Local history (nothing unpushed) plus the separate incoming set.
    let kinds: Vec<_> = ranges.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        vec![CommitRangeKind::Recent, CommitRangeKind::Incoming]
    );
    assert_eq!(subjects(&ranges[0]), vec!["initial"]);
    assert!(ranges[0].commits.iter().all(|c| !c.unpushed));
    assert_eq!(subjects(&ranges[1]), vec!["remote 1"]);
}

#[test]
fn commit_ranges_both_for_diverged_branch() {
    let (bare, clone) = cloned_repo();
    write(clone.path(), "a.txt", "a\n");
    commit_all(clone.path(), "local 1");
    let pusher = fresh_clone(bare.path());
    write(pusher.path(), "r.txt", "r\n");
    commit_all(pusher.path(), "remote 1");
    git(pusher.path(), &["push", "-q", "origin", "main"]);
    git(clone.path(), &["fetch", "-q", "origin"]);

    let ranges = commit_ranges(clone.path(), BranchKind::Local, "main").unwrap();
    let kinds: Vec<_> = ranges.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        vec![CommitRangeKind::Recent, CommitRangeKind::Incoming]
    );
    // Local history flags the unpushed commit and keeps the shared "initial".
    assert_eq!(subjects(&ranges[0]), vec!["local 1", "initial"]);
    let unpushed: Vec<_> = ranges[0].commits.iter().map(|c| c.unpushed).collect();
    assert_eq!(unpushed, vec![true, false]);
    assert_eq!(subjects(&ranges[1]), vec!["remote 1"]);
}

#[test]
fn commit_ranges_flag_merged_default_branch_commits_as_unpushed_intertwined() {
    // `feature` is pushed at "feature pushed"; the remote's main then gains an
    // OLDER-dated "main work". Merging origin/main into feature leaves the merge
    // and "main work" unpushed, with the already-pushed "feature pushed" sitting
    // between them by date — unpushed commits interleaved with pushed ones, which
    // a single contiguous "unpushed" section could not represent.
    let (bare, clone) = cloned_repo();
    git(clone.path(), &["checkout", "-q", "-b", "feature"]);
    write(clone.path(), "f.txt", "f\n");
    commit_all_at(clone.path(), "feature pushed", 5000);
    git(clone.path(), &["push", "-q", "-u", "origin", "feature"]);

    let pusher = fresh_clone(bare.path());
    write(pusher.path(), "m.txt", "m\n");
    commit_all_at(pusher.path(), "main work", 3000); // older than "feature pushed"
    git(pusher.path(), &["push", "-q", "origin", "main"]);

    git(clone.path(), &["fetch", "-q", "origin"]);
    git_dated(
        clone.path(),
        9000,
        &["merge", "--no-ff", "origin/main", "-m", "merge main"],
    );

    let ranges = commit_ranges(clone.path(), BranchKind::Local, "feature").unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].kind, CommitRangeKind::Recent);
    assert_eq!(
        subjects(&ranges[0]),
        vec!["merge main", "feature pushed", "main work", "initial"]
    );
    let unpushed: Vec<_> = ranges[0].commits.iter().map(|c| c.unpushed).collect();
    assert_eq!(unpushed, vec![true, false, true, false]);
}

#[test]
fn commit_ranges_marks_unpushed_on_feature_branch_keeping_shared_history() {
    // The everyday path: a feature branch tracking its own upstream, with a new
    // local commit on top of already-pushed history. The pushed commit and the
    // shared "initial" stay visible (full history), and `branch_work` /
    // `unpushed` are independent per commit.
    let (_bare, clone) = cloned_repo();
    git(clone.path(), &["checkout", "-q", "-b", "feature"]);
    write(clone.path(), "f1.txt", "f1\n");
    commit_all(clone.path(), "feature pushed");
    git(clone.path(), &["push", "-q", "-u", "origin", "feature"]);
    write(clone.path(), "f2.txt", "f2\n");
    commit_all(clone.path(), "feature 2");

    let ranges = commit_ranges(clone.path(), BranchKind::Local, "feature").unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].kind, CommitRangeKind::Recent);
    assert_eq!(
        subjects(&ranges[0]),
        vec!["feature 2", "feature pushed", "initial"]
    );
    let unpushed: Vec<_> = ranges[0].commits.iter().map(|c| c.unpushed).collect();
    assert_eq!(unpushed, vec![true, false, false]);
    // Orthogonal: both feature commits are branch work; "feature pushed" is
    // branch work yet already pushed, so the two flags differ on the same commit.
    let branch_work: Vec<_> = ranges[0].commits.iter().map(|c| c.branch_work).collect();
    assert_eq!(branch_work, vec![true, true, false]);
}

#[test]
fn commit_ranges_recent_for_local_only_branch() {
    let dir = repo_with_main();
    git(dir.path(), &["checkout", "-q", "-b", "feature"]);
    write(dir.path(), "f1.txt", "1\n");
    commit_all(dir.path(), "feat 1");
    write(dir.path(), "f2.txt", "2\n");
    commit_all(dir.path(), "feat 2");

    let ranges = commit_ranges(dir.path(), BranchKind::Local, "feature").unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].kind, CommitRangeKind::Recent);
    // Recent history newest-first, including the base commit.
    assert_eq!(subjects(&ranges[0]), vec!["feat 2", "feat 1", "initial"]);
    let branch_work: Vec<_> = ranges[0].commits.iter().map(|c| c.branch_work).collect();
    assert_eq!(branch_work, vec![true, true, false]);
}

#[test]
fn commit_ranges_do_not_mark_default_branch_integration_merge_as_branch_work() {
    // Explicit, strictly-increasing dates so the two sibling commits ("feature
    // work" and "main work") can't tie on a same-second timestamp and flip the
    // walk's sibling order. "feature work" is dated newer than "main work" so
    // the merge's first parent (feature) precedes its second (main), matching
    // the asserted order below.
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    write(dir.path(), "README.md", "hello\n");
    commit_all_at(dir.path(), "initial", 1000);

    git(dir.path(), &["checkout", "-q", "-b", "feature"]);
    write(dir.path(), "feature.txt", "feature\n");
    commit_all_at(dir.path(), "feature work", 3000);

    git(dir.path(), &["checkout", "-q", "main"]);
    write(dir.path(), "main.txt", "main\n");
    commit_all_at(dir.path(), "main work", 2000);

    git(dir.path(), &["checkout", "-q", "feature"]);
    git_dated(
        dir.path(),
        4000,
        &["merge", "--no-ff", "main", "-m", "merge main into feature"],
    );
    write(dir.path(), "feature-2.txt", "feature 2\n");
    commit_all_at(dir.path(), "more feature work", 5000);

    let ranges = commit_ranges(dir.path(), BranchKind::Local, "feature").unwrap();
    assert_eq!(
        subjects(&ranges[0]),
        vec![
            "more feature work",
            "merge main into feature",
            "feature work",
            "main work",
            "initial",
        ]
    );
    let branch_work: Vec<_> = ranges[0].commits.iter().map(|c| c.branch_work).collect();
    assert_eq!(
        branch_work,
        vec![true, false, true, false, false],
        "the merge commit imports default-branch history, but is not itself feature work"
    );
}

#[test]
fn commit_ranges_mark_octopus_merge_with_non_default_parent_as_branch_work() {
    let dir = repo_with_main();
    git(dir.path(), &["checkout", "-q", "-b", "feature"]);
    write(dir.path(), "feature.txt", "feature\n");
    commit_all(dir.path(), "feature work");

    git(dir.path(), &["checkout", "-q", "main"]);
    write(dir.path(), "main.txt", "main\n");
    commit_all(dir.path(), "main work");
    git(
        dir.path(),
        &["checkout", "-q", "-b", "other-topic", "HEAD~1"],
    );
    write(dir.path(), "other.txt", "other\n");
    commit_all(dir.path(), "other topic work");

    git(dir.path(), &["checkout", "-q", "feature"]);
    git(
        dir.path(),
        &[
            "merge",
            "--no-ff",
            "main",
            "other-topic",
            "-m",
            "octopus integration",
        ],
    );

    let ranges = commit_ranges(dir.path(), BranchKind::Local, "feature").unwrap();
    let octopus = ranges[0]
        .commits
        .iter()
        .find(|commit| commit.subject == "octopus integration")
        .expect("octopus merge is included in the feature history");
    assert!(
        octopus.branch_work,
        "an octopus merge that imports non-default work is branch work"
    );
}

#[test]
fn commit_ranges_recent_for_remote_only_branch() {
    let (bare, clone) = cloned_repo();
    let pusher = fresh_clone(bare.path());
    git(pusher.path(), &["checkout", "-q", "-b", "feature"]);
    write(pusher.path(), "f.txt", "f\n");
    commit_all(pusher.path(), "feature work");
    git(pusher.path(), &["push", "-q", "origin", "feature"]);
    git(clone.path(), &["fetch", "-q", "origin"]);

    // No local `feature`; only the remote-tracking ref exists.
    let ranges = commit_ranges(clone.path(), BranchKind::Remote, "origin/feature").unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].kind, CommitRangeKind::Recent);
    assert_eq!(ranges[0].commits[0].subject, "feature work");
}

#[test]
fn commit_ranges_caps_and_reports_truncated() {
    let dir = repo_with_main(); // 1 commit ("initial")
    for i in 0..55 {
        write(dir.path(), "f.txt", &format!("{i}\n"));
        commit_all(dir.path(), &format!("c{i}"));
    }
    // `main` has no upstream → a single recent range, capped at 50.
    let ranges = commit_ranges(dir.path(), BranchKind::Local, "main").unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].kind, CommitRangeKind::Recent);
    assert_eq!(ranges[0].commits.len(), 50);
    assert!(ranges[0].truncated);
}

#[test]
fn commit_ranges_missing_branch_is_empty() {
    let dir = repo_with_main();
    // A stale reference (branch deleted since the tree was listed) is not an
    // error — it degrades to no commits.
    let ranges = commit_ranges(dir.path(), BranchKind::Local, "nonexistent").unwrap();
    assert!(ranges.is_empty());
}

#[test]
fn commit_summary_carries_identity_and_authorship() {
    let dir = repo_with_main();
    write(dir.path(), "details.txt", "details\n");
    git(dir.path(), &["add", "-A"]);
    git(
        dir.path(),
        &[
            "commit",
            "-q",
            "-m",
            "subject line",
            "-m",
            "Body line one.\n\nBody line two.",
        ],
    );

    let ranges = commit_ranges(dir.path(), BranchKind::Local, "main").unwrap();
    let commit = &ranges[0].commits[0];
    assert_eq!(commit.subject, "subject line");
    assert_eq!(commit.short_oid.len(), 7);
    assert!(commit.oid.starts_with(&commit.short_oid));
    assert_eq!(commit.author_name.as_deref(), Some("Test"));
    assert_eq!(commit.author_email.as_deref(), Some("test@example.com"));
    assert!(commit.authored_at.is_some());
    assert!(!commit.branch_work);
}

// --- commit changed-files & diff -------------------------------------------

#[test]
fn commit_changed_files_lists_files_against_first_parent() {
    let dir = repo_with_main(); // "initial" adds README.md
    write(dir.path(), "added.txt", "new\n");
    write(dir.path(), "README.md", "hello\nmore\n");
    git(dir.path(), &["add", "-A"]);
    git(
        dir.path(),
        &[
            "commit",
            "-q",
            "-m",
            "second",
            "-m",
            "Body line one.\n\nBody line two.",
        ],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);

    let changes = commit_changed_files(dir.path(), &head).unwrap();
    assert!(changes.found);
    assert_eq!(
        changes.body.as_deref(),
        Some("Body line one.\n\nBody line two.")
    );
    let files = &changes.files;
    let kind = |name: &str| files.iter().find(|f| f.path == name).map(|f| f.change);
    assert_eq!(kind("added.txt"), Some(ChangeKind::Added));
    assert_eq!(kind("README.md"), Some(ChangeKind::Modified));
    // Unchanged tree entries are not listed.
    assert_eq!(files.len(), 2);
    // Committed-history counts come from the same tree diff as the listing.
    let counts = |name: &str| {
        files
            .iter()
            .find(|f| f.path == name)
            .map(|f| (f.additions, f.deletions))
    };
    assert_eq!(counts("added.txt"), Some((Some(1), Some(0))));
    // README.md went from "hello\n" (repo_with_main) to "hello\nmore\n".
    assert_eq!(counts("README.md"), Some((Some(1), Some(0))));
}

#[test]
fn commit_changed_files_for_root_commit_lists_all_as_added() {
    let dir = repo_with_main();
    let root = git(dir.path(), &["rev-parse", "HEAD"]); // the only (root) commit

    let changes = commit_changed_files(dir.path(), &root).unwrap();
    assert!(changes.found);
    assert_eq!(changes.files.len(), 1);
    assert_eq!(changes.files[0].path, "README.md");
    assert_eq!(changes.files[0].change, ChangeKind::Added);
}

#[test]
fn commit_file_diff_returns_hunks_for_a_committed_change() {
    let dir = repo_with_main();
    write(dir.path(), "code.txt", "one\ntwo\n");
    commit_all(dir.path(), "add code");
    write(dir.path(), "code.txt", "one\nTWO\n");
    commit_all(dir.path(), "change code");
    let head = git(dir.path(), &["rev-parse", "HEAD"]);

    let diff = commit_file_diff(dir.path(), &head, "code.txt").unwrap();
    assert_eq!(diff.path, "code.txt");
    assert!(!diff.binary);
    let kinds: Vec<_> = diff
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter().map(|l| l.origin))
        .collect();
    assert!(kinds.contains(&DiffLineKind::Added));
    assert!(kinds.contains(&DiffLineKind::Removed));
    let added: Vec<_> = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.origin == DiffLineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    assert!(added.contains(&"TWO"));
}

#[test]
fn commit_file_diff_marks_a_committed_file_over_the_limit_too_large() {
    // The commit path reaches the too-large gate through the diff's delta metadata —
    // the blob size comes from an object-DB header read, not a content load.
    let dir = repo_with_main();
    let size = TOO_LARGE_TO_DIFF_BYTES + 1024 * 1024; // ~11 MB
    write(dir.path(), "big.bin", &"a".repeat(size));
    commit_all(dir.path(), "add big file");
    let head = git(dir.path(), &["rev-parse", "HEAD"]);

    let diff = commit_file_diff(dir.path(), &head, "big.bin").unwrap();
    assert!(
        diff.too_large,
        "a committed file past the limit must be too_large"
    );
    assert!(diff.hunks.is_empty());
    assert_eq!(diff.too_large_bytes, Some(size as u64));
}

#[test]
fn commit_file_diff_pure_rename_of_large_file_is_not_too_large() {
    // On the commit path find_similar pairs the rename into one delta whose old/new
    // blob ids are identical — no content to render, so "no changes", not "too large".
    let dir = repo_with_main();
    let size = TOO_LARGE_TO_DIFF_BYTES + 1024 * 1024; // ~11 MB
    write(dir.path(), "old.bin", &"a".repeat(size));
    commit_all(dir.path(), "add big file");
    git(dir.path(), &["mv", "old.bin", "new.bin"]);
    commit_all(dir.path(), "rename big file");
    let head = git(dir.path(), &["rev-parse", "HEAD"]);

    let diff = commit_file_diff(dir.path(), &head, "new.bin").unwrap();
    assert!(
        !diff.too_large,
        "a pure rename has no content to be too large"
    );
    assert!(
        diff.hunks.is_empty(),
        "a pure rename shows no content hunks"
    );
}

#[test]
fn file_diff_marks_a_worktree_deletion_of_a_huge_file_too_large() {
    // A huge tracked file deleted in the worktree: the file is gone so the
    // filesystem fast path can't see it, and the diff content is the *old* blob.
    // The both-sides delta preflight must catch it via `old_file().size()`.
    let dir = repo_with_main();
    let size = TOO_LARGE_TO_DIFF_BYTES + 1024 * 1024; // ~11 MB
    write(dir.path(), "big.bin", &"a".repeat(size));
    commit_all(dir.path(), "add big file");
    std::fs::remove_file(dir.path().join("big.bin")).unwrap();

    let diff = file_diff(dir.path(), "big.bin").unwrap();
    assert!(
        diff.too_large,
        "a worktree deletion of a huge file must be caught via the old-blob size"
    );
    assert!(diff.hunks.is_empty());
}

#[test]
fn commit_diffs_for_invalid_or_unknown_oid_report_not_found() {
    let dir = repo_with_main();
    // Garbage oid (unparseable) and a well-formed but absent oid both report
    // `found: false` — distinct from a real commit that changed no files.
    let absent = "0".repeat(40);
    for oid in ["not-a-hash", absent.as_str()] {
        let changes = commit_changed_files(dir.path(), oid).unwrap();
        assert!(!changes.found, "absent oid {oid:?} must report not-found");
        assert!(changes.body.is_none());
        assert!(changes.files.is_empty());
        let diff = commit_file_diff(dir.path(), oid, "README.md").unwrap();
        assert!(diff.hunks.is_empty());
    }
}

#[test]
fn commit_changed_files_reports_found_for_a_real_empty_commit() {
    // A genuine empty commit (changed no files) is `found: true` with no files —
    // distinct from a vanished commit.
    let dir = repo_with_main();
    git(
        dir.path(),
        &["commit", "-q", "--allow-empty", "-m", "empty"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);

    let changes = commit_changed_files(dir.path(), &head).unwrap();
    assert!(changes.found);
    assert!(changes.files.is_empty());
}

#[test]
fn commit_file_diff_renders_a_rename_with_edit_as_its_real_edit() {
    // A file renamed *and* edited in one commit must show the actual edit, not the
    // whole file as added (the find_similar-aware path), and the list agrees it's
    // a rename.
    let dir = repo_with_main();
    write(dir.path(), "old.txt", "one\ntwo\nthree\n");
    commit_all(dir.path(), "add old");
    std::fs::rename(dir.path().join("old.txt"), dir.path().join("new.txt")).unwrap();
    write(dir.path(), "new.txt", "one\nTWO\nthree\n");
    commit_all(dir.path(), "rename and edit");
    let head = git(dir.path(), &["rev-parse", "HEAD"]);

    let changes = commit_changed_files(dir.path(), &head).unwrap();
    let renamed = changes.files.iter().find(|f| f.path == "new.txt").unwrap();
    assert_eq!(renamed.change, ChangeKind::Renamed);

    let diff = commit_file_diff(dir.path(), &head, "new.txt").unwrap();
    let added: Vec<_> = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.origin == DiffLineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    // Only the edited line is added — not the whole file (which would include the
    // unchanged "one"/"three" lines as additions).
    assert!(added.contains(&"TWO"), "the real edit shows: {added:?}");
    assert!(
        !added.contains(&"one"),
        "unchanged lines are not re-added: {added:?}"
    );
    assert!(
        !added.contains(&"three"),
        "unchanged lines are not re-added: {added:?}"
    );
}

#[test]
fn commit_file_diff_for_a_pure_rename_has_no_content_hunks() {
    // A pure rename (no edit) shows no content changes — honest, and pairs with
    // the "R" label, rather than rendering the whole file as added.
    let dir = repo_with_main();
    write(dir.path(), "old.txt", "stable\ncontent\n");
    commit_all(dir.path(), "add old");
    std::fs::rename(dir.path().join("old.txt"), dir.path().join("new.txt")).unwrap();
    commit_all(dir.path(), "pure rename");
    let head = git(dir.path(), &["rev-parse", "HEAD"]);

    let diff = commit_file_diff(dir.path(), &head, "new.txt").unwrap();
    let content_lines: usize = diff.hunks.iter().map(|h| h.lines.len()).sum();
    assert_eq!(content_lines, 0, "a pure rename has no content lines");
}
