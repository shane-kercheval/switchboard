//! The read-model the UI consumes — branch-primary, two-level status.
//!
//! Branches are the unit a user reasons about; a worktree is an attribute of the
//! branch checked out in it. So status splits two ways:
//!
//! - **Branch-level** ([`BranchView`]): sync-vs-upstream, behind-base,
//!   merged-into-default, dangling — computed for *every* local branch whether
//!   or not it's checked out anywhere.
//! - **Worktree-level** ([`WorktreeView`]): uncommitted changes and the
//!   orphaned/prunable warnings — only present for a branch that is checked out.
//!
//! Remote branches ([`RemoteBranchView`]) carry only the two cleanup signals
//! (`merged`, `behind_base`) — the rest is meaningless for a remote-tracking
//! ref (no working tree, no own-upstream, can't have a deleted upstream).

use std::path::PathBuf;

use serde::Serialize;

/// One tracked repository's full read-model.
///
/// `available: false` is the non-error "couldn't read this path" state (missing,
/// unreadable, or not a git repo); the tree is empty in that case. A genuine
/// mid-read failure surfaces as [`crate::GitError`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoView {
    /// Main worktree root (or the common dir for a bare repo).
    pub root: PathBuf,
    /// Folder name of `root`, for display.
    pub name: String,
    /// Resolved default branch (`origin/HEAD` → local `main` → `master`), or
    /// `None` when none resolves — in which case `merged`/`behind_base` are
    /// `None` throughout.
    pub default_branch: Option<String>,
    /// `false` => path missing/unreadable/not-a-repo; the branch/worktree lists
    /// are empty.
    pub available: bool,
    /// A bare repo (no working tree of its own — the `git clone --bare` +
    /// worktrees layout). Its branches and linked worktrees still list; the bare
    /// root simply reports no working-tree status of its own.
    pub is_bare: bool,
    pub local_branches: Vec<BranchView>,
    pub remote_branches: Vec<RemoteBranchView>,
    /// Worktrees checked out at a detached HEAD (no branch to attach them to),
    /// labelled by short commit hash.
    pub detached_worktrees: Vec<WorktreeView>,
}

impl RepoView {
    /// The non-error "this path can't be read as a repo" result: an empty,
    /// clearly-marked view rather than a `GitError`. Public so the command layer
    /// can represent a tracked repo that errored or vanished as a still-visible
    /// `available: false` row (partial-success aggregation) rather than dropping
    /// it from the list.
    pub fn unavailable(root: PathBuf, name: String) -> Self {
        Self {
            root,
            name,
            default_branch: None,
            available: false,
            is_bare: false,
            local_branches: Vec::new(),
            remote_branches: Vec::new(),
            detached_worktrees: Vec::new(),
        }
    }
}

/// A local branch and its full branch-level status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchView {
    pub name: String,
    /// The upstream (remote-tracking) branch this tracks, if configured —
    /// e.g. `origin/feature-x`.
    pub upstream: Option<String>,
    /// Position relative to the branch's *own* upstream.
    pub sync: SyncState,
    /// Commits the default branch has that this branch lacks — "main moved on,
    /// you're stale." `None` when the default branch can't be resolved. Distinct
    /// from [`SyncState::Behind`] (which is vs. the branch's own upstream).
    pub behind_base: Option<u32>,
    /// Whether this branch's tip is an ancestor of the default branch tip
    /// ("done — safe to delete"). `None` when the default branch can't be
    /// resolved.
    pub merged: Option<bool>,
    /// The branch had an upstream that no longer exists (the remote branch was
    /// deleted) — a stale-branch cleanup signal.
    pub dangling: bool,
    /// The worktree this branch is checked out in, if any.
    pub worktree: Option<WorktreeView>,
}

/// A branch's position relative to its own upstream. Each variant maps 1:1 to an
/// at-a-glance badge.
///
/// `LocalOnly` (no upstream configured — a clean never-pushed branch) is
/// deliberately distinct from `Unknown` (genuinely couldn't compute): the former
/// is a clear, common, fine state; the latter is rare and means a real failure
/// to determine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncState {
    InSync,
    Ahead {
        commits: u32,
    },
    Behind {
        commits: u32,
    },
    Diverged {
        ahead: u32,
        behind: u32,
    },
    /// No upstream configured — "not pushed," a fine state, not an error.
    LocalOnly,
    /// Has an upstream but the ahead/behind comparison couldn't be computed.
    Unknown,
}

/// A remote-tracking branch (`origin/*`). Carries only the cleanup signals — see
/// the module doc for why the local-branch fields don't apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteBranchView {
    pub name: String,
    /// Already an ancestor of the default branch? ("stale remote, safe to
    /// delete"). `None` when the default branch can't be resolved.
    pub merged: Option<bool>,
    /// Commits the default branch has that this remote ref lacks. `None` when the
    /// default branch can't be resolved.
    pub behind_base: Option<u32>,
}

/// A checked-out working directory and its worktree-level status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeView {
    pub path: PathBuf,
    /// Tracked changes, staged or unstaged.
    pub dirty: bool,
    /// Untracked files present (reported separately from `dirty`).
    pub untracked: bool,
    /// For a detached-HEAD worktree, the short commit hash it's parked on; `None`
    /// for a branch worktree.
    pub detached_hash: Option<String>,
    pub warning: Option<WorktreeWarning>,
}

/// The two worktree warning states the tree surfaces (but offers no destructive
/// remedy for in v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeWarning {
    /// The worktree's directory is on disk but its branch was deleted.
    Orphaned,
    /// Git holds a worktree record whose directory is gone.
    Prunable,
}

/// One changed file in a worktree (consumed by the M5 diff panel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedFile {
    /// Repo-relative path. For a rename this is the new path.
    pub path: String,
    pub change: ChangeKind,
}

/// The kind of change to a file in the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

/// One file's working-tree diff as structured hunks, for the M5 diff panel.
///
/// Built straight from libgit2's structured diff rather than parsed back from
/// unified-diff text — the frontend renders rows directly from this, so there's
/// no text round-trip and the renderer can highlight each line's content with the
/// app's existing syntax highlighter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDiff {
    /// Repo-relative path the diff is for (the new path on a rename).
    pub path: String,
    /// Binary change: `hunks` is empty and the UI shows a placeholder instead of
    /// a body (libgit2 declines to render binary content inline).
    pub binary: bool,
    /// The diff exceeded the render cap and `hunks` was truncated, so the UI can
    /// say so rather than imply the whole file is shown.
    pub truncated: bool,
    pub hunks: Vec<DiffHunk>,
}

impl FileDiff {
    /// An empty diff for `file` (clean, or a path that isn't a readable worktree)
    /// — no hunks, not binary, not truncated.
    #[must_use]
    pub fn empty(file: impl Into<String>) -> Self {
        Self {
            path: file.into(),
            binary: false,
            truncated: false,
            hunks: Vec::new(),
        }
    }
}

/// One contiguous run of changed lines plus its surrounding context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffHunk {
    /// The `@@ -a,b +c,d @@` header text, shown above the hunk.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// One line within a hunk: its role plus the old/new line numbers (each present
/// only on the side where the line exists) and its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    pub origin: DiffLineKind,
    /// Line number on the old side; `None` for an added line.
    pub old_lineno: Option<u32>,
    /// Line number on the new side; `None` for a removed line.
    pub new_lineno: Option<u32>,
    /// The line's text, without the leading +/-/space marker and without the
    /// trailing newline.
    pub content: String,
}

/// A diff line's role. Drives the add/remove line-background tokens and the
/// side-by-side column placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

/// One commit's summary line for the branch commit list — identity, subject, and
/// authorship. Deliberately *not* part of [`RepoView`]: commits are read on
/// demand for the one selected branch, so a normal Git-view refresh never pays
/// for a history walk across every branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCommitSummary {
    /// Full hex object id — the stable identity used to load the commit's diff.
    pub oid: String,
    /// Abbreviated id for display (7 chars, matching the rest of this module).
    pub short_oid: String,
    /// First line of the commit message; empty for a commit with no message.
    pub subject: String,
    /// `None` when the commit's author identity has no (UTF-8) name/email.
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    /// Author timestamp as RFC-3339 (the wire convention for instants). `None`
    /// when the stored time can't be represented (defensive — real commits have
    /// a valid time).
    pub authored_at: Option<String>,
}

/// Which slice of history a [`GitCommitRange`] holds. Serializes to a bare
/// `snake_case` string (`"recent"`, …), so the field reads as `kind: "recent"`
/// on the wire — not a tagged object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitRangeKind {
    /// Most recent commits on the ref (in-sync, local-only, or remote-only).
    Recent,
    /// Local commits the upstream doesn't have yet ("not pushed").
    Unpushed,
    /// Upstream commits the local branch doesn't have yet ("not pulled").
    Incoming,
}

/// A capped, labelled slice of a branch's history. A branch yields one range
/// (recent) when in sync / local-only / remote-only, and up to two
/// (unpushed + incoming) when it diverges from its upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCommitRange {
    pub kind: CommitRangeKind,
    /// Human label for the section header (e.g. "Recent commits").
    pub label: String,
    /// Newest first, capped (see `MAX_COMMITS` in `read`).
    pub commits: Vec<GitCommitSummary>,
    /// More commits existed past the cap, so the UI can say the list is partial.
    pub truncated: bool,
}
