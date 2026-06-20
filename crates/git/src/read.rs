//! The read functions: path → [`RepoView`], plus per-worktree changed-files and
//! diff text. All local, all `git2`, all synchronous.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use git2::{
    Branch, BranchType, Diff, DiffFormat, DiffOptions, ErrorCode, Oid, Repository, Status,
    StatusOptions,
};

use chrono::{FixedOffset, LocalResult, TimeZone};

use crate::error::{GitError, Result};
use crate::model::{
    BranchView, ChangeKind, ChangedFile, CommitChanges, CommitRangeKind, DiffHunk, DiffLine,
    DiffLineKind, FileDiff, GitCommitRange, GitCommitSummary, RemoteBranchView, RepoView,
    SyncState, WorktreeView, WorktreeWarning,
};

/// Resolve any path inside (or at) a git repo to the canonical **main-worktree /
/// common-dir root**. Returns `None` if the path is not inside a git repo. This
/// is the identity used for tracking and dedup (the M2 registry stores this).
///
/// Linked worktrees share one git database, so discovery from any of them — or
/// from a subdirectory — resolves to the same root.
pub fn resolve_repo_root(path: &Path) -> Option<PathBuf> {
    let repo = Repository::discover(path).ok()?;
    Some(repo_root(&repo))
}

/// Read the full view for one repo at `path`.
///
/// A path that isn't a readable git repo yields an `available: false` view (not
/// an error) — the expected non-error UI state. A genuine failure partway
/// through reading an opened repo yields [`GitError`].
pub fn read_repo(path: &Path) -> Result<RepoView> {
    let name = display_name(path);
    let Ok(discovered) = Repository::discover(path) else {
        return Ok(RepoView::unavailable(path.to_path_buf(), name));
    };
    let root = repo_root(&discovered);
    // Re-open at the canonical root so `workdir()`/`head()` always reflect the
    // *main* worktree, even if `path` pointed into a linked worktree or a
    // subdirectory (`discover` binds to whichever worktree we entered from). We
    // discovered a real repo, so a failure to open its own root is a genuine
    // mid-read failure — surface it rather than silently reading the wrong
    // worktree's view.
    let repo = Repository::open(&root).map_err(|e| GitError::read(root.clone(), e))?;
    let view_name = display_name(&root);
    build_repo_view(&repo, root, view_name)
}

/// The changed files in the worktree at `path` (working-tree changes vs. HEAD —
/// staged, unstaged, and untracked). The worktree path must be a checked-out
/// working directory.
pub fn changed_files(path: &Path) -> Result<Vec<ChangedFile>> {
    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(e) if is_not_found(&e) => return Ok(Vec::new()),
        Err(e) => return Err(GitError::read(path, e)),
    };
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .exclude_submodules(true);
    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(|e| GitError::read(path, e))?;

    let mut files = Vec::new();
    for entry in statuses.iter() {
        let status = entry.status();
        if status.is_ignored() {
            continue;
        }
        let Some(change) = classify_change(status) else {
            continue;
        };
        // For a rename, prefer the new path from the relevant diff delta.
        let file_path = renamed_target(&entry).or_else(|| entry.path().ok().map(str::to_owned));
        if let Some(file_path) = file_path {
            files.push(ChangedFile {
                path: file_path,
                change,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// The structured working-tree diff for a single `file` (repo-relative) in the
/// worktree at `path` — changes vs. HEAD, including untracked content. Returns an
/// empty [`FileDiff`] when the file has no diff (clean) or the path isn't a
/// readable worktree; `binary: true` (no hunks) for binary content libgit2
/// declines to render inline.
pub fn file_diff(path: &Path, file: &str) -> Result<FileDiff> {
    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(e) if is_not_found(&e) => return Ok(FileDiff::empty(file)),
        Err(e) => return Err(GitError::read(path, e)),
    };
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        // Match `file` as a literal path, not a glob. Without this, a real
        // filename containing pathspec metacharacters (`*`, `[`, `?`) would be
        // treated as a pattern and could pull in other files' deltas, which
        // `collect_file_diff` would then merge under the one requested name.
        .disable_pathspec_match(true)
        .pathspec(file);

    let diff = match head_tree(&repo) {
        Some(tree) => repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut opts)),
        // Unborn HEAD (no commits yet): everything is "new" against an empty tree.
        None => repo.diff_tree_to_workdir_with_index(None, Some(&mut opts)),
    }
    .map_err(|e| GitError::read(path, e))?;

    collect_file_diff(&diff, file, false).map_err(|e| GitError::read(path, e))
}

/// Whether a [`commit_ranges`] target names a local branch (`refs/heads/*`) or a
/// remote-tracking branch (`refs/remotes/*`). The caller knows which from the row
/// the user clicked; we resolve the ref accordingly. `Deserialize` so it crosses
/// the IPC boundary directly as `"local"` / `"remote"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchKind {
    Local,
    Remote,
}

/// Capped commit-summary ranges for one branch, read on demand (a revwalk, never
/// a fetch — like every read here, it sees only what's already local).
///
/// The local branch's history is one `recent` range (walked back from the tip),
/// with each commit carrying a per-commit `unpushed` flag for commits the
/// upstream doesn't have yet — unpushed commits can be interleaved with pushed
/// ones (e.g. after merging the default branch), so they're marked in place
/// rather than split into a contiguous section. When the upstream additionally
/// has commits the local branch lacks, an `incoming` range is appended (those
/// aren't in the local history, so they stay a separate set). A branch with no
/// upstream, and any remote branch, yields just `recent` with no unpushed flags.
///
/// A ref that no longer resolves (deleted between listing and click) or an empty
/// branch yields an empty `Vec` rather than an error — the same "absent is not a
/// failure" stance as [`changed_files`]/[`file_diff`]. Newest commit first.
pub fn commit_ranges(path: &Path, kind: BranchKind, name: &str) -> Result<Vec<GitCommitRange>> {
    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(e) if is_not_found(&e) => return Ok(Vec::new()),
        Err(e) => return Err(GitError::read(path, e)),
    };
    let branch_type = match kind {
        BranchKind::Local => BranchType::Local,
        BranchKind::Remote => BranchType::Remote,
    };
    let branch = match repo.find_branch(name, branch_type) {
        Ok(branch) => branch,
        // The branch was deleted between the tree listing and this click — a
        // stale reference, not a read failure. Degrade to "no commits".
        Err(e) if is_not_found(&e) => return Ok(Vec::new()),
        Err(e) => return Err(GitError::read(path, e)),
    };
    let Some(tip) = branch.get().target() else {
        return Ok(Vec::new()); // unborn / symbolic ref with no commit
    };
    let default_branch = resolve_default_branch(&repo);
    let selected_default_branch =
        kind == BranchKind::Local && default_branch.as_deref() == Some(name);
    let default_tip = default_branch
        .as_deref()
        .and_then(|b| default_branch_tip(&repo, b));

    // A remote-tracking ref has no own upstream — just show its recent history.
    let upstream = match kind {
        BranchKind::Local => branch.upstream().ok().and_then(|u| u.get().target()),
        BranchKind::Remote => None,
    };

    let ranges = build_commit_ranges(&repo, tip, upstream, default_tip, selected_default_branch)
        .map_err(|e| GitError::read(path, e))?;
    Ok(ranges)
}

/// The files a single commit changed, relative to its first parent (or the empty
/// tree for a root commit). This is the committed-history analogue of
/// [`changed_files`]: it needs no worktree, so it works for a branch with no
/// local folder or a remote-only branch.
///
/// `oid` is the full hex id from a [`GitCommitSummary`]. An unparseable or
/// no-longer-present id (a stale reference) yields [`CommitChanges`] with
/// `found: false` — calm, not an error, but distinguishable from a real commit
/// that changed nothing (`found: true`, empty `files`).
pub fn commit_changed_files(path: &Path, oid: &str) -> Result<CommitChanges> {
    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(e) if is_not_found(&e) => return Ok(CommitChanges::missing()),
        Err(e) => return Err(GitError::read(path, e)),
    };
    let mut opts = DiffOptions::new();
    let Some(mut diff) =
        commit_tree_diff(&repo, oid, &mut opts).map_err(|e| GitError::read(path, e))?
    else {
        return Ok(CommitChanges::missing());
    };
    // Coalesce add+delete pairs into renames for a cleaner list (matches the
    // worktree read's rename handling).
    diff.find_similar(None)
        .map_err(|e| GitError::read(path, e))?;
    Ok(CommitChanges {
        found: true,
        files: changed_files_from_diff(&diff),
    })
}

/// The structured diff of a single `file` within one commit, relative to its
/// first parent (empty tree for a root commit). The committed-history analogue of
/// [`file_diff`]; same [`FileDiff`] shape and same truncation cap. An unparseable
/// or unknown `oid` yields an empty [`FileDiff`].
///
/// Unlike [`file_diff`], this does **not** pathspec the read: rename detection
/// needs both the old and new path present in the diff to pair them, so we diff
/// the whole commit, run `find_similar`, then collect only this file's delta. A
/// renamed-and-edited file then shows its real edit (not the whole file as added,
/// which a pathspec'd add/delete view would show); a pure rename shows no content
/// hunks, matching the `R` label in the file list. The extra cost is bounded by
/// the commit's own change set and only paid on a click.
pub fn commit_file_diff(path: &Path, oid: &str, file: &str) -> Result<FileDiff> {
    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(e) if is_not_found(&e) => return Ok(FileDiff::empty(file)),
        Err(e) => return Err(GitError::read(path, e)),
    };
    let mut opts = DiffOptions::new();
    let Some(mut diff) =
        commit_tree_diff(&repo, oid, &mut opts).map_err(|e| GitError::read(path, e))?
    else {
        return Ok(FileDiff::empty(file));
    };
    diff.find_similar(None)
        .map_err(|e| GitError::read(path, e))?;
    collect_file_diff(&diff, file, true).map_err(|e| GitError::read(path, e))
}

// --- internals -------------------------------------------------------------

/// The canonical root we key a repo on: the **main** worktree, regardless of
/// which worktree (or subdirectory) we discovered from. `commondir()` always
/// resolves to the main repo's git dir even when opened via a linked worktree.
///
/// Bareness can't be read from `repo`: `is_bare()` reflects *how the handle was
/// opened*, and a linked worktree of a bare repo opens as non-bare. So we re-open
/// at the common dir to get the authoritative answer — a bare repo's root is the
/// common dir itself (no working tree); a normal repo's root is the working dir
/// (parent of `.git`).
fn repo_root(repo: &Repository) -> PathBuf {
    let common = repo.commondir().to_path_buf();
    if let Ok(canonical) = Repository::open(&common) {
        if canonical.is_bare() {
            return common;
        }
        if let Some(workdir) = canonical.workdir() {
            return workdir.to_path_buf();
        }
    }
    // Fallback if the common dir can't be re-opened: `common` is `<main>/.git`,
    // so the main worktree is its parent.
    common.parent().map_or(common.clone(), Path::to_path_buf)
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn build_repo_view(repo: &Repository, root: PathBuf, name: String) -> Result<RepoView> {
    let err = |e: git2::Error| GitError::read(root.clone(), e);

    let default_branch = resolve_default_branch(repo);
    let default_tip = default_branch
        .as_deref()
        .and_then(|b| default_branch_tip(repo, b));

    // Map each branch name to the worktree it's checked out in (if any), and
    // collect the prunable/orphaned warnings. Done once up front so branch
    // enumeration can attach worktrees without rescanning.
    let worktrees = collect_worktrees(repo).map_err(err)?;

    let local_branches =
        read_local_branches(repo, default_tip.as_ref(), &worktrees).map_err(err)?;
    let remote_branches = read_remote_branches(repo, default_tip.as_ref()).map_err(err)?;
    let detached_worktrees = worktrees.into_detached();

    Ok(RepoView {
        root,
        name,
        default_branch,
        available: true,
        is_bare: repo.is_bare(),
        local_branches,
        remote_branches,
        detached_worktrees,
    })
}

/// Default-branch detection, in order: `origin/HEAD`'s symbolic target, then a
/// local `main`, then a local `master`. `None` if none resolves — which makes
/// `merged`/`behind_base` `None` throughout.
///
/// The `origin/HEAD` ref is only populated on clone, so for `git init` repos the
/// local-`main` fallback is the common path in practice, not the exception.
fn resolve_default_branch(repo: &Repository) -> Option<String> {
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Ok(Some(target)) = reference.symbolic_target()
        && let Some(name) = target.rsplit('/').next()
    {
        // e.g. "refs/remotes/origin/main" → "main"
        return Some(name.to_owned());
    }
    for candidate in ["main", "master"] {
        if repo.find_branch(candidate, BranchType::Local).is_ok() {
            return Some(candidate.to_owned());
        }
    }
    None
}

/// The commit the default branch points at, preferring the remote-tracking ref
/// (`origin/<default>`) when present — that's the comparison that's meaningful
/// after a fetch — and falling back to the local branch.
fn default_branch_tip(repo: &Repository, default: &str) -> Option<Oid> {
    let remote = format!("refs/remotes/origin/{default}");
    if let Ok(reference) = repo.find_reference(&remote)
        && let Some(oid) = reference.target()
    {
        return Some(oid);
    }
    repo.find_branch(default, BranchType::Local)
        .ok()
        .and_then(|b| b.get().target())
}

fn read_local_branches(
    repo: &Repository,
    default_tip: Option<&Oid>,
    worktrees: &Worktrees,
) -> std::result::Result<Vec<BranchView>, git2::Error> {
    let mut views = Vec::new();
    for branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let Some(name) = branch.name()?.map(str::to_owned) else {
            continue; // non-UTF-8 branch name — skip rather than fail the whole read
        };
        let tip = branch.get().target();
        let (upstream, sync, dangling) = upstream_status(repo, &branch, tip);
        let (merged, behind_base) = ancestry_signals(repo, tip, default_tip);
        let worktree = worktrees.for_branch(&name);
        views.push(BranchView {
            name,
            upstream,
            sync,
            behind_base,
            merged,
            dangling,
            worktree,
        });
    }
    views.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(views)
}

fn read_remote_branches(
    repo: &Repository,
    default_tip: Option<&Oid>,
) -> std::result::Result<Vec<RemoteBranchView>, git2::Error> {
    let mut views = Vec::new();
    for branch in repo.branches(Some(BranchType::Remote))? {
        let (branch, _) = branch?;
        let Some(name) = branch.name()?.map(str::to_owned) else {
            continue;
        };
        // Skip the symbolic `origin/HEAD` pointer — it's not a real branch.
        if name.ends_with("/HEAD") {
            continue;
        }
        let (merged, behind_base) = ancestry_signals(repo, branch.get().target(), default_tip);
        views.push(RemoteBranchView {
            name,
            merged,
            behind_base,
        });
    }
    views.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(views)
}

/// A branch's position vs. its own upstream, plus whether that upstream is
/// "gone" (configured but the remote-tracking ref no longer exists → dangling).
///
/// Returns `(upstream_name, sync_state, dangling)`. No configured upstream is
/// `LocalOnly` (a fine state), distinct from a configured-but-missing upstream
/// (`dangling = true`).
fn upstream_status(
    repo: &Repository,
    branch: &Branch<'_>,
    tip: Option<Oid>,
) -> (Option<String>, SyncState, bool) {
    let Ok(refname) = branch.get().name() else {
        return (None, SyncState::Unknown, false);
    };
    // `branch_upstream_name` reads config — it returns a name even when the
    // remote-tracking ref itself has been deleted, which is exactly how we tell
    // "dangling" (config says it tracks X, but X is gone) from "local-only"
    // (no tracking config at all).
    let Some(configured) = repo
        .branch_upstream_name(refname)
        .ok()
        .and_then(|buf| buf.as_str().ok().map(str::to_owned))
    else {
        return (None, SyncState::LocalOnly, false);
    };

    match branch.upstream() {
        Ok(upstream) => {
            let name = upstream
                .name()
                .ok()
                .flatten()
                .map_or(configured, str::to_owned);
            let sync = match (tip, upstream.get().target()) {
                (Some(local), Some(up)) => sync_from_counts(repo, local, up),
                _ => SyncState::Unknown,
            };
            (Some(name), sync, false)
        }
        // Configured upstream that no longer resolves → the remote branch was
        // deleted. Dangling; sync vs. a missing upstream is meaningless.
        Err(e) if is_not_found(&e) => (Some(configured), SyncState::Unknown, true),
        Err(_) => (Some(configured), SyncState::Unknown, false),
    }
}

fn sync_from_counts(repo: &Repository, local: Oid, upstream: Oid) -> SyncState {
    match repo.graph_ahead_behind(local, upstream) {
        Ok((0, 0)) => SyncState::InSync,
        Ok((ahead, 0)) => SyncState::Ahead {
            commits: clamp_u32(ahead),
        },
        Ok((0, behind)) => SyncState::Behind {
            commits: clamp_u32(behind),
        },
        Ok((ahead, behind)) => SyncState::Diverged {
            ahead: clamp_u32(ahead),
            behind: clamp_u32(behind),
        },
        Err(_) => SyncState::Unknown,
    }
}

/// `merged` and `behind_base` against the default-branch tip. Both are ancestry
/// walks that work on any commit (local branch or remote ref). `None` when the
/// default branch (or this tip) can't be resolved.
fn ancestry_signals(
    repo: &Repository,
    tip: Option<Oid>,
    default_tip: Option<&Oid>,
) -> (Option<bool>, Option<u32>) {
    let (Some(tip), Some(&base)) = (tip, default_tip) else {
        return (None, None);
    };
    // merged = this branch's tip is an ancestor of (or equal to) the default tip.
    let merged = if tip == base {
        Some(true)
    } else {
        repo.graph_descendant_of(base, tip).ok()
    };
    // behind_base = commits the default has that this branch lacks. The "behind"
    // half of ahead/behind(this, default).
    let behind_base = repo
        .graph_ahead_behind(tip, base)
        .ok()
        .map(|(_, behind)| clamp_u32(behind));
    (merged, behind_base)
}

/// Worktree records for a repo, indexed for branch attachment and warning
/// detection. Built once per read.
struct Worktrees {
    /// branch shorthand (e.g. "feature-x") → its checked-out worktree view.
    by_branch: HashMap<String, WorktreeView>,
    /// Detached-HEAD worktrees (no branch), labelled by short hash.
    detached: Vec<WorktreeView>,
}

impl Worktrees {
    fn for_branch(&self, name: &str) -> Option<WorktreeView> {
        self.by_branch.get(name).cloned()
    }

    fn into_detached(self) -> Vec<WorktreeView> {
        let mut detached = self.detached;
        detached.sort_by(|a, b| a.path.cmp(&b.path));
        detached
    }
}

/// Enumerate the main worktree plus all linked worktrees, classifying each as a
/// branch worktree (attached by branch name) or detached, and flagging the
/// prunable/orphaned warnings.
fn collect_worktrees(repo: &Repository) -> std::result::Result<Worktrees, git2::Error> {
    let mut by_branch = HashMap::new();
    let mut detached = Vec::new();

    // The main worktree isn't in `repo.worktrees()` (that lists only *linked*
    // worktrees), so handle it directly from HEAD when there's a workdir.
    if let Some(workdir) = repo.workdir()
        && let Some(view) = main_worktree_view(repo, workdir)?
    {
        match view {
            MainWorktree::Branch(name, wt) => {
                by_branch.insert(name, wt);
            }
            MainWorktree::Detached(wt) => detached.push(wt),
        }
    }

    // `iter()` yields `Result<Option<&str>>`; keep only the valid UTF-8 names.
    let worktree_names = repo.worktrees()?;
    for wt_name in worktree_names
        .iter()
        .filter_map(std::result::Result::ok)
        .flatten()
    {
        let Ok(worktree) = repo.find_worktree(wt_name) else {
            continue;
        };
        let wt_path = worktree.path().to_path_buf();

        // Prunable: git holds the record but its directory is gone. `validate`
        // fails when the worktree dir/gitdir is missing. Git still considers the
        // recorded branch checked out here until the user prunes, so attach the
        // warning to *that branch* — otherwise M3 shows the branch as free and
        // M4 offers "create worktree" for it, which `git worktree add` refuses.
        if worktree.validate().is_err() {
            let prunable = WorktreeView {
                path: wt_path,
                dirty: false,
                untracked: false,
                detached_hash: None,
                warning: Some(WorktreeWarning::Prunable),
            };
            match prunable_record_branch(repo, wt_name) {
                Some(branch) => {
                    by_branch.insert(branch, prunable);
                }
                // Detached or unreadable record HEAD: a degenerate-but-recoverable
                // state — fall back to a branch-less row, not a hard error.
                None => detached.push(prunable),
            }
            continue;
        }

        // Open the linked worktree to read its HEAD + status.
        let Ok(wt_repo) = Repository::open(&wt_path) else {
            continue;
        };
        let (dirty, untracked) = worktree_changes(&wt_repo)?;
        match worktree_head(&wt_repo) {
            HeadKind::Branch(branch_name) => {
                by_branch.insert(
                    branch_name,
                    WorktreeView {
                        path: wt_path,
                        dirty,
                        untracked,
                        detached_hash: None,
                        warning: None,
                    },
                );
            }
            HeadKind::Detached(hash) => detached.push(WorktreeView {
                path: wt_path,
                dirty,
                untracked,
                detached_hash: Some(hash),
                warning: None,
            }),
            // Orphaned: the directory is on disk but its branch was deleted, so
            // HEAD points at a branch ref that no longer resolves. For a *linked*
            // worktree an unborn HEAD means exactly this (the branch was deleted
            // out from under it) — there's no "fresh repo" interpretation here.
            HeadKind::Orphaned | HeadKind::Unborn(_) => detached.push(WorktreeView {
                path: wt_path,
                dirty,
                untracked,
                detached_hash: None,
                warning: Some(WorktreeWarning::Orphaned),
            }),
        }
    }

    Ok(Worktrees {
        by_branch,
        detached,
    })
}

enum MainWorktree {
    Branch(String, WorktreeView),
    Detached(WorktreeView),
}

fn main_worktree_view(
    repo: &Repository,
    workdir: &Path,
) -> std::result::Result<Option<MainWorktree>, git2::Error> {
    let (dirty, untracked) = worktree_changes(repo)?;
    // An unborn HEAD on the *main* worktree is a fresh `git init` (no commits
    // yet) — list it as its configured branch so the repo isn't blank, the same
    // as a normal branch HEAD. (A main worktree can't be "orphaned": you can't
    // delete the branch you're on there.)
    let view = match worktree_head(repo) {
        HeadKind::Branch(name) | HeadKind::Unborn(Some(name)) => Some(MainWorktree::Branch(
            name,
            WorktreeView {
                path: workdir.to_path_buf(),
                dirty,
                untracked,
                detached_hash: None,
                warning: None,
            },
        )),
        HeadKind::Detached(hash) => Some(MainWorktree::Detached(WorktreeView {
            path: workdir.to_path_buf(),
            dirty,
            untracked,
            detached_hash: Some(hash),
            warning: None,
        })),
        HeadKind::Unborn(None) | HeadKind::Orphaned => None,
    };
    Ok(view)
}

enum HeadKind {
    Branch(String),
    Detached(String),
    /// HEAD is a symbolic ref to a branch that doesn't resolve to a commit. For
    /// the **main** worktree this is a fresh `git init` (unborn — list as the
    /// branch); for a **linked** worktree it means the branch was deleted out
    /// from under it (orphaned). The caller disambiguates by context. Carries
    /// the branch name when the symbolic target is readable.
    Unborn(Option<String>),
    Orphaned,
}

fn worktree_head(repo: &Repository) -> HeadKind {
    match repo.head() {
        Ok(reference) => {
            if reference.is_branch() {
                reference
                    .shorthand()
                    .map_or(HeadKind::Orphaned, |s| HeadKind::Branch(s.to_owned()))
            } else if let Some(oid) = reference.target() {
                HeadKind::Detached(short_hash(oid))
            } else {
                HeadKind::Orphaned
            }
        }
        // HEAD → a branch ref with no commit behind it. Ambiguous between fresh
        // (main) and orphaned (linked); the caller decides.
        Err(e) if e.code() == ErrorCode::UnbornBranch => HeadKind::Unborn(head_branch_name(repo)),
        // Any other HEAD failure → can't attach it to a branch.
        Err(_) => HeadKind::Orphaned,
    }
}

/// The branch a prunable worktree record was checked out on, recovered from the
/// record's own `HEAD` file (the worktree directory is gone, so we can't open it).
///
/// **Couples to git's internal layout** (`<commondir>/worktrees/<name>/HEAD`),
/// which is undocumented and could drift. Any failure — file missing, unreadable,
/// malformed, or a detached oid rather than a `ref:` — returns `None`, which the
/// caller treats as a branch-less prunable row (degenerate but recoverable), not
/// an error.
fn prunable_record_branch(repo: &Repository, wt_name: &str) -> Option<String> {
    let head_path = repo
        .commondir()
        .join("worktrees")
        .join(wt_name)
        .join("HEAD");
    let contents = std::fs::read_to_string(head_path).ok()?;
    // Symbolic form only: "ref: refs/heads/<branch>". A detached record has a raw
    // oid here, which has no branch to attach to → None.
    let target = contents.trim().strip_prefix("ref: ")?;
    target.rsplit('/').next().map(str::to_owned)
}

/// The branch name HEAD symbolically points at (e.g. `refs/heads/main` → `main`),
/// even when that ref doesn't resolve to a commit.
fn head_branch_name(repo: &Repository) -> Option<String> {
    repo.find_reference("HEAD")
        .ok()
        .and_then(|h| h.symbolic_target().ok().flatten().map(str::to_owned))
        .and_then(|t| t.rsplit('/').next().map(str::to_owned))
}

/// Whether the worktree has tracked changes (`dirty`) and/or untracked files,
/// reported separately. Submodules are excluded (not computed in v1).
///
/// A status read failure is propagated, never swallowed into a false "clean":
/// `dirty: false` is the "safe to delete/prune" signal, so a false negative on an
/// unreadable index is exactly the kind of mid-read failure `GitError` exists for.
fn worktree_changes(repo: &Repository) -> std::result::Result<(bool, bool), git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .exclude_submodules(true)
        .include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    let mut dirty = false;
    let mut untracked = false;
    for entry in statuses.iter() {
        let s = entry.status();
        if s.contains(Status::WT_NEW) {
            untracked = true;
        }
        if s.intersects(tracked_change_mask()) {
            dirty = true;
        }
    }
    Ok((dirty, untracked))
}

/// The status bits that count as a *tracked* change (staged or unstaged), i.e.
/// everything except untracked (`WT_NEW`) and ignored.
fn tracked_change_mask() -> Status {
    Status::INDEX_NEW
        | Status::INDEX_MODIFIED
        | Status::INDEX_DELETED
        | Status::INDEX_RENAMED
        | Status::INDEX_TYPECHANGE
        | Status::WT_MODIFIED
        | Status::WT_DELETED
        | Status::WT_RENAMED
        | Status::WT_TYPECHANGE
        | Status::CONFLICTED
}

fn head_tree(repo: &Repository) -> Option<git2::Tree<'_>> {
    repo.head().ok()?.peel_to_tree().ok()
}

fn classify_change(status: Status) -> Option<ChangeKind> {
    // Prefer the worktree side, then the index side; renames take precedence so
    // a rename isn't reported as add+delete.
    if status.intersects(Status::INDEX_RENAMED | Status::WT_RENAMED) {
        Some(ChangeKind::Renamed)
    } else if status.contains(Status::WT_NEW) {
        Some(ChangeKind::Untracked)
    } else if status.intersects(Status::INDEX_NEW) {
        Some(ChangeKind::Added)
    } else if status.intersects(Status::INDEX_DELETED | Status::WT_DELETED) {
        Some(ChangeKind::Deleted)
    } else if status.intersects(
        Status::INDEX_MODIFIED
            | Status::WT_MODIFIED
            | Status::INDEX_TYPECHANGE
            | Status::WT_TYPECHANGE
            | Status::CONFLICTED,
    ) {
        Some(ChangeKind::Modified)
    } else {
        None
    }
}

fn renamed_target(entry: &git2::StatusEntry<'_>) -> Option<String> {
    let from_workdir = entry
        .index_to_workdir()
        .and_then(|d| d.new_file().path())
        .map(|p| p.to_string_lossy().into_owned());
    from_workdir.or_else(|| {
        entry
            .head_to_index()
            .and_then(|d| d.new_file().path())
            .map(|p| p.to_string_lossy().into_owned())
    })
}

/// Cap on rendered diff lines per file. A generated file (lockfile, bundle) can
/// be tens of thousands of lines; rendering that many rows would freeze the
/// panel. Past the cap we stop collecting and flag `truncated` so the UI can say
/// the file was cut off rather than imply it's fully shown.
const MAX_DIFF_LINES: usize = 5_000;

/// Walk libgit2's structured diff for one file into [`FileDiff`] hunks. Uses the
/// same `print` traversal as the rest of this module, but collects structured
/// lines instead of flattening to unified text — the frontend renders from this
/// directly. A traversal failure (corrupt blob, mid-read I/O) is propagated.
///
/// When `filter_to_file` is set, only deltas whose path equals `file` are
/// collected — used when the diff spans the whole commit (so rename detection can
/// pair the old and new paths) and we want just this file's hunks. When unset the
/// diff is already pathspec-scoped to `file`, so every delta is collected.
fn collect_file_diff(
    diff: &Diff<'_>,
    file: &str,
    filter_to_file: bool,
) -> std::result::Result<FileDiff, git2::Error> {
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut binary = false;
    let mut truncated = false;
    let mut lines = 0usize;
    let target = std::path::Path::new(file);

    diff.print(DiffFormat::Patch, |delta, hunk, line| {
        // Past the cap, append nothing more — not content *and not hunk headers*
        // (otherwise a huge file leaves a tail of empty hunks). We keep returning
        // `true`: returning `false` aborts `print` as a `GIT_EUSER` error that the
        // `?` below would turn a successful-but-truncated diff into a `GitRead`
        // failure. libgit2 has already computed the diff before this walk, so
        // finishing the (now no-op) iteration is cheap.
        if truncated {
            return true;
        }
        // Skip lines belonging to other files' deltas when collecting one file
        // out of a whole-commit diff (a rename delta's path is its new name).
        if filter_to_file {
            let delta_path = delta.new_file().path().or_else(|| delta.old_file().path());
            if delta_path != Some(target) {
                return true;
            }
        }
        match line.origin() {
            // Binary content: libgit2 emits a marker line, no body. Record the
            // flag; the UI shows a placeholder.
            'B' => binary = true,
            // Hunk header: open a new hunk. The header text lives on `hunk`.
            'H' => {
                let header = hunk
                    .map(|h| String::from_utf8_lossy(h.header()).trim_end().to_owned())
                    .unwrap_or_default();
                hunks.push(DiffHunk {
                    header,
                    lines: Vec::new(),
                });
            }
            // Content lines. `=` is a context line on a file with no trailing
            // newline; treat it like ordinary context. The `>`/`<` EOFNL markers
            // and the `F` file header carry no renderable content — skip them.
            origin @ (' ' | '=' | '+' | '-') => {
                if lines >= MAX_DIFF_LINES {
                    truncated = true;
                    return true;
                }
                lines += 1;
                let kind = match origin {
                    '+' => DiffLineKind::Added,
                    '-' => DiffLineKind::Removed,
                    _ => DiffLineKind::Context,
                };
                // Strip the trailing newline (and a preceding CR for CRLF); a
                // last line with no newline is left as-is.
                let raw = String::from_utf8_lossy(line.content());
                let content = match raw.strip_suffix('\n') {
                    Some(without_lf) => without_lf.strip_suffix('\r').unwrap_or(without_lf),
                    None => &raw,
                }
                .to_owned();
                // Defensive: libgit2 always emits a hunk header before content,
                // but never index past an empty stack.
                if let Some(current) = hunks.last_mut() {
                    current.lines.push(DiffLine {
                        origin: kind,
                        old_lineno: line.old_lineno(),
                        new_lineno: line.new_lineno(),
                        content,
                    });
                }
            }
            _ => {}
        }
        true
    })?;

    Ok(FileDiff {
        path: file.to_owned(),
        binary,
        truncated,
        hunks,
    })
}

/// Open the tree-to-tree diff of a commit vs. its first parent (empty tree for a
/// root commit), applying caller-provided `opts` (e.g. a pathspec). Returns
/// `None` for an unparseable or unknown `oid` — a stale reference, not a failure.
fn commit_tree_diff<'r>(
    repo: &'r Repository,
    oid: &str,
    opts: &mut DiffOptions,
) -> std::result::Result<Option<Diff<'r>>, git2::Error> {
    let Ok(oid) = Oid::from_str(oid) else {
        return Ok(None);
    };
    let commit = match repo.find_commit(oid) {
        Ok(commit) => commit,
        Err(e) if is_not_found(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let commit_tree = commit.tree()?;
    // First parent only: a merge commit diffs against its first parent, the
    // mainline view. A *true* root commit (no parents) diffs against the empty
    // tree, so every file reads as added. Decide root by `parent_count` — which
    // reads the already-loaded commit header, no object lookup — so a non-root
    // commit whose parent object is genuinely missing (a corrupt repo, or a
    // shallow clone's boundary commit) surfaces as a read error instead of being
    // silently rendered as an all-added "root".
    let parent_tree = if commit.parent_count() == 0 {
        None
    } else {
        Some(commit.parent(0)?.tree()?)
    };
    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), Some(opts))?;
    Ok(Some(diff))
}

/// Collect a tree-to-tree diff's deltas into [`ChangedFile`]s, newest path order.
fn changed_files_from_diff(diff: &Diff<'_>) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    for delta in diff.deltas() {
        let Some(change) = classify_delta(delta.status()) else {
            continue;
        };
        // New path normally; old path for a delete (its new side is /dev/null).
        let file_path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().into_owned());
        if let Some(file_path) = file_path {
            files.push(ChangedFile {
                path: file_path,
                change,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// Map a tree-to-tree delta status to a [`ChangeKind`]. There is no `Untracked`
/// in committed history (that's a worktree-only state), so it never appears here.
fn classify_delta(status: git2::Delta) -> Option<ChangeKind> {
    use git2::Delta;
    match status {
        Delta::Added | Delta::Copied => Some(ChangeKind::Added),
        Delta::Deleted => Some(ChangeKind::Deleted),
        Delta::Modified | Delta::Typechange => Some(ChangeKind::Modified),
        Delta::Renamed => Some(ChangeKind::Renamed),
        _ => None,
    }
}

/// Cap on commits returned per range. The branch commit list is a navigation
/// aid, not a full `git log`; past this we stop walking and flag `truncated`.
const MAX_COMMITS: usize = 50;

/// Assemble the commit ranges for a resolved branch tip and its optional
/// upstream tip. See [`commit_ranges`] for the range semantics.
fn build_commit_ranges(
    repo: &Repository,
    tip: Oid,
    upstream: Option<Oid>,
    default_tip: Option<Oid>,
    selected_default_branch: bool,
) -> std::result::Result<Vec<GitCommitRange>, git2::Error> {
    let no_ahead = HashSet::new();
    let Some(upstream) = upstream else {
        // No upstream (local-only branch, or a remote ref): recent history,
        // nothing to mark as unpushed.
        let annotation = CommitAnnotation {
            default_tip,
            selected_default_branch,
            ahead: &no_ahead,
        };
        return Ok(vec![commit_range(
            repo,
            CommitRangeKind::Recent,
            "Recent commits",
            tip,
            None,
            &annotation,
        )?]);
    };

    // Unpushed commits are flagged per-commit inside the single local-history
    // list rather than split into their own section: a merge of the default
    // branch can leave unpushed commits interleaved with already-pushed ones
    // (by date), so a contiguous "unpushed" range can't represent them. The
    // ahead set is the local commits the upstream doesn't have yet.
    let ahead = collect_ahead(repo, tip, upstream);
    let local = CommitAnnotation {
        default_tip,
        selected_default_branch,
        ahead: &ahead,
    };
    let mut ranges = vec![commit_range(
        repo,
        CommitRangeKind::Recent,
        "Recent commits",
        tip,
        None,
        &local,
    )?];

    if has_incoming(repo, upstream, tip) {
        // Upstream commits the local branch lacks — a genuinely separate set
        // (not in the local history), so it stays its own section and carries no
        // unpushed flags (these commits are on the remote, not local).
        let incoming = CommitAnnotation {
            default_tip,
            selected_default_branch,
            ahead: &no_ahead,
        };
        ranges.push(commit_range(
            repo,
            CommitRangeKind::Incoming,
            "Incoming commits",
            upstream,
            Some(tip),
            &incoming,
        )?);
    }
    Ok(ranges)
}

/// OIDs reachable from `tip` but not from `upstream` — the local commits that
/// haven't been pushed. Used to flag commits within the local-history list.
///
/// Best-effort, matching this module's "absent is not an error" stance: a failed
/// revwalk setup yields an empty set and `flatten()` drops any commit that fails
/// to resolve mid-walk. The cost of a missed OID is a single commit rendering
/// without its "not pushed" dot — a degraded indicator, never a failed read.
fn collect_ahead(repo: &Repository, tip: Oid, upstream: Oid) -> HashSet<Oid> {
    let mut set = HashSet::new();
    let Ok(mut walk) = repo.revwalk() else {
        return set;
    };
    if walk.push(tip).is_err() || walk.hide(upstream).is_err() {
        return set;
    }
    for oid in walk.flatten() {
        set.insert(oid);
    }
    set
}

/// Whether `upstream` has any commit the local `tip` lacks (incoming/"not
/// pulled"). Short-circuits on the first such commit. Degrades to `false` on a
/// revwalk-setup failure, the same best-effort stance as [`collect_ahead`].
fn has_incoming(repo: &Repository, upstream: Oid, tip: Oid) -> bool {
    let Ok(mut walk) = repo.revwalk() else {
        return false;
    };
    if walk.push(upstream).is_err() || walk.hide(tip).is_err() {
        return false;
    }
    walk.flatten().next().is_some()
}

/// Per-commit annotation inputs shared across a range walk: the default-branch
/// tip and whether the default branch is selected (drive `branch_work`), plus
/// the ahead set — local OIDs not on the upstream (drives `unpushed`).
struct CommitAnnotation<'a> {
    default_tip: Option<Oid>,
    selected_default_branch: bool,
    ahead: &'a HashSet<Oid>,
}

/// One range: walk from `push`, optionally hiding everything reachable from
/// `hide`, capping at [`MAX_COMMITS`].
fn commit_range(
    repo: &Repository,
    kind: CommitRangeKind,
    label: &str,
    push: Oid,
    hide: Option<Oid>,
    annotation: &CommitAnnotation<'_>,
) -> std::result::Result<GitCommitRange, git2::Error> {
    let (commits, truncated) = walk_commits(repo, push, hide, annotation)?;
    Ok(GitCommitRange {
        kind,
        label: label.to_owned(),
        commits,
        truncated,
    })
}

/// Revwalk newest-first from `push` (hiding `hide`'s history when given),
/// collecting up to [`MAX_COMMITS`] summaries. `truncated` is `true` when at
/// least one more commit existed past the cap.
fn walk_commits(
    repo: &Repository,
    push: Oid,
    hide: Option<Oid>,
    annotation: &CommitAnnotation<'_>,
) -> std::result::Result<(Vec<GitCommitSummary>, bool), git2::Error> {
    let mut walk = repo.revwalk()?;
    // Topological (child before parent) with time as the tiebreak. Commit times
    // have 1-second resolution, so same-second commits tie under a pure time
    // sort and fall out in a non-linear order; topological keeps the list a
    // stable newest-first walk regardless.
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
    walk.push(push)?;
    if let Some(hide) = hide {
        walk.hide(hide)?;
    }

    let mut commits = Vec::new();
    let mut truncated = false;
    for oid in walk {
        if commits.len() == MAX_COMMITS {
            truncated = true;
            break;
        }
        let commit = repo.find_commit(oid?)?;
        commits.push(commit_summary(repo, &commit, annotation));
    }
    Ok((commits, truncated))
}

fn commit_summary(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    annotation: &CommitAnnotation<'_>,
) -> GitCommitSummary {
    let author = commit.author();
    GitCommitSummary {
        oid: commit.id().to_string(),
        short_oid: short_hash(commit.id()),
        subject: commit
            .summary()
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_owned(),
        author_name: author.name().ok().map(str::to_owned),
        author_email: author.email().ok().map(str::to_owned),
        authored_at: format_commit_time(author.when()),
        branch_work: !annotation.selected_default_branch
            && is_branch_work(repo, commit, annotation.default_tip),
        unpushed: annotation.ahead.contains(&commit.id()),
    }
}

fn is_branch_work(repo: &Repository, commit: &git2::Commit<'_>, default_tip: Option<Oid>) -> bool {
    let Some(default_tip) = default_tip else {
        return false;
    };
    let oid = commit.id();
    oid != default_tip
        && !repo.graph_descendant_of(default_tip, oid).unwrap_or(false)
        && !merge_imports_only_default_branch(repo, commit, default_tip)
}

fn merge_imports_only_default_branch(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    default_tip: Oid,
) -> bool {
    if commit.parent_count() < 2 {
        return false;
    }

    (1..commit.parent_count()).all(|parent_index| {
        commit.parent_id(parent_index).is_ok_and(|parent| {
            parent == default_tip
                || repo
                    .graph_descendant_of(default_tip, parent)
                    .unwrap_or(false)
        })
    })
}

/// git2's `Time` (epoch seconds + a per-commit UTC offset in minutes) → RFC-3339,
/// preserving the author's recorded offset. `None` if the stored values don't
/// form a valid instant (defensive).
fn format_commit_time(time: git2::Time) -> Option<String> {
    let offset = FixedOffset::east_opt(time.offset_minutes() * 60)?;
    match offset.timestamp_opt(time.seconds(), 0) {
        LocalResult::Single(dt) => Some(dt.to_rfc3339()),
        _ => None,
    }
}

fn short_hash(oid: Oid) -> String {
    let s = oid.to_string();
    s.chars().take(7).collect()
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "ahead/behind counts past u32::MAX are not realistic; clamp is defensive"
)]
fn clamp_u32(n: usize) -> u32 {
    n.min(u32::MAX as usize) as u32
}

fn is_not_found(e: &git2::Error) -> bool {
    e.code() == ErrorCode::NotFound
}
