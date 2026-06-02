//! Integration tests for the git read layer, built against fixture repos
//! constructed by shelling the real `git` CLI into a `tempfile` tempdir. Shelling
//! `git` (rather than building with `git2`) keeps the fixtures faithful to the
//! on-disk shapes real repos have — including the `origin/HEAD` symbolic ref,
//! worktree records, and upstream config that the read layer keys on.

use std::path::Path;
use std::process::Command;

use switchboard_git::{
    ChangeKind, SyncState, WorktreeWarning, changed_files, diff_text, read_repo, resolve_repo_root,
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
    // non-bare), so the root must be derived authoritatively — both the M2 dedup
    // key (`resolve_repo_root`) and the M1 rendered model (`read_repo`) must
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

// --- changed files + diff text (consumed by M5) ---------------------------

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
fn diff_text_renders_unified_diff_for_a_file() {
    let repo = repo_with_main();
    write(repo.path(), "code.txt", "line one\nline two\n");
    commit_all(repo.path(), "add code");
    write(repo.path(), "code.txt", "line one\nline CHANGED\n");

    let diff = diff_text(repo.path(), "code.txt").unwrap();
    assert!(
        diff.contains("-line two"),
        "diff should show the removed line: {diff}"
    );
    assert!(
        diff.contains("+line CHANGED"),
        "diff should show the added line: {diff}"
    );
}

#[test]
fn diff_text_includes_untracked_file_content() {
    let repo = repo_with_main();
    write(repo.path(), "brand-new.txt", "fresh content\n");
    let diff = diff_text(repo.path(), "brand-new.txt").unwrap();
    assert!(
        diff.contains("+fresh content"),
        "untracked file content should appear as additions: {diff}"
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
