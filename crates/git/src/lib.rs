//! Switchboard git read layer — pure Rust, no Tauri, no async, no UI.
//!
//! Given a path on disk, this crate produces the full read-model the Git view
//! needs: a repo's branches (local and remote), the worktrees its branches are
//! checked out in, per-branch and per-worktree status, and structured file diffs
//! for worktrees, commits, and PR-style branch comparisons.
//!
//! # `git2` reads, shell-out writes
//!
//! This crate is **read-only**. Every read here goes through `git2` (libgit2):
//! local, fast, no credentials, no network. The two *write/network* operations
//! the feature needs — `git fetch` and `git worktree add` — deliberately do
//! **not** live here: they are shelled out to the `git` CLI from the `app`
//! crate, because (a) fetch needs the user's configured credential helpers /
//! SSH agent, which libgit2's credential callbacks reproduce poorly, and
//! (b) `git`'s own error messages for a failed worktree add are better than
//! library errors and must be surfaced to the user verbatim. So this crate's
//! `git2` dependency is built with `default-features = false` — none of
//! libgit2's network/TLS features are needed.
//!
//! # Default-branch resolution
//!
//! The default branch (used for `merged` and `behind_base`) is detected in this
//! order: the symbolic target of `refs/remotes/origin/HEAD`, then a local
//! `main`, then a local `master`. If none resolves, `merged`/`behind_base` are
//! reported as "couldn't determine" (`None` / [`BehindBase::Unknown`]). Note
//! `origin/HEAD` only exists on cloned repos, so the local-`main` fallback is
//! the common path for `git init` repos — not an edge case.
//!
//! # Branch-primary, two-level status
//!
//! Branches are the primary unit; a worktree is an attribute of the branch
//! checked out in it. Branch-level status (sync, behind-base, merged, dangling)
//! is computed for every local branch; worktree-level status (dirty, untracked,
//! orphaned/prunable warnings) only for branches that are checked out. Remote
//! branches carry only the cleanup signals (`merged`, `behind_base`). See
//! [`mod@model`] for the full contract.
//!
//! # Many-branch repos
//!
//! Per-branch ancestry against the default branch is the expensive part of a
//! read: each is a history walk as long as the branch is stale, and repos that
//! never prune carry hundreds of stale branches. So `merged` for branches
//! outside the recent set comes from one shared walk of the default branch's
//! history instead of one walk per branch, and the behind-base count is skipped
//! for them entirely ([`BehindBase::NotComputed`]). That walk is bounded — it
//! reaches back only as far as the oldest such branch, and never past a fixed
//! commit budget — so on a very long history an older branch it didn't reach
//! reports `merged: None` rather than a guess.
//!
//! # Not computed (v1)
//!
//! Submodule status, stash counts, and Git-LFS state are intentionally not
//! computed. A bare repo (no working tree of its own) is handled gracefully —
//! its branches and linked worktrees list, with `is_bare: true` — rather than
//! marked unavailable.

mod error;
mod model;
mod read;

pub use error::{GitError, Result};
pub use model::{
    BehindBase, BranchComparison, BranchView, ChangeKind, ChangedFile, CommitChanges,
    CommitRangeKind, DiffHunk, DiffLine, DiffLineKind, FileDiff, GitCommitRange, GitCommitSummary,
    RemoteBranchView, RepoView, SyncState, WorktreeView, WorktreeWarning,
};
pub use read::{
    BranchKind, RECENT_BRANCH_LIMIT, branch_comparison, branch_comparison_file_diff, changed_files,
    commit_changed_files, commit_file_diff, commit_ranges, file_diff, read_repo, resolve_repo_root,
    validate_branch_comparison_endpoint,
};
