//! Switchboard git read layer — pure Rust, no Tauri, no async, no UI.
//!
//! Given a path on disk, this crate produces the full read-model the Git view
//! needs: a repo's branches (local and remote), the worktrees its branches are
//! checked out in, per-branch and per-worktree status, and the changed-files +
//! diff text for a single worktree.
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
//! reported as `None` ("couldn't determine"). Note `origin/HEAD` only exists on
//! cloned repos, so the local-`main` fallback is the common path for `git init`
//! repos — not an edge case.
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
    BranchView, ChangeKind, ChangedFile, RemoteBranchView, RepoView, SyncState, WorktreeView,
    WorktreeWarning,
};
pub use read::{changed_files, diff_text, read_repo, resolve_repo_root};
