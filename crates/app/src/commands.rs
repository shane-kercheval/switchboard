//! Free-function implementations behind each Tauri command. The
//! `#[tauri::command]` wrappers in `lib.rs` are thin shims that adapt these
//! to Tauri's `State<'_, AppState>` / `String` conventions; the free
//! functions are what the unit tests target.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use switchboard_core::{
    AgentId, AgentRecord, Attachment, CoreError, Directory, HarnessKind, Project, ProjectId,
    ProjectSummary, SelectionAxis, SendId, SessionLocator, normalize_selection,
};
use switchboard_dispatcher::{
    CancelOutcome, DispatchContextFactory, EventEmitter, OnBusy, RemovedQueuedMessage, SendOutcome,
};
use switchboard_harness::{CancelSource, HarnessAdapter, MessageId};
use switchboard_prompts::PromptService;
use uuid::Uuid;

use crate::dispatch_context::ProjectDispatchContextFactory;
use crate::error::AppError;
use crate::preferences::{self, Preferences};
use crate::state::{AppState, lock, persist_git_registry, persist_workspace};

/// Returned by `init_directory_impl` — gives the caller everything it needs
/// to render the directory header (path) and project list in one round trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryInfo {
    pub path: String,
    pub has_switchboard: bool,
    pub projects: Vec<ProjectSummary>,
}

/// One row of the flat cross-directory project list (`list_projects_impl`).
/// Carries the owning directory path and whether that directory is currently
/// available (loaded + readable). Wire type — serialized to the frontend.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectListing {
    pub id: ProjectId,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub directory: String,
    pub available: bool,
    /// Recency-ordering key for the flat project list: the later of the
    /// project's journal mtime and `created_at` (see
    /// [`switchboard_core::Directory::project_last_activity`]). For an
    /// unavailable directory (served from the cache) this is just `created_at`
    /// — its journal can't be stat'd while the directory is unreadable.
    pub last_activity: chrono::DateTime<chrono::Utc>,
    /// Whether the user has archived this project (hidden from the default
    /// view). User-global view-state from `workspace.yaml`, not on-disk project
    /// state — computed per row from the archived set, so it's reported even for
    /// rows served from the cache while their directory is unavailable.
    pub archived: bool,
}

/// Read-only inspection. Canonicalizes the path, checks whether
/// `.switchboard/` already exists, and lists projects if it does. **Does
/// not** create directories, write files, or modify `AppState` — the
/// frontend uses this to show the appropriate post-folder-picker CTA
/// (init / create-project / select-project) before committing.
pub async fn pick_directory_impl(path: &str) -> Result<DirectoryInfo, AppError> {
    let directory = Directory::at(Path::new(path))?;
    let has_switchboard = directory.has_switchboard();
    let projects = if has_switchboard {
        // Reject incompatible directory config versions before listing
        // projects. The version field exists explicitly so a future v2
        // schema can't be silently accepted by a v1 build.
        directory.config()?;
        directory.list_projects()?
    } else {
        Vec::new()
    };
    Ok(DirectoryInfo {
        path: directory.path.to_string_lossy().into_owned(),
        has_switchboard,
        projects,
    })
}

/// Additive + idempotent: creates `.switchboard/` if missing and adds the
/// directory to the multi-directory workspace. The directory is keyed by its
/// **canonical** path (`Directory::at` canonicalizes), so two spellings of the
/// same directory collapse to one entry. Re-initializing an already-loaded
/// directory just refreshes the handle and its cached project snapshot — it
/// never clears other directories or any shared map. Adding a *new* directory
/// leaves every already-loaded directory's projects, locks, and caches intact.
///
/// Also registers the canonical path in the user-global workspace registry
/// (`workspace.yaml`) and refreshes that directory's cached project snapshot.
pub async fn init_directory_impl(state: &AppState, path: &str) -> Result<DirectoryInfo, AppError> {
    // Serialize against concurrent registry writes (create_project,
    // register_agent). init_directory creates `.switchboard/` structure
    // and writes the directory's config.yaml — both modify the registry's
    // on-disk shape.
    let _write = lock(&state.registry_write);
    let directory = Directory::at(Path::new(path))?;
    directory.init()?;
    // Validate the directory's config version after init (init creates a
    // fresh v1 config if missing; this catches the case where the user
    // points at a directory with an incompatible existing config).
    directory.config()?;
    let projects = directory.list_projects()?;
    let canonical = directory.path.clone();
    let info = DirectoryInfo {
        path: canonical.to_string_lossy().into_owned(),
        has_switchboard: directory.has_switchboard(),
        projects: projects.clone(),
    };

    // Insert (or refresh) the handle. Re-init of an already-loaded canonical
    // path replaces only its own handle — every other loaded directory and all
    // shared maps are untouched (the additive contract).
    lock(&state.directories).insert(canonical.clone(), directory);

    // Register in the user-global workspace and refresh its cached snapshot.
    // The registry compares paths as-given, so only ever feed it canonical
    // paths (decision: "Directory identity is canonicalized at the boundary").
    {
        let mut workspace = lock(&state.workspace);
        workspace.add(canonical.clone());
        workspace.refresh_cache(&canonical, projects);
    }
    persist_workspace(state);

    // One-directional Git-view auto-sync: if this directory lives in a git repo,
    // track that repo's canonical root in the Git view. Adding a subdirectory or
    // a linked worktree resolves to the same root and dedups. A non-git
    // directory simply doesn't resolve — skipped, no error. `git_registry` is
    // acquired here under the held `registry_write`, per the documented order.
    if let Some(root) = switchboard_git::resolve_repo_root(&canonical) {
        let mut git_registry = lock(&state.git_registry);
        let added = !git_registry.contains(&root);
        git_registry.add(root);
        drop(git_registry);
        if added {
            persist_git_registry(state);
        }
    }

    Ok(info)
}

/// Remove a directory from the workspace. Drains any in-flight turns on its
/// agents, releases its project locks, drops its loaded projects/agents, and
/// removes it from the workspace registry. **Never deletes `.switchboard/` on
/// disk** — re-initializing the same path restores its projects. Idempotent:
/// removing an absent/unavailable directory is `Ok`.
pub async fn remove_directory_impl(state: &AppState, path: &str) -> Result<(), AppError> {
    let canonical = canonicalize_boundary(path);

    // **Teardown ordering (load-bearing).** Remove every piece of *routable*
    // in-memory state FIRST — atomically under `registry_write`, with no `.await`
    // crossing the guard — then release the guard and drain the actors. Clearing
    // the maps before releasing the guard closes the teardown race two ways:
    //   - a racing `send` can no longer resolve the agent (it misses
    //     `agents_by_id` → `AgentNotFound`), and a racing `create_*` /
    //     `open_project` can no longer resolve the removed directory or its
    //     now-cleared active project;
    //   - the narrow window where a send already passed `lookup_agent` but
    //     hasn't reached the dispatcher is closed by the dispatcher's own
    //     `Closing` slot — the late `Enqueue` is rejected, not resurrected.
    // So the actor drain (below) cannot be outrun by a new turn, and no orphan
    // actor/subprocess survives.
    //
    // `registry_write` serializes us against `create_project` / `create_agent` /
    // `attach` / first-open (all take it). It is `std::sync::Mutex` (its guard is
    // `!Send`), so it must be released before the drain `.await`.
    let agent_ids: Vec<AgentId>;
    let project_ids: Vec<ProjectId>;
    {
        let write = lock(&state.registry_write);

        let loaded = lock(&state.directories).contains_key(&canonical);
        if !loaded {
            // Not loaded — nothing routable to clear. Drop the guard, then fall
            // through to the always-run workspace removal below.
            drop(write);
            lock(&state.workspace).remove(&canonical);
            persist_workspace(state);
            return Ok(());
        }

        // Collect under brief, independent lock acquisitions, never nesting out
        // of the documented lock order. Snapshot the agent ids BEFORE clearing
        // `agents_by_id`.
        let pids: Vec<ProjectId> = lock(&state.projects)
            .values()
            .filter(|p| p.directory == canonical)
            .map(|p| p.id)
            .collect();
        let project_set: HashSet<ProjectId> = pids.iter().copied().collect();
        let aids: Vec<AgentId> = lock(&state.agents_by_id)
            .values()
            .filter(|r| project_set.contains(&r.project_id))
            .map(|r| r.id)
            .collect();

        // Clear routable state, each a brief independent acquisition, in the
        // documented lock order (directories → projects → active_project_id →
        // needs_session_meta → agents_by_id). **Do NOT touch `project_locks`
        // here** — the drain helper releases those locks AFTER the turns drain.
        lock(&state.directories).remove(&canonical);
        {
            let mut projects = lock(&state.projects);
            projects.retain(|id, _| !project_set.contains(id));
        }
        {
            let mut active = lock(&state.active_project_id);
            if matches!(*active, Some(id) if project_set.contains(&id)) {
                *active = None;
            }
        }
        {
            let mut needs = lock(&state.needs_session_meta);
            needs.retain(|id| !aids.contains(id));
        }
        {
            let mut agents = lock(&state.agents_by_id);
            agents.retain(|_, r| !project_set.contains(&r.project_id));
        }

        agent_ids = aids;
        project_ids = pids;
        // `write` drops here — the guard is released BEFORE the drain `.await`.
    }

    // Shut down each agent's dispatcher actor (cancels + drains any live turn)
    // and release the named project locks. Holds no other state lock across the
    // await.
    drain_agents_then_release_locks(state, &agent_ids, &project_ids, CancelSource::Shutdown).await;

    // Always drop the workspace entry + persist (idempotent for absent dirs).
    lock(&state.workspace).remove(&canonical);
    persist_workspace(state);
    Ok(())
}

/// *The* directory-identity chokepoint. Every command that resolves a working
/// directory to its canonical key — `init_directory_impl`, `create_project_impl`,
/// `remove_directory_impl` — funnels through here so a directory is identified
/// the same way on the way in (init/create) and on the way out (remove).
///
/// Canonicalizes via `std::fs::canonicalize` when the path exists on disk
/// (matching `Directory::at`, which is how loaded directories were keyed), and
/// falls back to the path as-given when it does not — so an unmounted/moved
/// directory still matches the canonical key stored while it was available.
fn canonicalize_boundary(path: &str) -> PathBuf {
    let raw = Path::new(path);
    std::fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf())
}

/// Flat cross-directory project list. Iterates the workspace registry in
/// insertion order; for each entry, reads projects from disk if the directory
/// is loaded (refreshing the cached snapshot, `available: true`), else falls
/// back to the cached snapshot (`available: false`).
///
/// **Persist-on-change only.** This is a hot read path (the UI hits it on every
/// project switch), so it persists `workspace.yaml` iff at least one cached
/// snapshot actually changed — avoiding a write storm of identical files.
///
/// **Corrupt vs. unavailable.** A loaded directory whose `list_projects` read
/// fails is *not* uniformly treated as "serve cache." A missing index / I/O
/// error (`CoreError::Io` / `MissingAppendOnlyFile` — unmounted, transient)
/// falls back to the cached snapshot as `available: false`. A *corruption*
/// error (`CoreError::CorruptJsonl` / `CorruptYaml` / `UnsupportedConfigVersion`
/// — a damaged Switchboard-owned file) is logged loudly and does **not** refresh
/// or persist the cache from the bad read; it still degrades to `available:
/// false` for now (the wire shape is unchanged — see below), but the read
/// boundary no longer makes corruption silently indistinguishable from
/// unmounted. One corrupt directory must never fail the whole aggregation — the
/// other directories still list.
//
// `available: bool` is intentionally kept (no status enum / `errored` variant):
// that is a frontend-facing wire change that lands additively with the switcher
// UI. For now corruption is distinct only in the logs.
//
// Returns `Result` even though it never errors today: it is the
// `#[tauri::command]` chokepoint for the cross-directory list. Keeping the
// fallible shape avoids a breaking signature change at the IPC boundary.
#[allow(clippy::unnecessary_wraps)]
pub fn list_projects_impl(state: &AppState) -> Result<Vec<ProjectListing>, AppError> {
    let entry_paths: Vec<PathBuf> = lock(&state.workspace)
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();

    let mut listings: Vec<ProjectListing> = Vec::new();
    let mut cache_changed = false;
    for path in &entry_paths {
        let dir_str = path.to_string_lossy().into_owned();
        let loaded = lock(&state.directories).get(path).cloned();

        // Distinguish three outcomes for a loaded directory's read:
        //   Some(summaries) → fresh read, refresh cache, available.
        //   None            → not loaded OR a transient/unavailable read error
        //                     (I/O, missing index) → serve cache, unavailable.
        // A corruption error logs loudly and also serves cache without
        // refreshing/persisting it (so the bad read can't overwrite the last
        // good snapshot).
        let fresh = match loaded
            .as_ref()
            .map(switchboard_core::Directory::list_projects)
        {
            Some(Ok(summaries)) => Some(summaries),
            None | Some(Err(CoreError::Io { .. } | CoreError::MissingAppendOnlyFile { .. })) => {
                None
            }
            Some(Err(
                e @ (CoreError::CorruptJsonl { .. }
                | CoreError::CorruptYaml { .. }
                | CoreError::UnsupportedConfigVersion { .. }),
            )) => {
                tracing::error!(
                    directory = %dir_str,
                    error = %e,
                    "directory registry is corrupt — listing its cached snapshot as unavailable; not refreshing cache from the bad read"
                );
                None
            }
            // Any other (future) CoreError variant: treat conservatively as
            // unavailable rather than refreshing the cache from a read we can't
            // classify. `CoreError` is `#[non_exhaustive]`.
            Some(Err(e)) => {
                tracing::warn!(
                    directory = %dir_str,
                    error = %e,
                    "directory registry read failed with an unclassified error — serving cached snapshot as unavailable"
                );
                None
            }
        };

        if let Some(summaries) = fresh {
            if lock(&state.workspace).refresh_cache(path, summaries.clone()) {
                cache_changed = true;
            }
            // `fresh` is `Some` only when the directory was loaded and read
            // cleanly, so `loaded` is `Some` here — stat each project's journal
            // for its recency key.
            let directory = loaded.as_ref();
            for s in summaries {
                let last_activity = directory.map_or(s.created_at, |d| {
                    d.project_last_activity(s.id, s.created_at)
                });
                let archived = lock(&state.workspace).is_archived(s.id);
                listings.push(ProjectListing {
                    id: s.id,
                    name: s.name,
                    created_at: s.created_at,
                    directory: dir_str.clone(),
                    available: true,
                    last_activity,
                    archived,
                });
            }
        } else {
            let cached: Vec<ProjectSummary> = lock(&state.workspace)
                .entries()
                .iter()
                .find(|e| &e.path == path)
                .map(|e| e.cached_projects.clone())
                .unwrap_or_default();
            for s in cached {
                let archived = lock(&state.workspace).is_archived(s.id);
                listings.push(ProjectListing {
                    id: s.id,
                    name: s.name,
                    created_at: s.created_at,
                    directory: dir_str.clone(),
                    available: false,
                    last_activity: s.created_at,
                    archived,
                });
            }
        }
    }
    if cache_changed {
        persist_workspace(state);
    }
    Ok(listings)
}

/// One directory row for the workspace switcher.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceDirectoryInfo {
    pub path: String,
    /// Whether the directory is currently loaded (openable on disk). An
    /// unavailable directory (unmounted/moved) still appears so the user can
    /// see and remove its cached entry.
    pub available: bool,
}

/// The switcher's view of the workspace registry: every registered directory
/// plus whether registry changes persist this session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceDirectories {
    pub directories: Vec<WorkspaceDirectoryInfo>,
    /// `false` when an existing `workspace.yaml` couldn't be read at startup
    /// (`workspace_path` left `None`). The UI surfaces this distinctly from a
    /// fresh install so a transient read error doesn't masquerade as a clean
    /// slate and lure the user into re-adding directories that silently fail to
    /// save (see [`crate::workspace::LoadOutcome`]).
    pub persistable: bool,
}

/// Every registered workspace directory for the switcher — including ones with
/// no projects yet (which `list_projects` omits) — plus the persistability
/// signal. Distinct from `list_projects` (the flat project list); the switcher
/// needs directory rows independent of project rows to render empty directories
/// and the add/remove-directory affordances.
pub fn list_workspace_directories_impl(state: &AppState) -> WorkspaceDirectories {
    let entry_paths: Vec<PathBuf> = lock(&state.workspace)
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    let directories = {
        let loaded = lock(&state.directories);
        entry_paths
            .into_iter()
            .map(|path| WorkspaceDirectoryInfo {
                available: loaded.contains_key(&path),
                path: path.to_string_lossy().into_owned(),
            })
            .collect()
    };
    WorkspaceDirectories {
        directories,
        persistable: state.workspace_path.is_some(),
    }
}

// --- Git view: tracked-repo registry, project linking, aggregate read --------

/// A Switchboard project linked to a worktree by exact path-match (decision 7).
/// The minimal identity the Git view needs to label a worktree's projects.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LinkedProject {
    pub id: ProjectId,
    pub name: String,
    /// The owning directory (== the worktree path, since linking is exact-match).
    pub directory: String,
}

/// One tracked repo for the Git view: the M1 read-model plus the project links.
///
/// `repo` is `switchboard_git::RepoView` verbatim — the single source of the git
/// contract, never re-mirrored here. Project linking is returned *alongside* as
/// a `worktree path → projects` map (computed on the backend, decision 7) rather
/// than nested into `RepoView`, so the git contract and the linking concern stay
/// decoupled and `RepoView` can't drift. The frontend joins them by worktree
/// path at render time (an O(1) lookup keyed by the path string).
#[derive(Debug, Clone, Serialize)]
pub struct RepoListing {
    pub repo: switchboard_git::RepoView,
    /// Keyed by worktree path (`to_string_lossy`), matching `WorktreeView.path`'s
    /// serialization, so the frontend can look up a worktree's links directly.
    pub linked_projects: HashMap<String, Vec<LinkedProject>>,
}

/// Add a repo to the Git-view registry by an explicit user action ("Add Repo").
///
/// Accepts any path inside a git repo — a subdirectory or a linked worktree
/// resolves to the same canonical root (decision 5a) and dedups. A path not
/// inside any git repo is rejected with [`AppError::NotAGitRepo`] for the
/// inline-error UX. Registry-only: never creates a workspace entry or a project.
pub fn add_tracked_repo_impl(state: &AppState, path: &str) -> Result<(), AppError> {
    let root = switchboard_git::resolve_repo_root(Path::new(path)).ok_or_else(|| {
        AppError::NotAGitRepo {
            path: path.to_owned(),
        }
    })?;
    // Serialize the registry mutation + its persistence under `registry_write`,
    // the same gate the auto-sync hook in `init_directory_impl` uses, so two
    // concurrent registry writes can't interleave a stale snapshot over a newer
    // one on disk.
    let _write = lock(&state.registry_write);
    let added = {
        let mut registry = lock(&state.git_registry);
        let added = !registry.contains(&root);
        registry.add(root);
        added
    };
    if added {
        persist_git_registry(state);
    }
    Ok(())
}

/// Remove a repo from the Git-view registry. **Registry only** — never touches
/// files, the on-disk repo, or `workspace.yaml` (decision 5). Idempotent.
///
/// Accepts the stored root or any path inside the tracked repo: a live path
/// resolves to its repo root (collapsing a subdirectory / linked worktree to the
/// stored entry); a dead path (the repo dir is gone) falls back to
/// `canonicalize_boundary`, which matches the root stored while it was available.
pub fn remove_tracked_repo_impl(state: &AppState, path: &str) {
    let target = switchboard_git::resolve_repo_root(Path::new(path))
        .unwrap_or_else(|| canonicalize_boundary(path));
    // Same serialization gate as add (see `add_tracked_repo_impl`).
    let _write = lock(&state.registry_write);
    let removed = lock(&state.git_registry).remove(&target);
    if removed {
        persist_git_registry(state);
    }
}

/// The aggregate Git-view read: for every tracked repo, the M1 `RepoView`
/// enriched with the Switchboard projects living in each worktree. The **sole**
/// read command (no separate cheap "list" — the per-repo availability is already
/// in each `RepoView`).
///
/// Partial success: a repo that errors mid-read (corrupt/I-O) is degraded to an
/// `available: false` row rather than failing the whole call — one bad repo
/// never blanks the view (mirrors `list_projects_impl`'s per-directory
/// resilience).
///
/// Split into a cheap state-reading half ([`tracked_repos_inputs`]) and this
/// pure compute half so the command shim can run the heavy `git2` reads under
/// `spawn_blocking` (decision 8) without borrowing `AppState` across threads.
#[must_use]
pub fn list_tracked_repos_from_inputs(inputs: &GitReadInputs) -> Vec<RepoListing> {
    inputs
        .roots
        .iter()
        .map(|root| read_one_repo_listing(root, &inputs.links))
        .collect()
}

/// Re-read a single tracked repo (per-repo refresh; decision 8 two-read split).
/// Same partial-success degradation and linking as the aggregate.
///
/// Honors the tracked set: `path` may be the stored root *or* any path inside a
/// tracked repo (a subdirectory / linked worktree) — it resolves to the repo
/// root and is read only if that root is tracked. An untracked path yields an
/// `available: false` row rather than live git data, so a refresh racing a
/// remove can't surface a ghost repo. A dead path (repo dir gone) falls back to
/// `canonicalize_boundary`, matching the root stored while it was available.
#[must_use]
pub fn read_tracked_repo_from_inputs(path: &str, inputs: &GitReadInputs) -> RepoListing {
    let root = switchboard_git::resolve_repo_root(Path::new(path))
        .unwrap_or_else(|| canonicalize_boundary(path));
    if !inputs.roots.contains(&root) {
        let name = root.file_name().map_or_else(
            || root.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        return RepoListing {
            repo: switchboard_git::RepoView::unavailable(root, name),
            linked_projects: HashMap::new(),
        };
    }
    read_one_repo_listing(&root, &inputs.links)
}

/// Snapshot the tracked repo roots from `AppState`. Run on the async thread so
/// the owned `Vec` can move into a `spawn_blocking` git read without holding the
/// registry lock across threads (mirrors [`tracked_repos_inputs`], but the diff
/// reads need only the roots, not the project-linking index).
#[must_use]
pub fn tracked_roots(state: &AppState) -> Vec<PathBuf> {
    lock(&state.git_registry).roots().to_vec()
}

/// Whether `path` resolves to a tracked repo root. The Git-view *data reads*
/// (this and [`read_tracked_repo_from_inputs`]) all honor the tracked set so the
/// backend never serves live git data for a repo the user hasn't added — a
/// worktree path resolves to its repo root (a subdirectory / linked / detached
/// worktree collapses to the same root), and a dead path falls back to
/// `canonicalize_boundary`, matching the root stored while it was available.
fn is_tracked_worktree(roots: &[PathBuf], path: &str) -> bool {
    let root = switchboard_git::resolve_repo_root(Path::new(path))
        .unwrap_or_else(|| canonicalize_boundary(path));
    roots.contains(&root)
}

/// The changed files in a worktree (working-tree changes vs. HEAD — staged,
/// unstaged, untracked), for the M5 diff panel's file list. A clean or
/// unreadable path yields an empty list (the non-error state); a genuine mid-read
/// failure surfaces as [`AppError::GitRead`] so the panel can say why it's empty.
///
/// `path` is the worktree directory itself (from the rendered tree), not a repo
/// root — the read is scoped to that one checked-out working tree. An untracked
/// path (a stale panel after "Remove from view") yields an empty list rather than
/// live data. Synchronous `git2`; the shim runs it on a blocking worker.
pub fn changed_files_impl(
    roots: &[PathBuf],
    path: &str,
) -> Result<Vec<switchboard_git::ChangedFile>, AppError> {
    if !is_tracked_worktree(roots, path) {
        return Ok(Vec::new());
    }
    switchboard_git::changed_files(Path::new(path)).map_err(|e| AppError::GitRead {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

/// The structured working-tree diff for one `file` (repo-relative) in the
/// worktree at `path`. Untracked path → empty [`FileDiff`]; clean/unreadable →
/// empty; binary content → `binary: true` with no hunks; a genuine mid-read
/// failure → [`AppError::GitRead`]. Synchronous `git2`; runs on a blocking worker.
pub fn file_diff_impl(
    roots: &[PathBuf],
    path: &str,
    file: &str,
) -> Result<switchboard_git::FileDiff, AppError> {
    if !is_tracked_worktree(roots, path) {
        return Ok(switchboard_git::FileDiff::empty(file));
    }
    switchboard_git::file_diff(Path::new(path), file).map_err(|e| AppError::GitRead {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

/// Whether `repo_root` is a tracked repo root. Same resolution as
/// [`is_tracked_worktree`] (a root resolves to itself), named for the commit
/// reads, which key on a repo root rather than a worktree path.
fn is_tracked_repo(roots: &[PathBuf], repo_root: &str) -> bool {
    is_tracked_worktree(roots, repo_root)
}

/// Capped commit-summary ranges for one branch (the branch commit list). Unlike
/// the worktree reads, an untracked repo root is **rejected** ([`AppError::RepoNotTracked`])
/// rather than served empty: this command is invoked deliberately for a branch in
/// a tracked repo, so an untracked root means a stale frontend reference, not a
/// clean-empty case. Synchronous `git2`; the shim runs it on a blocking worker.
pub fn commit_ranges_impl(
    roots: &[PathBuf],
    repo_root: &str,
    kind: switchboard_git::BranchKind,
    name: &str,
) -> Result<Vec<switchboard_git::GitCommitRange>, AppError> {
    if !is_tracked_repo(roots, repo_root) {
        return Err(AppError::RepoNotTracked {
            root: repo_root.to_owned(),
        });
    }
    switchboard_git::commit_ranges(Path::new(repo_root), kind, name).map_err(|e| {
        AppError::GitRead {
            path: repo_root.to_owned(),
            message: e.to_string(),
        }
    })
}

/// The files one commit changed (vs. its first parent), for the detail panel's
/// file list when a commit — rather than the worktree — is selected. Needs no
/// worktree, so it serves branches with no local folder and remote-only branches.
/// Untracked root → rejected; an unknown/invalid `oid` → empty (handled in the
/// read layer). Synchronous `git2`; runs on a blocking worker.
pub fn commit_changed_files_impl(
    roots: &[PathBuf],
    repo_root: &str,
    oid: &str,
) -> Result<switchboard_git::CommitChanges, AppError> {
    if !is_tracked_repo(roots, repo_root) {
        return Err(AppError::RepoNotTracked {
            root: repo_root.to_owned(),
        });
    }
    switchboard_git::commit_changed_files(Path::new(repo_root), oid).map_err(|e| {
        AppError::GitRead {
            path: repo_root.to_owned(),
            message: e.to_string(),
        }
    })
}

/// The structured diff of one `file` within one commit (vs. its first parent).
/// The committed-history analogue of [`file_diff_impl`]. Untracked root →
/// rejected; unknown/invalid `oid` or clean file → empty [`switchboard_git::FileDiff`].
/// Synchronous `git2`; runs on a blocking worker.
pub fn commit_file_diff_impl(
    roots: &[PathBuf],
    repo_root: &str,
    oid: &str,
    file: &str,
) -> Result<switchboard_git::FileDiff, AppError> {
    if !is_tracked_repo(roots, repo_root) {
        return Err(AppError::RepoNotTracked {
            root: repo_root.to_owned(),
        });
    }
    switchboard_git::commit_file_diff(Path::new(repo_root), oid, file).map_err(|e| {
        AppError::GitRead {
            path: repo_root.to_owned(),
            message: e.to_string(),
        }
    })
}

/// The `AppState`-derived inputs a Git-view read needs, snapshotted so the
/// `git2` work can move onto a blocking thread. Cheap to build (registry paths +
/// the flat project list); the expensive part is the git reads that consume it.
pub struct GitReadInputs {
    pub roots: Vec<PathBuf>,
    pub links: HashMap<PathBuf, Vec<LinkedProject>>,
}

/// Snapshot the registry roots + project-linking index from `AppState`. Run on
/// the async thread before handing `GitReadInputs` to `spawn_blocking`.
#[must_use]
pub fn tracked_repos_inputs(state: &AppState) -> GitReadInputs {
    GitReadInputs {
        roots: lock(&state.git_registry).roots().to_vec(),
        links: project_links_by_path(state),
    }
}

/// Read one repo's view and attach the worktree→projects links. A `GitError`
/// (genuine mid-read failure) degrades to a marked unavailable row, logged —
/// not propagated — so the aggregate never fails wholesale on one bad repo.
fn read_one_repo_listing(root: &Path, links: &HashMap<PathBuf, Vec<LinkedProject>>) -> RepoListing {
    let repo = switchboard_git::read_repo(root).unwrap_or_else(|e| {
        tracing::warn!(
            root = %root.display(),
            error = %e,
            "git read failed for a tracked repo — listing it as unavailable"
        );
        let name = root.file_name().map_or_else(
            || root.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        switchboard_git::RepoView::unavailable(root.to_path_buf(), name)
    });

    // Build the worktree-path → projects map for just this repo's worktrees.
    // Match on the canonicalized path (the `links` keys are canonicalized too),
    // since git2's worktree paths carry a trailing slash and other spelling
    // differences that an exact string compare would miss. The output map is
    // keyed by the *raw* worktree path string so the frontend can look it up
    // directly against `WorktreeView.path`'s serialization.
    let mut linked_projects: HashMap<String, Vec<LinkedProject>> = HashMap::new();
    for path in worktree_paths(&repo) {
        let canonical = canonicalize_boundary(&path.to_string_lossy());
        if let Some(projects) = links.get(&canonical) {
            linked_projects.insert(path.to_string_lossy().into_owned(), projects.clone());
        }
    }
    RepoListing {
        repo,
        linked_projects,
    }
}

/// Every checked-out worktree path in a repo view — the branch worktrees plus the
/// detached ones — the set against which project links are matched.
fn worktree_paths(repo: &switchboard_git::RepoView) -> Vec<PathBuf> {
    repo.local_branches
        .iter()
        .filter_map(|b| b.worktree.as_ref().map(|w| w.path.clone()))
        .chain(repo.detached_worktrees.iter().map(|w| w.path.clone()))
        .collect()
}

/// Build the project-linking index: canonical worktree/working-directory path →
/// the projects whose directory is exactly that path (decision 7, exact match —
/// a project in a *subfolder* of a worktree is intentionally not linked). Keyed
/// by canonicalized `PathBuf` so it matches `RepoView` worktree paths regardless
/// of spelling.
///
/// Reads the **in-memory** workspace cached snapshots, **not** `list_projects_impl`.
/// The Git-view read is polled by M3, so it must be side-effect-free: going
/// through `list_projects_impl` would re-scan every directory from disk and could
/// rewrite `workspace.yaml` as a cache-refresh side effect. The cached snapshot
/// is the workspace registry's purpose and is kept current by project
/// create/init/list, so linking stays accurate without that cost. (A brand-new
/// project links on the next workspace refresh — but create already refreshes the
/// cache, so in practice it's immediate.)
fn project_links_by_path(state: &AppState) -> HashMap<PathBuf, Vec<LinkedProject>> {
    let mut map: HashMap<PathBuf, Vec<LinkedProject>> = HashMap::new();
    for entry in lock(&state.workspace).entries() {
        let dir = entry.path.to_string_lossy().into_owned();
        let canonical = canonicalize_boundary(&dir);
        for s in &entry.cached_projects {
            map.entry(canonical.clone())
                .or_default()
                .push(LinkedProject {
                    id: s.id,
                    name: s.name.clone(),
                    directory: dir.clone(),
                });
        }
    }
    map
}

/// Shell out `git fetch` for a tracked repo to refresh its remote-tracking refs
/// (so the next local read's sync / behind-base is current). Shelled rather than
/// via `git2` (decision 2) because fetch needs the user's configured credential
/// helpers / SSH agent, which `git2`'s callbacks reproduce poorly.
///
/// Best-effort: returns the git error (stderr) so the caller can record a
/// "fetch failed" state, but a failure is never fatal — the view degrades to a
/// stale read, not an error surface. Runs against the repo root; a repo with no
/// remote simply fetches nothing.
///
/// Gated on the tracked set: `path` is resolved to a repo root (the stored root,
/// or any subdirectory / linked worktree inside it; a dead path falls back to
/// `canonicalize_boundary`) and the fetch runs **only** if that root is tracked,
/// else [`AppError::RepoNotTracked`]. Fetch is the one Git-view command that
/// spawns a subprocess, so it never acts on an arbitrary caller-supplied path.
pub async fn fetch_repo_impl(state: &AppState, path: &str) -> Result<(), AppError> {
    let root = switchboard_git::resolve_repo_root(Path::new(path))
        .unwrap_or_else(|| canonicalize_boundary(path));
    if !lock(&state.git_registry).contains(&root) {
        return Err(AppError::RepoNotTracked {
            root: root.to_string_lossy().into_owned(),
        });
    }
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("fetch")
        .arg("--all")
        .arg("--quiet")
        .output()
        .await
        .map_err(|source| AppError::GitFetch {
            root: root.to_string_lossy().into_owned(),
            message: source.to_string(),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(AppError::GitFetch {
            root: root.to_string_lossy().into_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

const EMPTY_TREE_OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[must_use]
pub fn worktree_difftool_argv(
    worktree_path: &str,
    file: &str,
    change: switchboard_git::ChangeKind,
) -> Vec<String> {
    if change == switchboard_git::ChangeKind::Untracked {
        vec![
            "-C".to_owned(),
            worktree_path.to_owned(),
            "difftool".to_owned(),
            "--no-prompt".to_owned(),
            "--no-index".to_owned(),
            "--".to_owned(),
            "/dev/null".to_owned(),
            file.to_owned(),
        ]
    } else {
        vec![
            "-C".to_owned(),
            worktree_path.to_owned(),
            "difftool".to_owned(),
            "--no-prompt".to_owned(),
            "HEAD".to_owned(),
            "--".to_owned(),
            file.to_owned(),
        ]
    }
}

#[must_use]
pub fn commit_difftool_argv(repo_root: &str, base_oid: &str, oid: &str, file: &str) -> Vec<String> {
    vec![
        "-C".to_owned(),
        repo_root.to_owned(),
        "difftool".to_owned(),
        "--no-prompt".to_owned(),
        base_oid.to_owned(),
        oid.to_owned(),
        "--".to_owned(),
        file.to_owned(),
    ]
}

fn git_output_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr
    }
}

async fn commit_first_parent_or_empty_tree(
    repo_root: &Path,
    oid: &str,
) -> Result<String, AppError> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-list")
        .arg("--parents")
        .arg("-n")
        .arg("1")
        .arg(oid)
        .output()
        .await
        .map_err(|source| AppError::GitDifftool {
            root: repo_root.to_string_lossy().into_owned(),
            message: source.to_string(),
        })?;
    if !output.status.success() {
        return Err(AppError::GitDifftool {
            root: repo_root.to_string_lossy().into_owned(),
            message: git_output_message(&output),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut parts = stdout.split_whitespace();
    let Some(_commit) = parts.next() else {
        return Err(AppError::GitDifftool {
            root: repo_root.to_string_lossy().into_owned(),
            message: "git rev-list returned no commit".to_owned(),
        });
    };
    Ok(parts.next().unwrap_or(EMPTY_TREE_OID).to_owned())
}

async fn run_git_difftool(root: &Path, argv: Vec<String>) -> Result<(), AppError> {
    let output = tokio::process::Command::new("git")
        .args(argv)
        .output()
        .await
        .map_err(|source| AppError::GitDifftool {
            root: root.to_string_lossy().into_owned(),
            message: source.to_string(),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(AppError::GitDifftool {
            root: root.to_string_lossy().into_owned(),
            message: git_output_message(&output),
        })
    }
}

pub async fn open_worktree_file_difftool_impl(
    state: &AppState,
    worktree_path: &str,
    file: &str,
    change: switchboard_git::ChangeKind,
) -> Result<(), AppError> {
    let root = switchboard_git::resolve_repo_root(Path::new(worktree_path))
        .unwrap_or_else(|| canonicalize_boundary(worktree_path));
    if !lock(&state.git_registry).contains(&root) {
        return Err(AppError::RepoNotTracked {
            root: root.to_string_lossy().into_owned(),
        });
    }
    run_git_difftool(&root, worktree_difftool_argv(worktree_path, file, change)).await
}

pub async fn open_commit_file_difftool_impl(
    state: &AppState,
    repo_root: &str,
    oid: &str,
    file: &str,
) -> Result<(), AppError> {
    let root = switchboard_git::resolve_repo_root(Path::new(repo_root))
        .unwrap_or_else(|| canonicalize_boundary(repo_root));
    if !lock(&state.git_registry).contains(&root) {
        return Err(AppError::RepoNotTracked {
            root: root.to_string_lossy().into_owned(),
        });
    }
    let base_oid = commit_first_parent_or_empty_tree(&root, oid).await?;
    run_git_difftool(
        &root,
        commit_difftool_argv(&root.to_string_lossy(), &base_oid, oid, file),
    )
    .await
}

// --- Git-view open actions --------------------------------------------------

/// The macOS argv for opening a worktree folder in an external editor: the
/// user's configured `editor_command` run against the path via the user's
/// login shell, or the OS
/// folder-open (`open <path>`) when no editor command is set. The blank-command
/// fallback means open-in-editor works with zero config. argv[0] is the program;
/// the rest are its arguments.
///
/// The editor command is **shell-split** so a command carrying flags
/// (`code --reuse-window`, `cursor -n`) resolves the program from the first
/// token and forwards the rest as arguments, rather than treating the whole
/// string as one impossible binary name. A command that splits to nothing
/// (malformed quoting) falls back to the OS folder-open so the action still does
/// something useful instead of silently failing. The split command is then
/// passed as positional args to `zsh -lc 'exec "$@"'`, which lets macOS GUI
/// launches use login-shell PATH setup (`.zprofile`, `/etc/paths`, and
/// `/etc/paths.d`) without interpolating the worktree path into shell source.
#[must_use]
pub fn editor_open_argv(editor_command: Option<&str>, path: &str) -> Vec<String> {
    let Some(cmd) = editor_command else {
        return vec!["open".to_owned(), path.to_owned()];
    };
    match shlex::split(cmd) {
        Some(mut tokens) if !tokens.is_empty() => {
            tokens.push(path.to_owned());
            tokens.splice(
                0..0,
                [
                    "/bin/zsh".to_owned(),
                    "-lc".to_owned(),
                    "exec \"$@\"".to_owned(),
                    "switchboard-editor".to_owned(),
                ],
            );
            tokens
        }
        _ => vec!["open".to_owned(), path.to_owned()],
    }
}

/// The macOS argv for opening a path in the user's terminal app
/// (`open -a <terminal_app> <path>`).
#[must_use]
pub fn terminal_open_argv(terminal_app: &str, path: &str) -> Vec<String> {
    vec![
        "open".to_owned(),
        "-a".to_owned(),
        terminal_app.to_owned(),
        path.to_owned(),
    ]
}

/// The macOS argv for revealing a path in Finder (`open -R <path>` selects the
/// item in its containing folder rather than opening it).
#[must_use]
pub fn reveal_in_finder_argv(path: &str) -> Vec<String> {
    vec!["open".to_owned(), "-R".to_owned(), path.to_owned()]
}

// --- Preferences (config.yaml) ----------------------------------------------

/// Return the current personal preferences (`config.yaml`).
#[must_use]
pub fn get_preferences_impl(state: &AppState) -> Preferences {
    lock(&state.preferences).clone()
}

/// Replace the personal preferences and persist them to `config.yaml`. The value
/// is `normalized` at this boundary (blank editor → `None`, blank terminal →
/// default) so consumers never see an empty command. Unlike the best-effort
/// registry persists, a save failure is surfaced (the user explicitly asked to
/// save) — but the in-memory value is updated regardless, so the running session
/// reflects the change even if the write fails. A `None` path (no resolvable
/// config location — tests/exotic host) updates memory only.
///
/// **The `preferences` guard is held across the file write** (the one place we
/// hold a state lock across I/O). `write_yaml` uses a fixed `<file>.tmp`, so two
/// unserialized saves would race on that temp file and could corrupt
/// `config.yaml`. Serializing here is safe and clearer than routing through
/// `registry_write`: `preferences` is a singleton touched only by get/set, the
/// write is a tiny YAML file on an explicit user action, and nothing
/// latency-sensitive waits behind it. See the lock-order note in `state.rs`.
pub fn set_preferences_impl(state: &AppState, prefs: &Preferences) -> Result<(), AppError> {
    let normalized = prefs.clone().normalized();
    let mut guard = lock(&state.preferences);
    guard.clone_from(&normalized);
    let Some(path) = state.preferences_path.as_ref() else {
        return Ok(());
    };
    preferences::save(path, &normalized)
}

/// All prompts across configured providers (user-global; no project argument).
/// Never hard-fails: an unreachable/misconfigured provider contributes nothing
/// rather than breaking the listing.
pub fn list_prompts_impl(state: &AppState) -> Vec<switchboard_prompts::Prompt> {
    state.prompts.list()
}

/// Render a prompt to its finished text. Provider-dispatched (local → `MiniJinja`,
/// MCP → `prompts/get`). Serves both preview and send with the identical args.
/// Async because the MCP path does network I/O.
pub async fn render_prompt_impl(
    state: &AppState,
    provider: &str,
    name: &str,
    args: &std::collections::BTreeMap<String, String>,
) -> Result<switchboard_prompts::RenderedPrompt, AppError> {
    Ok(state.prompts.render(provider, name, args).await?)
}

/// Configured MCP providers with their last-build status and whether a token is
/// stored — drives the Settings provider list.
pub fn list_mcp_providers_impl(state: &AppState) -> Vec<switchboard_prompts::McpProviderInfo> {
    state.prompts.list_mcp_providers()
}

/// Add a generic MCP provider (name + URL + optional bearer): writes its config
/// entry, stores the bearer in the keychain, and kicks off a background cache
/// rebuild so its prompts appear without blocking the command on a slow server.
pub fn add_mcp_provider_impl(
    state: &AppState,
    name: &str,
    url: &str,
    bearer: Option<&str>,
) -> Result<(), AppError> {
    state.prompts.add_mcp_provider(name, url, bearer)?;
    spawn_prompt_sync(state);
    Ok(())
}

/// Remove a generic MCP provider: deletes its config entry + keychain token and
/// rebuilds the cache in the background.
pub fn remove_mcp_provider_impl(state: &AppState, name: &str) -> Result<(), AppError> {
    state.prompts.remove_mcp_provider(name)?;
    spawn_prompt_sync(state);
    Ok(())
}

/// Probe a candidate provider before saving (connect + list); returns the prompt
/// count on success or an actionable error.
pub async fn test_mcp_connection_impl(
    state: &AppState,
    url: &str,
    bearer: Option<String>,
) -> Result<usize, AppError> {
    Ok(state.prompts.test_mcp_connection(url, bearer).await?)
}

/// Event emitted after a prompt-cache rebuild settles, so the frontend can
/// refresh provider status and restore a prompt-mode compose draft that needed
/// the cache warm. Every sync path — startup warm sync, the `sync_prompts`
/// command, and add/remove — emits it via [`sync_prompts_and_notify`]; binding
/// the emit to the operation is what keeps a draft from getting stuck unrestored
/// when the cache is warmed by a path other than add/remove.
pub const PROMPTS_SYNCED_EVENT: &str = "prompts:synced";

/// Rebuild the prompt cache, then emit [`PROMPTS_SYNCED_EVENT`]. The emit is
/// bound to the sync here so no caller can warm the cache without notifying — the
/// single chokepoint every sync path routes through.
pub async fn sync_prompts_and_notify(prompts: PromptService, emitter: Arc<dyn EventEmitter>) {
    prompts.sync().await;
    emitter.emit(PROMPTS_SYNCED_EVENT, serde_json::Value::Null);
}

/// Rebuild the prompt cache off the command thread. `PromptService` is cheaply
/// cloneable and shares its cache, so the spawned clone warms the same cache.
/// Emits [`PROMPTS_SYNCED_EVENT`] when the rebuild finishes so Settings can
/// refresh a just-added provider's status (the add/remove command returns before
/// the background build completes, so the first read shows `Unknown`).
fn spawn_prompt_sync(state: &AppState) {
    let prompts = state.prompts.clone();
    let emitter = Arc::clone(&state.emitter);
    tauri::async_runtime::spawn(sync_prompts_and_notify(prompts, emitter));
}

pub fn create_project_impl(
    state: &AppState,
    name: &str,
    directory_path: &str,
) -> Result<ProjectSummary, AppError> {
    // Serialize the uniqueness check + JSONL append against concurrent
    // `create_project` / `register_agent` / `init_directory` calls. Without
    // this, two concurrent IPC calls could both pass the canonical-name
    // uniqueness check (which reads disk) and then both append colliding
    // records (which write disk).
    let _write = lock(&state.registry_write);
    let canonical = canonicalize_boundary(directory_path);
    let directory = lock(&state.directories)
        .get(&canonical)
        .cloned()
        .ok_or(AppError::NoDirectory)?;
    let project = directory.create_project(name)?;
    let summary = ProjectSummary {
        id: project.id,
        name: project.config.name.clone(),
        created_at: project.config.created_at,
    };
    // Lock kept LOCAL until commit (same pattern as `open_project_impl`). A
    // fresh project id can't be contended, but keeping the handle local means
    // a failure before the final inserts can't strand it. A new project has
    // no agents, so there is nothing to cache.
    let lock_handle = acquire_project_lock(project.id, &project.root)?;
    lock(&state.project_locks).insert(project.id, lock_handle);
    lock(&state.projects).insert(project.id, project);

    // Refresh the workspace cache for this directory so the flat list reflects
    // the new project even before the next `list_projects` round-trip.
    if let Ok(summaries) = directory.list_projects() {
        lock(&state.workspace).refresh_cache(&canonical, summaries);
        persist_workspace(state);
    }
    Ok(summary)
}

/// Rename a project: validate + dual-write its identity (`config.yaml` +
/// `projects.jsonl`) in core, then sync in-memory state. Synchronous under
/// `registry_write` — rename never touches running agents, so no dispatcher
/// drain (contrast delete). Resolves the project's owning directory from the
/// loaded set; an unavailable (unloaded) directory can't be mutated, so this
/// surfaces `ProjectNotLoaded` (the frontend gates Rename on availability — this
/// is the defensive backstop). Format + per-directory uniqueness are
/// re-validated in core; the frontend pre-checks, but the backend stays
/// authoritative.
pub fn rename_project_impl(
    state: &AppState,
    project_id: ProjectId,
    new_name: &str,
) -> Result<ProjectListing, AppError> {
    let _write = lock(&state.registry_write);
    let directory = resolve_owning_directory(state, project_id)?;
    let summary = directory.rename_project(project_id, new_name)?;

    // Sync the in-memory `Project` (canonical name) if the project is loaded.
    if let Some(project) = lock(&state.projects).get_mut(&project_id) {
        summary.name.clone_into(&mut project.config.name);
    }

    // Refresh the workspace cache for this directory so the flat list reflects
    // the new name before the next `list_projects` round-trip.
    if let Ok(summaries) = directory.list_projects() {
        lock(&state.workspace).refresh_cache(&directory.path, summaries);
        persist_workspace(state);
    }

    let archived = lock(&state.workspace).is_archived(summary.id);
    Ok(ProjectListing {
        directory: directory.path.to_string_lossy().into_owned(),
        available: true,
        last_activity: directory.project_last_activity(summary.id, summary.created_at),
        id: summary.id,
        name: summary.name,
        created_at: summary.created_at,
        archived,
    })
}

/// Archive or unarchive a project — a user-global *view-state* flip stored in
/// `workspace.yaml`. Deliberately the lightest of the project ops: it takes only
/// the `workspace` lock, touches **no** on-disk project state, **no**
/// `registry_write`, **no** directory resolution, and **no** dispatcher — so it
/// works whether the project's directory is loaded, available, or offline, and
/// never interrupts a running agent. Validates the id is one the workspace knows
/// (present in some cached snapshot) so a bogus id can't accumulate in the set.
/// Returns `()`; the frontend flips the row locally and the next `list_projects`
/// confirms it from the persisted set.
pub fn set_project_archived_impl(
    state: &AppState,
    project_id: ProjectId,
    archived: bool,
) -> Result<(), AppError> {
    let changed = {
        let mut workspace = lock(&state.workspace);
        if !workspace.knows_project(project_id) {
            return Err(AppError::ProjectNotLoaded(project_id));
        }
        workspace.set_archived(project_id, archived)
    };
    if changed {
        persist_workspace(state);
    }
    Ok(())
}

/// Permanently delete one project's Switchboard state. Mirrors
/// `remove_directory_impl`'s two phases, scoped to a single project:
///
/// **(a)** With no locks held, drain every loaded agent in the project
/// (`shutdown_agent` cancels any in-flight turn and waits) so no orphaned
/// subprocess survives the removal.
///
/// **(b)** Under `registry_write`: delete the on-disk state via
/// `Directory::delete_project`, then drop the stale `instance.lock` handle and
/// prune in-memory state (the `Project`, its cached agents, its
/// `needs_session_meta` entries, and `active_project_id` if it pointed here)
/// and refresh the workspace cache.
///
/// **Error policy (engineer-approved).** "Already gone" is benign success:
/// - the project isn't resolvable in any loaded directory (`ProjectNotLoaded`
///   from `find_directory_for_project`) → nothing on disk we can reach;
/// - the directory's index file vanished out-of-band (`MissingAppendOnlyFile`)
///   → the entry is effectively gone.
///
/// In both cases we still prune in-memory state and return `Ok`. The directory
/// removal is best-effort after the index rewrite commits: a removal failure
/// leaves a benign, unlisted orphan. Only a genuine failure to update what lists
/// (such as an unreadable or corrupt index we must not rewrite) surfaces as
/// `Err` — and on that path we leave the in-memory maps intact so the row is
/// kept and a retry can succeed.
///
/// **Not atomic across phases.** Phase (a) is irreversible (it cancels in-flight
/// turns); a phase-(b) failure means work may have been cancelled even though
/// the project remains. Same accepted trade-off as `remove_agent_impl`.
pub async fn delete_project_impl(state: &AppState, project_id: ProjectId) -> Result<(), AppError> {
    // Resolve the owning directory up front. If no loaded directory claims the
    // id, there's nothing on disk we can reach — treat as already-gone and fall
    // through to in-memory pruning.
    let directory = match resolve_owning_directory(state, project_id) {
        Ok(dir) => Some(dir),
        Err(AppError::ProjectNotLoaded(_)) => None,
        Err(e) => return Err(e),
    };

    // Phase (a): drain this project's loaded agents (only loaded projects have
    // cached agents, so an unopened/unavailable project drains nothing). No lock
    // is held across the await.
    let agent_ids: Vec<AgentId> = lock(&state.agents_by_id)
        .values()
        .filter(|r| r.project_id == project_id)
        .map(|r| r.id)
        .collect();
    for &agent_id in &agent_ids {
        state
            .dispatcher
            .shutdown_agent(agent_id, CancelSource::Shutdown)
            .await;
    }

    // Phase (b): synchronous under `registry_write`, no `.await`.
    let _write = lock(&state.registry_write);

    // Delete on disk first, *before* dropping the project's inter-process lock.
    // `Directory::delete_project` only returns `Err` when it couldn't change
    // what lists (index read/rewrite failure) — i.e. nothing was removed; a
    // best-effort directory-removal failure is folded into `Ok` (benign orphan).
    // So on `Err` we keep both the row and the lock (project stays safely owned
    // and the delete is retryable). On unix `remove_dir_all` unlinks the in-dir
    // `instance.lock` despite our held handle, so holding the lock across the
    // removal is fine; a future Windows target would instead need
    // drop-before-removal + re-acquire-on-failure.
    if let Some(directory) = &directory {
        directory.delete_project(project_id)?;
    }

    // Committed (or nothing was on disk to reach) — drop the now-stale lock and
    // prune routable in-memory state, in the documented lock order.
    lock(&state.project_locks).remove(&project_id);
    lock(&state.projects).remove(&project_id);
    {
        let mut active = lock(&state.active_project_id);
        if *active == Some(project_id) {
            *active = None;
        }
    }
    {
        let mut needs = lock(&state.needs_session_meta);
        needs.retain(|id| !agent_ids.contains(id));
    }
    lock(&state.agents_by_id).retain(|_, r| r.project_id != project_id);
    // Scrub the archived flag so a future project re-created with this id (ids
    // are minted fresh, but be defensive) can't inherit a stale archived state.
    lock(&state.workspace).set_archived(project_id, false);

    // Keep the workspace cache from resurrecting the deleted project: refresh
    // from a fresh index read when available, else drop just the deleted id from
    // the cached snapshot (the index read can fail in the same out-of-band cases
    // the delete tolerated, and `list_projects_impl` serves the cache on those).
    if let Some(directory) = &directory {
        match directory.list_projects() {
            Ok(summaries) => {
                lock(&state.workspace).refresh_cache(&directory.path, summaries);
            }
            Err(_) => {
                lock(&state.workspace).remove_cached_project(&directory.path, project_id);
            }
        }
    } else {
        // No reachable directory (its folder/volume is gone): the on-disk index
        // can't be touched, so this prunes the project from the workspace
        // *listing* only. Dropping its cached snapshot in `workspace.yaml` stops
        // the row from resurrecting while the directory stays gone — which is the
        // bug being fixed. Accepted limit: if that directory is later
        // reconnected/re-added, its surviving on-disk index re-lists the project.
        // That's an unavoidable consequence of deleting an offline project (we
        // can't unlink files on an absent volume), not a leak this can close.
        lock(&state.workspace).remove_cached_project_by_id(project_id);
    }
    persist_workspace(state);
    Ok(())
}

pub fn open_project_impl(
    state: &AppState,
    project_id: ProjectId,
) -> Result<ProjectSummary, AppError> {
    // Fast path (lock-free): already loaded → intra-process re-open is a
    // no-op returning the existing handle, with no second lock acquisition
    // (`flock` is the inter-process guard only; a process re-locking its own
    // held file via a second fd would spuriously conflict). Keeping this
    // check lock-free means M4.6 project switches don't serialize against
    // creates/registers.
    if let Some(loaded) = lock(&state.projects).get(&project_id) {
        return Ok(ProjectSummary {
            id: loaded.id,
            name: loaded.config.name.clone(),
            created_at: loaded.config.created_at,
        });
    }
    // Serialize first-opens (against each other and against create/register)
    // so two concurrent opens of the same not-yet-loaded project don't both
    // try to flock it — the second would conflict with this process's own
    // first handle and spuriously report `ProjectLocked` for an idempotent
    // action. `registry_write` is the head of the lock order, so taking it
    // here is order-safe.
    let _open = lock(&state.registry_write);
    // Re-check under the guard: another thread may have loaded it while we
    // waited → return the existing handle without re-locking.
    if let Some(loaded) = lock(&state.projects).get(&project_id) {
        return Ok(ProjectSummary {
            id: loaded.id,
            name: loaded.config.name.clone(),
            created_at: loaded.config.created_at,
        });
    }
    let project = find_project_in_directories(state, project_id)?;
    let summary = ProjectSummary {
        id: project.id,
        name: project.config.name.clone(),
        created_at: project.config.created_at,
    };
    // Own the inter-process lock (fail fast on contention), but keep the
    // handle LOCAL until every fallible step has succeeded. If the registry
    // read below fails (e.g. `CorruptJsonl`), `lock_handle` drops here and the
    // flock is released — no wedged lock, and the error surfaces as the
    // corruption it is rather than a misleading `ProjectLocked` on retry.
    let lock_handle = acquire_project_lock(project.id, &project.root)?;
    let agents = project.list_agents()?;
    // All fallible work done — commit the shared maps together.
    lock(&state.project_locks).insert(project.id, lock_handle);
    {
        let mut cache = lock(&state.agents_by_id);
        for agent in agents {
            cache.insert(agent.id, agent);
        }
    }
    lock(&state.projects).insert(project.id, project);
    Ok(summary)
}

pub fn set_active_project_impl(state: &AppState, project_id: ProjectId) -> Result<(), AppError> {
    if !lock(&state.projects).contains_key(&project_id) {
        return Err(AppError::ProjectNotLoaded(project_id));
    }
    *lock(&state.active_project_id) = Some(project_id);
    Ok(())
}

pub fn create_agent_impl(
    state: &AppState,
    name: &str,
    harness: HarnessKind,
    model: Option<String>,
    effort: Option<String>,
) -> Result<AgentRecord, AppError> {
    let model = normalize_selection(model);
    let effort = normalize_selection(effort);
    check_selection_supported(harness, model.as_deref(), effort.as_deref())?;
    // Same TOCTOU protection as create_project_impl — register_agent has
    // an internal read-check-then-append window that two concurrent IPC
    // calls could race through.
    let _write = lock(&state.registry_write);
    let active = lock(&state.active_project_id).ok_or(AppError::NoActiveProject)?;
    let project = lock(&state.projects)
        .get(&active)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(active))?;
    let record = project.register_agent(name, harness, model, effort)?;
    lock(&state.agents_by_id).insert(record.id, record.clone());
    Ok(record)
}

/// Reject a model on a harness without model support, or an effort on a harness
/// without effort support — the capability invariant, checked at the command
/// boundary so the caller gets a clear error before any registry work. `core`
/// re-checks at its persistence boundary (defense in depth, [`Project`]'s
/// registration + `set_agent_*` gates); the attach path *requires* this check
/// because its per-harness `register_attached_*` fns can't even receive an
/// unsupported axis. Call **after** [`normalize_selection`] so a blank selection
/// (which means "unset," always allowed) doesn't trip the capability error.
fn check_selection_supported(
    harness: HarnessKind,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<(), AppError> {
    if model.is_some() && !harness.supports_model_selection() {
        return Err(CoreError::SelectionUnsupported {
            harness,
            axis: SelectionAxis::Model,
        }
        .into());
    }
    if effort.is_some() && !harness.supports_effort_selection() {
        return Err(CoreError::SelectionUnsupported {
            harness,
            axis: SelectionAxis::Effort,
        }
        .into());
    }
    Ok(())
}

/// Remove an agent: tear down its actor, then delete its registry record and
/// Switchboard sidecars. Resolves the agent's *own* project from its id (never
/// the active project), so it's free of the active-project coupling.
///
/// **Two phases by necessity.** `shutdown_agent` is async and the registry
/// mutation runs under the synchronous `registry_write` guard, which can't be
/// held across an `.await` (the guard isn't `Send`).
///
/// **Active agents are torn down, not rejected.** If a turn is in flight,
/// `shutdown_agent` cancels it (surfaced as a `Cancelled` terminal, same as
/// "Stop"), drains the harness subprocess, and drops the slot — the UI gates
/// Remove on the active state; the backend just makes teardown robust whatever
/// the state. This inherits `shutdown_agent`'s drain latency: a slow subprocess
/// wind-down blocks the command for that duration (same path as remove-directory
/// and quit).
///
/// **Not atomic across the two phases.** Phase (a) is irreversible (it cancels
/// any in-flight turn), but phase (b)'s registry write can fail (disk I/O). On
/// that path the turn was already cancelled yet the record remains — so a failed
/// remove can still have cancelled work; "remove errored" does not mean "nothing
/// happened." It self-heals functionally (the next send re-spawns the actor),
/// and registry-write failure is rare, so this is accepted rather than made
/// transactional across an async cancel + a sync write.
pub async fn remove_agent_impl(state: &AppState, agent_id: AgentId) -> Result<(), AppError> {
    // Resolve the agent's *own* project from its id (never the active project),
    // before the irreversible phase (a) so a non-existent agent fails fast —
    // nothing is torn down before we know the agent exists.
    let (project, _agent) = lookup_agent(state, agent_id)?;

    // Phase (a) — no lock held.
    state
        .dispatcher
        .shutdown_agent(agent_id, CancelSource::Shutdown)
        .await;

    // No lock spans (a)→(b), so a concurrent `send_message` to this id can
    // re-spawn an `Active` actor via `ensure_actor` between the teardown and the
    // registry removal — leaving an idle orphan actor after phase (b) deletes the
    // record. Tolerated: it's UI-gated (you can't send to an agent you're
    // removing), self-limiting (the orphan is idle), and reaped on restart.
    // Phase (b) — fully synchronous under `registry_write`, no `.await`.
    let _write = lock(&state.registry_write);
    project.remove_agent(agent_id)?;
    delete_agent_sidecars(&project, agent_id);
    lock(&state.agents_by_id).remove(&agent_id);
    Ok(())
}

/// Best-effort deletion of an agent's Switchboard sidecar. A missing file is
/// fine; a failed delete is logged and tolerated — the registry removal is the
/// authoritative effect. Harness-native session files (`~/.claude/…`,
/// `~/.codex/…`, …) are deliberately left untouched.
///
/// Only the per-agent metadata sidecar (`.meta.json`) remains: every harness
/// now carries its session locator on the registry record, so no harness writes
/// a session-link sidecar. (A pre-migration Codex/Antigravity agent removed
/// before the one-time migration runs may leave its legacy `.jsonl` orphaned —
/// harmless, a tiny stale file; whether it's reclaimed is up to the migration's
/// cleanup.)
fn delete_agent_sidecars(project: &Project, agent_id: AgentId) {
    let path = switchboard_harness::meta_sidecar::meta_sidecar_path(
        project.directory.as_path(),
        project.id,
        agent_id,
    );
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to delete agent sidecar");
        }
    }
}

/// Rename an agent's registry record (and its in-memory mirror). Synchronous
/// under `registry_write` — no actor interaction. Format and uniqueness are
/// re-validated in core; the frontend pre-checks, but the backend stays
/// authoritative, so a collision or invalid name surfaces as the matching error.
pub fn rename_agent_impl(
    state: &AppState,
    agent_id: AgentId,
    new_name: &str,
) -> Result<AgentRecord, AppError> {
    let _write = lock(&state.registry_write);
    let (project, _) = lookup_agent(state, agent_id)?;
    let updated = project.rename_agent(agent_id, new_name)?;
    lock(&state.agents_by_id).insert(agent_id, updated.clone());
    Ok(updated)
}

/// Persist a runtime-captured session locator onto an agent's registry record,
/// in place, and refresh the `agents_by_id` cache. Serialized against the other
/// registry mutations by `registry_write`, exactly like `rename_agent_impl`.
///
/// This is the app-side mechanism the runtime-capture sink calls when a
/// Codex/Antigravity adapter learns (or, on an Antigravity fork-and-heal,
/// re-learns) its locator. Returns the updated record. (The dispatch factory
/// freezes the agent record at construction today, so the refreshed cache is
/// not yet read mid-session; the factory's live-read of `agents_by_id` lands
/// alongside the capture sink, at which point the next turn sees the update.)
// Exercised by tests but not yet on a production call path — the M2 runtime-
// capture sink is its first caller.
#[allow(dead_code)]
pub fn set_agent_session_locator_impl(
    state: &AppState,
    agent_id: AgentId,
    locator: SessionLocator,
) -> Result<AgentRecord, AppError> {
    let _write = lock(&state.registry_write);
    let (project, _) = lookup_agent(state, agent_id)?;
    let updated = project.set_session_locator(agent_id, locator)?;
    lock(&state.agents_by_id).insert(agent_id, updated.clone());
    Ok(updated)
}

/// Change (or clear, with `None`) an agent's selected model, re-persisting the
/// registry and refreshing the cache. Mirrors `rename_agent_impl`. Empty/
/// whitespace normalizes to "unset"; the model-selection capability is enforced
/// by `Project::set_agent_model` (so an unsupported harness is rejected
/// regardless of caller). The new value applies on the agent's next dispatch —
/// no in-flight turn is touched.
pub fn set_agent_model_impl(
    state: &AppState,
    agent_id: AgentId,
    model: Option<String>,
) -> Result<AgentRecord, AppError> {
    let model = normalize_selection(model);
    let _write = lock(&state.registry_write);
    let (project, _) = lookup_agent(state, agent_id)?;
    let updated = project.set_agent_model(agent_id, model)?;
    lock(&state.agents_by_id).insert(agent_id, updated.clone());
    Ok(updated)
}

/// Change (or clear, with `None`) an agent's selected reasoning effort. The
/// effort-axis counterpart to [`set_agent_model_impl`].
pub fn set_agent_effort_impl(
    state: &AppState,
    agent_id: AgentId,
    effort: Option<String>,
) -> Result<AgentRecord, AppError> {
    let effort = normalize_selection(effort);
    let _write = lock(&state.registry_write);
    let (project, _) = lookup_agent(state, agent_id)?;
    let updated = project.set_agent_effort(agent_id, effort)?;
    lock(&state.agents_by_id).insert(agent_id, updated.clone());
    Ok(updated)
}

/// Attach an existing harness session (Claude Code, Codex, Gemini, or
/// Antigravity) as a new Switchboard agent in the active project.
///
/// Validation order:
/// 1. Normalize the selections and capability-check them — **before** taking
///    the lock, so an unsupported model/effort is refused without touching the
///    registry (the per-harness `register_attached_*` methods structurally
///    can't carry an axis they don't support, so this is the only place an
///    unsupported axis from the IPC is rejected rather than silently dropped).
/// 2. Take the directory-level `registry_write` mutex; the remaining steps run
///    under it so the cross-project session-id check + register form one atomic
///    step. Resolve the active project and its owning directory.
/// 3. `existing_session_id` parses as a UUID — Claude/Gemini/Antigravity only;
///    Codex's thread-id is an arbitrary string and is used verbatim.
/// 4. Per-harness session existence under `home_dir`. Claude / Gemini check a
///    session file; Codex's discovery also returns the parsed `YYYY-MM-DD`
///    partition date; Antigravity checks that the server-assigned conversation
///    directory `brain/<uuid>/` exists (the transcript inside may be absent —
///    hydration degrades gracefully).
/// 5. Session-id collision scan (loaded or not — the scan walks projects on
///    disk). Scope differs by harness: Claude and Gemini scan only the active
///    project's **own directory** (`enumerate_directory_projects`) because
///    their session ids are caller-controlled and cwd-namespaced, so a widened
///    scan would false-reject a legitimately-distinct same-id-different-cwd
///    session. Codex and Antigravity scan **all loaded directories**
///    (`enumerate_all_projects`) because their ids are server-assigned and
///    globally unique. Every harness scans `AgentRecord.session_locator` (the
///    locator — UUID, or Codex's thread-id — lives on the record; no sidecar).
///    Two `AgentRecord`s pointing at the same harness session is the
///    same-session-parallel-invocation hazard
///    (`docs/research/same-session-parallel-invocation.md`); unloaded projects
///    could still be opened and dispatched concurrently later, so loaded-only
///    scope would miss the collision.
/// 6. Register via the harness-specific `register_attached_*` method, which
///    writes the session locator (and any model/effort) straight onto the
///    registry record — no sidecar for any harness.
/// 7. (Codex only) Insert the new `agent_id` into `needs_session_meta` (after
///    registration) so every dispatch up to and including the one that observes
///    `SessionMeta` runs with `is_first_dispatch_after_attach: true` — forces
///    `SessionMeta` emission for the Codex sidebar. The per-dispatch emitter
///    decorator clears the flag once `session_meta` is genuinely observed on the
///    wire. Claude and Antigravity attaches do **not** populate this set: both
///    emit `SessionMeta` on every dispatch (Claude from `system/init`;
///    Antigravity constructs it post-terminal each turn), so the override has
///    nothing to do.
///
/// `home_dir` is passed in (not resolved here) so tests can stage a temp
/// directory without mutating process-wide `$HOME`. The Tauri command shim
/// reads `$HOME` and forwards.
pub fn attach_agent_impl(
    state: &AppState,
    name: &str,
    harness: HarnessKind,
    existing_session_id: &str,
    home_dir: &Path,
    model: Option<String>,
    effort: Option<String>,
) -> Result<AgentRecord, AppError> {
    let model = normalize_selection(model);
    let effort = normalize_selection(effort);
    // Capability check first — before any session-file lookup or registry
    // write. Load-bearing on this path (not just defense in depth): the
    // per-harness `register_attached_*` methods structurally omit the axes they
    // can't carry (Gemini takes no effort, Antigravity takes neither), so an
    // unsupported axis from the IPC must be rejected here or it would be
    // silently dropped rather than refused.
    check_selection_supported(harness, model.as_deref(), effort.as_deref())?;
    let _write = lock(&state.registry_write);
    let active = lock(&state.active_project_id).ok_or(AppError::NoActiveProject)?;
    let project = lock(&state.projects)
        .get(&active)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(active))?;
    // The active project's owning directory — the cwd that namespaces the
    // harnesses' session files.
    let directory = lock(&state.directories)
        .get(&project.directory)
        .cloned()
        .ok_or(AppError::NoDirectory)?;

    // Claude/Gemini/Antigravity identify a session by a UUID, parsed per-arm
    // below. Codex's thread-id is an arbitrary string (not guaranteed a UUID),
    // so its arm uses `existing_session_id` verbatim — parsing it here would
    // wrongly reject a valid non-UUID Codex session.
    let record = match harness {
        HarnessKind::ClaudeCode => {
            let session_uuid = parse_uuid(existing_session_id)?;
            let expected = switchboard_harness::claude_session_file_path(
                home_dir,
                &directory.path,
                &session_uuid,
            );
            if !expected.exists() {
                return Err(AppError::SessionFileNotFound {
                    harness,
                    expected_path: expected.to_string_lossy().into_owned(),
                });
            }
            check_claude_session_id_unique(state, &directory, &session_uuid)?;
            project.register_attached_claude_agent(name, session_uuid, model, effort)?
        }
        HarnessKind::Codex => {
            let (_path, session_partition_date) =
                switchboard_harness::find_codex_session_file_for_attach(
                    home_dir,
                    existing_session_id,
                )
                .map_err(map_codex_attach_lookup_error(harness, home_dir))?;
            check_codex_session_id_unique(state, existing_session_id)?;
            // The thread-id + partition-date are the session locator and are
            // written straight onto the registry record — no sidecar, no
            // pre-generated-id ordering.
            let record = project.register_attached_codex_agent(
                name,
                existing_session_id.to_owned(),
                session_partition_date,
                model,
                effort,
            )?;
            // Codex-only: force SessionMeta on subsequent dispatches until one
            // is genuinely observed. Claude/Gemini/Antigravity emit SessionMeta
            // every dispatch — see step 7 docstring.
            lock(&state.needs_session_meta).insert(record.id);
            record
        }
        HarnessKind::Gemini => {
            let session_uuid = parse_uuid(existing_session_id)?;
            let candidate = locate_gemini_candidate(home_dir, &directory.path, session_uuid)?;
            tracing::debug!(
                session_id = %session_uuid,
                path = %candidate.display(),
                "gemini attach: bound to candidate"
            );
            check_gemini_session_id_unique(state, &directory, &session_uuid)?;
            // Effort is already guaranteed `None` by `check_selection_supported`
            // (Gemini lacks effort support); only `model` flows through.
            project.register_attached_gemini_agent(name, session_uuid, model)?
        }
        HarnessKind::Antigravity => {
            let session_uuid = parse_uuid(existing_session_id)?;
            // Claude-shaped attach: a conversation UUID maps to exactly one
            // path (`brain/<uuid>/`), so validate that directory exists inline
            // — no Codex/Gemini-style ambiguity locator (there is nothing to
            // disambiguate). The brain dir, not the deeper
            // `.system_generated/.../transcript.jsonl`, is the existence
            // marker: a conversation present only as the encrypted `.pb` store
            // (or whose transcript artifact was pruned) still attaches and
            // hydrates empty, matching the loader's missing-transcript path.
            let brain_dir = switchboard_harness::antigravity::paths::conversation_brain_dir(
                home_dir,
                session_uuid,
            );
            if !brain_dir.is_dir() {
                return Err(AppError::SessionFileNotFound {
                    harness,
                    expected_path: brain_dir.to_string_lossy().into_owned(),
                });
            }
            check_antigravity_session_id_unique(state, session_uuid)?;
            // Claude/Gemini-shaped: the conversation UUID is the session
            // locator and is written straight onto the registry record — no
            // sidecar, no pre-generated-id ordering.
            project.register_attached_antigravity_agent(name, session_uuid)?
        }
        _ => return Err(AppError::UnsupportedHarness),
    };

    // Register-cache (M4.1): the new attached agent's project is `active`,
    // which is loaded (resolved above), so a subsequent `lookup_agent` hits
    // the cache without a disk scan.
    lock(&state.agents_by_id).insert(record.id, record.clone());
    Ok(record)
}

fn map_codex_attach_lookup_error(
    harness: HarnessKind,
    home_dir: &Path,
) -> impl FnOnce(switchboard_harness::AttachLookupError) -> AppError + '_ {
    move |err| match err {
        switchboard_harness::AttachLookupError::NotFound { session_id } => {
            let expected = home_dir
                .join(".codex")
                .join("sessions")
                .join("*/*/*")
                .join(format!("rollout-*-{session_id}.jsonl"));
            AppError::SessionFileNotFound {
                harness,
                expected_path: expected.to_string_lossy().into_owned(),
            }
        }
        switchboard_harness::AttachLookupError::Ambiguous { session_id, paths } => {
            AppError::AmbiguousSessionFile {
                harness: HarnessKind::Codex,
                session_id,
                paths,
            }
        }
        // `AttachLookupError` is `#[non_exhaustive]` across crate boundaries.
        // A future variant we don't recognize lands here with a non-misleading
        // message — not `SessionFileNotFound` (would mislead the user into
        // looking for a missing file) and not `UnsupportedHarness` (would
        // mis-route the cause). Logged so we notice the addition.
        other => {
            tracing::error!(error = ?other, "unhandled AttachLookupError variant — surfacing as AttachLookupFailed");
            AppError::AttachLookupFailed {
                message: other.to_string(),
            }
        }
    }
}

/// Enumerate every project on disk across **all loaded directories**,
/// preferring the in-memory `state.projects` entry for already-loaded projects
/// (avoids a redundant disk read of the same `config.yaml`). Unloaded projects
/// are constructed via `directory.open_project(id)`, a pure read that does
/// **not** mutate `state.projects` or register any listeners.
///
/// Used by the Codex / Antigravity attach-flow collision scans, whose session
/// ids are server-assigned and globally unique — a collision across *any* two
/// loaded directories is a genuine same-session-parallel-invocation hazard, so
/// the scan must span every directory the app holds.
fn enumerate_all_projects(state: &AppState) -> Result<Vec<Project>, AppError> {
    let directories: Vec<Directory> = lock(&state.directories).values().cloned().collect();
    // Snapshot the loaded-project map under the lock, then release it before any
    // disk I/O (`list_projects` / `open_project`). Holding `state.projects`
    // across filesystem reads — now amplified across every directory and taken
    // under `registry_write` — would serialize unrelated work behind disk
    // latency. Same no-lock-across-I/O discipline as `persist_workspace`.
    let loaded: HashMap<ProjectId, Project> = lock(&state.projects).clone();
    let mut all: Vec<Project> = Vec::new();
    for directory in directories {
        for summary in directory.list_projects()? {
            if let Some(p) = loaded.get(&summary.id) {
                all.push(p.clone());
            } else {
                all.push(directory.open_project(summary.id)?);
            }
        }
    }
    Ok(all)
}

/// Enumerate every project on disk under a **single** directory, preferring
/// the in-memory `state.projects` entry for already-loaded projects.
///
/// Used by the Claude / Gemini attach-flow collision scans. Those harnesses'
/// session ids are caller-supplied and namespaced by cwd (the directory), so a
/// scan must stay **per-directory** — widening it across directories would
/// false-reject a legitimately-distinct same-id-different-cwd session.
fn enumerate_directory_projects(
    state: &AppState,
    directory: &Directory,
) -> Result<Vec<Project>, AppError> {
    // Snapshot the loaded-project map under the lock, then release it before the
    // disk reads below (see `enumerate_all_projects` for the rationale).
    let loaded: HashMap<ProjectId, Project> = lock(&state.projects).clone();
    let mut all: Vec<Project> = Vec::new();
    for summary in directory.list_projects()? {
        if let Some(p) = loaded.get(&summary.id) {
            all.push(p.clone());
        } else {
            all.push(directory.open_project(summary.id)?);
        }
    }
    Ok(all)
}

/// The UUID an agent's session locator carries, if it's the `Uuid` variant
/// (Claude/Gemini/Antigravity). `None` for a Codex locator (which has no single
/// UUID) or an unset locator. Thin `agent`-level adapter over
/// [`SessionLocator::as_uuid`]; used by the Claude/Gemini collision scans,
/// hydration, and session-info, which compare against a session UUID.
fn locator_uuid(agent: &AgentRecord) -> Option<Uuid> {
    agent
        .session_locator
        .as_ref()
        .and_then(SessionLocator::as_uuid)
}

/// Per-directory Claude session-id collision check. Walks every project on
/// disk in the **attach's target directory** — not just `state.projects` —
/// because an unloaded project's `AgentRecord` could still be opened later and
/// dispatched concurrently, which is the same-session-parallel-invocation
/// hazard the invariant is defending against. Scoped per-directory because
/// Claude session ids are cwd-namespaced (the same id under a different cwd is
/// a distinct session). Held under `registry_write` so it's atomic with the
/// subsequent register.
fn check_claude_session_id_unique(
    state: &AppState,
    directory: &Directory,
    candidate: &Uuid,
) -> Result<(), AppError> {
    for project in enumerate_directory_projects(state, directory)? {
        for agent in project.list_agents()? {
            if locator_uuid(&agent) == Some(*candidate) {
                return Err(AppError::SessionAlreadyAttached {
                    existing_agent_id: agent.id,
                    existing_agent_name: agent.name,
                    existing_project_id: project.id,
                    existing_project_name: project.config.name.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Per-directory Gemini session-id collision check. Gemini agents carry
/// `AgentRecord.session_locator = Some(SessionLocator::Uuid(uuid))` (Claude shape). Walks every project on
/// disk in the **attach's target directory** and rejects if any agent already
/// attached to the same UUID. Scoped per-directory for the same cwd-namespacing
/// reason as Claude.
fn check_gemini_session_id_unique(
    state: &AppState,
    directory: &Directory,
    candidate: &Uuid,
) -> Result<(), AppError> {
    for project in enumerate_directory_projects(state, directory)? {
        for agent in project.list_agents()? {
            if agent.harness != HarnessKind::Gemini {
                continue;
            }
            if locator_uuid(&agent) == Some(*candidate) {
                return Err(AppError::SessionAlreadyAttached {
                    existing_agent_id: agent.id,
                    existing_agent_name: agent.name,
                    existing_project_id: project.id,
                    existing_project_name: project.config.name.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Locate the Gemini session file for `session_id` in the cwd's
/// `~/.gemini/tmp/<project-name>/chats/` directory. Wraps the shared
/// header-scan classifier so attach uses the same disambiguation rule as
/// transcript hydration — divergence between attach and hydrate is the
/// exact bug class this helper exists to prevent.
///
/// Outcomes:
/// - One candidate classifies as `Unambiguous` against the target → return
///   its path.
/// - Any candidate is `Ambiguous` (single file, multiple distinct session
///   headers) → `AmbiguousSessionFile` with the candidate path. Under UUID
///   v4 this is ~1/2^32, but `tracing::warn!` keeps the case forensically
///   visible if it ever fires in production.
/// - Candidate read fails (permissions, EIO, race-removed) →
///   `AttachLookupFailed` carrying the path + source. Failing loud
///   matches hydration's behavior; swallowing the error would silently
///   collapse "session file unreadable" into "session does not exist"
///   and send the user chasing UUID red herrings instead of `chmod`.
/// - No candidate matches → `SessionFileNotFound`.
fn locate_gemini_candidate(
    home_dir: &Path,
    cwd: &Path,
    session_id: Uuid,
) -> Result<PathBuf, AppError> {
    let candidates =
        switchboard_harness::gemini_session_file_candidates(home_dir, cwd, &session_id);
    let mut chosen: Option<PathBuf> = None;
    for path in &candidates {
        let content =
            std::fs::read_to_string(path).map_err(|err| AppError::AttachLookupFailed {
                message: format!(
                    "failed to read Gemini session candidate {}: {err}",
                    path.display()
                ),
            })?;
        match switchboard_harness::classify_gemini_candidate(&content, session_id) {
            switchboard_harness::GeminiCandidateMatch::Unambiguous => {
                chosen = Some(path.clone());
                break;
            }
            switchboard_harness::GeminiCandidateMatch::Ambiguous => {
                tracing::warn!(
                    session_id = %session_id,
                    path = %path.display(),
                    "gemini attach: candidate file contains multiple session headers; rejecting as ambiguous"
                );
                return Err(AppError::AmbiguousSessionFile {
                    harness: HarnessKind::Gemini,
                    session_id: session_id.to_string(),
                    paths: vec![path.clone()],
                });
            }
            // `NoTarget` plus any future additive variant of the
            // `#[non_exhaustive]` enum we don't yet recognize: doesn't
            // match this target, continue to the next candidate.
            // Conservative default — safer to fall through to
            // `SessionFileNotFound` than to bind an unknown classifier
            // outcome to the user's UUID.
            _ => {}
        }
    }
    chosen.ok_or_else(|| {
        let expected = home_dir
            .join(".gemini")
            .join("tmp")
            .join("<project>")
            .join("chats")
            .join(format!(
                "session-*-{}.jsonl",
                switchboard_harness::gemini_session_id_prefix(&session_id)
            ));
        AppError::SessionFileNotFound {
            harness: HarnessKind::Gemini,
            expected_path: expected.to_string_lossy().into_owned(),
        }
    })
}

/// Cross-directory Codex session-id collision check. The `thread_id` now lives
/// on the `AgentRecord` (`session_locator` → `Codex`), so the scan reads the
/// record. Codex thread-ids are server-assigned and globally unique, so the
/// scan spans **all loaded directories** (`enumerate_all_projects`).
///
/// **Accepted migration-window gap:** a Codex agent created before the locator
/// moved onto the record still carries its thread-id in an unmigrated
/// `<agent-id>.jsonl` sidecar (`session_locator: None`), so this scan can't see
/// it and a duplicate attach would slip through until the one-time migration
/// folds the sidecar into the record — the same dev-only window as the legacy
/// `session_id` and Antigravity sidecar cases.
fn check_codex_session_id_unique(state: &AppState, candidate: &str) -> Result<(), AppError> {
    for project in enumerate_all_projects(state)? {
        for agent in project.list_agents()? {
            if agent.harness != HarnessKind::Codex {
                continue;
            }
            if agent
                .session_locator
                .as_ref()
                .and_then(SessionLocator::as_codex)
                .is_some_and(|(thread_id, _)| thread_id == candidate)
            {
                return Err(AppError::SessionAlreadyAttached {
                    existing_agent_id: agent.id,
                    existing_agent_name: agent.name,
                    existing_project_id: project.id,
                    existing_project_name: project.config.name.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Reject attaching a conversation UUID already bound to another Antigravity
/// agent across **all loaded directories**. The conversation id now lives on the
/// `AgentRecord` (`session_locator`), so the scan reads the record — the same
/// source Claude/Gemini use. Cross-directory because Antigravity conversation
/// ids are server-assigned and globally unique: two agents resuming one
/// `--conversation <uuid>` would interleave server-side
/// (same-session-parallel-invocation).
///
/// **Accepted migration-window gap:** an Antigravity agent created before the
/// locator moved onto the record still carries its conversation id in an
/// unmigrated `.antigravity.jsonl` sidecar (`session_locator: None`), so this
/// scan can't see it and a duplicate attach of that conversation would slip
/// through until the one-time migration folds the sidecar into the record. The
/// migration handles Antigravity sidecars explicitly; the exposure is the
/// dev-only window before it runs (same accepted window as legacy `session_id`
/// records).
fn check_antigravity_session_id_unique(state: &AppState, candidate: Uuid) -> Result<(), AppError> {
    for project in enumerate_all_projects(state)? {
        for agent in project.list_agents()? {
            if agent.harness != HarnessKind::Antigravity {
                continue;
            }
            if locator_uuid(&agent) == Some(candidate) {
                return Err(AppError::SessionAlreadyAttached {
                    existing_agent_id: agent.id,
                    existing_agent_name: agent.name,
                    existing_project_id: project.id,
                    existing_project_name: project.config.name.clone(),
                });
            }
        }
    }
    Ok(())
}

pub fn list_agents_impl(
    state: &AppState,
    project_id: Option<ProjectId>,
) -> Result<Vec<AgentRecord>, AppError> {
    let pid = match project_id {
        Some(p) => p,
        None => lock(&state.active_project_id).ok_or(AppError::NoActiveProject)?,
    };
    let project = lock(&state.projects)
        .get(&pid)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(pid))?;
    let agents = project.list_agents()?;
    // Keep the register-cache (M4.1) in sync with what's on disk for this
    // project (insert-only — v1 has no agent deletion).
    {
        let mut cache = lock(&state.agents_by_id);
        for agent in &agents {
            cache.insert(agent.id, agent.clone());
        }
    }
    Ok(agents)
}

pub fn search_project_files_root_impl(
    state: &AppState,
    project_id: ProjectId,
) -> Result<PathBuf, AppError> {
    let project = lock(&state.projects)
        .get(&project_id)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(project_id))?;
    Ok(project.directory)
}

/// Search user-visible project files under `root`, honoring ignore rules while
/// keeping hidden files eligible for explicit mentions.
pub fn search_project_files_in_root(
    root: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<String>, AppError> {
    let query = query.to_lowercase();
    let limit = limit.clamp(1, 100);
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .require_git(false)
        .sort_by_file_path(std::cmp::Ord::cmp);

    let mut matches = Vec::new();
    for entry in builder
        .filter_entry(|entry| {
            entry.file_name() != std::ffi::OsStr::new(".git")
                && entry.file_name() != std::ffi::OsStr::new(".switchboard")
        })
        .build()
    {
        let entry = entry.map_err(|source| AppError::ProjectFileSearch {
            root: root.to_path_buf(),
            source,
        })?;
        if matches.len() >= limit {
            break;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let path = relative.to_string_lossy().replace('\\', "/");
        if query.is_empty() || path.to_lowercase().contains(&query) {
            matches.push(path);
        }
    }
    Ok(matches)
}

/// Resolves the agent (across all loaded projects) and accepts the send into
/// the dispatcher, returning the minted `MessageId` immediately. The turn's
/// `turn_id` and lifecycle flow over the per-agent event channel (the
/// correlated `TurnStart` carries this `message_id`); a failure before the turn
/// starts surfaces as a `MessageFailed` event. The `Result` carries only
/// **routing** failures (unknown agent, unsupported harness), resolved here
/// before the dispatcher is touched.
/// The result of staging one dropped file: where it now lives and the original
/// basename for display. The frontend assigns the `label`/`kind` (it owns the
/// extension→kind mapping per the M1 contract) and builds the full
/// [`Attachment`] from these two values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StagedAttachment {
    pub path: String,
    pub original_name: String,
}

/// Strip a dropped file's basename down to a safe filename component: no path
/// separators or control characters, and never the relative `.`/`..` names.
/// Falls back to `file` for an empty/degenerate name. Collision-safety is the
/// caller's `<uuid>__` prefix; this only keeps a crafted name from escaping the
/// attachments dir.
fn sanitize_basename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || std::path::is_separator(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "file".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Pure, synchronous file-staging I/O: copy `source_path` into `attachments_dir`
/// as `<uuid>__<sanitized-basename>`, returning the staged **absolute** path (the
/// dir is canonical, so the join is absolute). Self-contained so it runs on the
/// blocking pool and is unit-testable directly; the `<uuid>__` prefix makes
/// concurrent stages of the same filename collision-safe.
fn stage_attachment_io(
    attachments_dir: &Path,
    source_path: &Path,
) -> Result<StagedAttachment, AppError> {
    let original_name = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .map_or_else(|| "file".to_owned(), str::to_owned);
    let stage_err = |source: std::io::Error| AppError::AttachmentStage {
        source_path: source_path.to_string_lossy().into_owned(),
        source,
    };
    std::fs::create_dir_all(attachments_dir).map_err(stage_err)?;
    let dest = attachments_dir.join(format!(
        "{}__{}",
        Uuid::now_v7(),
        sanitize_basename(&original_name)
    ));
    std::fs::copy(source_path, &dest).map_err(stage_err)?;
    Ok(StagedAttachment {
        path: dest.to_string_lossy().into_owned(),
        original_name,
    })
}

/// Copy a dropped file into the project's `attachments/` dir and return its
/// staged absolute path. The copy runs in Rust (no frontend fs-plugin
/// permission) **on the blocking pool**: a user can drop an arbitrarily large
/// file (the feature has no size cap), so the copy must not sit on the async
/// command thread and stall unrelated IPC / event handling. Resolving the
/// project is a cheap lock lookup, kept on the async side; only the file copy is
/// offloaded — matching how `load_project_conversation_impl` offloads transcript
/// parsing. Classification/labeling is the frontend's job (M1 contract).
pub async fn stage_attachment_impl(
    state: &AppState,
    project_id: ProjectId,
    source_path: &Path,
) -> Result<StagedAttachment, AppError> {
    let project = match lock(&state.projects).get(&project_id).cloned() {
        Some(loaded) => loaded,
        None => find_project_in_directories(state, project_id)?,
    };
    let attachments_dir = project.attachments_dir();
    let source_path = source_path.to_path_buf();
    let source_display = source_path.to_string_lossy().into_owned();
    tokio::task::spawn_blocking(move || stage_attachment_io(&attachments_dir, &source_path))
        .await
        .map_err(|join_err| AppError::AttachmentStage {
            source_path: source_display,
            source: std::io::Error::other(join_err.to_string()),
        })?
}

pub async fn send_message_impl(
    state: &AppState,
    agent_id: AgentId,
    prompt: &str,
    attachments: Vec<Attachment>,
    send_id: SendId,
) -> Result<MessageId, AppError> {
    let (project, agent) = lookup_agent(state, agent_id)?;
    // Claude is spawned with cwd = the user's bound working directory (the
    // folder they opened), NOT the per-project metadata directory inside
    // `.switchboard/projects/<uuid>/`. The working directory is what
    // contains the user's actual code that claude needs to see via its
    // Read/Glob/Bash tools — the metadata directory is just where
    // Switchboard stores its own state. Multiple projects in the same
    // working directory share the same cwd; their per-agent sessions are
    // distinguished by session UUID, which is unique per agent.
    // Per-harness routing: select the adapter by agent.harness. The
    // dispatcher is harness-agnostic (keyed by AgentId); the match here is
    // the substantive failure surface — a regression that routes Codex
    // through the Claude adapter would silently spawn the wrong binary.
    // App routing test in the test module below pins this against
    // regression.
    let adapter: Arc<dyn HarnessAdapter> = match agent.harness {
        HarnessKind::ClaudeCode => Arc::clone(&state.claude_adapter),
        HarnessKind::Codex => Arc::clone(&state.codex_adapter),
        HarnessKind::Gemini => Arc::clone(&state.gemini_adapter),
        HarnessKind::Antigravity => Arc::clone(&state.antigravity_adapter),
        _ => return Err(AppError::UnsupportedHarness),
    };
    // The actor (created on first send) owns this builder and calls it per turn
    // — so `is_first_dispatch_after_attach` and the agent's current
    // `session_locator` are read live, never frozen at enqueue. See
    // `crate::dispatch_context`, `AppState::needs_session_meta`, and
    // `AppState::agents_by_id`.
    let factory: Arc<dyn DispatchContextFactory> = Arc::new(ProjectDispatchContextFactory::new(
        project,
        agent,
        adapter,
        Arc::clone(&state.emitter),
        Arc::clone(&state.needs_session_meta),
        Arc::clone(&state.agents_by_id),
        Arc::clone(&state.registry_write),
    ));
    // `send_id` is minted by the frontend and shared across a fan-out's
    // recipients (one `send_message` call per recipient with the same id), so
    // hydration groups the user's message once. A single-recipient send is a
    // trivially-grouped 1-element fan-out with its own id.
    match state
        .dispatcher
        .send_message(
            agent_id,
            prompt,
            attachments,
            send_id,
            factory,
            OnBusy::Enqueue,
        )
        .await
    {
        SendOutcome::Accepted(message_id) => Ok(message_id),
        // Unreachable on the Enqueue path; FailFast (workflow §7) is not used
        // here yet. Map defensively so a future caller can't silently misread.
        SendOutcome::Busy => Err(AppError::AgentBusy),
    }
}

/// Remove a not-yet-dispatched queued message by id, returning its payload so
/// the compose bar can restore the text. Race-safe: `NotQueued` (already
/// dequeued/started or never existed) maps to [`AppError::QueuedMessageNotFound`].
pub async fn remove_queued_message_impl(
    state: &AppState,
    agent_id: AgentId,
    message_id: MessageId,
) -> Result<RemovedQueuedMessage, AppError> {
    state
        .dispatcher
        .remove_queued_message(agent_id, message_id)
        .await
        .map_err(|_| AppError::QueuedMessageNotFound(message_id))
}

/// Cancel an agent's in-flight turn (user-initiated stop). Idempotent: a
/// clean no-op (`NothingToCancel`) when the agent has no cancellable in-flight
/// turn — idle, unknown, or already past its terminal event (e.g. during
/// Codex's post-terminal enrichment window). The adapter performs the
/// harness-specific kill; the dispatcher synthesizes the `Cancelled` terminal.
pub fn cancel_turn_impl(state: &AppState, agent_id: AgentId) -> CancelOutcome {
    state.dispatcher.cancel(agent_id, CancelSource::User)
}

/// Cancel a whole send across its `recipients` (system-design §7 "Cancel a
/// send"). Send-scoped and actor-decided: each recipient cancels its in-flight
/// turn iff that turn belongs to `send_id` and drops any still-queued item of
/// the send, never touching a later, unrelated turn. Fire-and-forget — the
/// per-turn `Cancelled` terminals + return-to-idle flow back over the per-agent
/// event channels, so this just delegates and acks.
pub fn cancel_send_impl(state: &AppState, send_id: SendId, recipients: &[AgentId]) {
    state
        .dispatcher
        .cancel_send(send_id, recipients, CancelSource::User);
}

/// Stop an agent entirely (sidebar "Stop agent"): cancel its in-flight turn and
/// drop its whole queued backlog, leaving the agent loaded and idle. Idempotent
/// — `NothingToCancel` when the agent has no actor. The running turn's
/// `Cancelled` terminal + return-to-idle flow back over the event channel; the
/// dropped queued items are discarded silently (never journaled), so the
/// frontend's optimistic `stopAgent` cleanup is what resolves their cards.
pub fn cancel_agent_impl(state: &AppState, agent_id: AgentId) -> CancelOutcome {
    state.dispatcher.cancel_agent(agent_id, CancelSource::User)
}

/// Cancel every in-flight turn for `agents`, await their drains, then release
/// `project_ids`' instance locks — strictly in that order, so a project lock
/// is never released while one of its agents' turns is still live (which would
/// reopen the double-drive race the lock guards). The reusable
/// cancel-and-drain lifecycle primitive: standalone and unit-tested in M4.2;
/// M4.6 wires it to remove-directory (passing one directory's agents +
/// project), and the app-quit handler is deferred to M8.
// Exercised by tests but not yet on a production call path — the
// remove-working-directory lifecycle consumes it once that command exists.
#[allow(dead_code)]
pub async fn drain_agents_then_release_locks(
    state: &AppState,
    agents: &[AgentId],
    project_ids: &[ProjectId],
    source: CancelSource,
) {
    // `shutdown_agent` is atomic per agent: it abandons the backlog, cancels any
    // running turn, drains it, and only then returns — so no *fresh* turn starts
    // mid-teardown (the orphan-subprocess problem M4.3 fixed) and the lock is
    // never released while a turn is still driving the harness session.
    for &agent_id in agents {
        state.dispatcher.shutdown_agent(agent_id, source).await;
    }
    let mut locks = lock(&state.project_locks);
    for project_id in project_ids {
        locks.remove(project_id);
    }
}

/// Reload an agent's prior conversation history from its harness session
/// file. Per-harness parsers project the on-disk records into a normalized
/// `LoadedTranscript`. The frontend feeds the result through the reducer's
/// `hydrate` input to populate the unified-view transcript.
///
/// **Error scope.** `Err(AppError::LoadTranscript)` is reserved for
/// lookup-mechanism failures (I/O on a file that exists). Per-line parse
/// damage degrades silently to `LoadedTranscript.warnings`; missing files
/// degrade to `LoadedTranscript::default()`. Stale Codex sidecars (file at
/// recorded path no longer exists) surface as a warning inside an
/// otherwise-empty `Ok` result.
///
/// `home_dir` is passed in (not resolved here) so tests can stage a temp
/// directory without mutating process-wide `$HOME`. The Tauri command shim
/// reads `$HOME` and forwards.
pub fn load_transcript_impl(
    state: &AppState,
    agent_id: AgentId,
    home_dir: &Path,
) -> Result<switchboard_harness::LoadedTranscript, AppError> {
    let (project, agent) = lookup_agent(state, agent_id)?;
    load_agent_transcript(&project, &agent, home_dir)
}

/// Load one agent's prior conversation from its harness session file. The
/// harness-dispatch body factored out of [`load_transcript_impl`] so the
/// project-level conversation loader can reuse it per agent. Error scope:
/// lookup-I/O surfaces; per-harness defaults degrade to an empty transcript.
/// Session identity is read from the agent's registry record (`session_locator`),
/// so there's no per-agent session-link sidecar to corrupt.
fn load_agent_transcript(
    project: &Project,
    agent: &AgentRecord,
    home_dir: &Path,
) -> Result<switchboard_harness::LoadedTranscript, AppError> {
    let mut transcript = load_agent_transcript_raw(project, agent, home_dir)?;
    // Overlay the per-agent metadata sidecar (stream-only / class-C metadata
    // that the harness session file doesn't carry, so it would otherwise be
    // lost on restart). Done here — the one chokepoint both hydration paths
    // funnel through — rather than in the per-harness loaders, which have no
    // `project_id` and don't know the sidecar layout. Best-effort: a
    // missing/corrupt sidecar reads as absent and the overlay is a no-op.
    let sidecar_path = switchboard_harness::meta_sidecar::meta_sidecar_path(
        &project.directory,
        project.id,
        agent.id,
    );
    apply_meta_sidecar_overlay(
        &mut transcript,
        switchboard_harness::meta_sidecar::read(&sidecar_path),
    );
    // Re-attach persisted per-turn cost + overage by joining the turn-metadata
    // sidecar's records onto the hydrated turns by `stable_message_id`. Same
    // best-effort posture: a missing/corrupt log reads as empty and the join is
    // a no-op (turns render no cost/overage — the no-backfill case).
    let turnmeta_path = switchboard_harness::turnmeta_sidecar::turnmeta_sidecar_path(
        &project.directory,
        project.id,
        agent.id,
    );
    apply_turnmeta_overlay(
        &mut transcript,
        &switchboard_harness::turnmeta_sidecar::read(&turnmeta_path),
    );
    Ok(transcript)
}

/// Overlay persisted per-turn cost + overage onto a freshly-loaded transcript
/// by joining on `stable_message_id`.
///
/// Each [`TurnMetaRecord`] is keyed on the turn's final non-subagent
/// assistant-message id — the same value the Claude session-file parser stamps
/// onto `Turn::Agent.stable_message_id`. For every agent turn carrying that id,
/// we fill (fill-if-empty) the turn's `spend` from the record and its
/// `usage.total_cost_usd` from the persisted cost, so the inline cost +
/// "using credits" marker re-renders exactly as it did live.
///
/// Records are indexed by `message_id`; the last record wins for a repeated key
/// (a turn re-run after resume appends a fresh record — the newest is correct).
/// A turn with no matching record keeps its loaded values (none, for a
/// pre-feature or non-Claude turn). Best-effort: an empty record set is a no-op.
fn apply_turnmeta_overlay(
    transcript: &mut switchboard_harness::LoadedTranscript,
    records: &[switchboard_harness::turnmeta_sidecar::TurnMetaRecord],
) {
    if records.is_empty() {
        return;
    }
    let by_message_id: std::collections::HashMap<&str, &_> =
        records.iter().map(|r| (r.message_id.as_str(), r)).collect();
    for turn in &mut transcript.turns {
        if let switchboard_harness::Turn::Agent {
            stable_message_id: Some(message_id),
            spend,
            usage,
            ..
        } = turn
            && let Some(record) = by_message_id.get(message_id.as_str())
        {
            if spend.is_none() {
                *spend = Some(record.spend.clone());
            }
            // `spend` (the overage marker) restores unconditionally, but the
            // dollar figure lives *inside* `usage` — so cost reattaches only
            // when the turn carries `usage`. For Claude this is always true
            // (completed turns carry usage), so a marker-without-cost turn is
            // currently unreachable; we don't synthesize a `usage` shell to
            // hold an orphaned cost (that struct also drives the context bar).
            if let (Some(usage), Some(cost)) = (usage.as_mut(), record.total_cost_usd)
                && usage.total_cost_usd.is_none()
            {
                usage.total_cost_usd = Some(cost);
            }
        }
    }
}

/// Overlay a metadata sidecar's snapshots onto a freshly-loaded transcript.
///
/// Two independent stream-only fields are restored, each fill-if-empty:
///
/// - **Rate limit** (transcript-level): fills `last_rate_limit` (+ its
///   `last_rate_limit_as_of` capture time) *only* when the loader left it
///   unset. A loader-provided value is a class-B source (e.g. Codex's
///   session-file rate-limit) that's already durable and authoritative — it
///   wins, and carries no `as_of` qualifier because it isn't a stale snapshot.
/// - **Context window** (per-turn): Claude's window is stream-only, so a
///   hydrated turn has `usage.context_window == None` and the context bar would
///   blank until the next send. Fill it on the most recent agent turn the bar
///   would actually read — i.e. one with `usage`, a usable `context_input_tokens`,
///   and no usable window. The selection **must mirror `contextUtilization` in
///   `Sidebar.svelte`**: that scans newest→oldest and skips any agent turn
///   missing *either* a window or `context_input_tokens`. If the overlay filled
///   a turn the bar then skips (e.g. one lacking `context_input_tokens`), the
///   snapshot would go unread and the bar would stay blank — the exact failure
///   this milestone targets. So scan past non-qualifying turns rather than
///   stopping at the first turn with `usage`. No agent turn qualifies → no-op
///   (bar stays hidden); never synthesize a turn or a `TurnUsage`. No `as_of`
///   qualifier — a model's window is fixed, so the value doesn't go stale.
///
/// A `None` sidecar (missing/corrupt) is a no-op. Mirrors the frontend
/// reducer's hydrate fill-if-empty semantics.
fn apply_meta_sidecar_overlay(
    transcript: &mut switchboard_harness::LoadedTranscript,
    sidecar: Option<switchboard_harness::meta_sidecar::MetaSidecar>,
) {
    let Some(sidecar) = sidecar else {
        return;
    };

    if transcript.last_rate_limit.is_none()
        && let Some(snapshot) = sidecar.rate_limit
    {
        transcript.last_rate_limit = Some(snapshot.payload);
        transcript.last_rate_limit_as_of = Some(snapshot.captured_at);
    }

    if let Some(snapshot) = sidecar.context_window {
        for turn in transcript.turns.iter_mut().rev() {
            if let switchboard_harness::Turn::Agent {
                usage: Some(usage), ..
            } = turn
            {
                // Match `contextUtilization`: the bar reads the latest agent
                // turn with BOTH a usable window and `context_input_tokens`. A
                // turn missing `context_input_tokens` is skipped by the bar, so
                // the overlay skips it too (scan to an earlier qualifying turn)
                // rather than filling an unread turn. `Some(0)` is "no usable
                // window" on both sides.
                if usage.context_input_tokens.is_none() {
                    continue;
                }
                if usage.context_window.is_none() || usage.context_window == Some(0) {
                    usage.context_window = Some(snapshot.context_window);
                }
                break;
            }
        }
    }
}

/// The per-harness session-file load, without the metadata-sidecar overlay.
/// Split out so [`load_agent_transcript`] can apply the overlay at a single
/// chokepoint covering both hydration paths.
fn load_agent_transcript_raw(
    project: &Project,
    agent: &AgentRecord,
    home_dir: &Path,
) -> Result<switchboard_harness::LoadedTranscript, AppError> {
    // The cwd / sidecar root is the project's own owning directory.
    let directory_path = project.directory.clone();
    match agent.harness {
        HarnessKind::ClaudeCode => {
            let Some(session_id) = locator_uuid(agent) else {
                return Ok(switchboard_harness::LoadedTranscript::default());
            };
            Ok(switchboard_harness::load_claude_transcript(
                home_dir,
                &directory_path,
                session_id,
                agent.id,
            )?)
        }
        HarnessKind::Codex => {
            // The thread-id + partition-date now live on the record
            // (`session_locator` → `Codex`), like Gemini — no sidecar lookup.
            // A never-dispatched agent (no locator) loads as empty but still
            // surfaces registry meta (empty thread-id → loader's empty path).
            let (session_id, partition_date) = match agent
                .session_locator
                .as_ref()
                .and_then(SessionLocator::as_codex)
            {
                Some((thread_id, date)) => (thread_id.to_owned(), Some(date)),
                None => (String::new(), None),
            };
            Ok(switchboard_harness::load_codex_transcript(
                home_dir,
                &directory_path,
                &session_id,
                partition_date,
                agent.id,
            )?)
        }
        HarnessKind::Gemini => {
            let Some(session_id) = locator_uuid(agent) else {
                return Ok(switchboard_harness::LoadedTranscript::default());
            };
            Ok(switchboard_harness::load_gemini_transcript(
                home_dir,
                &directory_path,
                session_id,
                agent.id,
            )?)
        }
        HarnessKind::Antigravity => {
            // The conversation UUID now lives on the record (`session_locator`),
            // like Gemini — no sidecar lookup. `None` (never dispatched) is
            // passed through so the loader still surfaces registry meta.
            Ok(switchboard_harness::load_antigravity_transcript(
                home_dir,
                &directory_path,
                locator_uuid(agent),
                agent.id,
            )?)
        }
        _ => Err(AppError::UnsupportedHarness),
    }
}

/// Per-agent session actions surfaced in the sidebar: the path of the harness
/// session file to open, and the interactive command to resume the session in a
/// terminal. Both are `None` until the agent has a resolvable session — `Open`
/// needs the file to exist on disk; `Resume` needs the agent to have dispatched
/// at least once (for Claude/Gemini/Antigravity that coincides with the locator
/// being on the record; for Codex the id lives in a sidecar written
/// post-dispatch, so resume can be offered even if the local transcript file is
/// absent).
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct AgentSessionInfo {
    /// Absolute path of the harness session file, present only if it exists.
    pub session_file: Option<String>,
    /// Full copy-ready resume command (`cd '<dir>' && <harness> …`), present
    /// only if the session can be resumed.
    pub resume_command: Option<String>,
}

/// Resolve an agent's on-disk session-file path, or `None` when the agent has
/// no locator / no file yet (or a harness we don't resolve). The single
/// per-harness path-resolution authority — both [`agent_session_info_impl`] and
/// [`project_session_fingerprints_impl`] go through here so the freshness check
/// reads the *same* file transcript loading does, with no second copy of the
/// resolution logic to drift.
fn resolve_session_file(agent: &AgentRecord, directory: &Path, home_dir: &Path) -> Option<PathBuf> {
    match agent.harness {
        HarnessKind::ClaudeCode => {
            let sid = locator_uuid(agent)?;
            let path = switchboard_harness::claude_session_file_path(home_dir, directory, &sid);
            path.exists().then_some(path)
        }
        HarnessKind::Gemini => {
            let sid = locator_uuid(agent)?;
            let mut candidates =
                switchboard_harness::gemini_session_file_candidates(home_dir, directory, &sid);
            candidates.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            candidates.pop()
        }
        HarnessKind::Codex => {
            let (thread_id, partition_date) = agent
                .session_locator
                .as_ref()
                .and_then(SessionLocator::as_codex)?;
            switchboard_harness::codex::session_file::locate_session_file(
                home_dir,
                partition_date,
                thread_id,
            )
        }
        HarnessKind::Antigravity => {
            let conversation_id = locator_uuid(agent)?;
            let path =
                switchboard_harness::antigravity::paths::transcript_path(home_dir, conversation_id);
            path.exists().then_some(path)
        }
        _ => None,
    }
}

/// A session file's freshness fingerprint — the inputs to "did this file change
/// since we last read it." Gated on `(source_path, modified_at, byte_len)`
/// together: `byte_len` is a near-free, more reliable signal than mtime alone
/// for an append-only JSONL (and the offset baseline if incremental re-read is
/// ever added), and `source_path` catches a file that moved (e.g. Gemini's
/// candidate selection picking a different file).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionFingerprint {
    pub source_path: String,
    pub modified_at: chrono::DateTime<chrono::Utc>,
    pub byte_len: u64,
}

/// Per-agent freshness record for the staleness-refresh gate.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentSessionFingerprint {
    pub agent_id: AgentId,
    /// Whether this agent's harness may be refreshed at all (the live-matched
    /// capability — see [`HarnessKind::supports_refresh`]). The frontend only
    /// acts on a changed fingerprint when this is true.
    pub refresh_capable: bool,
    /// The current fingerprint, or `None` when refresh is unsupported (we don't
    /// stat a file that can never trigger a refresh) or no session file exists.
    pub fingerprint: Option<SessionFingerprint>,
}

fn fingerprint_of(path: &Path) -> Option<SessionFingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    let modified_at: chrono::DateTime<chrono::Utc> = meta.modified().ok()?.into();
    Some(SessionFingerprint {
        source_path: path.to_string_lossy().into_owned(),
        modified_at,
        byte_len: meta.len(),
    })
}

/// Cheap freshness check for a project's agents: resolve + `stat` each
/// refresh-capable agent's session file (no parse), returning a fingerprint the
/// frontend diffs against the value it stored at last hydration to decide
/// whether to re-read. Non-refresh-capable agents return
/// `refresh_capable: false` with no fingerprint — they can never trigger a
/// refresh, so statting them would be wasted I/O. This is the gate that keeps an
/// unchanged file from ever being re-parsed: the (expensive) transcript load is
/// only invoked when this (cheap) check shows movement.
///
/// `home_dir` is injected for testability; the Tauri shim reads `$HOME`.
pub fn project_session_fingerprints_impl(
    state: &AppState,
    project_id: ProjectId,
    home_dir: &Path,
) -> Result<Vec<AgentSessionFingerprint>, AppError> {
    let project = match lock(&state.projects).get(&project_id).cloned() {
        Some(loaded) => loaded,
        None => find_project_in_directories(state, project_id)?,
    };
    let directory = project.directory.clone();
    let agents = project.list_agents()?;
    Ok(agents
        .into_iter()
        .map(|agent| {
            let refresh_capable = agent.harness.supports_refresh();
            let fingerprint = refresh_capable
                .then(|| {
                    resolve_session_file(&agent, &directory, home_dir)
                        .and_then(|p| fingerprint_of(&p))
                })
                .flatten();
            AgentSessionFingerprint {
                agent_id: agent.id,
                refresh_capable,
                fingerprint,
            }
        })
        .collect())
}

/// Resolve the per-agent session actions ([`AgentSessionInfo`]). Mirrors
/// [`load_agent_transcript`]'s per-harness session-id resolution
/// (Claude/Gemini/Antigravity from `AgentRecord.session_locator`; Codex from its
/// sidecar — a corrupt Codex sidecar fails loud, never-dispatched is the
/// legitimate empty case). `home_dir` is injected for testability; the Tauri
/// shim reads `$HOME`.
pub fn agent_session_info_impl(
    state: &AppState,
    agent_id: AgentId,
    home_dir: &Path,
) -> Result<AgentSessionInfo, AppError> {
    let (project, agent) = lookup_agent(state, agent_id)?;
    let directory = project.directory.clone();

    let session_file = resolve_session_file(&agent, &directory, home_dir);
    // Resume identifier (only when the agent can be resumed): Claude/Gemini by
    // session uuid but only once a file exists; Codex/Antigravity by their
    // locator regardless. Shares the file resolution above; this match only
    // covers the identifier (and the unsupported-harness guard).
    let resume_ref: Option<String> = match agent.harness {
        HarnessKind::ClaudeCode | HarnessKind::Gemini => locator_uuid(&agent)
            .filter(|_| session_file.is_some())
            .map(|sid| sid.to_string()),
        HarnessKind::Codex => agent
            .session_locator
            .as_ref()
            .and_then(SessionLocator::as_codex)
            .map(|(thread_id, _)| thread_id.to_owned()),
        HarnessKind::Antigravity => locator_uuid(&agent).map(|c| c.to_string()),
        _ => return Err(AppError::UnsupportedHarness),
    };

    let resume_command = resume_ref
        .and_then(|r| {
            switchboard_harness::interactive_resume_command(agent.harness, &r, &directory)
        })
        .map(|tokens| {
            let args = tokens
                .iter()
                .map(|t| shell_quote_if_needed(t))
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "cd {} && {args}",
                shell_single_quote(&directory.to_string_lossy())
            )
        });

    Ok(AgentSessionInfo {
        session_file: session_file.map(|p| p.to_string_lossy().into_owned()),
        resume_command,
    })
}

/// POSIX single-quote a string for safe interpolation into a shell command:
/// wrap in single quotes, and replace any embedded single quote with the
/// `'\''` close-reopen idiom. Used only to render a copy-ready resume command.
/// Gate which URLs the `open_external_url` command will hand to the OS opener.
/// Markdown links are agent/user-controlled text, so only well-formed `http`/
/// `https` URLs with a host are forwarded; `file:`, `javascript:`, `data:`,
/// relative, and scheme-only/hostless inputs are refused, so a hallucinated
/// `file://…` link can't open an arbitrary local file when clicked. Parsing
/// (rather than a scheme-prefix check) is what rejects malformed "web" URLs like
/// `https:` or `http:foo` that have no real host.
pub fn validate_external_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("not a valid URL ({e}): {url}"))?;
    let has_host = parsed.host_str().is_some_and(|h| !h.is_empty());
    if matches!(parsed.scheme(), "http" | "https") && has_host {
        Ok(())
    } else {
        Err(format!("refusing to open non-web URL: {url}"))
    }
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Quote a resume-command token only when it contains anything outside a
/// shell-safe charset. The fixed program/flag tokens pass through unquoted (so
/// the copied command stays readable), while a session id sourced from a
/// malformed/edited sidecar that smuggles in shell metacharacters is
/// single-quoted, keeping the copy-only command well-formed.
fn shell_quote_if_needed(token: &str) -> String {
    let safe = !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b'='));
    if safe {
        token.to_owned()
    } else {
        shell_single_quote(token)
    }
}

/// The unified, post-restart project conversation: the merge of Switchboard's
/// conversation journal (the user's sends + non-completed-turn outcome markers)
/// with the project's per-agent harness transcripts (agent-produced content),
/// ordered by timestamp. The wire contract the unified-view frontend consumes.
///
/// The three rendered kinds in `items` are disjoint by source (system-design
/// §3 / §7 "Unified history after restart"): user messages come only from the
/// journal, agent content only from harness files, failed/cancelled markers
/// only from the journal — so no correlation or de-dup between sources is
/// performed.
///
/// **Same-turn `AgentTurn` + `Outcome` overlap is intentional, not duplication.**
/// A non-completed turn can legitimately produce *both* an
/// [`ConversationItem::AgentTurn`] with `status: Failed` and (possibly partial)
/// harness-persisted `items`, *and* a journal-sourced [`ConversationItem::Outcome`]
/// marker for the *same* `turn_id` (system-design §7). They are complementary:
/// the `AgentTurn` carries the partial content, the `Outcome` carries the
/// authoritative non-completed status that annotates it. Consumers render both;
/// the merge deliberately does not correlate or de-dup across the two sources.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProjectConversation {
    pub items: Vec<ConversationItem>,
    pub agents: Vec<AgentConversationMeta>,
}

/// One rendered entry in the unified transcript. Tagged `kind` to match the
/// wire convention; `#[non_exhaustive]` so a future rendered kind lands
/// additively for consumers.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConversationItem {
    /// The user's side of the conversation, from one of two sources:
    /// - A **dispatched send** from the journal (the common case): `send_id` is
    ///   `Some` and groups a fan-out (the message renders once across its N
    ///   recipients).
    /// - An **imported prompt** read from a harness session file when it predates
    ///   journaling (an attached session's history that the journal never saw):
    ///   `send_id` is `None` and there is a single recipient.
    ///
    /// `id` is the stable render identity in both cases — the journal `send_id`
    /// for a dispatched send, the harness `turn_id` for an imported prompt. It
    /// keys the rendered row and is never a join key to a `Send` (so grouping
    /// must key off `send_id`, not `id`). `agent_ids` are the recipients in
    /// first-seen order; `text` is the prompt (identical across a fan-out); `at`
    /// is the earliest `at` in the group.
    UserMessage {
        id: Uuid,
        send_id: Option<SendId>,
        agent_ids: Vec<AgentId>,
        text: String,
        /// Files attached to this send, taken from the grouped `Send` (identical
        /// across a fan-out's recipients). Empty for an imported prompt (no
        /// journal `Send` to carry them) and for any pre-attachments send.
        attachments: Vec<Attachment>,
        at: chrono::DateTime<chrono::Utc>,
    },
    /// One agent's completed (or harness-failed) turn content, sourced from the
    /// harness session file. A harness user-role turn is dropped when the journal
    /// already holds that prompt (a dispatched send is canonical there); a prompt
    /// that predates journaling has no `Send`, so it is surfaced as an imported
    /// `UserMessage` rather than lost.
    ///
    /// `send_id` is recovered by joining this turn's `turn_id` against the
    /// journal's `Send` records (which persist `send_id` + `turn_id` at
    /// turn-start), so a fan-out's historical responses group the same way live
    /// ones do. `None` when no journal `Send` matches (e.g. a turn whose
    /// send-record write failed, or pre-journal history).
    AgentTurn {
        turn_id: switchboard_harness::TurnId,
        agent_id: AgentId,
        send_id: Option<SendId>,
        started_at: chrono::DateTime<chrono::Utc>,
        ended_at: Option<chrono::DateTime<chrono::Utc>>,
        status: switchboard_harness::TurnStatus,
        items: Vec<switchboard_harness::TurnItem>,
        usage: Option<switchboard_harness::TurnUsage>,
        /// Per-turn cost + overage, present when this turn was a real-spend turn
        /// whose telemetry was persisted (re-joined from the turn-metadata
        /// sidecar on reopen). `None` for normal-quota or pre-feature turns —
        /// the message then renders no cost and no "using credits" marker.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spend: Option<switchboard_harness::TurnSpend>,
        /// The turn's stable hydration key (see [`switchboard_harness::Turn`]),
        /// carried through so the frontend merge can dedup this turn against an
        /// already-loaded copy. `None` for keyless harnesses (Antigravity).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hydration_key: Option<String>,
    },
    /// A non-completed-turn marker (failed or cancelled), sourced from the
    /// journal. Carries no agent content; `reason` is a best-effort
    /// human-readable detail parsed from the opaque outcome value.
    Outcome {
        turn_id: switchboard_harness::TurnId,
        send_id: SendId,
        agent_id: AgentId,
        status: OutcomeStatus,
        reason: Option<String>,
        at: chrono::DateTime<chrono::Utc>,
    },
}

/// The non-completed terminal kinds the journal records. This is where
/// `cancelled` enters the rendered model — `TurnStatus` (harness-sourced) has
/// no `Cancelled` because the harness never persists a cancelled turn.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutcomeStatus {
    Cancelled,
    Failed,
}

/// Per-agent session metadata carried alongside the merged items, so the
/// unified view can populate per-agent meta / quota without re-loading.
///
/// Warnings and load errors are agent-scoped (not project-scoped) so the
/// unified view can attribute them: `warnings` are this agent's per-line parse
/// degradations from its harness transcript; `load_error`, when present, means
/// this agent's transcript failed to load entirely (e.g. corrupt sidecar) — its
/// turns are absent but the rest of the project (journal + healthy agents) still
/// renders. One bad agent never blanks the project.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentConversationMeta {
    pub agent_id: AgentId,
    pub meta: Option<switchboard_harness::SessionMetaInfo>,
    pub last_rate_limit: Option<serde_json::Value>,
    /// Capture time of `last_rate_limit` when restored from the metadata
    /// sidecar (stream-only/class-C value); drives the UI staleness
    /// qualifier. `None` for live values and for class-B (durable) sources.
    pub last_rate_limit_as_of: Option<chrono::DateTime<chrono::Utc>>,
    pub warnings: Vec<switchboard_harness::ParseWarning>,
    pub load_error: Option<String>,
}

/// Accumulator for one grouped user message during the merge: `(send_id,
/// recipients in first-seen order, prompt, attachments, earliest `at`)`.
type UserMessageGroup = (
    SendId,
    Vec<AgentId>,
    String,
    Vec<Attachment>,
    chrono::DateTime<chrono::Utc>,
);

/// Pure merge of the two conversation sources into the unified transcript. No
/// I/O — the testable core. See [`ProjectConversation`] for the disjoint-source
/// contract and system-design §7 for the worked scenarios this implements.
// One linear pass over the journal + one over the transcripts; splitting it
// would scatter the user-message grouping, outcome collection, and order-zip
// correlation that are read together.
#[allow(clippy::too_many_lines)]
fn merge_project_conversation(
    journal: Vec<switchboard_core::JournalRecord>,
    agent_transcripts: Vec<(
        AgentId,
        switchboard_harness::LoadedTranscript,
        Option<String>,
    )>,
) -> ProjectConversation {
    let mut items: Vec<ConversationItem> = Vec::new();

    // User messages ← `Send` records grouped by `send_id`. One rendered message
    // per group: recipients in first-seen order (dedup-preserving), prompt from
    // any record (identical across the group), `at` = min of the group.
    // `index_of` maps a send_id to its slot in `user_messages`, preserving
    // first-appearance order without a separate removal pass.
    let mut index_of: HashMap<SendId, usize> = HashMap::new();
    let mut user_messages: Vec<UserMessageGroup> = Vec::new();
    // The journal's `turn_id` is the dispatcher's, distinct from the harness
    // session file's own turn ids, so they can't be joined directly. Instead we
    // correlate each agent's harness turns to its sends by ORDER: the Nth harness
    // agent turn answers the Nth send dispatched to it (the dispatcher runs an
    // agent's turns FIFO and journals in that order). `agent_sends` is each
    // agent's sends in dispatch order — **all** of them, completed and not: a
    // send cancelled/failed *after* the agent wrote content leaves a partial
    // harness turn that must be paired (excluding non-completed sends here is the
    // bug that double-rendered the prompt). A non-completed send's cancelled/failed
    // badge comes from its Outcome marker, which renders alongside the turn
    // (`ProjectConversation` render-both contract).
    let mut agent_sends: HashMap<AgentId, Vec<SendId>> = HashMap::new();
    for record in journal {
        match record {
            switchboard_core::JournalRecord::Send {
                send_id,
                turn_id: _,
                agent_id,
                prompt,
                attachments,
                at,
            } => {
                agent_sends.entry(agent_id).or_default().push(send_id);
                if let Some(&i) = index_of.get(&send_id) {
                    let entry = &mut user_messages[i];
                    if !entry.1.contains(&agent_id) {
                        entry.1.push(agent_id);
                    }
                    if at < entry.4 {
                        entry.4 = at;
                    }
                } else {
                    // Prompt and attachments are shared across a fan-out's
                    // recipients (the compose bar snapshots one attachment list
                    // and sends it to every recipient), so taking the first
                    // record's is correct; M6 templated per-recipient prompts
                    // will need this revisited.
                    index_of.insert(send_id, user_messages.len());
                    user_messages.push((send_id, vec![agent_id], prompt, attachments, at));
                }
            }
            switchboard_core::JournalRecord::Outcome {
                turn_id,
                send_id,
                agent_id,
                outcome,
                started_at,
                ..
            } => {
                let (status, reason) = parse_outcome(&outcome);
                items.push(ConversationItem::Outcome {
                    turn_id,
                    send_id,
                    agent_id,
                    status,
                    reason,
                    at: started_at,
                });
            }
            // A future journal-record kind we don't yet render — degrade by
            // skipping it rather than failing the whole load.
            _ => {}
        }
    }
    for (send_id, agent_ids, text, attachments, at) in user_messages {
        items.push(ConversationItem::UserMessage {
            id: send_id,
            send_id: Some(send_id),
            agent_ids,
            text,
            attachments,
            at,
        });
    }

    // Agent content ← each agent's harness transcript. Warnings and any load
    // error are agent-scoped: attach them to this transcript's
    // `AgentConversationMeta` so the unified view can attribute them (one bad
    // agent never blanks the project).
    let mut agents: Vec<AgentConversationMeta> = Vec::new();
    for (agent_id, transcript, load_error) in agent_transcripts {
        let turns = transcript.turns;
        // Correlate harness turns to journaled sends by ORDER (the journal's
        // turn_id is the dispatcher's, unrelated to the harness file's).
        //
        // Front-aligned against **all** sends (`agent_sends`, not completed-only):
        // extra agent turns at the FRONT are pre-journaling history (older than the
        // first journaled send — typically an attached session predating
        // Switchboard) and get no send_id (rendered un-grouped); the rest pair with
        // sends in order. Extra sends at the BACK have no harness turn — an
        // in-flight send (turn still running) or a cancelled-before-output send —
        // and are dropped here rather than mislabeling a completed turn.
        //
        // The offset is on the **turn** side (`turn_offset`), not the send side:
        // do NOT skip leading sends (tail-anchoring), which would pair the last
        // completed turn with a trailing in-flight send — see
        // `merge_in_flight_send_does_not_mislabel_completed_turns`. Pairing against
        // all sends is what lets a cancel-mid turn claim its own send (so its
        // prompt drops); its cancelled status comes from the coexisting Outcome
        // marker, not from this correlation. Residual: a cancelled-before-output
        // send positioned *before* a content-bearing turn shifts subsequent labels
        // by one (content mis-grouping, not prompt duplication — the journal still
        // owns the prompt). User-visible symptom: a completed answer can render
        // under a `cancelled` badge (wrong *status* on a real answer), not a
        // duplicated or missing prompt. Pinned by characterization tests below;
        // the deferred durable key-join (plan doc) dissolves it.
        //
        // User turns are classified by their REPLY, not by a suffix count — so a
        // non-journaled prompt that lands *after* journaled history (e.g. the user
        // ran the CLI directly in the same dir between Switchboard sessions) is
        // still handled. The journal renders a UserMessage for every `Send`
        // (regardless of outcome), so a harness user turn is "journaled" (drop it,
        // the journal owns it) exactly when it corresponds to a send:
        //   - A user turn WITH a following reply is journaled iff that reply is
        //     journaled (`agent_seen >= turn_offset`).
        //   - A *dangling* user turn (no reply — a cancelled-before-output send or
        //     an in-flight one, or an imported prompt with no reply yet) is
        //     journaled while sends that produced no harness turn remain to account
        //     for it (`dangling_journaled` of them = trailing send-excess); beyond
        //     that it's imported and rendered.
        // This needs 1:1 prompt/reply alternation in the file (true for Claude and
        // Codex — tool results fold into the agent turn). Two edges remain, BOTH
        // confined to the already-discouraged pattern of running the bare CLI on a
        // session Switchboard is also driving (the resume-in-terminal panel warns
        // this corrupts the session file), and both pinned by characterization
        // tests so the behavior stays a conscious decision:
        //   - A send cancelled *before* the harness recorded its prompt has no
        //     harness user turn, so `dangling_journaled` overcounts by one and can
        //     drop a co-occurring imported dangling prompt.
        //   - Dangling turns are classified front-to-back (the first
        //     `dangling_journaled` are treated as journaled). If an *imported*
        //     dangling prompt precedes a *journaled* one (e.g. a bare-CLI prompt,
        //     then an in-flight Switchboard send), the imported prompt is dropped
        //     and the journaled one duplicated. Order alone can't disambiguate
        //     interleaved dangling sources — a timestamp join could, but would be
        //     inconsistent with the order-based agent-side correlation above.
        // Both mirror the order assumptions the agent-side front-alignment already
        // makes.
        let agent_turn_count = turns
            .iter()
            .filter(|t| matches!(t, switchboard_harness::Turn::Agent { .. }))
            .count();
        let all_sends = agent_sends.get(&agent_id).map_or(&[][..], Vec::as_slice);
        let pairs = agent_turn_count.min(all_sends.len());
        let turn_offset = agent_turn_count - pairs;
        let dangling_journaled = all_sends.len() - pairs;
        let mut agent_seen = 0usize;
        let mut dangling_seen = 0usize;
        for turn in turns {
            match turn {
                switchboard_harness::Turn::Agent {
                    turn_id,
                    agent_id: a_id,
                    started_at,
                    ended_at,
                    status,
                    items: t_items,
                    usage,
                    spend,
                    hydration_key,
                    ..
                } => {
                    let send_id = if agent_seen >= turn_offset {
                        all_sends.get(agent_seen - turn_offset).copied()
                    } else {
                        None
                    };
                    items.push(ConversationItem::AgentTurn {
                        turn_id,
                        agent_id: a_id,
                        send_id,
                        started_at,
                        ended_at,
                        status,
                        items: t_items,
                        usage,
                        spend,
                        hydration_key,
                    });
                    agent_seen += 1;
                }
                switchboard_harness::Turn::User {
                    turn_id,
                    agent_id: a_id,
                    started_at,
                    text,
                } => {
                    let imported = if agent_seen < agent_turn_count {
                        // A reply follows (the agent turn at index `agent_seen`):
                        // imported iff that reply is pre-journaling.
                        agent_seen < turn_offset
                    } else {
                        // Dangling (no reply): journaled while non-completed /
                        // in-flight sends remain to account for it; otherwise it's
                        // an imported prompt with no reply yet.
                        let journaled = dangling_seen < dangling_journaled;
                        dangling_seen += 1;
                        !journaled
                    };
                    if imported {
                        // Un-grouped (single recipient, `send_id` None), keyed by
                        // the harness turn_id — the prompt lives only here.
                        items.push(ConversationItem::UserMessage {
                            id: turn_id,
                            send_id: None,
                            agent_ids: vec![a_id],
                            text,
                            attachments: Vec::new(),
                            at: started_at,
                        });
                    }
                }
                _ => {}
            }
        }
        agents.push(AgentConversationMeta {
            agent_id,
            meta: transcript.meta,
            last_rate_limit: transcript.last_rate_limit,
            last_rate_limit_as_of: transcript.last_rate_limit_as_of,
            warnings: transcript.warnings,
            load_error,
        });
    }

    // Sort ascending by an explicit `(timestamp, kind_rank)` key so that at an
    // equal instant a user message always precedes the content/markers it
    // annotates: `UserMessage` (0) < `AgentTurn` (1) < `Outcome` (2). The common
    // failed-to-start/cancelled case has `Send.at == Outcome.started_at`, so a
    // timestamp-only sort would render the marker before its own message.
    items.sort_by_key(conversation_item_sort_key);

    ProjectConversation { items, agents }
}

/// The sort key for an item — its own timestamp (`UserMessage`→`at`,
/// `AgentTurn`→`started_at`, `Outcome`→`at`).
fn conversation_item_timestamp(item: &ConversationItem) -> chrono::DateTime<chrono::Utc> {
    match item {
        ConversationItem::UserMessage { at, .. } | ConversationItem::Outcome { at, .. } => *at,
        ConversationItem::AgentTurn { started_at, .. } => *started_at,
    }
}

/// The ordering key for an item: its timestamp, tie-broken by kind rank so a
/// user message (0) sorts before agent content (1) and an outcome marker (2) at
/// the same instant.
fn conversation_item_sort_key(item: &ConversationItem) -> (chrono::DateTime<chrono::Utc>, u8) {
    let rank = match item {
        ConversationItem::UserMessage { .. } => 0,
        ConversationItem::AgentTurn { .. } => 1,
        ConversationItem::Outcome { .. } => 2,
    };
    (conversation_item_timestamp(item), rank)
}

/// Parse the opaque journal outcome value into the rendered status + reason.
/// The value is the terminal outcome's wire shape, e.g.
/// `{"status":"cancelled","source":"user"}` or
/// `{"status":"failed","kind":"harness_error","message":"…"}`. Anything other
/// than an explicit `cancelled` reads as `failed` (the conservative default for
/// a non-completed terminal we couldn't classify). `reason` is the `message`
/// for failures, the `source` for cancellations — `None` if absent.
fn parse_outcome(outcome: &serde_json::Value) -> (OutcomeStatus, Option<String>) {
    let status_str = outcome.get("status").and_then(serde_json::Value::as_str);
    match status_str {
        Some("cancelled") => (
            OutcomeStatus::Cancelled,
            outcome
                .get("source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        ),
        _ => (
            OutcomeStatus::Failed,
            outcome
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        ),
    }
}

/// Rebuild a project's unified conversation after restart by merging its
/// conversation journal with each agent's harness transcript. Resolves the
/// project (loaded fast-path, else via the owning directory), reads the journal
/// (missing file → empty, like the per-agent default-on-missing), and loads
/// each agent's transcript via [`load_agent_transcript`] on the blocking pool,
/// in parallel.
///
/// **Per-agent load errors are non-fatal.** Unlike [`load_transcript_impl`]
/// (single-agent, fail-loud), a corrupt sidecar or read failure for *one* agent
/// here is recorded on that agent's [`AgentConversationMeta::load_error`] (with
/// an empty transcript for it) and the rest of the project — journal plus the
/// healthy agents — still renders. Corruption stays loud (surfaced per-agent),
/// just not fatal to the whole project.
///
/// `home_dir` is passed in (not resolved here) so tests can stage a temp
/// directory without mutating process-wide `$HOME`; the Tauri command shim
/// reads `$HOME` and forwards.
/// The set of staged attachment paths still referenced by a `Send` record —
/// everything GC must keep. Absolute paths, exactly as stored at stage time.
fn collect_referenced_attachment_paths(
    journal: &[switchboard_core::JournalRecord],
) -> HashSet<PathBuf> {
    journal
        .iter()
        .filter_map(|record| match record {
            switchboard_core::JournalRecord::Send { attachments, .. } => Some(attachments),
            _ => None,
        })
        .flatten()
        .map(|attachment| PathBuf::from(&attachment.path))
        .collect()
}

/// Delete every file in `attachments_dir` not in `referenced`. Best-effort: a
/// missing dir is a no-op (nothing staged yet), and a failed unlink logs a
/// warning rather than failing the project load (mirrors the registry
/// "degrade with a warning" posture). The only place attachments are deleted.
fn gc_unreferenced_attachments(attachments_dir: &Path, referenced: &HashSet<PathBuf>) {
    let entries = match std::fs::read_dir(attachments_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(
                dir = %attachments_dir.display(),
                error = %e,
                "could not read attachments dir for GC — skipping cleanup this load"
            );
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if referenced.contains(&path) {
            continue;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to remove unreferenced attachment — leaving it in place"
            );
        }
    }
}

pub async fn load_project_conversation_impl(
    state: &AppState,
    project_id: ProjectId,
    home_dir: &Path,
) -> Result<ProjectConversation, AppError> {
    // Resolve the project and collect each agent's *owned* inputs while holding
    // the lock, then release it before doing any read+parse. `load_agent_transcript`
    // is CPU-bound (parsing a real session file is ~1s for 30 MB), so it runs on
    // the blocking pool — never on an async executor worker.
    let project = match lock(&state.projects).get(&project_id).cloned() {
        Some(loaded) => loaded,
        None => find_project_in_directories(state, project_id)?,
    };
    let journal = switchboard_core::journal::read_records(&project.journal_path())?;

    // Reclaim disk on load: delete any staged file no longer referenced by a
    // `Send` record — orphans from a staged-but-unsent drop, or files whose
    // conversation was removed. Pure function of on-disk state, so it's
    // crash-safe (just re-runs next load) and needs no completion signal.
    gc_unreferenced_attachments(
        &project.attachments_dir(),
        &collect_referenced_attachment_paths(&journal),
    );

    let agents = project.list_agents()?;

    // Parse each agent's transcript in parallel on the blocking pool. A
    // per-agent load error is recorded on that agent (empty transcript + the
    // error string) rather than aborting the whole project — the journal and
    // the healthy agents still render.
    let loads = agents.into_iter().map(|agent| {
        let project = project.clone();
        let home_dir = home_dir.to_path_buf();
        async move {
            let agent_id = agent.id;
            let result = tokio::task::spawn_blocking(move || {
                load_agent_transcript(&project, &agent, &home_dir)
            })
            .await;
            match result {
                Ok(Ok(transcript)) => (agent_id, transcript, None),
                Ok(Err(err)) => (
                    agent_id,
                    switchboard_harness::LoadedTranscript::default(),
                    Some(err.to_string()),
                ),
                Err(join_err) => (
                    agent_id,
                    switchboard_harness::LoadedTranscript::default(),
                    Some(join_err.to_string()),
                ),
            }
        }
    });
    let agent_transcripts: Vec<(
        AgentId,
        switchboard_harness::LoadedTranscript,
        Option<String>,
    )> = futures::future::join_all(loads).await;

    Ok(merge_project_conversation(journal, agent_transcripts))
}

pub fn check_claude_binary_impl(state: &AppState) -> Result<(), AppError> {
    state.claude_adapter.probe().map_err(AppError::Probe)
}

pub fn check_codex_binary_impl(state: &AppState) -> Result<(), AppError> {
    state.codex_adapter.probe().map_err(AppError::Probe)
}

pub fn check_gemini_binary_impl(state: &AppState) -> Result<(), AppError> {
    state.gemini_adapter.probe().map_err(AppError::Probe)
}

/// Probe `agy` on PATH via the registered Antigravity adapter — same shape
/// as the other three harness binary checks.
pub fn check_antigravity_binary_impl(state: &AppState) -> Result<(), AppError> {
    state.antigravity_adapter.probe().map_err(AppError::Probe)
}

/// Supported macOS Keychain service / account for Antigravity auth.
///
/// Surprising load-bearing detail: the service name is `"gemini"`, NOT
/// `"antigravity"`. Antigravity stores its credentials under the shared
/// Gemini Keychain service to match the `~/.gemini/` directory namespace
/// theme. Source: `security dump-keychain` on an authed dev machine
/// showed `svce="gemini" acct="antigravity"`. Documented in
/// `docs/research/archive/antigravity-cli-observed.md` line 99.
const ANTIGRAVITY_KEYCHAIN_SERVICE: &str = "gemini";
const ANTIGRAVITY_KEYCHAIN_ACCOUNT: &str = "antigravity";

/// Best-effort Antigravity subscription-auth detection. Invokes the macOS
/// `security` CLI to look up the Antigravity Keychain entry. Returns
/// `Ok(())` if the entry exists; `Err(AppError::AuthNotConfigured)`
/// otherwise (including when `security` itself is missing — non-macOS
/// hosts will surface as auth-missing, which is correct because
/// Antigravity is macOS-only in v1).
///
/// Unlike Codex/Gemini, there is no on-disk config file we can probe —
/// `agy` reads credentials exclusively via macOS Keychain. The signature
/// therefore takes no `home_dir` parameter; the keychain lookup is
/// system-wide. The Tauri shim drops the `$HOME` forwarding it does for
/// the other harnesses.
pub fn check_antigravity_auth_impl() -> Result<(), AppError> {
    let probe_result = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            ANTIGRAVITY_KEYCHAIN_SERVICE,
            "-a",
            ANTIGRAVITY_KEYCHAIN_ACCOUNT,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success());
    interpret_antigravity_keychain_probe(&probe_result)
}

/// Pure interpretation of the `security` CLI's exit status. Factored out
/// so unit tests can pin all three branches without invoking the actual
/// CLI. Takes the result by reference because `std::io::Error` is not
/// `Clone` and the function only inspects, never owns, the result.
fn interpret_antigravity_keychain_probe(
    probe_result: &std::io::Result<bool>,
) -> Result<(), AppError> {
    let expected_path = format!(
        "macOS Keychain (service: {ANTIGRAVITY_KEYCHAIN_SERVICE}, account: {ANTIGRAVITY_KEYCHAIN_ACCOUNT})"
    );
    let auth_err = || AppError::AuthNotConfigured {
        harness: HarnessKind::Antigravity,
        expected_path: expected_path.clone(),
    };
    // `Ok(false)` (entry not in Keychain) and `Err(_)` (couldn't run
    // `security` at all — non-macOS host, missing tool) both surface as
    // auth-missing. The user-facing outcome is the same: the agent isn't
    // dispatchable until they authenticate.
    match probe_result {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(auth_err()),
    }
}

/// macOS Keychain service Claude Code stores its OAuth credentials under
/// when logged in. Confirmed via `security dump-keychain` on an authed dev
/// machine: a generic-password item with `svce="Claude Code-credentials"`.
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Best-effort Claude Code subscription-auth detection (macOS only). Looks
/// up the Keychain service the CLI stores its OAuth token under; presence
/// means "logged in at some point," not a validity guarantee — the
/// authoritative test is a successful send. Mirrors
/// [`check_antigravity_auth_impl`]: `Ok(false)` (no entry) and `Err(_)`
/// (couldn't run `security` — non-macOS host, missing tool) both surface as
/// auth-missing.
///
/// Queried by service name only (no `-a` account filter): the heuristic only
/// needs "does any Claude credential item exist," and the account value isn't
/// stable enough to pin. A Linux build would instead check
/// `~/.claude/.credentials.json` — out of v1 scope (macOS-only).
pub fn check_claude_auth_impl() -> Result<(), AppError> {
    let probe_result = std::process::Command::new("security")
        .args(["find-generic-password", "-s", CLAUDE_KEYCHAIN_SERVICE])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success());
    interpret_claude_keychain_probe(&probe_result)
}

/// Pure interpretation of the Claude Keychain probe's exit status. Factored
/// out so unit tests pin all branches without invoking the real `security`
/// CLI (which would couple the test to the dev machine's login state).
fn interpret_claude_keychain_probe(probe_result: &std::io::Result<bool>) -> Result<(), AppError> {
    let auth_err = || AppError::AuthNotConfigured {
        harness: HarnessKind::ClaudeCode,
        expected_path: format!("macOS Keychain (service: {CLAUDE_KEYCHAIN_SERVICE})"),
    };
    match probe_result {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(auth_err()),
    }
}

/// Best-effort Codex subscription-auth detection. Returns `Ok(())` if the
/// auth file is present at the default location (`<home>/.codex/auth.json`),
/// `Err(AppError::AuthNotConfigured)` otherwise.
///
/// **Known limitations** (best effort, not robust):
/// - **False positive on API-key-only setups.** A user with only
///   `OPENAI_API_KEY` env var and no `codex login` may still have a stale
///   `auth.json` from a prior login; we report "authenticated" but a real
///   dispatch may surface an `AuthFailure`. The banner's actionable copy
///   ("run `codex login`") is still correct guidance under that case.
/// - **Claude uses a Keychain presence heuristic.** Claude Code on macOS
///   stores OAuth tokens in the Keychain (no reliable on-disk file); see
///   [`check_claude_auth_impl`] for the equivalent best-effort probe.
///
/// `home_dir` is a parameter (not derived from `$HOME` inside) for the
/// same testability reason as `attach_agent_impl` — the Tauri shim reads
/// `$HOME` and forwards.
pub fn check_codex_auth_impl(home_dir: &Path) -> Result<(), AppError> {
    let auth_path = home_dir.join(".codex").join("auth.json");
    if auth_path.exists() {
        Ok(())
    } else {
        Err(AppError::AuthNotConfigured {
            harness: HarnessKind::Codex,
            expected_path: auth_path.to_string_lossy().into_owned(),
        })
    }
}

/// Supported Gemini auth methods. The file is considered authenticated
/// iff `security.auth.selectedType` is one of these. Failing closed on
/// missing/unknown values means a malformed or empty `settings.json`
/// surfaces as "not authenticated," prompting the user to run `gemini`
/// interactively rather than silently letting a dispatch fail with a
/// less-actionable error.
const SUPPORTED_GEMINI_AUTH_TYPES: &[&str] =
    &["oauth-personal", "gemini-api-key", "vertex-ai", "workspace"];

/// Best-effort Gemini subscription-auth detection. Reads
/// `<home>/.gemini/settings.json` and checks
/// `security.auth.selectedType` against the supported set. Returns
/// `Err(AppError::AuthNotConfigured)` if the file is missing, unparseable,
/// the field is absent, or the value isn't recognized. Mirrors
/// `check_codex_auth_impl` shape; `home_dir` is a parameter so tests
/// stage a temp directory without touching `$HOME`.
pub fn check_gemini_auth_impl(home_dir: &Path) -> Result<(), AppError> {
    let settings_path = home_dir.join(".gemini").join("settings.json");
    let auth_err = || AppError::AuthNotConfigured {
        harness: HarnessKind::Gemini,
        expected_path: settings_path.to_string_lossy().into_owned(),
    };
    let bytes = std::fs::read(&settings_path).map_err(|_| auth_err())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| auth_err())?;
    let selected = value
        .get("security")
        .and_then(|s| s.get("auth"))
        .and_then(|a| a.get("selectedType"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(auth_err)?;
    if SUPPORTED_GEMINI_AUTH_TYPES.contains(&selected) {
        Ok(())
    } else {
        Err(auth_err())
    }
}

/// Install status of a harness CLI, for the getting-started surface.
/// A missing binary is `installed: false` with no version — *data*, not an
/// error path (unlike `check_*_binary`, which gates agent creation and so
/// returns `Result<(), _>`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HarnessInstallStatus {
    pub installed: bool,
    pub version: Option<String>,
}

/// Derive install status from an adapter: present-on-PATH plus its
/// best-effort `--version`. Version is only read when the binary is present
/// (a missing binary has no version to report). Free of harness identity —
/// works for any adapter, which keeps it trivially unit-testable.
fn install_status_for(adapter: &dyn HarnessAdapter) -> HarnessInstallStatus {
    let installed = adapter.probe().is_ok();
    HarnessInstallStatus {
        installed,
        version: if installed { adapter.version() } else { None },
    }
}

/// Install status for a given harness. The `match harness` here is adapter
/// *routing* (the same pattern as `send_message_impl`), not failure
/// classification — it selects which CLI to inspect.
pub fn get_harness_install_status_impl(
    state: &AppState,
    harness: HarnessKind,
) -> HarnessInstallStatus {
    let adapter: &dyn HarnessAdapter = match harness {
        HarnessKind::ClaudeCode => state.claude_adapter.as_ref(),
        HarnessKind::Codex => state.codex_adapter.as_ref(),
        HarnessKind::Gemini => state.gemini_adapter.as_ref(),
        HarnessKind::Antigravity => state.antigravity_adapter.as_ref(),
        _ => {
            return HarnessInstallStatus {
                installed: false,
                version: None,
            };
        }
    };
    install_status_for(adapter)
}

/// Find a not-yet-loaded project's owning directory by searching every loaded
/// directory for the one whose on-disk project list contains `project_id`, then
/// read the project from it. Used by `open_project_impl` (lazy-lock-on-open):
/// the directory is known to be loaded (the flat list only offers projects from
/// loaded directories), but the project handle itself may not be in
/// `state.projects` yet.
/// Locate the project across every loaded directory, opening it from the
/// directory that lists it.
///
/// **Resilience to an unrelated corrupt directory.** Iteration order over
/// `state.directories` is a `HashMap`'s nondeterministic order, so a corrupt
/// *unrelated* directory could be visited before the healthy one that owns the
/// target. We therefore **skip-and-log** a directory whose `list_projects`
/// errors and keep searching, rather than propagating mid-iteration and failing
/// the open of a perfectly healthy project. Only when no directory yields the
/// project do we return `ProjectNotLoaded`.
///
/// (Contrast `enumerate_all_projects`, the collision scan, which deliberately
/// fails loud — a scan that can't read a directory must not let a possibly
/// colliding attach through.)
fn find_project_in_directories(
    state: &AppState,
    project_id: ProjectId,
) -> Result<Project, AppError> {
    let directories: Vec<Directory> = lock(&state.directories).values().cloned().collect();
    for directory in directories {
        let summaries = match directory.list_projects() {
            Ok(summaries) => summaries,
            Err(e) => {
                tracing::warn!(
                    directory = %directory.path.display(),
                    error = %e,
                    "skipping directory while locating project — its registry could not be read"
                );
                continue;
            }
        };
        if summaries.iter().any(|s| s.id == project_id) {
            return directory.open_project(project_id).map_err(AppError::from);
        }
    }
    Err(AppError::ProjectNotLoaded(project_id))
}

/// Resolve the loaded `Directory` that owns `project_id` by scanning each
/// loaded directory's on-disk index. Unlike [`find_project_in_directories`],
/// this returns the owning `Directory` (not the loaded `Project`) and does not
/// open/lock the project — used by metadata mutations (`rename`, `delete`) that
/// rewrite the directory's files directly. A directory whose index can't be
/// read is skipped with a warning. Returns `ProjectNotLoaded` if no loaded
/// directory claims the id (e.g. its directory is currently unavailable).
fn find_directory_for_project(
    state: &AppState,
    project_id: ProjectId,
) -> Result<Directory, AppError> {
    let directories: Vec<Directory> = lock(&state.directories).values().cloned().collect();
    for directory in directories {
        match directory.list_projects() {
            Ok(summaries) if summaries.iter().any(|s| s.id == project_id) => {
                return Ok(directory);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    directory = %directory.path.display(),
                    error = %e,
                    "skipping directory while locating project's owning directory — its index could not be read"
                );
            }
        }
    }
    Err(AppError::ProjectNotLoaded(project_id))
}

/// Resolve the owning `Directory` for `project_id`, preferring an in-memory
/// lookup. A loaded project carries its canonical `directory` path, so we get
/// the handle straight from `state.directories` without reading any index —
/// which also means a transient index read error can't masquerade as "project
/// not found" and ghost-delete a project whose files are actually present. Falls
/// back to the on-disk index scan for an available-but-never-opened project.
/// Returns `ProjectNotLoaded` only when no loaded directory claims the id.
fn resolve_owning_directory(
    state: &AppState,
    project_id: ProjectId,
) -> Result<Directory, AppError> {
    let loaded_dir = lock(&state.projects)
        .get(&project_id)
        .map(|p| p.directory.clone());
    if let Some(dir_path) = loaded_dir
        && let Some(directory) = lock(&state.directories).get(&dir_path).cloned()
    {
        return Ok(directory);
    }
    find_directory_for_project(state, project_id)
}

fn lookup_agent(state: &AppState, agent_id: AgentId) -> Result<(Project, AgentRecord), AppError> {
    // Register-cache hit (M4.1): the cached `AgentRecord` carries its
    // `project_id`, so we resolve the owning project without scanning every
    // loaded project's `registry.jsonl` from disk. The project is always
    // loaded when its agents are cached (the cache is populated on open and
    // cleared on rebind together with `projects`), so a missing project here
    // is a genuine ProjectNotLoaded, not a stale-cache artifact.
    let agent = lock(&state.agents_by_id)
        .get(&agent_id)
        .cloned()
        .ok_or(AppError::AgentNotFound(agent_id))?;
    let project = lock(&state.projects)
        .get(&agent.project_id)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(agent.project_id))?;
    Ok((project, agent))
}

/// Filename of the per-project inter-process lock inside its metadata dir.
const INSTANCE_LOCK_FILE: &str = "instance.lock";

/// Acquire the per-project advisory file lock (M4.1) using the standard
/// library's `File::try_lock` (stable since Rust 1.89 — no external crate
/// needed). Returns the live `File` handle that *is* the lock — the caller
/// stores it in `AppState.project_locks` for the project's loaded lifetime;
/// dropping it (rebind, process exit/crash) releases the lock with no explicit
/// unlock. Contention (another process holds it) maps to `ProjectLocked`; any
/// other I/O failure to `ProjectLockIo`.
fn acquire_project_lock(project_id: ProjectId, root: &Path) -> Result<File, AppError> {
    let lock_path = root.join(INSTANCE_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        // The file is a pure lock token — we never write content to it, so
        // neither truncate nor preserve matters; pick the non-destructive one.
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| AppError::ProjectLockIo { project_id, source })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(AppError::ProjectLocked(project_id)),
        Err(std::fs::TryLockError::Error(source)) => {
            Err(AppError::ProjectLockIo { project_id, source })
        }
    }
}

pub(crate) fn parse_uuid(value: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|e| AppError::invalid_uuid(value, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use switchboard_core::CoreError;
    use switchboard_dispatcher::{EventEmitter, RecordingEmitter};
    use switchboard_harness::{ClaudeCodeAdapter, HarnessAdapter, MockHarnessAdapter};
    use tempfile::TempDir;

    /// Test convenience: create a project in the sole loaded directory. Most
    /// tests load exactly one directory, so this keeps their call sites terse
    /// while the production API requires an explicit directory path.
    fn create_project_in_only_dir(state: &AppState, name: &str) -> ProjectSummary {
        let path = {
            let dirs = lock(&state.directories);
            assert_eq!(
                dirs.len(),
                1,
                "create_project_in_only_dir requires exactly one loaded directory"
            );
            dirs.keys().next().unwrap().to_string_lossy().into_owned()
        };
        create_project_impl(state, name, &path).unwrap()
    }

    #[tokio::test]
    async fn search_project_files_honors_gitignore_and_includes_hidden_files() {
        let (tmp, state, _emitter) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");

        std::fs::write(tmp.path().join(".gitignore"), "ignored.log\nignored-dir/\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("ignored-dir")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(tmp.path().join("README.md"), "# Read me\n").unwrap();
        std::fs::write(tmp.path().join(".env"), "TOKEN=secret\n").unwrap();
        std::fs::write(tmp.path().join("ignored.log"), "ignored\n").unwrap();
        std::fs::write(tmp.path().join("ignored-dir/secret.rs"), "ignored\n").unwrap();
        std::fs::write(tmp.path().join(".git/config"), "ignored\n").unwrap();

        let root = search_project_files_root_impl(&state, project.id).unwrap();
        let matches = search_project_files_in_root(&root, "", 20).unwrap();
        assert!(matches.contains(&".env".to_owned()));
        assert!(matches.contains(&".gitignore".to_owned()));
        assert!(matches.contains(&"src/main.rs".to_owned()));
        assert!(!matches.contains(&"ignored.log".to_owned()));
        assert!(!matches.contains(&"ignored-dir/secret.rs".to_owned()));
        assert!(!matches.contains(&".git/config".to_owned()));
        assert!(!matches.iter().any(|path| path.starts_with(".switchboard/")));

        let queried = search_project_files_in_root(&root, "readme", 20).unwrap();
        assert_eq!(queried, vec!["README.md"]);

        let limited = search_project_files_in_root(&root, "", 1).unwrap();
        assert_eq!(limited.len(), 1);
    }

    #[tokio::test]
    async fn search_project_files_reports_walk_failures() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        drop(tmp);
        let err = search_project_files_in_root(&root, "", 20).unwrap_err();
        match err {
            AppError::ProjectFileSearch { root: actual, .. } => assert_eq!(actual, root),
            other => panic!("expected project file search error, got {other:?}"),
        }
    }

    fn fresh_state_with_mock() -> (TempDir, AppState, Arc<RecordingEmitter>) {
        let tmp = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter.clone() as Arc<dyn EventEmitter>,
        );
        (tmp, state, emitter)
    }

    /// Like `fresh_state_with_mock` but every harness adapter runs the given
    /// scenario — used by the cancellation tests, which need the
    /// `AwaitCancellation` scenario (parks until the token fires) to keep a
    /// turn deterministically in flight.
    fn fresh_state_with_scenario(
        scenario: switchboard_harness::MockScenario,
    ) -> (TempDir, AppState, Arc<RecordingEmitter>) {
        let tmp = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::with_scenario(scenario));
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter.clone() as Arc<dyn EventEmitter>,
        );
        (tmp, state, emitter)
    }

    /// Default deadline for any emitter `wait_for*` so a logic bug fails as a
    /// bounded timeout rather than hanging the suite.
    const WAIT: std::time::Duration = std::time::Duration::from_secs(5);

    /// Await an emitter wait-future under the shared timeout, panicking with a
    /// snapshot of what *was* recorded if it doesn't resolve in time. The actor
    /// model is fire-and-forget, so tests await turn/chain completion by waiting
    /// for the recorded `agent_idle` (or other terminal) event rather than a
    /// per-send join handle.
    async fn within<F: std::future::Future<Output = ()>>(
        emitter: &RecordingEmitter,
        label: &str,
        fut: F,
    ) {
        assert!(
            tokio::time::timeout(WAIT, fut).await.is_ok(),
            "timed out waiting for {label}; recorded events: {:?}",
            emitter
                .snapshot()
                .iter()
                .map(|(n, v)| (n.clone(), v["type"].as_str().unwrap_or("?").to_owned()))
                .collect::<Vec<_>>()
        );
    }

    /// Extract the `message_id` from an event payload, asserting it parses.
    fn extract_message_id(value: &serde_json::Value) -> MessageId {
        let s = value["message_id"].as_str().expect("event has message_id");
        Uuid::parse_str(s).expect("message_id parses as UUID")
    }

    /// Count recorded events whose wire `type` tag equals `ty`.
    fn count_type(events: &[(String, serde_json::Value)], ty: &str) -> usize {
        events.iter().filter(|(_, v)| v["type"] == ty).count()
    }

    /// Stand up a directory + project + Claude agent and return the agent and
    /// its project id. Shared setup for the cancellation/lifecycle tests.
    async fn project_with_agent(state: &AppState, tmp: &TempDir) -> (AgentRecord, ProjectId) {
        init_directory_impl(state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(state, "proj");
        set_active_project_impl(state, project.id).unwrap();
        let agent =
            create_agent_impl(state, "assistant", HarnessKind::ClaudeCode, None, None).unwrap();
        (agent, project.id)
    }

    /// Single-recipient send helper for tests that don't correlate a fan-out's
    /// shared `send_id` — mints a fresh one per call, mirroring the frontend's
    /// per-Send minting.
    async fn send_msg(
        state: &AppState,
        agent_id: AgentId,
        prompt: &str,
    ) -> Result<MessageId, AppError> {
        send_message_impl(state, agent_id, prompt, Vec::new(), Uuid::now_v7()).await
    }

    #[tokio::test]
    async fn init_directory_creates_switchboard_layout() {
        let (tmp, state, _) = fresh_state_with_mock();
        let info = init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(info.has_switchboard);
        assert!(info.projects.is_empty());
        assert!(tmp.path().join(".switchboard").is_dir());
        assert!(tmp.path().join(".switchboard/config.yaml").is_file());
    }

    #[tokio::test]
    async fn adding_a_second_directory_leaves_the_first_intact() {
        // Additive is the new contract (replacing the old "rebind clears
        // everything"): adding a second directory must not disturb the first
        // directory's loaded projects, locks, register-cache, active project,
        // or in-flight one-shots.
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        );

        init_directory_impl(&state, tmp_a.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, proj.id).unwrap();
        let agent =
            create_agent_impl(&state, "assistant", HarnessKind::ClaudeCode, None, None).unwrap();

        // Add a second directory.
        let info_b = init_directory_impl(&state, tmp_b.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(info_b.projects.len(), 0);

        // The first directory's state is fully intact.
        assert_eq!(lock(&state.directories).len(), 2, "both directories loaded");
        assert!(lock(&state.projects).contains_key(&proj.id));
        assert!(lock(&state.project_locks).contains_key(&proj.id));
        assert!(lock(&state.agents_by_id).contains_key(&agent.id));
        assert_eq!(*lock(&state.active_project_id), Some(proj.id));

        // Sending to the original agent still resolves and dispatches.
        send_msg(&state, agent.id, "still works").await.unwrap();
    }

    #[tokio::test]
    async fn init_directory_is_idempotent() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        // Second call must succeed and preserve any created projects.
        create_project_in_only_dir(&state, "alpha");
        let info = init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "alpha");
    }

    #[test]
    fn list_projects_with_empty_workspace_is_empty() {
        // The flat list is workspace-driven: with no directories added, it is
        // an empty list (not an error) — the cross-directory model has no
        // single "bound directory" whose absence is an error.
        let (_tmp, state, _) = fresh_state_with_mock();
        assert!(list_projects_impl(&state).unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_open_set_active_round_trip() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let summary = create_project_in_only_dir(&state, "alpha");
        // open_project on an already-loaded project is a no-op equivalent.
        let reopened = open_project_impl(&state, summary.id).unwrap();
        assert_eq!(reopened.id, summary.id);
        set_active_project_impl(&state, summary.id).unwrap();
        assert_eq!(
            *lock(&state.active_project_id),
            Some(summary.id),
            "active project set"
        );
    }

    #[tokio::test]
    async fn set_active_project_rejects_unloaded() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let unknown = Uuid::now_v7();
        let err = set_active_project_impl(&state, unknown).unwrap_err();
        assert!(matches!(err, AppError::ProjectNotLoaded(_)));
    }

    #[tokio::test]
    async fn create_agent_without_active_project_errors() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let err = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::NoActiveProject));
    }

    #[tokio::test]
    async fn send_message_dispatches_and_emits_events() {
        let (tmp, state, emitter) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, project.id).unwrap();
        let agent = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
            None,
            None,
        )
        .unwrap();

        let message_id = send_msg(&state, agent.id, "hello").await.unwrap();
        // Fire-and-forget actor model: await the turn completing by waiting for
        // its terminal `agent_idle` (the dispatcher's last event for the turn)
        // rather than a join handle.
        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;

        let events = emitter.snapshot();
        assert!(!events.is_empty(), "expected events to be emitted");
        let channel = format!("agent:{}", agent.id);
        for (name, _) in &events {
            assert_eq!(name, &channel);
        }
        // The first event is the dispatcher-owned TurnStart, and it carries the
        // `message_id` returned by `send_message_impl` (replaces the old
        // `DispatchHandle.turn_id` correlation assertion).
        assert_eq!(events[0].1["type"], "turn_start");
        assert_eq!(extract_message_id(&events[0].1), message_id);
        // Terminal `agent_idle` was emitted (the actor returned to idle) —
        // the event-based equivalent of the old `agent_status == Idle` check.
        assert_eq!(
            count_type(&events, "agent_idle"),
            1,
            "turn reached idle exactly once"
        );
    }

    #[tokio::test]
    async fn pick_directory_rejects_incompatible_config_version() {
        // Set up a directory with a v99 config — `Directory::config()`
        // returns UnsupportedConfigVersion which we want propagated up
        // through pick_directory so the user can't proceed against a
        // future-schema directory with an older Switchboard build.
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        std::fs::write(tmp.path().join(".switchboard/config.yaml"), "version: 99\n").unwrap();

        let err = pick_directory_impl(tmp.path().to_str().unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                AppError::Core(CoreError::UnsupportedConfigVersion { found: 99, .. })
            ),
            "expected UnsupportedConfigVersion(99), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_create_project_same_name_serializes_via_registry_write_lock() {
        // TOCTOU regression: two concurrent IPC calls for create_project
        // with the same name must not both succeed. Without the
        // registry_write mutex, both could pass the uniqueness check
        // before either writes the index. With the mutex, exactly one
        // succeeds and one returns DuplicateProjectName.
        let tmp = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = Arc::new(AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        ));
        let info = init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let dir_path = info.path;

        let state_a = Arc::clone(&state);
        let state_b = Arc::clone(&state);
        let dir_a = dir_path.clone();
        let dir_b = dir_path;
        // Run on real threads so the mutex contention is real (not
        // single-threaded cooperative scheduling). The work inside
        // create_project_impl is synchronous once it enters the locked
        // section.
        let a = tokio::task::spawn_blocking(move || {
            create_project_impl(&state_a, "shared-name", &dir_a)
        });
        let b = tokio::task::spawn_blocking(move || {
            create_project_impl(&state_b, "shared-name", &dir_b)
        });
        let results = [a.await.unwrap(), b.await.unwrap()];

        let successes = results.iter().filter(|r| r.is_ok()).count();
        let dup_errors = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Err(AppError::Core(CoreError::DuplicateProjectName { .. }))
                )
            })
            .count();
        assert_eq!(successes, 1, "exactly one create must succeed: {results:?}");
        assert_eq!(
            dup_errors, 1,
            "the other must return DuplicateProjectName: {results:?}"
        );
    }

    #[tokio::test]
    async fn send_message_unknown_agent_errors() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, project.id).unwrap();
        let err = send_msg(&state, Uuid::now_v7(), "hi").await.unwrap_err();
        assert!(matches!(err, AppError::AgentNotFound(_)));
    }

    #[test]
    fn check_claude_binary_with_mock_adapter_returns_ok() {
        let (_tmp, state, _) = fresh_state_with_mock();
        assert!(check_claude_binary_impl(&state).is_ok());
    }

    #[test]
    fn check_claude_binary_with_missing_binary_returns_error() {
        let claude: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::with_binary_path(
            "/nonexistent/claude-xyz",
        ));
        let codex: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let gemini: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let antigravity: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let state = AppState::new(
            claude,
            codex,
            gemini,
            antigravity,
            emitter as Arc<dyn EventEmitter>,
        );
        let err = check_claude_binary_impl(&state).unwrap_err();
        assert!(matches!(err, AppError::Probe(_)));
    }

    #[test]
    fn check_codex_binary_with_mock_adapter_returns_ok() {
        let (_tmp, state, _) = fresh_state_with_mock();
        assert!(check_codex_binary_impl(&state).is_ok());
    }

    #[test]
    fn check_codex_auth_returns_ok_when_auth_json_exists() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        std::fs::write(tmp.path().join(".codex/auth.json"), "{}").unwrap();
        assert!(check_codex_auth_impl(tmp.path()).is_ok());
    }

    #[test]
    fn check_codex_auth_returns_error_when_auth_json_missing() {
        let tmp = TempDir::new().unwrap();
        let err = check_codex_auth_impl(tmp.path()).unwrap_err();
        match err {
            AppError::AuthNotConfigured {
                harness,
                expected_path,
            } => {
                assert_eq!(harness, HarnessKind::Codex);
                assert!(expected_path.contains(".codex"));
                assert!(expected_path.ends_with("auth.json"));
            }
            other => panic!("expected AuthNotConfigured, got {other:?}"),
        }
    }

    /// Drift-detection live test: if Codex moves its auth file (e.g., into
    /// the macOS keychain), this assertion fails on the developer's machine
    /// before the silent-misclassification regression ships to users. The
    /// fixture tests above prove the path-existence check works; this one
    /// proves the assumed path is still the path the real CLI writes to.
    ///
    /// Run with: `make test-live`.
    #[test]
    #[ignore = "requires codex login — run with: make test-live"]
    fn live_codex_check_auth_finds_real_auth_file() {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .expect("HOME must be set");
        check_codex_auth_impl(&home).expect(
            "Codex auth.json must live at ~/.codex/auth.json on a logged-in machine; \
             if this fails, `codex login` may have changed the auth file location",
        );
    }

    #[test]
    fn check_codex_binary_with_missing_binary_returns_error() {
        use switchboard_harness::CodexAdapter;
        let claude: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let codex: Arc<dyn HarnessAdapter> =
            Arc::new(CodexAdapter::with_binary_path("/nonexistent/codex-xyz"));
        let gemini: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let antigravity: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let state = AppState::new(
            claude,
            codex,
            gemini,
            antigravity,
            emitter as Arc<dyn EventEmitter>,
        );
        let err = check_codex_binary_impl(&state).unwrap_err();
        assert!(matches!(err, AppError::Probe(_)));
    }

    #[test]
    fn check_gemini_binary_with_mock_adapter_returns_ok() {
        let (_tmp, state, _) = fresh_state_with_mock();
        assert!(check_gemini_binary_impl(&state).is_ok());
    }

    #[test]
    fn check_gemini_binary_with_missing_binary_returns_error() {
        use switchboard_harness::GeminiAdapter;
        let claude: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let codex: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let gemini: Arc<dyn HarnessAdapter> =
            Arc::new(GeminiAdapter::with_binary_path("/nonexistent/gemini-xyz"));
        let emitter = Arc::new(RecordingEmitter::new());
        let antigravity: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let state = AppState::new(
            claude,
            codex,
            gemini,
            antigravity,
            emitter as Arc<dyn EventEmitter>,
        );
        let err = check_gemini_binary_impl(&state).unwrap_err();
        assert!(matches!(err, AppError::Probe(_)));
    }

    fn stage_gemini_settings(home: &Path, body: &str) {
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        std::fs::write(home.join(".gemini").join("settings.json"), body).unwrap();
    }

    #[test]
    fn check_gemini_auth_returns_error_when_settings_missing() {
        let tmp = TempDir::new().unwrap();
        let err = check_gemini_auth_impl(tmp.path()).unwrap_err();
        match err {
            AppError::AuthNotConfigured {
                harness,
                expected_path,
            } => {
                assert_eq!(harness, HarnessKind::Gemini);
                assert!(expected_path.contains(".gemini"));
                assert!(expected_path.ends_with("settings.json"));
            }
            other => panic!("expected AuthNotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn check_gemini_auth_returns_error_when_selected_type_missing() {
        let tmp = TempDir::new().unwrap();
        stage_gemini_settings(tmp.path(), r#"{"security":{"auth":{}}}"#);
        assert!(matches!(
            check_gemini_auth_impl(tmp.path()),
            Err(AppError::AuthNotConfigured { .. })
        ));
    }

    #[test]
    fn check_gemini_auth_returns_error_when_selected_type_unknown() {
        // Fail-closed: unknown auth type surfaces as not-authenticated
        // rather than silently allowing the user past the gate.
        let tmp = TempDir::new().unwrap();
        stage_gemini_settings(
            tmp.path(),
            r#"{"security":{"auth":{"selectedType":"future-method"}}}"#,
        );
        assert!(matches!(
            check_gemini_auth_impl(tmp.path()),
            Err(AppError::AuthNotConfigured { .. })
        ));
    }

    #[test]
    fn check_gemini_auth_accepts_each_supported_selected_type() {
        for selected in &["oauth-personal", "gemini-api-key", "vertex-ai", "workspace"] {
            let tmp = TempDir::new().unwrap();
            stage_gemini_settings(
                tmp.path(),
                &format!(r#"{{"security":{{"auth":{{"selectedType":"{selected}"}}}}}}"#),
            );
            assert!(
                check_gemini_auth_impl(tmp.path()).is_ok(),
                "expected Ok for selected_type={selected}"
            );
        }
    }

    #[test]
    fn interpret_antigravity_keychain_probe_ok_true_returns_ok() {
        assert!(interpret_antigravity_keychain_probe(&Ok(true)).is_ok());
    }

    #[test]
    fn interpret_antigravity_keychain_probe_ok_false_returns_auth_not_configured() {
        let err = interpret_antigravity_keychain_probe(&Ok(false)).unwrap_err();
        match err {
            AppError::AuthNotConfigured {
                harness,
                expected_path,
            } => {
                assert_eq!(harness, HarnessKind::Antigravity);
                // The message references Keychain, not a file path —
                // Antigravity's auth lives in the macOS Keychain, and the
                // string the user sees must communicate that.
                assert!(
                    expected_path.contains("Keychain"),
                    "expected_path should reference Keychain: {expected_path}"
                );
                assert!(
                    expected_path.contains("gemini"),
                    "expected_path should pin the surprising service name: {expected_path}"
                );
                assert!(
                    expected_path.contains("antigravity"),
                    "expected_path should pin the account name: {expected_path}"
                );
            }
            other => panic!("expected AuthNotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn interpret_antigravity_keychain_probe_io_error_returns_auth_not_configured() {
        // Simulates `security` itself missing (non-macOS host, etc.).
        // Auth is reported as missing, which is the correct user-facing
        // outcome — Antigravity is macOS-only in v1, and a missing
        // `security` CLI means we cannot demonstrate authentication.
        let probe_result = Err::<bool, _>(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "security missing",
        ));
        let err = interpret_antigravity_keychain_probe(&probe_result).unwrap_err();
        assert!(matches!(err, AppError::AuthNotConfigured { .. }));
    }

    /// Drift-detection live test: if Antigravity moves its auth from the
    /// macOS Keychain or changes the service/account name, this assertion
    /// fails on the developer's machine before silent miscategorization
    /// ships.
    #[test]
    #[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
    fn live_antigravity_check_auth_finds_real_keychain_entry() {
        check_antigravity_auth_impl().expect(
            "Antigravity Keychain entry must live at service=gemini account=antigravity on a \
             logged-in machine; if this fails, Antigravity may have changed its keychain \
             naming or removed Keychain-based auth entirely",
        );
    }

    #[test]
    fn interpret_claude_keychain_probe_ok_true_returns_ok() {
        assert!(interpret_claude_keychain_probe(&Ok(true)).is_ok());
    }

    #[test]
    fn interpret_claude_keychain_probe_ok_false_returns_auth_not_configured() {
        let err = interpret_claude_keychain_probe(&Ok(false)).unwrap_err();
        match err {
            AppError::AuthNotConfigured {
                harness,
                expected_path,
            } => {
                assert_eq!(harness, HarnessKind::ClaudeCode);
                assert!(
                    expected_path.contains("Keychain"),
                    "expected_path should reference Keychain: {expected_path}"
                );
                assert!(
                    expected_path.contains("Claude Code-credentials"),
                    "expected_path should pin the service name: {expected_path}"
                );
            }
            other => panic!("expected AuthNotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn interpret_claude_keychain_probe_io_error_returns_auth_not_configured() {
        // Simulates `security` missing (non-macOS host). Auth reports missing,
        // which is correct: the heuristic is macOS-only in v1.
        let probe_result = Err::<bool, _>(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "security missing",
        ));
        let err = interpret_claude_keychain_probe(&probe_result).unwrap_err();
        assert!(matches!(err, AppError::AuthNotConfigured { .. }));
    }

    /// Drift-detection live test: if Claude Code moves its credentials out of
    /// the macOS Keychain or renames the service, this fails on a logged-in
    /// machine before the presence heuristic silently starts reporting ✗.
    #[test]
    #[ignore = "requires claude auth login — run with: make test-live"]
    fn live_claude_check_auth_finds_real_keychain_entry() {
        check_claude_auth_impl().expect(
            "Claude Keychain entry must live at service=\"Claude Code-credentials\" on a \
             logged-in machine; if this fails, `claude auth login` may have changed its keychain \
             naming or moved off Keychain auth",
        );
    }

    #[test]
    fn install_status_for_mock_reports_installed_without_version() {
        // Mock adapter probes Ok and reports no version — the "installed but
        // version unknown" composition.
        let status = install_status_for(&MockHarnessAdapter::new());
        assert_eq!(
            status,
            HarnessInstallStatus {
                installed: true,
                version: None,
            }
        );
    }

    #[test]
    fn install_status_for_missing_binary_reports_not_installed() {
        let adapter = ClaudeCodeAdapter::with_binary_path("/nonexistent/claude-xyz123");
        let status = install_status_for(&adapter);
        assert_eq!(
            status,
            HarnessInstallStatus {
                installed: false,
                version: None,
            }
        );
    }

    #[test]
    fn install_status_for_present_binary_reports_version() {
        // `cargo` is guaranteed present wherever `cargo test` runs and supports
        // `--version`; it stands in for a real harness CLI to exercise the
        // installed-and-versioned branch deterministically without a login.
        let adapter = ClaudeCodeAdapter::with_binary_path("cargo");
        let status = install_status_for(&adapter);
        assert!(status.installed);
        // `cargo --version` prints "cargo 1.xx.y"; we surface just the parsed
        // number, not the binary name.
        let version = status
            .version
            .expect("cargo --version should report a number");
        assert!(
            version.starts_with(|c: char| c.is_ascii_digit()) && version.contains('.'),
            "version should be a dotted number, not the raw line: {version}"
        );
    }

    #[test]
    fn get_harness_install_status_routes_per_harness() {
        // Claude pointed at a missing binary; the others mocked (installed).
        let claude: Arc<dyn HarnessAdapter> = Arc::new(ClaudeCodeAdapter::with_binary_path(
            "/nonexistent/claude-xyz123",
        ));
        let codex: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let gemini: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let antigravity: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(claude, codex, gemini, antigravity, emitter);

        assert!(!get_harness_install_status_impl(&state, HarnessKind::ClaudeCode).installed);
        assert!(get_harness_install_status_impl(&state, HarnessKind::Codex).installed);
    }

    /// Drift-detection live test: if `agy` is renamed or moved off PATH,
    /// surface here before users see a confusing dispatch-time error.
    #[test]
    #[ignore = "requires agy installed — run with: make test-live"]
    fn live_antigravity_check_binary_finds_real_agy_on_path() {
        use switchboard_harness::AntigravityAdapter;
        let claude: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let codex: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let gemini: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let antigravity: Arc<dyn HarnessAdapter> = Arc::new(AntigravityAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            claude,
            codex,
            gemini,
            antigravity,
            emitter as Arc<dyn EventEmitter>,
        );
        check_antigravity_binary_impl(&state)
            .expect("agy binary must be on PATH; install from https://antigravity.google/download");
    }

    #[test]
    fn check_antigravity_binary_with_missing_binary_returns_error() {
        use switchboard_harness::AntigravityAdapter;
        let claude: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let codex: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let gemini: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let antigravity: Arc<dyn HarnessAdapter> =
            Arc::new(AntigravityAdapter::with_binary_path("/nonexistent/agy-xyz"));
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            claude,
            codex,
            gemini,
            antigravity,
            emitter as Arc<dyn EventEmitter>,
        );
        let err = check_antigravity_binary_impl(&state).unwrap_err();
        assert!(matches!(err, AppError::Probe(_)));
    }

    /// Drift-detection live test: if Gemini moves its auth file or
    /// renames the `security.auth.selectedType` key, this assertion fails
    /// on the developer's machine before silent miscategorization ships.
    #[test]
    #[ignore = "requires gemini login — run with: make test-live"]
    fn live_gemini_check_auth_finds_real_settings_file() {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .expect("HOME must be set");
        check_gemini_auth_impl(&home).expect(
            "Gemini settings.json must live at ~/.gemini/settings.json with a supported \
             `security.auth.selectedType` on a logged-in machine; if this fails, the \
             Gemini CLI may have moved its auth file or renamed the field",
        );
    }

    #[tokio::test]
    async fn list_agents_defaults_to_active_project() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj_a = create_project_in_only_dir(&state, "alpha");
        let proj_b = create_project_in_only_dir(&state, "beta");
        set_active_project_impl(&state, proj_a.id).unwrap();
        create_agent_impl(
            &state,
            "a-agent",
            switchboard_core::HarnessKind::ClaudeCode,
            None,
            None,
        )
        .unwrap();
        set_active_project_impl(&state, proj_b.id).unwrap();
        create_agent_impl(
            &state,
            "b-agent",
            switchboard_core::HarnessKind::ClaudeCode,
            None,
            None,
        )
        .unwrap();

        // Default = active project (beta).
        let agents = list_agents_impl(&state, None).unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "b-agent");

        // Explicit project id returns that project's agents.
        let agents_a = list_agents_impl(&state, Some(proj_a.id)).unwrap();
        assert_eq!(agents_a.len(), 1);
        assert_eq!(agents_a[0].name, "a-agent");
    }

    /// Test-only adapter that emits a `ContentChunk` containing a known tag
    /// and counts how many times it has been dispatched to. Used by the
    /// app routing test below to prove that `send_message_impl` selects
    /// the right adapter based on `agent.harness`.
    struct TaggedMockAdapter {
        tag: &'static str,
        dispatch_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl HarnessAdapter for TaggedMockAdapter {
        fn probe(&self) -> Result<(), switchboard_harness::DispatchError> {
            Ok(())
        }

        fn version(&self) -> Option<String> {
            None
        }

        async fn dispatch(
            &self,
            _agent: &AgentRecord,
            _cwd: &Path,
            _prompt: &str,
            turn_id: switchboard_harness::TurnId,
            _options: switchboard_harness::DispatchOptions,
        ) -> Result<switchboard_harness::EventStream, switchboard_harness::DispatchError> {
            self.dispatch_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let tag = self.tag.to_owned();
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                let _ = tx.send(switchboard_harness::AdapterEvent::ContentChunk {
                    turn_id,
                    kind: switchboard_harness::ContentKind::Text,
                    text: tag,
                });
                let _ = tx.send(switchboard_harness::AdapterEvent::TurnEnd {
                    turn_id,
                    outcome: switchboard_harness::TurnOutcome::Completed,
                    ended_at: chrono::Utc::now(),
                    usage: None,
                    context_window_source: None,
                    stable_message_id: None,
                    first_message_id: None,
                    spend: None,
                    model: None,
                    effort: None,
                });
            });
            Ok(Box::pin(
                tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
            ))
        }
    }

    /// App routing test. The dispatcher is harness-agnostic (keyed by
    /// `AgentId` alone), so adapter cross-talk is structurally impossible
    /// there. The substantive failure mode is at the App layer:
    /// `send_message_impl` selects an adapter via `match agent.harness`,
    /// and a regression that hard-codes one adapter would silently spawn
    /// the wrong binary. This test pins that routing against regression
    /// using four distinguishable adapters tagged per harness.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Four harnesses × (construct + dispatch + assert) is inherently long but linear.
    async fn send_message_routes_to_adapter_matching_agent_harness() {
        let claude_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let codex_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gemini_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let antigravity_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let claude: Arc<dyn HarnessAdapter> = Arc::new(TaggedMockAdapter {
            tag: "from-claude-adapter",
            dispatch_count: claude_count.clone(),
        });
        let codex: Arc<dyn HarnessAdapter> = Arc::new(TaggedMockAdapter {
            tag: "from-codex-adapter",
            dispatch_count: codex_count.clone(),
        });
        let gemini: Arc<dyn HarnessAdapter> = Arc::new(TaggedMockAdapter {
            tag: "from-gemini-adapter",
            dispatch_count: gemini_count.clone(),
        });
        let antigravity: Arc<dyn HarnessAdapter> = Arc::new(TaggedMockAdapter {
            tag: "from-antigravity-adapter",
            dispatch_count: antigravity_count.clone(),
        });
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            claude,
            codex,
            gemini,
            antigravity,
            emitter.clone() as Arc<dyn EventEmitter>,
        );
        let tmp = TempDir::new().unwrap();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, proj.id).unwrap();
        let claude_agent =
            create_agent_impl(&state, "c1", HarnessKind::ClaudeCode, None, None).unwrap();
        let codex_agent = create_agent_impl(&state, "x1", HarnessKind::Codex, None, None).unwrap();
        let gemini_agent =
            create_agent_impl(&state, "g1", HarnessKind::Gemini, None, None).unwrap();
        let antigravity_agent =
            create_agent_impl(&state, "a1", HarnessKind::Antigravity, None, None).unwrap();

        // Four distinct agents → four independent actors. Each
        // `send_message_impl` returns immediately; await each agent's turn
        // completing via the cumulative `agent_idle` count (one per agent).
        send_msg(&state, claude_agent.id, "hi").await.unwrap();
        within(
            &emitter,
            "claude agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;
        send_msg(&state, codex_agent.id, "hi").await.unwrap();
        within(
            &emitter,
            "codex agent_idle",
            emitter.wait_for_type("agent_idle", 2),
        )
        .await;
        send_msg(&state, gemini_agent.id, "hi").await.unwrap();
        within(
            &emitter,
            "gemini agent_idle",
            emitter.wait_for_type("agent_idle", 3),
        )
        .await;
        send_msg(&state, antigravity_agent.id, "hi").await.unwrap();
        within(
            &emitter,
            "antigravity agent_idle",
            emitter.wait_for_type("agent_idle", 4),
        )
        .await;

        assert_eq!(
            claude_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "ClaudeCode agent dispatch must hit the Claude adapter exactly once"
        );
        assert_eq!(
            codex_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Codex agent dispatch must hit the Codex adapter exactly once"
        );
        assert_eq!(
            gemini_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Gemini agent dispatch must hit the Gemini adapter exactly once"
        );
        assert_eq!(
            antigravity_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Antigravity agent dispatch must hit the Antigravity adapter exactly once"
        );

        // Secondary check: the emitted ContentChunk tags match the
        // adapter-of-origin per agent_id. Catches mis-routing where dispatch
        // counts are still 1/1/1/1 but the wrong adapter served each.
        let events = emitter.snapshot();
        let claude_channel = format!("agent:{}", claude_agent.id);
        let codex_channel = format!("agent:{}", codex_agent.id);
        let gemini_channel = format!("agent:{}", gemini_agent.id);
        let claude_text = events
            .iter()
            .find(|(name, payload)| name == &claude_channel && payload["type"] == "content_chunk")
            .expect("content_chunk on claude channel");
        let codex_text = events
            .iter()
            .find(|(name, payload)| name == &codex_channel && payload["type"] == "content_chunk")
            .expect("content_chunk on codex channel");
        let gemini_text = events
            .iter()
            .find(|(name, payload)| name == &gemini_channel && payload["type"] == "content_chunk")
            .expect("content_chunk on gemini channel");
        let antigravity_channel = format!("agent:{}", antigravity_agent.id);
        let antigravity_text = events
            .iter()
            .find(|(name, payload)| {
                name == &antigravity_channel && payload["type"] == "content_chunk"
            })
            .expect("content_chunk on antigravity channel");
        assert_eq!(claude_text.1["text"], "from-claude-adapter");
        assert_eq!(codex_text.1["text"], "from-codex-adapter");
        assert_eq!(gemini_text.1["text"], "from-gemini-adapter");
        assert_eq!(antigravity_text.1["text"], "from-antigravity-adapter");
    }

    #[tokio::test]
    async fn needs_session_meta_persists_when_no_session_meta_observed() {
        // Read-don't-drain: a successful dispatch that does NOT carry a
        // session_meta event must leave the flag intact, so a follow-up
        // dispatch still forces SessionMeta.
        let (tmp, state, emitter) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "a", HarnessKind::ClaudeCode, None, None).unwrap();
        lock(&state.needs_session_meta).insert(agent.id);

        send_msg(&state, agent.id, "hi").await.unwrap();
        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;

        // MockHarnessAdapter's Streaming scenario emits TurnStart + chunks +
        // TurnEnd + AgentIdle — no SessionMeta — so the decorator never fires
        // and the flag must survive.
        assert!(
            lock(&state.needs_session_meta).contains(&agent.id),
            "flag must persist when no session_meta was observed on the wire"
        );
    }

    #[tokio::test]
    async fn needs_session_meta_persists_through_pre_stream_error() {
        // Pre-stream failure paths (binary missing, spawn failure) also leave
        // the flag set: read-don't-drain means there's nothing to "restore"
        // — the flag was never moved. Under the actor model the send is
        // accepted synchronously and the dispatch failure surfaces as a
        // `MessageFailed` event (the async analogue of the old pre-stream
        // `Err`); the flag-persistence behavior is unchanged.
        use switchboard_harness::MockScenario;
        let failing: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::with_scenario(
            MockScenario::DispatchFails,
        ));
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&failing),
            Arc::clone(&failing),
            Arc::clone(&failing),
            Arc::clone(&failing),
            emitter.clone() as Arc<dyn EventEmitter>,
        );
        let tmp = TempDir::new().unwrap();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "a", HarnessKind::ClaudeCode, None, None).unwrap();
        lock(&state.needs_session_meta).insert(agent.id);

        // Routing succeeds → the send is accepted; the dispatch failure is
        // reported asynchronously via `message_failed`, keyed by this id.
        let message_id = send_msg(&state, agent.id, "hi").await.unwrap();
        within(
            &emitter,
            "message_failed",
            emitter.wait_for_type("message_failed", 1),
        )
        .await;
        let failed = emitter
            .snapshot()
            .into_iter()
            .find(|(_, v)| v["type"] == "message_failed")
            .expect("a message_failed event");
        assert_eq!(
            extract_message_id(&failed.1),
            message_id,
            "message_failed is keyed by the accepted send's message_id"
        );
        assert!(
            lock(&state.needs_session_meta).contains(&agent.id),
            "flag must persist through pre-stream failure so a retry still forces SessionMeta"
        );
    }

    #[tokio::test]
    async fn needs_session_meta_unset_means_default_flag() {
        // Sanity: agents that never went through attach get
        // is_first_dispatch_after_attach=false (the default). Captured via a
        // recording adapter so we can inspect the DispatchOptions.
        use std::sync::atomic::{AtomicBool, Ordering};

        struct RecordingAdapter {
            saw_flag: Arc<AtomicBool>,
        }

        #[async_trait]
        impl HarnessAdapter for RecordingAdapter {
            fn probe(&self) -> Result<(), switchboard_harness::DispatchError> {
                Ok(())
            }
            fn version(&self) -> Option<String> {
                None
            }
            async fn dispatch(
                &self,
                _agent: &AgentRecord,
                _cwd: &Path,
                _prompt: &str,
                turn_id: switchboard_harness::TurnId,
                options: switchboard_harness::DispatchOptions,
            ) -> Result<switchboard_harness::EventStream, switchboard_harness::DispatchError>
            {
                self.saw_flag
                    .store(options.is_first_dispatch_after_attach, Ordering::SeqCst);
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                tokio::spawn(async move {
                    let _ = tx.send(switchboard_harness::AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: switchboard_harness::TurnOutcome::Completed,
                        ended_at: chrono::Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
                Ok(Box::pin(
                    tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
                ))
            }
        }

        let saw_flag = Arc::new(AtomicBool::new(false));
        let adapter: Arc<dyn HarnessAdapter> = Arc::new(RecordingAdapter {
            saw_flag: saw_flag.clone(),
        });
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&adapter),
            Arc::clone(&adapter),
            Arc::clone(&adapter),
            Arc::clone(&adapter),
            emitter.clone() as Arc<dyn EventEmitter>,
        );
        let tmp = TempDir::new().unwrap();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, proj.id).unwrap();
        let agent_default =
            create_agent_impl(&state, "a", HarnessKind::ClaudeCode, None, None).unwrap();
        send_msg(&state, agent_default.id, "hi").await.unwrap();
        within(
            &emitter,
            "first agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;
        assert!(
            !saw_flag.load(Ordering::SeqCst),
            "default send must pass is_first_dispatch_after_attach=false"
        );

        // Now stash the flag and re-send for the same agent — adapter must see
        // true. Sends to the same agent chain through one actor, so await the
        // second turn's own idle (cumulative count 2) before asserting.
        lock(&state.needs_session_meta).insert(agent_default.id);
        send_msg(&state, agent_default.id, "again").await.unwrap();
        within(
            &emitter,
            "second agent_idle",
            emitter.wait_for_type("agent_idle", 2),
        )
        .await;
        assert!(
            saw_flag.load(Ordering::SeqCst),
            "post-attach send must pass is_first_dispatch_after_attach=true"
        );
    }

    #[tokio::test]
    async fn needs_session_meta_clears_only_after_session_meta_is_observed() {
        // The load-bearing invariant of the read-don't-drain design:
        // - Dispatches #1 and #2 stream + complete WITHOUT emitting
        //   session_meta → flag survives both → adapter sees
        //   `is_first_dispatch_after_attach: true` each time.
        // - Dispatch #3 emits a session_meta event → the decorator clears
        //   the flag mid-stream → flag is gone.
        // - Dispatch #4 sees `is_first_dispatch_after_attach: false`.
        // Captures both directions of the invariant in one sequence so a
        // regression on either side ("drain at start" or "clear without
        // observation") fails this test.
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ProgrammableAdapter {
            dispatch_count: AtomicUsize,
            seen_flags: Arc<Mutex<Vec<bool>>>,
            // Dispatch index (0-based) at which SessionMeta+TurnEnd should be emitted.
            emit_session_meta_at: usize,
        }

        #[async_trait]
        impl HarnessAdapter for ProgrammableAdapter {
            fn probe(&self) -> Result<(), switchboard_harness::DispatchError> {
                Ok(())
            }
            fn version(&self) -> Option<String> {
                None
            }
            async fn dispatch(
                &self,
                agent: &AgentRecord,
                _cwd: &Path,
                _prompt: &str,
                turn_id: switchboard_harness::TurnId,
                options: switchboard_harness::DispatchOptions,
            ) -> Result<switchboard_harness::EventStream, switchboard_harness::DispatchError>
            {
                let index = self.dispatch_count.fetch_add(1, Ordering::SeqCst);
                lock(&self.seen_flags).push(options.is_first_dispatch_after_attach);
                let emit_meta = index == self.emit_session_meta_at;
                let agent_id = agent.id;
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                tokio::spawn(async move {
                    if emit_meta {
                        let _ = tx.send(switchboard_harness::AdapterEvent::SessionMeta {
                            agent_id,
                            model: "test-model".to_owned(),
                            harness_version: "0.0.0".to_owned(),
                            tools: vec![],
                            mcp_servers: vec![],
                            skills: vec![],
                            raw: serde_json::Value::Null,
                        });
                    }
                    let _ = tx.send(switchboard_harness::AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: switchboard_harness::TurnOutcome::Completed,
                        ended_at: chrono::Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
                Ok(Box::pin(
                    tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
                ))
            }
        }

        let seen_flags = Arc::new(Mutex::new(Vec::new()));
        let adapter: Arc<dyn HarnessAdapter> = Arc::new(ProgrammableAdapter {
            dispatch_count: AtomicUsize::new(0),
            seen_flags: Arc::clone(&seen_flags),
            emit_session_meta_at: 2, // 0-based: third dispatch emits SessionMeta
        });
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&adapter),
            Arc::clone(&adapter),
            Arc::clone(&adapter),
            Arc::clone(&adapter),
            emitter.clone() as Arc<dyn EventEmitter>,
        );
        let tmp = TempDir::new().unwrap();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "a", HarnessKind::Codex, None, None).unwrap();
        // Simulate the Codex-attach state: the flag is set on a real attach,
        // but `create_agent_impl` doesn't trigger that path, so set it
        // directly to isolate the read-don't-drain behavior under test.
        lock(&state.needs_session_meta).insert(agent.id);

        // Run four dispatches sequentially. Sends to the same agent chain
        // through one actor; the actor reads the flag when each turn STARTS,
        // so each dispatch must fully complete (await its `agent_idle`) before
        // the next is sent — otherwise the actor could start turn N+1 before
        // turn N's SessionMeta-driven clear lands.
        for completed in 1..=4 {
            send_msg(&state, agent.id, "hi").await.unwrap();
            within(
                &emitter,
                "agent_idle",
                emitter.wait_for_type("agent_idle", completed),
            )
            .await;
        }

        let flags = lock(&seen_flags).clone();
        // Why dispatch #3 sees `true` (not `false`): `send_message_impl`
        // reads the flag at dispatch start, BEFORE the adapter spawns the
        // task that emits SessionMeta. The decorator only clears the
        // flag once SessionMeta flows through the emitter, which happens
        // AFTER `is_first_dispatch_after_attach` has already been
        // captured into `DispatchOptions` for that dispatch. Dispatch #4
        // is the first that observes the cleared flag.
        assert_eq!(
            flags,
            vec![true, true, true, false],
            "flag must persist across dispatches 1+2 (no session_meta) and on dispatch 3 \
             (which emits session_meta); only dispatch 4 — after observation — sees false"
        );
        assert!(
            !lock(&state.needs_session_meta).contains(&agent.id),
            "set must be empty after session_meta is observed"
        );
    }

    #[tokio::test]
    async fn cross_project_concurrent_send_no_cross_talk() {
        let (tmp, state, emitter) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj_a = create_project_in_only_dir(&state, "alpha");
        let proj_b = create_project_in_only_dir(&state, "beta");

        // Two projects in same directory; same agent name in each is fine.
        set_active_project_impl(&state, proj_a.id).unwrap();
        let agent_a = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
            None,
            None,
        )
        .unwrap();
        set_active_project_impl(&state, proj_b.id).unwrap();
        let agent_b = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
            None,
            None,
        )
        .unwrap();

        let (accepted_a, accepted_b) = tokio::join!(
            send_msg(&state, agent_a.id, "A's prompt"),
            send_msg(&state, agent_b.id, "B's prompt"),
        );
        accepted_a.unwrap();
        accepted_b.unwrap();
        // Two independent actors → await both turns reaching idle (cumulative
        // `agent_idle` count of 2 across the two distinct channels).
        within(
            &emitter,
            "both agent_idle",
            emitter.wait_for_type("agent_idle", 2),
        )
        .await;

        let events = emitter.snapshot();
        let ch_a = format!("agent:{}", agent_a.id);
        let ch_b = format!("agent:{}", agent_b.id);
        let a_count = events.iter().filter(|(n, _)| n == &ch_a).count();
        let b_count = events.iter().filter(|(n, _)| n == &ch_b).count();
        // Per channel: TurnStart + 3 ContentChunks + TurnEnd + AgentIdle = 6.
        assert_eq!(a_count, 6, "agent A's channel got the wrong event count");
        assert_eq!(b_count, 6, "agent B's channel got the wrong event count");
    }

    #[tokio::test]
    async fn pick_directory_does_not_create_switchboard_dir() {
        let tmp = TempDir::new().unwrap();
        let info = pick_directory_impl(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(!info.has_switchboard);
        assert!(info.projects.is_empty());
        assert!(
            !tmp.path().join(".switchboard").exists(),
            "pick_directory must not write to disk"
        );
    }

    #[tokio::test]
    async fn pick_directory_lists_projects_when_switchboard_exists() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        create_project_in_only_dir(&state, "alpha");

        // Use a fresh state with no directory bound — pick_directory is
        // stateless, it just inspects the path.
        let info = pick_directory_impl(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(info.has_switchboard);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "alpha");
    }

    #[tokio::test]
    async fn pick_directory_rejects_missing_path() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let err = pick_directory_impl(missing.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Core(_)));
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        let err = parse_uuid("not-a-uuid").unwrap_err();
        assert!(matches!(err, AppError::InvalidUuid { .. }));
    }

    /// Stage a Claude session file under `home_dir` so it matches what the
    /// adapter would expect for the given cwd + `session_id` pair. Returns the
    /// staged path.
    fn stage_claude_session_file(
        home_dir: &Path,
        cwd: &Path,
        session_id: &Uuid,
    ) -> std::path::PathBuf {
        let canonical_cwd = cwd.canonicalize().unwrap();
        let target =
            switchboard_harness::claude_session_file_path(home_dir, &canonical_cwd, session_id);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "{}\n").unwrap();
        target
    }

    /// Stage a Codex rollout file under `home_dir` for the given `session_id`
    /// + date. Returns the staged path.
    fn stage_codex_session_file(
        home_dir: &Path,
        date: chrono::NaiveDate,
        session_id: &str,
    ) -> std::path::PathBuf {
        let dir = home_dir
            .join(".codex")
            .join("sessions")
            .join(date.format("%Y").to_string())
            .join(date.format("%m").to_string())
            .join(date.format("%d").to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-1700000000000-{session_id}.jsonl"));
        std::fs::write(&path, "{}\n").unwrap();
        path
    }

    /// Stage an Antigravity conversation under `home`: the `brain/<uuid>/`
    /// directory always, and (optionally) a minimal one-turn `transcript.jsonl`.
    fn stage_antigravity_conversation(home: &Path, uuid: Uuid, with_transcript: bool) {
        let brain = switchboard_harness::antigravity::paths::conversation_brain_dir(home, uuid);
        std::fs::create_dir_all(&brain).unwrap();
        if with_transcript {
            let transcript = switchboard_harness::antigravity::paths::transcript_path(home, uuid);
            std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
            std::fs::write(
                &transcript,
                concat!(
                    r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
                    "\n",
                    r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:01Z","content":"ack"}"#,
                    "\n",
                ),
            )
            .unwrap();
        }
    }

    async fn fresh_state_with_active_project(
        name: &str,
    ) -> (TempDir, TempDir, AppState, switchboard_core::ProjectSummary) {
        let tmp_workdir = TempDir::new().unwrap();
        let tmp_home = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        );
        init_directory_impl(&state, tmp_workdir.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_in_only_dir(&state, name);
        set_active_project_impl(&state, proj.id).unwrap();
        (tmp_workdir, tmp_home, state, proj)
    }

    #[tokio::test]
    async fn attach_claude_succeeds_when_session_file_exists() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);

        let record = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Uuid(session_id))
        );
        assert_eq!(record.harness, HarnessKind::ClaudeCode);
        // Codex-only invariant: Claude attaches must NOT populate
        // `needs_session_meta`. Claude emits SessionMeta from its
        // `system/init` stream event on every dispatch (see
        // `crates/harness/src/claude_code.rs`), so the override has nothing
        // to do. Pins the asymmetry against "let me just delete the
        // if-match to simplify" refactors.
        assert!(
            !lock(&state.needs_session_meta).contains(&record.id),
            "Claude attach must NOT populate needs_session_meta"
        );
    }

    #[tokio::test]
    async fn attach_claude_rejects_missing_session_file_with_expected_path() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionFileNotFound {
                harness,
                expected_path,
            } => {
                assert_eq!(harness, HarnessKind::ClaudeCode);
                assert!(expected_path.contains(&session_id.to_string()));
                assert!(expected_path.contains(".claude"));
            }
            other => panic!("expected SessionFileNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_rejects_invalid_uuid() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::ClaudeCode,
            "not-a-uuid",
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::InvalidUuid { .. }));
    }

    #[tokio::test]
    async fn attach_codex_writes_locator_to_registry() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        stage_codex_session_file(tmp_home.path(), date, &session_id.to_string());

        let record = attach_agent_impl(
            &state,
            "attached-codex",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        // The thread-id + discovered partition-date are written onto the record
        // (no sidecar).
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Codex {
                thread_id: session_id.to_string(),
                partition_date: date,
            })
        );
        assert!(
            lock(&state.needs_session_meta).contains(&record.id),
            "Codex attach must populate needs_session_meta so first dispatch forces SessionMeta"
        );
    }

    #[tokio::test]
    async fn attach_codex_accepts_non_uuid_thread_id() {
        // Codex thread-ids are arbitrary strings, not guaranteed UUIDs (unlike
        // Claude/Gemini/Antigravity). Attach must use the raw string, not reject
        // a valid session whose rollout filename ends in a non-UUID id.
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let thread_id = "thread-not-a-uuid";
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        stage_codex_session_file(tmp_home.path(), date, thread_id);

        let record = attach_agent_impl(
            &state,
            "attached-codex",
            HarnessKind::Codex,
            thread_id,
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Codex {
                thread_id: thread_id.to_owned(),
                partition_date: date,
            })
        );
    }

    #[tokio::test]
    async fn attach_codex_rejects_missing_session_file() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        let err = attach_agent_impl(
            &state,
            "attached-codex",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionFileNotFound {
                harness,
                expected_path,
            } => {
                assert_eq!(harness, HarnessKind::Codex);
                assert!(expected_path.contains(".codex"));
                assert!(expected_path.contains("rollout-*"));
            }
            other => panic!("expected SessionFileNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_claude_rejects_cross_project_session_id_collision() {
        // Two projects in the same directory. Attach session_id S in alpha;
        // attempt to attach the same S in beta → SessionAlreadyAttached.
        let (tmp_workdir, tmp_home, state, alpha) = fresh_state_with_active_project("alpha").await;
        let beta = create_project_in_only_dir(&state, "beta");
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);

        attach_agent_impl(
            &state,
            "attached",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        set_active_project_impl(&state, beta.id).unwrap();
        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionAlreadyAttached {
                existing_project_name,
                existing_project_id,
                ..
            } => {
                assert_eq!(existing_project_name, "alpha");
                assert_eq!(existing_project_id, alpha.id);
            }
            other => panic!("expected SessionAlreadyAttached, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_claude_rejects_same_project_session_id_collision() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);

        attach_agent_impl(
            &state,
            "first",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        let err = attach_agent_impl(
            &state,
            "second",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::SessionAlreadyAttached { .. }));
    }

    #[tokio::test]
    async fn attach_codex_rejects_cross_project_session_id_collision() {
        let (tmp_workdir, tmp_home, state, _alpha) = fresh_state_with_active_project("alpha").await;
        let beta = create_project_in_only_dir(&state, "beta");
        let session_id = Uuid::now_v7();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        stage_codex_session_file(tmp_home.path(), date, &session_id.to_string());

        attach_agent_impl(
            &state,
            "a",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        set_active_project_impl(&state, beta.id).unwrap();
        let err = attach_agent_impl(
            &state,
            "b",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        // Discovery (existence check) runs before the sidecar collision scan
        // — but here the collision IS the only failure surface (session file
        // still exists). Confirm we surface the collision, not "not found."
        match err {
            AppError::SessionAlreadyAttached {
                existing_project_name,
                ..
            } => {
                assert_eq!(existing_project_name, "alpha");
            }
            other => panic!("expected SessionAlreadyAttached, got {other:?}"),
        }

        let _ = tmp_workdir;
    }

    #[tokio::test]
    async fn attach_rejects_duplicate_name_in_active_project() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        create_agent_impl(&state, "taken", HarnessKind::ClaudeCode, None, None).unwrap();
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);

        let err = attach_agent_impl(
            &state,
            "taken",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(switchboard_core::CoreError::DuplicateAgentName { .. })
        ));
    }

    #[tokio::test]
    async fn attach_codex_surfaces_ambiguous_session_file() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        let id_str = session_id.to_string();
        stage_codex_session_file(
            tmp_home.path(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &id_str,
        );
        stage_codex_session_file(
            tmp_home.path(),
            chrono::NaiveDate::from_ymd_opt(2026, 2, 2).unwrap(),
            &id_str,
        );

        let err = attach_agent_impl(
            &state,
            "attached-codex",
            HarnessKind::Codex,
            &id_str,
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::AmbiguousSessionFile {
                harness,
                session_id: id,
                paths,
            } => {
                assert_eq!(harness, HarnessKind::Codex);
                assert_eq!(id, id_str);
                assert_eq!(paths.len(), 2);
            }
            other => panic!("expected AmbiguousSessionFile, got {other:?}"),
        }
    }

    /// A failed register (duplicate name) must leave the registry unchanged and
    /// write nothing — the locator goes onto the record atomically with the
    /// register, so there's no pre-write to orphan (the old sidecar-first
    /// ordering, and its orphan-sidecar invariant, are gone).
    #[tokio::test]
    async fn attach_codex_register_failure_leaves_registry_unchanged() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();

        let first_session = Uuid::now_v7();
        stage_codex_session_file(tmp_home.path(), date, &first_session.to_string());
        attach_agent_impl(
            &state,
            "taken",
            HarnessKind::Codex,
            &first_session.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        // Second attach: distinct session_id (collision scan passes) +
        // colliding name (register fails).
        let second_session = Uuid::now_v7();
        stage_codex_session_file(tmp_home.path(), date, &second_session.to_string());
        let err = attach_agent_impl(
            &state,
            "taken",
            HarnessKind::Codex,
            &second_session.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(switchboard_core::CoreError::DuplicateAgentName { .. })
        ));

        // Registry has exactly one "taken", bound to the first session — name
        // uniqueness held and the failed attach persisted nothing.
        let agents = list_agents_impl(&state, None).unwrap();
        let taken: Vec<_> = agents.iter().filter(|a| a.name == "taken").collect();
        assert_eq!(
            taken.len(),
            1,
            "registry must not double-add on name collision"
        );
        assert_eq!(
            taken[0].session_locator,
            Some(SessionLocator::Codex {
                thread_id: first_session.to_string(),
                partition_date: date,
            }),
            "the surviving record is the first attach; the second persisted nothing"
        );
    }

    #[tokio::test]
    async fn attach_codex_rejects_same_project_session_id_collision() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        stage_codex_session_file(tmp_home.path(), date, &session_id.to_string());

        attach_agent_impl(
            &state,
            "first",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        let err = attach_agent_impl(
            &state,
            "second",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::SessionAlreadyAttached { .. }));
    }

    /// Collision detection must scan **all on-disk projects**, not just
    /// loaded ones. The hazard the invariant defends against: an unloaded
    /// project's Claude `AgentRecord` can be opened later and dispatched
    /// concurrently with a Switchboard agent in the currently-open project
    /// that targets the same `session_id` — corrupting the harness session
    /// per `docs/research/same-session-parallel-invocation.md`.
    #[tokio::test]
    async fn attach_claude_detects_collision_against_unloaded_project() {
        // Phase 1: create project A in a fresh AppState, attach session-id S.
        let tmp_workdir = TempDir::new().unwrap();
        let tmp_home = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);

        {
            let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
            let emitter = Arc::new(RecordingEmitter::new());
            let state_a = AppState::new(
                Arc::clone(&mock),
                Arc::clone(&mock),
                Arc::clone(&mock),
                Arc::clone(&mock),
                emitter as Arc<dyn EventEmitter>,
            );
            init_directory_impl(&state_a, tmp_workdir.path().to_str().unwrap())
                .await
                .unwrap();
            let proj_a = create_project_in_only_dir(&state_a, "alpha");
            set_active_project_impl(&state_a, proj_a.id).unwrap();
            attach_agent_impl(
                &state_a,
                "attached",
                HarnessKind::ClaudeCode,
                &session_id.to_string(),
                tmp_home.path(),
                None,
                None,
            )
            .unwrap();
        } // state_a dropped — project A's registry is persisted but no longer loaded in any AppState.

        // Phase 2: fresh AppState bound to the same directory. Only open
        // project B; A is on disk but unloaded. Attempt to attach the same
        // session-id in B → must detect the collision against A.
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state_b = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        );
        init_directory_impl(&state_b, tmp_workdir.path().to_str().unwrap())
            .await
            .unwrap();
        let proj_b = create_project_in_only_dir(&state_b, "beta");
        set_active_project_impl(&state_b, proj_b.id).unwrap();

        let err = attach_agent_impl(
            &state_b,
            "attached",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionAlreadyAttached {
                existing_project_name,
                ..
            } => assert_eq!(existing_project_name, "alpha"),
            other => {
                panic!("expected SessionAlreadyAttached against unloaded project, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn attach_codex_detects_collision_against_unloaded_project() {
        let tmp_workdir = TempDir::new().unwrap();
        let tmp_home = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        stage_codex_session_file(tmp_home.path(), date, &session_id.to_string());

        {
            let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
            let emitter = Arc::new(RecordingEmitter::new());
            let state_a = AppState::new(
                Arc::clone(&mock),
                Arc::clone(&mock),
                Arc::clone(&mock),
                Arc::clone(&mock),
                emitter as Arc<dyn EventEmitter>,
            );
            init_directory_impl(&state_a, tmp_workdir.path().to_str().unwrap())
                .await
                .unwrap();
            let proj_a = create_project_in_only_dir(&state_a, "alpha");
            set_active_project_impl(&state_a, proj_a.id).unwrap();
            attach_agent_impl(
                &state_a,
                "attached",
                HarnessKind::Codex,
                &session_id.to_string(),
                tmp_home.path(),
                None,
                None,
            )
            .unwrap();
        }

        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state_b = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        );
        init_directory_impl(&state_b, tmp_workdir.path().to_str().unwrap())
            .await
            .unwrap();
        let proj_b = create_project_in_only_dir(&state_b, "beta");
        set_active_project_impl(&state_b, proj_b.id).unwrap();

        let err = attach_agent_impl(
            &state_b,
            "attached",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionAlreadyAttached {
                existing_project_name,
                ..
            } => assert_eq!(existing_project_name, "alpha"),
            other => {
                panic!("expected SessionAlreadyAttached against unloaded project, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn attach_without_active_project_errors() {
        let (_tmp_workdir, tmp_home, state) = {
            let tmp_workdir = TempDir::new().unwrap();
            let tmp_home = TempDir::new().unwrap();
            let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
            let emitter = Arc::new(RecordingEmitter::new());
            let state = AppState::new(
                Arc::clone(&mock),
                Arc::clone(&mock),
                Arc::clone(&mock),
                Arc::clone(&mock),
                emitter as Arc<dyn EventEmitter>,
            );
            init_directory_impl(&state, tmp_workdir.path().to_str().unwrap())
                .await
                .unwrap();
            (tmp_workdir, tmp_home, state)
        };
        let err = attach_agent_impl(
            &state,
            "x",
            HarnessKind::ClaudeCode,
            &Uuid::now_v7().to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::NoActiveProject));
    }

    #[tokio::test]
    async fn load_transcript_for_claude_agent_with_no_session_file_returns_empty() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        // Stage the file so attach succeeds but the file content is empty.
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);
        let record = attach_agent_impl(
            &state,
            "x",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        let result = load_transcript_impl(&state, record.id, tmp_home.path()).unwrap();
        assert!(result.turns.is_empty());
        assert!(result.warnings.is_empty());
        // No metadata sidecar staged → both rate-limit fields stay None.
        assert!(result.last_rate_limit.is_none());
        assert!(result.last_rate_limit_as_of.is_none());
    }

    #[tokio::test]
    async fn project_session_fingerprints_marks_claude_capable_with_file_and_codex_incapable() {
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;

        // Claude agent with a staged session file → refresh-capable, fingerprint present.
        let session_id = Uuid::now_v7();
        let staged = stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);
        let claude = attach_agent_impl(
            &state,
            "claude",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        // Codex agent (no dispatch yet) → not refresh-capable.
        let codex = create_agent_impl(&state, "codex", HarnessKind::Codex, None, None).unwrap();

        let fps = project_session_fingerprints_impl(&state, proj.id, tmp_home.path()).unwrap();

        let claude_fp = fps.iter().find(|f| f.agent_id == claude.id).unwrap();
        assert!(
            claude_fp.refresh_capable,
            "Claude is the live-matched harness"
        );
        let fp = claude_fp
            .fingerprint
            .as_ref()
            .expect("a staged Claude file yields a fingerprint");
        assert_eq!(fp.source_path, staged.to_string_lossy());
        assert_eq!(fp.byte_len, 3, "the staged `{{}}\\n` is 3 bytes");

        let codex_fp = fps.iter().find(|f| f.agent_id == codex.id).unwrap();
        assert!(!codex_fp.refresh_capable);
        assert!(
            codex_fp.fingerprint.is_none(),
            "non-refresh-capable agents are not statted"
        );
    }

    #[tokio::test]
    async fn project_session_fingerprints_claude_without_file_is_capable_but_unfingerprinted() {
        let (_tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        // Claude agent, no session file on disk → refresh-capable but no fingerprint.
        let claude =
            create_agent_impl(&state, "claude", HarnessKind::ClaudeCode, None, None).unwrap();

        let fps = project_session_fingerprints_impl(&state, proj.id, tmp_home.path()).unwrap();
        let f = fps.iter().find(|f| f.agent_id == claude.id).unwrap();
        assert!(f.refresh_capable);
        assert!(f.fingerprint.is_none(), "no file yet → no fingerprint");
    }

    #[test]
    fn overlay_fills_rate_limit_when_loader_left_it_empty() {
        // Claude-shape: the loader produces no rate_limit (class C); the
        // sidecar fills it and stamps the capture time.
        let mut transcript = switchboard_harness::LoadedTranscript::default();
        let captured = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:42:11Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let sidecar = switchboard_harness::meta_sidecar::MetaSidecar {
            schema_version: 1,
            rate_limit: Some(switchboard_harness::meta_sidecar::RateLimitSnapshot {
                payload: serde_json::json!({"isUsingOverage": true}),
                captured_at: captured,
            }),
            context_window: None,
        };
        apply_meta_sidecar_overlay(&mut transcript, Some(sidecar));
        assert_eq!(
            transcript.last_rate_limit,
            Some(serde_json::json!({"isUsingOverage": true}))
        );
        assert_eq!(transcript.last_rate_limit_as_of, Some(captured));
    }

    #[test]
    fn overlay_does_not_override_loader_provided_rate_limit() {
        // Codex-shape (class B): the loader already populated last_rate_limit
        // from the session file (durable, authoritative). A stray sidecar
        // must NOT override it, and no `as_of` qualifier is added — the
        // session value isn't a stale snapshot.
        let mut transcript = switchboard_harness::LoadedTranscript {
            last_rate_limit: Some(serde_json::json!({"primary": {"used_percent": 10.0}})),
            ..Default::default()
        };
        let sidecar = switchboard_harness::meta_sidecar::MetaSidecar {
            schema_version: 1,
            rate_limit: Some(switchboard_harness::meta_sidecar::RateLimitSnapshot {
                payload: serde_json::json!({"should": "not win"}),
                captured_at: chrono::Utc::now(),
            }),
            context_window: None,
        };
        apply_meta_sidecar_overlay(&mut transcript, Some(sidecar));
        assert_eq!(
            transcript.last_rate_limit,
            Some(serde_json::json!({"primary": {"used_percent": 10.0}})),
            "class-B session-file value must win over the sidecar"
        );
        assert!(
            transcript.last_rate_limit_as_of.is_none(),
            "a durable class-B value carries no staleness qualifier"
        );
    }

    #[test]
    fn overlay_missing_sidecar_is_a_noop() {
        let mut transcript = switchboard_harness::LoadedTranscript::default();
        apply_meta_sidecar_overlay(&mut transcript, None);
        assert!(transcript.last_rate_limit.is_none());
        assert!(transcript.last_rate_limit_as_of.is_none());
    }

    /// An agent turn carrying `usage` with the given `context_input_tokens` and
    /// `context_window`. `context_input_tokens: None` models a turn that
    /// terminated before any assistant content streamed (the bar skips it).
    fn overlay_agent_turn(
        context_input_tokens: Option<u64>,
        context_window: Option<u32>,
    ) -> switchboard_harness::Turn {
        switchboard_harness::Turn::Agent {
            turn_id: Uuid::now_v7(),
            agent_id: Uuid::now_v7(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: switchboard_harness::TurnStatus::Complete,
            items: vec![],
            usage: Some(switchboard_harness::TurnUsage {
                input_tokens: 100,
                output_tokens: 25,
                cached_input_tokens: None,
                cache_creation_input_tokens: None,
                context_input_tokens,
                reasoning_output_tokens: None,
                context_window,
                total_cost_usd: None,
            }),
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
            stable_message_id: None,
        }
    }

    fn overlay_turn_window(turn: &switchboard_harness::Turn) -> Option<u32> {
        match turn {
            switchboard_harness::Turn::Agent { usage: Some(u), .. } => u.context_window,
            _ => None,
        }
    }

    fn context_window_sidecar(window: u32) -> switchboard_harness::meta_sidecar::MetaSidecar {
        switchboard_harness::meta_sidecar::MetaSidecar {
            schema_version: 1,
            rate_limit: None,
            context_window: Some(switchboard_harness::meta_sidecar::ContextWindowSnapshot {
                context_window: window,
                captured_at: chrono::Utc::now(),
            }),
        }
    }

    #[test]
    fn overlay_fills_context_window_on_latest_agent_turn() {
        // Claude hydrate shape: the session file carries no window (stream-only),
        // so the latest agent turn has usage + context_input_tokens but no
        // window. The snapshot fills it so the bar renders on reopen.
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![
                overlay_agent_turn(Some(100), None),
                overlay_agent_turn(Some(100), None),
            ],
            ..Default::default()
        };
        apply_meta_sidecar_overlay(&mut transcript, Some(context_window_sidecar(200_000)));
        assert_eq!(
            overlay_turn_window(&transcript.turns[1]),
            Some(200_000),
            "the latest qualifying agent turn gets the persisted window"
        );
        assert_eq!(
            overlay_turn_window(&transcript.turns[0]),
            None,
            "only the turn the bar reads is filled"
        );
    }

    #[test]
    fn overlay_skips_latest_turn_lacking_context_input_tokens() {
        // Regression guard for overlay/bar divergence: the LATEST agent turn has
        // usage + window-absent but NO context_input_tokens (e.g. it terminated
        // before any assistant content), so the bar skips it and reads an
        // earlier turn. The overlay must fill that earlier turn — the one the
        // bar actually reads — not the latest. Filling the latest would leave
        // the snapshot unread and the bar blank.
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![
                overlay_agent_turn(Some(100), None), // earlier: qualifies
                overlay_agent_turn(None, None),      // latest: no context_input → bar skips
            ],
            ..Default::default()
        };
        apply_meta_sidecar_overlay(&mut transcript, Some(context_window_sidecar(200_000)));
        assert_eq!(
            overlay_turn_window(&transcript.turns[0]),
            Some(200_000),
            "the earlier turn the bar reads must be filled"
        );
        assert_eq!(
            overlay_turn_window(&transcript.turns[1]),
            None,
            "the latest turn (skipped by the bar) must not be filled"
        );
    }

    #[test]
    fn overlay_context_window_no_qualifying_turn_is_a_noop() {
        // No agent turn with usage → nothing to fill; must not panic or
        // synthesize. (Here: a user-only transcript.)
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![switchboard_harness::Turn::User {
                turn_id: Uuid::now_v7(),
                agent_id: Uuid::now_v7(),
                started_at: chrono::Utc::now(),
                text: "hi".to_owned(),
            }],
            ..Default::default()
        };
        apply_meta_sidecar_overlay(&mut transcript, Some(context_window_sidecar(200_000)));
        assert_eq!(transcript.turns.len(), 1, "no synthetic turn is created");
        assert!(matches!(
            transcript.turns[0],
            switchboard_harness::Turn::User { .. }
        ));
    }

    #[test]
    fn overlay_does_not_override_loader_provided_context_window() {
        // Codex-shape (class B): the session file already supplied the window.
        // The snapshot must not clobber it.
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![overlay_agent_turn(Some(100), Some(272_000))],
            ..Default::default()
        };
        apply_meta_sidecar_overlay(&mut transcript, Some(context_window_sidecar(200_000)));
        assert_eq!(
            overlay_turn_window(&transcript.turns[0]),
            Some(272_000),
            "a loader-provided window must win over the sidecar"
        );
    }

    /// An agent turn carrying the given join key (`stable_message_id`) and an
    /// optional `total_cost_usd`, with `spend: None` (the un-rejoined hydrate
    /// shape the turnmeta overlay fills).
    fn turnmeta_agent_turn(
        message_id: Option<&str>,
        cost: Option<f64>,
    ) -> switchboard_harness::Turn {
        switchboard_harness::Turn::Agent {
            turn_id: Uuid::now_v7(),
            agent_id: Uuid::now_v7(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: switchboard_harness::TurnStatus::Complete,
            items: vec![],
            usage: Some(switchboard_harness::TurnUsage {
                input_tokens: 100,
                output_tokens: 25,
                cached_input_tokens: None,
                cache_creation_input_tokens: None,
                context_input_tokens: Some(100),
                reasoning_output_tokens: None,
                context_window: None,
                total_cost_usd: cost,
            }),
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
            stable_message_id: message_id.map(str::to_owned),
        }
    }

    fn turnmeta_record(
        message_id: &str,
        cost: Option<f64>,
        is_overage: bool,
    ) -> switchboard_harness::turnmeta_sidecar::TurnMetaRecord {
        switchboard_harness::turnmeta_sidecar::TurnMetaRecord {
            message_id: message_id.to_owned(),
            total_cost_usd: cost,
            spend: switchboard_harness::TurnSpend {
                real_spend: is_overage,
                is_overage,
                overage_resets_at: None,
            },
            captured_at: chrono::Utc::now(),
        }
    }

    fn turn_spend_and_cost(turn: &switchboard_harness::Turn) -> (Option<bool>, Option<f64>) {
        match turn {
            switchboard_harness::Turn::Agent { spend, usage, .. } => (
                spend.as_ref().map(|s| s.is_overage),
                usage.as_ref().and_then(|u| u.total_cost_usd),
            ),
            _ => (None, None),
        }
    }

    #[test]
    fn turnmeta_overlay_fills_spend_and_cost_by_message_id() {
        // The reopen join: a persisted record keyed on the turn's message id
        // fills both the overage `spend` and the `total_cost_usd` so the inline
        // cost + marker re-render exactly as they did live.
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![
                turnmeta_agent_turn(Some("msg_other"), None),
                turnmeta_agent_turn(Some("msg_test31"), None),
            ],
            ..Default::default()
        };
        apply_turnmeta_overlay(
            &mut transcript,
            &[turnmeta_record("msg_test31", Some(0.0125), true)],
        );
        assert_eq!(
            turn_spend_and_cost(&transcript.turns[1]),
            (Some(true), Some(0.0125)),
            "the matching turn gets the persisted overage + cost"
        );
        assert_eq!(
            turn_spend_and_cost(&transcript.turns[0]),
            (None, None),
            "a turn with no matching record is untouched"
        );
    }

    #[test]
    fn turnmeta_overlay_no_matching_record_is_noop() {
        // A pre-feature / non-Claude turn (no record for its id) renders
        // neither cost nor marker.
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![turnmeta_agent_turn(Some("msg_unmatched"), None)],
            ..Default::default()
        };
        apply_turnmeta_overlay(
            &mut transcript,
            &[turnmeta_record("msg_test31", Some(0.0125), true)],
        );
        assert_eq!(turn_spend_and_cost(&transcript.turns[0]), (None, None));
    }

    #[test]
    fn turnmeta_overlay_empty_records_is_noop() {
        // Missing/corrupt log (read returns empty) → no-op, no panic.
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![turnmeta_agent_turn(Some("msg_test31"), None)],
            ..Default::default()
        };
        apply_turnmeta_overlay(&mut transcript, &[]);
        assert_eq!(turn_spend_and_cost(&transcript.turns[0]), (None, None));
    }

    #[test]
    fn turnmeta_overlay_does_not_override_existing_spend() {
        // Defensive fill-if-empty: a turn that already carries spend/cost (e.g.
        // a live stamp that somehow persisted to the loaded turn) wins over the
        // record — the join never clobbers an existing value.
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![{
                let mut turn = turnmeta_agent_turn(Some("msg_test31"), Some(9.99));
                if let switchboard_harness::Turn::Agent { spend, .. } = &mut turn {
                    *spend = Some(switchboard_harness::TurnSpend {
                        real_spend: true,
                        is_overage: false,
                        overage_resets_at: None,
                    });
                }
                turn
            }],
            ..Default::default()
        };
        apply_turnmeta_overlay(
            &mut transcript,
            &[turnmeta_record("msg_test31", Some(0.0125), true)],
        );
        assert_eq!(
            turn_spend_and_cost(&transcript.turns[0]),
            (Some(false), Some(9.99)),
            "existing spend + cost on the turn win over the persisted record"
        );
    }

    #[test]
    fn turnmeta_overlay_last_record_wins_on_duplicate_key() {
        // A turn re-run after resume appends a fresh record under the same id;
        // the newest record is authoritative.
        let mut transcript = switchboard_harness::LoadedTranscript {
            turns: vec![turnmeta_agent_turn(Some("msg_test31"), None)],
            ..Default::default()
        };
        apply_turnmeta_overlay(
            &mut transcript,
            &[
                turnmeta_record("msg_test31", Some(0.01), true),
                turnmeta_record("msg_test31", Some(0.99), false),
            ],
        );
        assert_eq!(
            turn_spend_and_cost(&transcript.turns[0]),
            (Some(false), Some(0.99)),
            "the last record for a repeated key wins"
        );
    }

    #[tokio::test]
    async fn load_transcript_rejoins_persisted_turn_spend_for_claude_agent() {
        // End-to-end wiring through the real load path: a Claude agent whose
        // session file produces a turn with message id `msg_test31`, plus a
        // staged turnmeta sidecar record keyed on that id, surfaces the
        // persisted cost + overage on the hydrated turn after reopen.
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        let canonical_cwd = tmp_workdir.path().canonicalize().unwrap();
        let session_path = switchboard_harness::claude_session_file_path(
            tmp_home.path(),
            &canonical_cwd,
            &session_id,
        );
        std::fs::create_dir_all(session_path.parent().unwrap()).unwrap();
        let session_jsonl = [
            serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": "hello" },
                "timestamp": "2026-05-31T18:00:00Z",
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": "msg_test31",
                    "model": "claude-opus-4-8",
                    "role": "assistant",
                    "content": [{ "type": "text", "text": "hi" }],
                    "usage": { "input_tokens": 10, "output_tokens": 5 }
                },
                "timestamp": "2026-05-31T18:00:01Z",
            }),
        ]
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(&session_path, session_jsonl).unwrap();

        let record = attach_agent_impl(
            &state,
            "x",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        let turnmeta_path = switchboard_harness::turnmeta_sidecar::turnmeta_sidecar_path(
            tmp_workdir.path(),
            proj.id,
            record.id,
        );
        switchboard_harness::turnmeta_sidecar::append(
            &turnmeta_path,
            &turnmeta_record("msg_test31", Some(0.0125), true),
        )
        .unwrap();

        let result = load_transcript_impl(&state, record.id, tmp_home.path()).unwrap();
        let agent_turn = result
            .turns
            .iter()
            .find(|t| matches!(t, switchboard_harness::Turn::Agent { .. }))
            .expect("an agent turn is hydrated from the session file");
        assert_eq!(
            turn_spend_and_cost(agent_turn),
            (Some(true), Some(0.0125)),
            "the staged turnmeta record re-attaches its cost + overage to the matching turn on reopen"
        );
    }

    #[tokio::test]
    async fn load_transcript_overlays_metadata_sidecar_for_claude_agent() {
        // End-to-end wiring: a Claude agent with a staged metadata sidecar
        // surfaces the persisted rate-limit + its capture time through the
        // real load path (proves the sidecar-path resolution + overlay are
        // wired into load_agent_transcript, not just the pure helper).
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);
        let record = attach_agent_impl(
            &state,
            "x",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        let captured = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:42:11Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let sidecar_path = switchboard_harness::meta_sidecar::meta_sidecar_path(
            tmp_workdir.path(),
            proj.id,
            record.id,
        );
        switchboard_harness::meta_sidecar::write_rate_limit(
            &sidecar_path,
            serde_json::json!({"isUsingOverage": true, "resetsAt": 1_778_701_800u64}),
            captured,
        )
        .unwrap();

        let result = load_transcript_impl(&state, record.id, tmp_home.path()).unwrap();
        assert_eq!(
            result.last_rate_limit,
            Some(serde_json::json!({"isUsingOverage": true, "resetsAt": 1_778_701_800u64}))
        );
        assert_eq!(result.last_rate_limit_as_of, Some(captured));
    }

    #[tokio::test]
    async fn agent_session_info_for_claude_with_file_offers_open_and_resume() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);
        let record = attach_agent_impl(
            &state,
            "x",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        let info = agent_session_info_impl(&state, record.id, tmp_home.path()).unwrap();
        assert!(
            info.session_file.is_some(),
            "an existing session file is openable"
        );
        let cmd = info.resume_command.expect("resume command offered");
        assert!(
            cmd.starts_with("cd '"),
            "command cds into the project dir: {cmd}"
        );
        assert!(cmd.contains("claude --resume"), "got: {cmd}");
        assert!(cmd.contains(&session_id.to_string()), "got: {cmd}");
        assert!(cmd.contains("--dangerously-skip-permissions"), "got: {cmd}");
    }

    #[test]
    fn shell_quote_if_needed_passes_safe_tokens_and_quotes_metacharacters() {
        // Fixed program/flag tokens and UUID-ish ids stay readable.
        assert_eq!(shell_quote_if_needed("claude"), "claude");
        assert_eq!(shell_quote_if_needed("--resume"), "--resume");
        assert_eq!(
            shell_quote_if_needed("019e62e6-ae07-77a1-9a0c-47a6e1628531"),
            "019e62e6-ae07-77a1-9a0c-47a6e1628531"
        );
        // A session ref from a malformed/edited sidecar with shell metacharacters
        // is single-quoted so the copy-only command stays well-formed.
        assert_eq!(shell_quote_if_needed("a b;rm -rf /"), "'a b;rm -rf /'");
        assert_eq!(shell_quote_if_needed("x'y"), "'x'\\''y'");
        assert_eq!(shell_quote_if_needed(""), "''");
    }

    #[test]
    fn validate_external_url_allows_only_web_urls_with_a_host() {
        assert!(validate_external_url("http://example.com").is_ok());
        assert!(validate_external_url("https://example.com/a?b=c#d").is_ok());
        // Scheme/host casing is normalized by the parser.
        assert!(validate_external_url("HTTPS://Example.com").is_ok());
        // Odd-but-hostful inputs normalize to a real host (harmless — they route
        // to the browser, not a file opener), so they're accepted.
        assert!(validate_external_url("http:foo").is_ok());
        assert!(validate_external_url("https:/example.com").is_ok());
        assert!(validate_external_url("https:////example.com").is_ok());

        // Non-web schemes that could open local files or execute are refused.
        assert!(validate_external_url("file:///etc/passwd").is_err());
        assert!(validate_external_url("javascript:alert(1)").is_err());
        assert!(validate_external_url("data:text/html,<script>").is_err());
        assert!(validate_external_url("vscode://open").is_err());
        // Well-formed scheme but no host — refused.
        assert!(validate_external_url("https:").is_err());
        assert!(validate_external_url("http://").is_err());
        // Relative / scheme-less and malformed inputs are refused.
        assert!(validate_external_url("/local/path").is_err());
        assert!(validate_external_url("example.com").is_err());
        assert!(validate_external_url("a b:c").is_err());
        assert!(validate_external_url("").is_err());
    }

    #[tokio::test]
    async fn agent_session_info_quotes_a_metacharacter_session_id() {
        // End-to-end: a Codex thread-id with shell metacharacters is
        // single-quoted in the rendered resume command.
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let agent =
            create_agent_impl(&state, "codex_evil", HarnessKind::Codex, None, None).unwrap();
        set_agent_session_locator_impl(
            &state,
            agent.id,
            SessionLocator::Codex {
                thread_id: "a;rm -rf".to_owned(),
                partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 25).unwrap(),
            },
        )
        .unwrap();

        let info = agent_session_info_impl(&state, agent.id, tmp_home.path()).unwrap();
        let cmd = info.resume_command.expect("resume offered");
        assert!(
            cmd.contains("'a;rm -rf'"),
            "metacharacter id is quoted: {cmd}"
        );
    }

    #[tokio::test]
    async fn agent_session_info_for_never_dispatched_agent_is_empty() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        // Codex agent with no sidecar (never dispatched) → nothing to open/resume.
        let record =
            create_agent_impl(&state, "codex_one", HarnessKind::Codex, None, None).unwrap();
        let info = agent_session_info_impl(&state, record.id, tmp_home.path()).unwrap();
        assert_eq!(info, AgentSessionInfo::default());
    }

    #[tokio::test]
    async fn agent_session_info_for_codex_resolves_resume_id_from_record() {
        // The resume id is the Codex locator's thread-id on the record. Resume
        // is offered from it even when the local session file isn't present.
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let agent = create_agent_impl(&state, "codex_two", HarnessKind::Codex, None, None).unwrap();
        set_agent_session_locator_impl(
            &state,
            agent.id,
            SessionLocator::Codex {
                thread_id: "sess-xyz".to_owned(),
                partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 25).unwrap(),
            },
        )
        .unwrap();

        let info = agent_session_info_impl(&state, agent.id, tmp_home.path()).unwrap();
        assert!(
            info.session_file.is_none(),
            "no transcript file staged on disk"
        );
        let cmd = info
            .resume_command
            .expect("resume offered from record locator");
        assert!(cmd.contains("codex resume sess-xyz"), "got: {cmd}");
    }

    #[tokio::test]
    async fn load_transcript_for_codex_agent_without_sidecar_returns_meta_only_empty() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        // Create a Codex agent the normal way (no sidecar — no first dispatch).
        let record =
            create_agent_impl(&state, "codex_one", HarnessKind::Codex, None, None).unwrap();

        let result = load_transcript_impl(&state, record.id, tmp_home.path()).unwrap();
        assert!(result.turns.is_empty());
        // Meta is populated from config loaders, never None.
        assert!(result.meta.is_some());
    }

    #[tokio::test]
    async fn load_transcript_for_missing_agent_returns_agent_not_found() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let err = load_transcript_impl(&state, Uuid::now_v7(), tmp_home.path()).unwrap_err();
        assert!(matches!(err, AppError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn attach_antigravity_writes_locator_to_registry() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let conversation_id = Uuid::now_v7();
        stage_antigravity_conversation(tmp_home.path(), conversation_id, true);

        let record = attach_agent_impl(
            &state,
            "attached-agy",
            HarnessKind::Antigravity,
            &conversation_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Uuid(conversation_id)),
            "Antigravity attach writes the conversation UUID onto the record (no sidecar)"
        );
        // Unlike Codex, Antigravity emits SessionMeta every turn, so attach
        // does not force it via needs_session_meta.
        assert!(
            !lock(&state.needs_session_meta).contains(&record.id),
            "Antigravity attach must not populate needs_session_meta"
        );
    }

    #[tokio::test]
    async fn attach_antigravity_brain_dir_without_transcript_succeeds_and_hydrates_empty() {
        // The attach contract is "the conversation directory exists" — a
        // brain dir without a transcript (encrypted-only / pruned) still
        // attaches, and hydration then degrades to empty turns + registry
        // meta, matching the loader's missing-transcript path.
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let conversation_id = Uuid::now_v7();
        stage_antigravity_conversation(tmp_home.path(), conversation_id, false);

        let record = attach_agent_impl(
            &state,
            "attached-agy",
            HarnessKind::Antigravity,
            &conversation_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        let result = load_transcript_impl(&state, record.id, tmp_home.path()).unwrap();
        assert!(result.turns.is_empty(), "no transcript → no turns");
        assert!(result.meta.is_some(), "registry meta still surfaces");
    }

    #[tokio::test]
    async fn attach_antigravity_rejects_missing_brain_dir() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let conversation_id = Uuid::now_v7();
        let err = attach_agent_impl(
            &state,
            "attached-agy",
            HarnessKind::Antigravity,
            &conversation_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionFileNotFound {
                harness,
                expected_path,
            } => {
                assert_eq!(harness, HarnessKind::Antigravity);
                assert!(expected_path.contains("antigravity-cli"));
                assert!(expected_path.contains("brain"));
            }
            other => panic!("expected SessionFileNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_antigravity_rejects_duplicate_conversation_uuid() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let conversation_id = Uuid::now_v7();
        stage_antigravity_conversation(tmp_home.path(), conversation_id, true);

        attach_agent_impl(
            &state,
            "agy-one",
            HarnessKind::Antigravity,
            &conversation_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        let err = attach_agent_impl(
            &state,
            "agy-two",
            HarnessKind::Antigravity,
            &conversation_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionAlreadyAttached {
                existing_agent_name,
                ..
            } => assert_eq!(existing_agent_name, "agy-one"),
            other => panic!("expected SessionAlreadyAttached, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_transcript_for_antigravity_agent_without_locator_returns_meta_only_empty() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        // Antigravity agent never dispatched → no locator → empty turns, but
        // loader-derived registry meta still surfaces (mirrors the Codex arm)
        // so the sidebar populates the moment the agent is selected.
        let record =
            create_agent_impl(&state, "agy_one", HarnessKind::Antigravity, None, None).unwrap();
        assert!(record.session_locator.is_none());
        let result = load_transcript_impl(&state, record.id, tmp_home.path()).unwrap();
        assert!(result.turns.is_empty());
        assert!(result.meta.is_some());
    }

    #[tokio::test]
    async fn load_transcript_for_antigravity_agent_hydrates_prior_turns() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let agent =
            create_agent_impl(&state, "agy_hydrate", HarnessKind::Antigravity, None, None).unwrap();

        // The server-assigned conversation UUID now lives on the record — the
        // same path the runtime-capture sink writes. Set it directly.
        let conversation_id = Uuid::new_v4();
        set_agent_session_locator_impl(&state, agent.id, SessionLocator::Uuid(conversation_id))
            .unwrap();

        // Transcript: one user prompt + one model answer.
        let transcript = switchboard_harness::antigravity::paths::transcript_path(
            tmp_home.path(),
            conversation_id,
        );
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript,
            concat!(
                r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nremember mango\n</USER_REQUEST>"}"#,
                "\n",
                r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:01Z","content":"mango"}"#,
                "\n",
            ),
        )
        .unwrap();

        let result = load_transcript_impl(&state, agent.id, tmp_home.path()).unwrap();
        assert_eq!(result.turns.len(), 2);
        assert!(result.meta.is_some());
    }

    // -----------------------------------------------------------------
    // Gemini attach tests
    // -----------------------------------------------------------------

    /// Stage `~/.gemini/projects.json` + a single Gemini session file
    /// under `home/.gemini/tmp/<project>/chats/`. Returns the staged
    /// path. The session file is a minimal `kind:"main"` header line so
    /// `classify_candidate` returns `Unambiguous` for the target.
    fn stage_gemini_session_file(home: &Path, cwd: &Path, session_id: &Uuid) -> PathBuf {
        let canonical = cwd.canonicalize().unwrap();
        let gemini = home.join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let projects = serde_json::json!({
            "projects": { canonical.to_str().unwrap(): "proj" }
        });
        std::fs::write(gemini.join("projects.json"), projects.to_string()).unwrap();
        let chats = gemini.join("tmp").join("proj").join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        let prefix = switchboard_harness::gemini_session_id_prefix(session_id);
        let path = chats.join(format!("session-2026-05-18T00-00-{prefix}.jsonl"));
        let header = format!(
            r#"{{"sessionId":"{session_id}","projectHash":"x","startTime":"2026-05-18T00:00:00Z","lastUpdated":"2026-05-18T00:00:00Z","kind":"main"}}"#
        );
        std::fs::write(&path, format!("{header}\n")).unwrap();
        path
    }

    #[tokio::test]
    async fn attach_gemini_succeeds_when_session_file_exists() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::new_v4();
        stage_gemini_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);

        let record = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Uuid(session_id))
        );
        assert_eq!(record.harness, HarnessKind::Gemini);
        // Gemini follows the Claude pattern (caller-controlled session
        // UUID); no sidecar, no needs_session_meta override.
        assert!(
            !lock(&state.needs_session_meta).contains(&record.id),
            "Gemini attach must NOT populate needs_session_meta"
        );
    }

    #[tokio::test]
    async fn attach_gemini_rejects_missing_session_file_with_expected_path() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::new_v4();
        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionFileNotFound {
                harness,
                expected_path,
            } => {
                assert_eq!(harness, HarnessKind::Gemini);
                assert!(expected_path.contains(".gemini"));
            }
            other => panic!("expected SessionFileNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_gemini_multi_candidate_picks_full_uuid_match() {
        // Two files sharing the 8-char prefix in their filename, different
        // timestamps. Each file holds a different conversation's header.
        // `classify_candidate` picks the unambiguous-target file; the other
        // is `NoTarget` and skipped.
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let canonical = tmp_workdir.path().canonicalize().unwrap();
        let gemini = tmp_home.path().join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let projects = serde_json::json!({
            "projects": { canonical.to_str().unwrap(): "proj" }
        });
        std::fs::write(gemini.join("projects.json"), projects.to_string()).unwrap();
        let chats = gemini.join("tmp").join("proj").join("chats");
        std::fs::create_dir_all(&chats).unwrap();

        let id_a = Uuid::parse_str("00000000-0000-4000-8000-000000000010").unwrap();
        let id_b = Uuid::parse_str("00000000-0000-4000-8000-000000000020").unwrap();
        let prefix = switchboard_harness::gemini_session_id_prefix(&id_a);
        assert_eq!(
            prefix,
            switchboard_harness::gemini_session_id_prefix(&id_b),
            "test setup requires identical 8-char prefixes"
        );
        let header_a = format!(
            r#"{{"sessionId":"{id_a}","projectHash":"x","startTime":"2026-05-18T00:00:00Z","lastUpdated":"2026-05-18T00:00:00Z","kind":"main"}}"#
        );
        let header_b = format!(
            r#"{{"sessionId":"{id_b}","projectHash":"x","startTime":"2026-05-18T00:05:00Z","lastUpdated":"2026-05-18T00:05:00Z","kind":"main"}}"#
        );
        std::fs::write(
            chats.join(format!("session-2026-05-18T00-00-{prefix}.jsonl")),
            format!("{header_a}\n"),
        )
        .unwrap();
        std::fs::write(
            chats.join(format!("session-2026-05-18T00-05-{prefix}.jsonl")),
            format!("{header_b}\n"),
        )
        .unwrap();

        let record = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &id_b.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(record.session_locator, Some(SessionLocator::Uuid(id_b)));
    }

    #[tokio::test]
    async fn attach_gemini_multi_candidate_with_no_match_returns_not_found() {
        // A candidate file exists at the prefix glob, but its sessionId is
        // for a different UUID — must not be claimed silently as the
        // user's target.
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let other = Uuid::parse_str("00000000-0000-4000-8000-000000000010").unwrap();
        stage_gemini_session_file(tmp_home.path(), tmp_workdir.path(), &other);

        let asked = Uuid::parse_str("00000000-0000-4000-8000-000000000099").unwrap();
        assert_eq!(
            switchboard_harness::gemini_session_id_prefix(&other),
            switchboard_harness::gemini_session_id_prefix(&asked)
        );
        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &asked.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, AppError::SessionFileNotFound { harness, .. } if harness == HarnessKind::Gemini)
        );
    }

    /// Pin the ambiguity invariant: an ambiguous candidate (one file,
    /// multiple distinct session headers) must surface as
    /// `AmbiguousSessionFile`, never as `SessionFileNotFound` or a
    /// silent merge. UUID v4 makes this ~1/2^32; the test ensures the
    /// code path is correctly wired if it ever fires.
    #[tokio::test]
    async fn attach_gemini_ambiguous_candidate_surfaces_ambiguous_error() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let canonical = tmp_workdir.path().canonicalize().unwrap();
        let gemini = tmp_home.path().join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let projects = serde_json::json!({
            "projects": { canonical.to_str().unwrap(): "proj" }
        });
        std::fs::write(gemini.join("projects.json"), projects.to_string()).unwrap();
        let chats = gemini.join("tmp").join("proj").join("chats");
        std::fs::create_dir_all(&chats).unwrap();

        let target = Uuid::parse_str("00000000-0000-4000-8000-000000000009").unwrap();
        let other = Uuid::parse_str("00000000-0000-4000-8000-00000000000A").unwrap();
        let prefix = switchboard_harness::gemini_session_id_prefix(&target);
        let body = format!(
            r#"{{"sessionId":"{target}","projectHash":"x","startTime":"2026-05-17T22:20:35.615Z","lastUpdated":"2026-05-17T22:20:35.615Z","kind":"main"}}
{{"sessionId":"{other}","projectHash":"x","startTime":"2026-05-17T22:20:35.654Z","lastUpdated":"2026-05-17T22:20:35.654Z","kind":"main"}}
"#
        );
        let staged = chats.join(format!("session-2026-05-17T22-20-{prefix}.jsonl"));
        std::fs::write(&staged, body).unwrap();

        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &target.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::AmbiguousSessionFile {
                harness,
                session_id,
                paths,
            } => {
                assert_eq!(harness, HarnessKind::Gemini);
                assert_eq!(session_id, target.to_string());
                assert_eq!(paths, vec![staged]);
            }
            other => panic!("expected AmbiguousSessionFile, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_gemini_rejects_duplicate_name() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::new_v4();
        stage_gemini_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);
        attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        // Reuse the same name; even with a different session UUID
        // (which we'd have to stage too) the name-clash check fires
        // first. We use the same name + same session for simplicity.
        let other = Uuid::new_v4();
        stage_gemini_session_file(tmp_home.path(), tmp_workdir.path(), &other);
        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &other.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(switchboard_core::CoreError::DuplicateAgentName { .. })
        ));
    }

    #[tokio::test]
    async fn attach_gemini_rejects_same_project_session_id_collision() {
        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::new_v4();
        stage_gemini_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);
        attach_agent_impl(
            &state,
            "first",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        let err = attach_agent_impl(
            &state,
            "second",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionAlreadyAttached {
                existing_agent_name,
                ..
            } => assert_eq!(existing_agent_name, "first"),
            other => panic!("expected SessionAlreadyAttached, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_gemini_rejects_cross_project_session_id_collision() {
        let (tmp_workdir, tmp_home, state, _proj_a) =
            fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::new_v4();
        stage_gemini_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);
        attach_agent_impl(
            &state,
            "first",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        let proj_b = create_project_in_only_dir(&state, "beta");
        set_active_project_impl(&state, proj_b.id).unwrap();
        let err = attach_agent_impl(
            &state,
            "first-in-beta",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        match err {
            AppError::SessionAlreadyAttached {
                existing_project_name,
                ..
            } => assert_eq!(existing_project_name, "alpha"),
            other => panic!("expected SessionAlreadyAttached, got {other:?}"),
        }
    }

    /// I/O errors on a candidate file must surface as `AttachLookupFailed`
    /// rather than silently routing to `SessionFileNotFound`. The user's
    /// remediation differs (chmod / fs repair vs. verify UUID); the wrong
    /// error sends them chasing red herrings. Unix-only because file-mode
    /// 0o000 has no Windows analog.
    #[cfg(unix)]
    #[tokio::test]
    async fn attach_gemini_propagates_io_error_for_unreadable_candidate() {
        use std::os::unix::fs::PermissionsExt;

        let (tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::new_v4();
        let path = stage_gemini_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Self-check for root-equivalent containers: if `chmod 000` doesn't
        // actually block reads (root ignores file modes), the failure path
        // we're trying to exercise won't fire. Restore mode and skip.
        if std::fs::read(&path).is_ok() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        // Restore mode **before** asserting so TempDir's Drop can rmdir
        // even if the assertion fails on a future regression.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        match err {
            AppError::AttachLookupFailed { message } => {
                assert!(
                    message.contains(path.to_str().unwrap()),
                    "expected error to name the unreadable path, got: {message}"
                );
            }
            other => panic!("expected AttachLookupFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_gemini_rejects_missing_projects_json_as_not_found() {
        // No `~/.gemini/projects.json` at all → cwd resolution fails →
        // candidate set is empty → SessionFileNotFound.
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::new_v4();
        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, AppError::SessionFileNotFound { harness, .. } if harness == HarnessKind::Gemini)
        );
    }

    // ---- M4.1: project instance lock + register-cache ----

    fn mock_app_state() -> AppState {
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::new(RecordingEmitter::new()) as Arc<dyn EventEmitter>,
        )
    }

    #[tokio::test]
    async fn project_lock_refuses_second_process_then_releases_on_remove() {
        let (tmp_workdir, _home, state_a, proj) = fresh_state_with_active_project("alpha").await;

        // A second Switchboard "process" binds the same directory and opens
        // the same project — refused while state_a holds the instance lock.
        // (Independent `open()`s of the same lock file conflict under flock,
        // even within one OS process, which is what lets this run in-process.)
        let state_b = mock_app_state();
        init_directory_impl(&state_b, tmp_workdir.path().to_str().unwrap())
            .await
            .unwrap();
        let err = open_project_impl(&state_b, proj.id).unwrap_err();
        assert!(
            matches!(err, AppError::ProjectLocked(id) if id == proj.id),
            "second process must be refused with ProjectLocked, got {err:?}"
        );

        // state_a removes the directory → its lock `File` drops → released.
        remove_directory_impl(&state_a, tmp_workdir.path().to_str().unwrap())
            .await
            .unwrap();
        open_project_impl(&state_b, proj.id)
            .expect("lock released on remove; second process can now open");
    }

    #[tokio::test]
    async fn intra_process_reopen_is_noop_and_does_not_relock() {
        let (_tmp, _home, state, proj) = fresh_state_with_active_project("alpha").await;
        // `create_project` already loaded + locked it; re-open is a no-op
        // returning the same project, with no second lock acquired.
        let again = open_project_impl(&state, proj.id).unwrap();
        assert_eq!(again.id, proj.id);
        assert_eq!(
            lock(&state.project_locks).len(),
            1,
            "intra-process re-open must not acquire a second lock"
        );
    }

    #[tokio::test]
    async fn register_cache_populates_clears_on_remove_and_repopulates_on_open() {
        let (tmp_workdir, _home, state, proj) = fresh_state_with_active_project("alpha").await;
        let agent =
            create_agent_impl(&state, "assistant", HarnessKind::ClaudeCode, None, None).unwrap();

        // create_agent populated the cache → lookup resolves the owning
        // project without scanning any registry from disk.
        assert!(lock(&state.agents_by_id).contains_key(&agent.id));
        let (project, found) = lookup_agent(&state, agent.id).unwrap();
        assert_eq!(found.id, agent.id);
        assert_eq!(project.id, proj.id);

        // Removing the directory prunes its cache entries; the stale agent no
        // longer resolves.
        remove_directory_impl(&state, tmp_workdir.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(lock(&state.agents_by_id).is_empty());
        assert!(matches!(
            lookup_agent(&state, agent.id),
            Err(AppError::AgentNotFound(_))
        ));

        // Re-init the same directory and open the project → cache repopulated
        // from disk (the `.switchboard/` state was never deleted).
        init_directory_impl(&state, tmp_workdir.path().to_str().unwrap())
            .await
            .unwrap();
        open_project_impl(&state, proj.id).unwrap();
        let (project2, found2) = lookup_agent(&state, agent.id).unwrap();
        assert_eq!(found2.id, agent.id);
        assert_eq!(project2.id, proj.id);
    }

    #[tokio::test]
    async fn open_with_corrupt_registry_errors_without_wedging_the_lock() {
        let (tmp_workdir, _home, state, proj) = fresh_state_with_active_project("alpha").await;
        let agent =
            create_agent_impl(&state, "assistant", HarnessKind::ClaudeCode, None, None).unwrap();

        // Simulate a fresh process that hasn't loaded this project: drop the
        // in-memory maps (clearing project_locks releases the flock).
        lock(&state.projects).clear();
        lock(&state.project_locks).clear();
        lock(&state.agents_by_id).clear();

        // Corrupt the on-disk registry with a torn line after the valid record.
        let registry = tmp_workdir
            .path()
            .join(".switchboard")
            .join("projects")
            .join(proj.id.to_string())
            .join("registry.jsonl");
        let good_line = serde_json::to_string(&agent).unwrap();
        std::fs::write(&registry, format!("{good_line}\nthis is not json\n")).unwrap();

        // Open must surface the corruption — not a misleading ProjectLocked —
        // and must not strand the lock handle.
        let err = open_project_impl(&state, proj.id).unwrap_err();
        assert!(
            matches!(err, AppError::Core(CoreError::CorruptJsonl { .. })),
            "expected CorruptJsonl, got {err:?}"
        );
        assert!(
            lock(&state.project_locks).is_empty(),
            "a failed open must not leave the lock handle stranded"
        );

        // Repair the registry → open now succeeds (the lock was never wedged).
        std::fs::write(&registry, format!("{good_line}\n")).unwrap();
        open_project_impl(&state, proj.id)
            .expect("open succeeds after repair; the lock was not wedged");
        let (_p, a) = lookup_agent(&state, agent.id).unwrap();
        assert_eq!(a.id, agent.id);
    }

    #[tokio::test]
    async fn concurrent_first_open_of_same_project_is_idempotent() {
        let (_tmp, _home, state, proj) = fresh_state_with_active_project("alpha").await;
        // Simulate not-yet-loaded so both threads race the first-open path.
        lock(&state.projects).clear();
        lock(&state.project_locks).clear();
        lock(&state.agents_by_id).clear();

        // Two concurrent opens of the same not-loaded project must both
        // succeed — one loads it, the other re-checks under the serialization
        // guard and returns the existing handle — never one spuriously
        // ProjectLocked against this process's own first handle.
        let (r1, r2) = std::thread::scope(|s| {
            let h1 = s.spawn(|| open_project_impl(&state, proj.id));
            let h2 = s.spawn(|| open_project_impl(&state, proj.id));
            (h1.join().unwrap(), h2.join().unwrap())
        });
        assert!(r1.is_ok(), "first concurrent open failed: {r1:?}");
        assert!(r2.is_ok(), "second concurrent open failed: {r2:?}");
        assert_eq!(
            lock(&state.project_locks).len(),
            1,
            "exactly one lock handle for the project"
        );
        assert_eq!(lock(&state.projects).len(), 1);
    }

    #[tokio::test]
    async fn distinct_projects_lock_independently() {
        let (_tmp, _home, state, alpha) = fresh_state_with_active_project("alpha").await;
        let beta = create_project_in_only_dir(&state, "beta");
        // Per-project lock, not per-directory: both are held simultaneously.
        assert_eq!(lock(&state.project_locks).len(), 2);
        // Both remain openable (intra-process re-open is a no-op).
        open_project_impl(&state, alpha.id).unwrap();
        open_project_impl(&state, beta.id).unwrap();
    }

    #[tokio::test]
    async fn cancel_turn_cancels_in_flight_turn() {
        // App-level cancel routing: the detailed cancellation state machine is
        // covered in the dispatcher crate; here we assert that cancel is
        // delivered to a live turn (`Requested`) and that the turn then reaches
        // a cancelled terminal + idle. "In flight" is observed via the emitted
        // `turn_start` (the actor started the turn) rather than `agent_status`.
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, _project_id) = project_with_agent(&state, &tmp).await;

        send_msg(&state, agent.id, "long task").await.unwrap();
        // Wait until the actor has actually started the turn (the
        // `AwaitCancellation` scenario then parks until the cancel token fires).
        within(
            &emitter,
            "turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;

        let outcome = cancel_turn_impl(&state, agent.id);
        assert_eq!(outcome, CancelOutcome::Requested);

        // The cancel drives the turn to a cancelled terminal and back to idle —
        // the event-based equivalent of the old `agent_status == Idle` check.
        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;
        let channel = format!("agent:{}", agent.id);
        let cancelled = emitter.snapshot().into_iter().any(|(name, v)| {
            name == channel && v["type"] == "turn_end" && v["outcome"]["status"] == "cancelled"
        });
        assert!(cancelled, "cancel synthesizes a cancelled terminal");
    }

    // --- remove_agent / rename_agent (M5) ---
    // Fixture-level only — no live test: these commands don't change how we talk
    // to a real CLI, just registry/sidecar/in-memory state.

    fn meta_sidecar(tmp: &TempDir, project_id: ProjectId, agent_id: AgentId) -> PathBuf {
        switchboard_harness::meta_sidecar::meta_sidecar_path(tmp.path(), project_id, agent_id)
    }

    fn write_dummy(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"{}").unwrap();
    }

    #[tokio::test]
    async fn remove_agent_drops_record_and_cache_entry() {
        let (tmp, state, _) = fresh_state_with_mock();
        let (agent, project_id) = project_with_agent(&state, &tmp).await;
        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        assert_eq!(project.list_agents().unwrap().len(), 1);

        remove_agent_impl(&state, agent.id).await.unwrap();

        assert!(project.list_agents().unwrap().is_empty());
        assert!(!lock(&state.agents_by_id).contains_key(&agent.id));
    }

    #[tokio::test]
    async fn remove_agent_deletes_present_sidecar_and_tolerates_absent() {
        let (tmp, state, _) = fresh_state_with_mock();
        let (agent, project_id) = project_with_agent(&state, &tmp).await;
        // Only the meta sidecar exists; the codex/antigravity ones don't —
        // removal must delete the present one and not fail on the absent ones.
        let sidecar = meta_sidecar(&tmp, project_id, agent.id);
        write_dummy(&sidecar);

        remove_agent_impl(&state, agent.id).await.unwrap();

        assert!(!sidecar.exists(), "the agent's meta sidecar is deleted");
    }

    #[tokio::test]
    async fn remove_agent_leaves_other_agents_sidecar_intact() {
        // Scope guard: removal touches only this agent's sidecars. A sibling
        // agent's sidecar (and, by construction — we only ever target the three
        // `.switchboard/.../sessions/<id>.*` paths — any harness-native file)
        // is untouched.
        let (tmp, state, _) = fresh_state_with_mock();
        let (a, project_id) = project_with_agent(&state, &tmp).await;
        let b = create_agent_impl(&state, "second", HarnessKind::ClaudeCode, None, None).unwrap();
        let sidecar_a = meta_sidecar(&tmp, project_id, a.id);
        let sidecar_b = meta_sidecar(&tmp, project_id, b.id);
        write_dummy(&sidecar_a);
        write_dummy(&sidecar_b);

        remove_agent_impl(&state, a.id).await.unwrap();

        assert!(!sidecar_a.exists());
        assert!(sidecar_b.exists(), "sibling agent's sidecar untouched");
    }

    #[tokio::test]
    async fn remove_agent_nonexistent_errors() {
        let (_tmp, state, _) = fresh_state_with_mock();
        let err = remove_agent_impl(&state, Uuid::now_v7()).await.unwrap_err();
        assert!(matches!(err, AppError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn remove_agent_with_in_flight_turn_cancels_then_removes() {
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, project_id) = project_with_agent(&state, &tmp).await;
        send_msg(&state, agent.id, "long task").await.unwrap();
        within(
            &emitter,
            "turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;

        // Phase (a) cancels the live turn + tears down; phase (b) deletes.
        remove_agent_impl(&state, agent.id).await.unwrap();

        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        assert!(project.list_agents().unwrap().is_empty());
        assert!(!lock(&state.agents_by_id).contains_key(&agent.id));
        let channel = format!("agent:{}", agent.id);
        let cancelled = emitter.snapshot().into_iter().any(|(name, v)| {
            name == channel && v["type"] == "turn_end" && v["outcome"]["status"] == "cancelled"
        });
        assert!(
            cancelled,
            "the in-flight turn is cancelled, not silently dropped"
        );
    }

    #[tokio::test]
    async fn rename_agent_updates_record_and_cache() {
        let (tmp, state, _) = fresh_state_with_mock();
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        let updated = rename_agent_impl(&state, agent.id, "renamed").unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.id, agent.id);

        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        assert_eq!(project.list_agents().unwrap()[0].name, "renamed");
        assert_eq!(
            lock(&state.agents_by_id).get(&agent.id).unwrap().name,
            "renamed"
        );
    }

    #[tokio::test]
    async fn rename_agent_rejects_duplicate_name() {
        let (tmp, state, _) = fresh_state_with_mock();
        let (a, _pid) = project_with_agent(&state, &tmp).await;
        create_agent_impl(&state, "second", HarnessKind::ClaudeCode, None, None).unwrap();
        let err = rename_agent_impl(&state, a.id, "SECOND").unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(CoreError::DuplicateAgentName { .. })
        ));
        // The reject path leaves the cache untouched.
        assert_eq!(
            lock(&state.agents_by_id).get(&a.id).unwrap().name,
            "assistant"
        );
    }

    #[tokio::test]
    async fn rename_agent_nonexistent_errors() {
        let (_tmp, state, _) = fresh_state_with_mock();
        let err = rename_agent_impl(&state, Uuid::now_v7(), "x").unwrap_err();
        assert!(matches!(err, AppError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn create_agent_stores_model_and_effort() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, project.id).unwrap();

        let agent = create_agent_impl(
            &state,
            "a",
            HarnessKind::ClaudeCode,
            Some("opus".to_owned()),
            Some("high".to_owned()),
        )
        .unwrap();
        assert_eq!(agent.model.as_deref(), Some("opus"));
        assert_eq!(agent.effort.as_deref(), Some("high"));
        // Durable on the registry, not just the returned record.
        let reloaded = lock(&state.projects).get(&project.id).cloned().unwrap();
        let stored = &reloaded.list_agents().unwrap()[0];
        assert_eq!(stored.model.as_deref(), Some("opus"));
        assert_eq!(stored.effort.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn create_agent_normalizes_blank_selections_to_none() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, project.id).unwrap();

        // Blank/whitespace must persist as unset, never `Some("")` (which would
        // dispatch `--model ""` / `-c model_reasoning_effort=` every turn).
        let agent = create_agent_impl(
            &state,
            "a",
            HarnessKind::ClaudeCode,
            Some("   ".to_owned()),
            Some(String::new()),
        )
        .unwrap();
        assert_eq!(agent.model, None);
        assert_eq!(agent.effort, None);
    }

    #[tokio::test]
    async fn create_agent_rejects_unsupported_selection_and_persists_nothing() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, project.id).unwrap();

        let err = create_agent_impl(
            &state,
            "a",
            HarnessKind::Antigravity,
            Some("anything".to_owned()),
            None,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(CoreError::SelectionUnsupported {
                harness: HarnessKind::Antigravity,
                axis: SelectionAxis::Model
            })
        ));
        let err = create_agent_impl(
            &state,
            "g",
            HarnessKind::Gemini,
            None,
            Some("high".to_owned()),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(CoreError::SelectionUnsupported {
                harness: HarnessKind::Gemini,
                axis: SelectionAxis::Effort
            })
        ));
        // No partial agent landed in the registry or cache.
        let reloaded = lock(&state.projects).get(&project.id).cloned().unwrap();
        assert!(reloaded.list_agents().unwrap().is_empty());
        assert!(lock(&state.agents_by_id).is_empty());
    }

    #[tokio::test]
    async fn set_agent_model_and_effort_update_record_cache_and_clear() {
        let (tmp, state, _) = fresh_state_with_mock();
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        let updated = set_agent_model_impl(&state, agent.id, Some("opus".to_owned())).unwrap();
        assert_eq!(updated.model.as_deref(), Some("opus"));
        set_agent_effort_impl(&state, agent.id, Some("high".to_owned())).unwrap();

        // Both registry and cache reflect the change.
        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        let stored = &project.list_agents().unwrap()[0];
        assert_eq!(stored.model.as_deref(), Some("opus"));
        assert_eq!(stored.effort.as_deref(), Some("high"));
        let cached = lock(&state.agents_by_id).get(&agent.id).cloned().unwrap();
        assert_eq!(cached.model.as_deref(), Some("opus"));
        assert_eq!(cached.effort.as_deref(), Some("high"));

        // Clearing persists `None`.
        let cleared = set_agent_model_impl(&state, agent.id, None).unwrap();
        assert_eq!(cleared.model, None);
        set_agent_effort_impl(&state, agent.id, None).unwrap();
        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        let stored = &project.list_agents().unwrap()[0];
        assert_eq!(stored.model, None);
        assert_eq!(stored.effort, None);
    }

    #[tokio::test]
    async fn set_agent_selection_normalizes_blank_to_none() {
        let (tmp, state, _) = fresh_state_with_mock();
        let (agent, _pid) = project_with_agent(&state, &tmp).await;

        let updated = set_agent_model_impl(&state, agent.id, Some("  ".to_owned())).unwrap();
        assert_eq!(updated.model, None);
        let updated = set_agent_effort_impl(&state, agent.id, Some(String::new())).unwrap();
        assert_eq!(updated.effort, None);
    }

    #[tokio::test]
    async fn set_agent_selection_rejects_unsupported_harness() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        set_active_project_impl(&state, project.id).unwrap();
        let gemini = create_agent_impl(&state, "g", HarnessKind::Gemini, None, None).unwrap();
        let antigravity =
            create_agent_impl(&state, "a", HarnessKind::Antigravity, None, None).unwrap();

        let err = set_agent_effort_impl(&state, gemini.id, Some("high".to_owned())).unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(CoreError::SelectionUnsupported {
                harness: HarnessKind::Gemini,
                axis: SelectionAxis::Effort
            })
        ));
        let err = set_agent_model_impl(&state, antigravity.id, Some("x".to_owned())).unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(CoreError::SelectionUnsupported {
                harness: HarnessKind::Antigravity,
                axis: SelectionAxis::Model
            })
        ));
    }

    #[tokio::test]
    async fn attach_stores_model_and_effort() {
        let (tmp_workdir, tmp_home, state, project) =
            fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);

        let record = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            Some("sonnet".to_owned()),
            Some("low".to_owned()),
        )
        .unwrap();
        assert_eq!(record.model.as_deref(), Some("sonnet"));
        assert_eq!(record.effort.as_deref(), Some("low"));
        let reloaded = lock(&state.projects).get(&project.id).cloned().unwrap();
        let stored = &reloaded.list_agents().unwrap()[0];
        assert_eq!(stored.model.as_deref(), Some("sonnet"));
        assert_eq!(stored.effort.as_deref(), Some("low"));
    }

    #[tokio::test]
    async fn attach_rejects_unsupported_selection_before_session_lookup() {
        let (_workdir, tmp_home, state, project) = fresh_state_with_active_project("alpha").await;

        // A bogus session id with no staged file: if the capability check did
        // NOT run first, attach would fail with SessionFileNotFound. Getting
        // SelectionUnsupported instead proves the check precedes the lookup and
        // the registry write.
        let err = attach_agent_impl(
            &state,
            "g",
            HarnessKind::Gemini,
            &Uuid::now_v7().to_string(),
            tmp_home.path(),
            None,
            Some("high".to_owned()),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(CoreError::SelectionUnsupported {
                harness: HarnessKind::Gemini,
                axis: SelectionAxis::Effort
            })
        ));
        // Nothing was registered.
        let reloaded = lock(&state.projects).get(&project.id).cloned().unwrap();
        assert!(reloaded.list_agents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rename_project_updates_state_cache_and_returns_listing() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");

        let listing = rename_project_impl(&state, project.id, "renamed").unwrap();
        assert_eq!(listing.name, "renamed");
        assert_eq!(listing.id, project.id);
        assert!(listing.available);

        // In-memory `Project` (canonical name) reflects the change.
        assert_eq!(
            lock(&state.projects).get(&project.id).unwrap().name(),
            "renamed"
        );
        // The flat list (from disk + refreshed cache) reflects the change.
        let listed = list_projects_impl(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "renamed");
    }

    #[tokio::test]
    async fn rename_project_rejects_duplicate_in_same_directory() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let a = create_project_in_only_dir(&state, "alpha");
        create_project_in_only_dir(&state, "beta");

        let err = rename_project_impl(&state, a.id, "BETA").unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(CoreError::DuplicateProjectName { .. })
        ));
        // The reject path leaves the canonical name untouched.
        assert_eq!(lock(&state.projects).get(&a.id).unwrap().name(), "alpha");
    }

    #[tokio::test]
    async fn rename_project_rejects_invalid_name() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let a = create_project_in_only_dir(&state, "alpha");
        let err = rename_project_impl(&state, a.id, "bad name").unwrap_err();
        assert!(matches!(err, AppError::Core(CoreError::InvalidName { .. })));
    }

    #[tokio::test]
    async fn rename_project_unknown_id_errors_without_loaded_directory() {
        let (_tmp, state, _) = fresh_state_with_mock();
        // No loaded directory owns this id → resolution fails (the same path an
        // unavailable directory takes).
        let err = rename_project_impl(&state, Uuid::now_v7(), "x").unwrap_err();
        assert!(matches!(err, AppError::ProjectNotLoaded(_)));
    }

    #[tokio::test]
    async fn rename_project_succeeds_when_not_yet_opened() {
        // The project exists on disk in a loaded directory but was never
        // activated (not in `AppState.projects`); rename resolves the owning
        // directory from the index and still works.
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        // Drop the loaded `Project` to simulate "available but not opened".
        lock(&state.projects).remove(&project.id);

        let listing = rename_project_impl(&state, project.id, "renamed").unwrap();
        assert_eq!(listing.name, "renamed");
        assert_eq!(
            list_projects_impl(&state).unwrap()[0].name,
            "renamed",
            "the on-disk rename is reflected in the flat list"
        );
        // The in-memory sync block (`if let Some(..) = ...get_mut`) must *skip* an
        // unloaded project, not insert a phantom entry for it.
        assert!(
            lock(&state.projects).get(&project.id).is_none(),
            "rename of an unopened project must not insert it into state.projects"
        );
    }

    #[tokio::test]
    async fn delete_project_removes_project_agents_lock_and_clears_active() {
        let (tmp, state, _) = fresh_state_with_mock();
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        delete_project_impl(&state, project_id).await.unwrap();

        // In-memory state for the project and its agent is gone.
        assert!(lock(&state.projects).get(&project_id).is_none());
        assert!(lock(&state.agents_by_id).get(&agent.id).is_none());
        assert!(lock(&state.project_locks).get(&project_id).is_none());
        // It was the active project → active is cleared.
        assert!(lock(&state.active_project_id).is_none());
        // Gone from disk too — the flat list no longer shows it.
        assert!(list_projects_impl(&state).unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_project_leaves_sibling_and_active_intact() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let a = create_project_in_only_dir(&state, "alpha");
        let b = create_project_in_only_dir(&state, "beta");
        set_active_project_impl(&state, b.id).unwrap();

        delete_project_impl(&state, a.id).await.unwrap();

        // The non-active sibling is untouched and still active.
        assert_eq!(*lock(&state.active_project_id), Some(b.id));
        assert!(lock(&state.projects).get(&b.id).is_some());
        assert!(lock(&state.project_locks).get(&b.id).is_some());
        // Only `alpha` is gone, on disk and in memory.
        assert!(lock(&state.projects).get(&a.id).is_none());
        let listed = list_projects_impl(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, b.id);
    }

    #[tokio::test]
    async fn delete_project_drains_in_flight_turn() {
        // Phase (a) must cancel + drain a running turn before removal, with no
        // deadlock (no `.await` under `registry_write`). The `AwaitCancellation`
        // scenario parks the turn until the shutdown cancel fires.
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        send_msg(&state, agent.id, "long task").await.unwrap();
        within(
            &emitter,
            "turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;

        // Bounded: if delete failed to drain it would hang here and trip the
        // timeout rather than deadlocking the suite.
        within(&emitter, "delete drains the project", async {
            delete_project_impl(&state, project_id).await.unwrap();
        })
        .await;

        assert!(lock(&state.projects).get(&project_id).is_none());
        assert!(lock(&state.agents_by_id).get(&agent.id).is_none());
        let channel = format!("agent:{}", agent.id);
        let cancelled = emitter.snapshot().into_iter().any(|(name, v)| {
            name == channel && v["type"] == "turn_end" && v["outcome"]["status"] == "cancelled"
        });
        assert!(
            cancelled,
            "draining a deleted project cancels its in-flight turn"
        );
    }

    #[tokio::test]
    async fn delete_project_succeeds_when_not_yet_opened() {
        // Available but never activated (not in `state.projects` / not locked):
        // delete resolves the owning directory from the index and still removes
        // the on-disk state.
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        // Simulate "available, not opened": drop the loaded handle + lock.
        lock(&state.projects).remove(&project.id);
        lock(&state.project_locks).remove(&project.id);

        delete_project_impl(&state, project.id).await.unwrap();
        assert!(list_projects_impl(&state).unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_project_unknown_id_is_benign_success() {
        // No loaded directory owns the id (e.g. a stale row / already removed
        // out-of-band) → benign success, not an error (engineer-approved policy).
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let keep = create_project_in_only_dir(&state, "alpha");

        delete_project_impl(&state, Uuid::now_v7()).await.unwrap();

        // The real project is untouched.
        assert!(lock(&state.projects).get(&keep.id).is_some());
        assert_eq!(list_projects_impl(&state).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn set_project_archived_flips_flag_in_listing() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");

        let archived_of = |s: &AppState| list_projects_impl(s).unwrap()[0].archived;
        assert!(!archived_of(&state), "new projects start un-archived");

        set_project_archived_impl(&state, project.id, true).unwrap();
        assert!(archived_of(&state));

        set_project_archived_impl(&state, project.id, false).unwrap();
        assert!(!archived_of(&state));
    }

    #[tokio::test]
    async fn set_project_archived_unknown_id_errors() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        create_project_in_only_dir(&state, "alpha");

        let err = set_project_archived_impl(&state, Uuid::now_v7(), true).unwrap_err();
        assert!(matches!(err, AppError::ProjectNotLoaded(_)));
    }

    #[tokio::test]
    async fn set_project_archived_works_for_unavailable_project() {
        // Archive is global view-state — it must work even when the project's
        // directory is offline (the "clear a stale unavailable row" lever).
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        // Simulate the directory going unavailable: drop the loaded handle but
        // keep the workspace cache (which is how an unavailable row is served).
        lock(&state.directories).clear();

        set_project_archived_impl(&state, project.id, true).unwrap();
        assert!(lock(&state.workspace).is_archived(project.id));
    }

    #[tokio::test]
    async fn set_project_archived_does_not_interrupt_a_running_agent() {
        // Archive is display-only: it must not cancel/drain an in-flight turn.
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        send_msg(&state, agent.id, "long task").await.unwrap();
        within(
            &emitter,
            "turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;

        set_project_archived_impl(&state, project_id, true).unwrap();

        // The turn is untouched: no cancellation terminal, still parked.
        let channel = format!("agent:{}", agent.id);
        let cancelled = emitter
            .snapshot()
            .into_iter()
            .any(|(name, v)| name == channel && v["type"] == "turn_end");
        assert!(!cancelled, "archiving must not end the running turn");

        // Wind the parked turn down so the test doesn't leave it hanging.
        cancel_agent_impl(&state, agent.id);
        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn delete_project_keeps_row_and_lock_when_on_disk_delete_fails() {
        // A real index-rewrite failure (the only thing core surfaces) must keep
        // the project loaded AND keep its inter-process lock — never leave it
        // routable-without-lock (the concurrency hazard).
        use std::os::unix::fs::PermissionsExt;

        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");

        // Make `.switchboard/` read-only so `write_jsonl` (index rewrite) can't
        // create its tmp file → core delete fails before the commit.
        let sb = tmp.path().join(".switchboard");
        std::fs::set_permissions(&sb, std::fs::Permissions::from_mode(0o555)).unwrap();

        let err = delete_project_impl(&state, project.id).await.unwrap_err();
        assert!(matches!(err, AppError::Core(CoreError::Io { .. })));

        std::fs::set_permissions(&sb, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Row kept, and crucially the lock is retained.
        assert!(lock(&state.projects).get(&project.id).is_some());
        assert!(lock(&state.project_locks).get(&project.id).is_some());
    }

    #[tokio::test]
    async fn delete_project_with_missing_index_removes_dir_and_does_not_resurrect_from_cache() {
        // Out-of-band missing index: the fast-path still resolves the loaded
        // project's directory (no ghosting), core removes the directory, and the
        // deleted id is dropped from the workspace cache so a later list can't
        // serve it back from the stale snapshot.
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        let root = lock(&state.projects)
            .get(&project.id)
            .map(|p| p.root.clone())
            .unwrap();
        // Prime the cache, then remove the index out-of-band.
        let _ = list_projects_impl(&state).unwrap();
        let canonical = tmp.path().canonicalize().unwrap();
        std::fs::remove_file(canonical.join(".switchboard").join("projects.jsonl")).unwrap();

        delete_project_impl(&state, project.id).await.unwrap();

        // The directory was actually removed (fast-path resolved it, not ghosted)…
        assert!(!root.exists());
        // …and the project does not reappear from the cached snapshot.
        assert!(
            list_projects_impl(&state)
                .unwrap()
                .iter()
                .all(|p| p.id != project.id)
        );
    }

    #[tokio::test]
    async fn delete_project_removes_unavailable_project_and_does_not_resurrect_from_cache() {
        // The user-facing bug: a project whose directory is gone shows as an
        // unavailable cached row, and deleting it must drop it for good — both
        // from the listing and from the persisted workspace cache that serves
        // unavailable rows (otherwise it resurrects on the next list / restart).
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        // Prime the workspace cache so the project can be served as a row.
        let _ = list_projects_impl(&state).unwrap();

        // Simulate the directory becoming unreachable (folder/volume gone): drop
        // the loaded handle + lock and the loaded directory registry, leaving only
        // the persisted cache — exactly how an unavailable row is served.
        lock(&state.projects).remove(&project.id);
        lock(&state.project_locks).remove(&project.id);
        lock(&state.directories).clear();

        // Precondition: it lists as an unavailable row, so delete is meaningful.
        let before = list_projects_impl(&state).unwrap();
        let row = before.iter().find(|p| p.id == project.id).unwrap();
        assert!(!row.available, "expected an unavailable cached row");

        delete_project_impl(&state, project.id).await.unwrap();

        // Gone from the listing and from the workspace cache (no resurrection).
        assert!(
            list_projects_impl(&state)
                .unwrap()
                .iter()
                .all(|p| p.id != project.id)
        );
        assert!(!lock(&state.workspace).knows_project(project.id));
    }

    #[tokio::test]
    async fn deleting_an_offline_project_leaves_disk_so_reconnecting_relists_it() {
        // Accepted best-effort limit (engineer-approved): deleting a project
        // whose directory is offline removes the listing row but cannot delete
        // the on-disk files. If that directory is reconnected (re-init reads the
        // surviving index), the project legitimately reappears. Pinned here so
        // the behavior is a conscious choice, not a silent surprise.
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        let _ = list_projects_impl(&state).unwrap();

        // Go offline: drop the loaded handle/lock/registry, leaving the on-disk
        // index intact.
        lock(&state.projects).remove(&project.id);
        lock(&state.project_locks).remove(&project.id);
        lock(&state.directories).clear();

        delete_project_impl(&state, project.id).await.unwrap();
        assert!(
            list_projects_impl(&state)
                .unwrap()
                .iter()
                .all(|p| p.id != project.id),
            "delete clears the offline listing row"
        );

        // Reconnect: re-init the same on-disk directory. Its index still lists the
        // project (delete never reached disk), so it comes back as available.
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let row = list_projects_impl(&state)
            .unwrap()
            .into_iter()
            .find(|p| p.id == project.id)
            .expect("offline-deleted project relists after the directory reconnects");
        assert!(row.available);
    }

    #[tokio::test]
    async fn set_session_locator_persists_to_registry_and_cache() {
        // The runtime-capture mechanism (M1 plumbing, wired to the M2 sink): a
        // Codex agent starts with no locator; setting one must land both on
        // disk (registry.jsonl) and in the `agents_by_id` cache, so the next
        // dispatch's DispatchContext reads the captured locator.
        let (tmp, state, _) = fresh_state_with_mock();
        let (_claude, project_id) = project_with_agent(&state, &tmp).await;
        let codex = create_agent_impl(&state, "codex1", HarnessKind::Codex, None, None).unwrap();
        assert!(codex.session_locator.is_none());

        let locator = SessionLocator::Codex {
            thread_id: "thread-abc".to_owned(),
            partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        };
        let updated = set_agent_session_locator_impl(&state, codex.id, locator.clone()).unwrap();
        assert_eq!(updated.session_locator, Some(locator.clone()));

        // On disk.
        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        let on_disk = project
            .list_agents()
            .unwrap()
            .into_iter()
            .find(|a| a.id == codex.id)
            .unwrap();
        assert_eq!(on_disk.session_locator, Some(locator.clone()));

        // In the cache.
        assert_eq!(
            lock(&state.agents_by_id)
                .get(&codex.id)
                .unwrap()
                .session_locator,
            Some(locator)
        );
    }

    #[tokio::test]
    async fn set_session_locator_nonexistent_errors() {
        let (_tmp, state, _) = fresh_state_with_mock();
        let err = set_agent_session_locator_impl(
            &state,
            Uuid::now_v7(),
            SessionLocator::Uuid(Uuid::new_v4()),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn project_session_locator_sink_writes_registry_and_cache() {
        // The dispatcher-injected sink (built per dispatch by the factory) must
        // persist a captured locator to both the registry and `agents_by_id`,
        // under `registry_write` — the same effect as the app op, reached via
        // the `SessionLocatorSink` trait the dispatcher calls on a capture event.
        use switchboard_dispatcher::SessionLocatorSink;

        let (tmp, state, _) = fresh_state_with_mock();
        let (_claude, project_id) = project_with_agent(&state, &tmp).await;
        let codex = create_agent_impl(&state, "codex1", HarnessKind::Codex, None, None).unwrap();

        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        let sink = crate::locator_sink::ProjectSessionLocatorSink::new(
            project,
            Arc::clone(&state.registry_write),
            Arc::clone(&state.agents_by_id),
        );

        let locator = SessionLocator::Codex {
            thread_id: "thread-xyz".to_owned(),
            partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        };
        sink.persist(codex.id, locator.clone()).unwrap();

        let on_disk = lock(&state.projects)
            .get(&project_id)
            .cloned()
            .unwrap()
            .list_agents()
            .unwrap()
            .into_iter()
            .find(|a| a.id == codex.id)
            .unwrap();
        assert_eq!(on_disk.session_locator, Some(locator.clone()));
        assert_eq!(
            lock(&state.agents_by_id)
                .get(&codex.id)
                .unwrap()
                .session_locator,
            Some(locator)
        );
    }

    #[tokio::test]
    async fn project_session_locator_sink_surfaces_harness_mismatch_as_error() {
        // A wrong-shape locator (a `Uuid` on a Codex agent) is rejected by the
        // core op and surfaces as a sink error — which the dispatcher turns into
        // a failed turn rather than persisting an unresumable record.
        use switchboard_dispatcher::SessionLocatorSink;

        let (tmp, state, _) = fresh_state_with_mock();
        let (_claude, project_id) = project_with_agent(&state, &tmp).await;
        let codex = create_agent_impl(&state, "codex1", HarnessKind::Codex, None, None).unwrap();
        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        let sink = crate::locator_sink::ProjectSessionLocatorSink::new(
            project,
            Arc::clone(&state.registry_write),
            Arc::clone(&state.agents_by_id),
        );

        assert!(
            sink.persist(codex.id, SessionLocator::Uuid(Uuid::new_v4()))
                .is_err(),
            "a Uuid locator on a Codex agent must be refused"
        );
    }

    #[tokio::test]
    async fn cancel_turn_on_idle_agent_is_a_no_op() {
        let (tmp, state, _emitter) = fresh_state_with_mock();
        let (agent, _project_id) = project_with_agent(&state, &tmp).await;
        // Never dispatched → nothing to cancel.
        assert_eq!(
            cancel_turn_impl(&state, agent.id),
            CancelOutcome::NothingToCancel
        );
    }

    #[tokio::test]
    async fn cancel_agent_stops_in_flight_turn() {
        // App-level routing for "stop agent"; the cancel-running + clear-backlog
        // + actor-survives semantics are covered in the dispatcher crate. Here we
        // assert the impl drives a live turn to a cancelled terminal + idle.
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, _project_id) = project_with_agent(&state, &tmp).await;

        send_msg(&state, agent.id, "long task").await.unwrap();
        within(
            &emitter,
            "turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;

        assert_eq!(
            cancel_agent_impl(&state, agent.id),
            CancelOutcome::Requested
        );

        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;
        let channel = format!("agent:{}", agent.id);
        let cancelled = emitter.snapshot().into_iter().any(|(name, v)| {
            name == channel && v["type"] == "turn_end" && v["outcome"]["status"] == "cancelled"
        });
        assert!(cancelled, "cancel_agent synthesizes a cancelled terminal");
    }

    #[tokio::test]
    async fn cancel_agent_on_idle_agent_is_a_no_op() {
        let (tmp, state, _emitter) = fresh_state_with_mock();
        let (agent, _project_id) = project_with_agent(&state, &tmp).await;
        assert_eq!(
            cancel_agent_impl(&state, agent.id),
            CancelOutcome::NothingToCancel
        );
    }

    #[tokio::test]
    async fn cancel_send_impl_cancels_every_recipient_of_the_send() {
        // App-level routing: the send-scoped cancellation semantics (the
        // `send_id` scoping guard, queued-item removal) are covered in the
        // dispatcher crate. Here we assert the impl fans the cancel out to all
        // recipients of one shared `send_id`, driving each to a cancelled
        // terminal.
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent_a, _project_id) = project_with_agent(&state, &tmp).await;
        let agent_b =
            create_agent_impl(&state, "assistant-2", HarnessKind::ClaudeCode, None, None).unwrap();

        // One Send fanned out to both: same `send_id`, one call per recipient.
        let send_id = Uuid::now_v7();
        send_message_impl(&state, agent_a.id, "fan-out", Vec::new(), send_id)
            .await
            .unwrap();
        send_message_impl(&state, agent_b.id, "fan-out", Vec::new(), send_id)
            .await
            .unwrap();
        within(
            &emitter,
            "both turns in flight",
            emitter.wait_for_type("turn_start", 2),
        )
        .await;

        cancel_send_impl(&state, send_id, &[agent_a.id, agent_b.id]);

        within(
            &emitter,
            "both agents idle",
            emitter.wait_for_type("agent_idle", 2),
        )
        .await;
        for agent in [&agent_a, &agent_b] {
            let channel = format!("agent:{}", agent.id);
            let cancelled = emitter.snapshot().into_iter().any(|(name, v)| {
                name == channel && v["type"] == "turn_end" && v["outcome"]["status"] == "cancelled"
            });
            assert!(
                cancelled,
                "recipient {} is driven to a cancelled terminal by cancel_send",
                agent.name
            );
        }
    }

    #[tokio::test]
    async fn drain_helper_cancels_drains_then_releases_lock() {
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        send_msg(&state, agent.id, "long task").await.unwrap();
        // Wait until the turn is actually live (started) before draining — the
        // event-based equivalent of the old `agent_status == InFlight` check.
        within(
            &emitter,
            "turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;
        assert!(
            lock(&state.project_locks).contains_key(&project_id),
            "project lock is held while the turn is live"
        );

        drain_agents_then_release_locks(&state, &[agent.id], &[project_id], CancelSource::Shutdown)
            .await;

        // `shutdown_agent` returns only after the turn is cancelled and drained,
        // so by the time the helper returns the cancelled-shutdown `turn_end`
        // must already be on the wire — the event-based equivalent of the old
        // `agent_status == Idle` check (the actor tears down on shutdown rather
        // than emitting `agent_idle`, so the drained turn's terminal is the
        // observable proof it finished). The lock must have been released after
        // — never before — that drain.
        let channel = format!("agent:{}", agent.id);
        let cancelled = emitter.snapshot().into_iter().any(|(name, v)| {
            name == channel
                && v["type"] == "turn_end"
                && v["outcome"]["status"] == "cancelled"
                && v["outcome"]["source"] == "shutdown"
        });
        assert!(
            cancelled,
            "drain helper cancels with source = shutdown and the turn drained before returning"
        );
        assert!(
            !lock(&state.project_locks).contains_key(&project_id),
            "project lock released only after the turn drained"
        );
    }

    #[tokio::test]
    async fn send_message_writes_send_record_to_project_journal() {
        // End-to-end through the app sink: a completed turn writes one `send`
        // record (the user's side) and no `outcome` record (content is in the
        // harness file) to the project's journal.jsonl.
        let (tmp, state, emitter) = fresh_state_with_mock();
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        send_msg(&state, agent.id, "journal me").await.unwrap();
        // The send record is written at turn-start, but await the terminal
        // `agent_idle` so the whole turn (and its journaling) has settled.
        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;

        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        let records = switchboard_core::journal::read_records(&project.journal_path()).unwrap();
        assert_eq!(
            records.len(),
            1,
            "one send record, no outcome for a completed turn"
        );
        match &records[0] {
            switchboard_core::JournalRecord::Send {
                prompt, agent_id, ..
            } => {
                assert_eq!(prompt, "journal me");
                assert_eq!(*agent_id, agent.id);
            }
            other => panic!("expected a send record, got {other:?}"),
        }
    }

    fn test_attachment(
        label: &str,
        kind: switchboard_core::AttachmentKind,
        path: &str,
    ) -> Attachment {
        Attachment {
            label: label.to_owned(),
            kind,
            path: path.to_owned(),
            original_name: "orig".to_owned(),
        }
    }

    #[tokio::test]
    async fn stage_attachment_copies_into_project_dir_and_returns_absolute_path() {
        let (tmp, state, _emitter) = fresh_state_with_mock();
        let (_agent, project_id) = project_with_agent(&state, &tmp).await;

        let source = tmp.path().join("diagram.png");
        std::fs::write(&source, b"PNG-BYTES").unwrap();

        let staged = stage_attachment_impl(&state, project_id, &source)
            .await
            .unwrap();

        let staged_path = Path::new(&staged.path);
        assert!(staged_path.is_absolute(), "staged path is absolute");
        assert_eq!(std::fs::read(staged_path).unwrap(), b"PNG-BYTES");
        assert_eq!(staged.original_name, "diagram.png");
        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        assert!(
            staged_path.starts_with(project.attachments_dir()),
            "staged under the project attachments dir"
        );
    }

    #[tokio::test]
    async fn stage_attachment_is_collision_safe_for_same_filename() {
        let (tmp, state, _emitter) = fresh_state_with_mock();
        let (_agent, project_id) = project_with_agent(&state, &tmp).await;
        let source = tmp.path().join("notes.txt");

        std::fs::write(&source, b"one").unwrap();
        let first = stage_attachment_impl(&state, project_id, &source)
            .await
            .unwrap();
        std::fs::write(&source, b"two").unwrap();
        let second = stage_attachment_impl(&state, project_id, &source)
            .await
            .unwrap();

        assert_ne!(
            first.path, second.path,
            "same basename stages to distinct files"
        );
        assert_eq!(std::fs::read(&first.path).unwrap(), b"one");
        assert_eq!(std::fs::read(&second.path).unwrap(), b"two");
    }

    #[test]
    fn sanitize_basename_strips_separators_and_dot_names() {
        assert_eq!(sanitize_basename("clean.png"), "clean.png");
        assert_eq!(sanitize_basename("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_basename("with\nctrl"), "with_ctrl");
        assert_eq!(sanitize_basename(".."), "file");
        assert_eq!(sanitize_basename("."), "file");
        assert_eq!(sanitize_basename("   "), "file");
    }

    #[tokio::test]
    async fn send_with_attachments_footers_adapter_prompt_and_journals_clean_text() {
        let (tmp, state, emitter) = fresh_state_with_mock();
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        let attachment = test_attachment(
            "image-1",
            switchboard_core::AttachmentKind::Image,
            "/abs/attachments/u__diagram.png",
        );
        send_message_impl(
            &state,
            agent.id,
            "look at this",
            vec![attachment.clone()],
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;

        // The mock echoes the dispatched prompt into a content_chunk, so the
        // footer the adapter actually received is observable on the wire.
        let channel = format!("agent:{}", agent.id);
        let footered = emitter.snapshot().into_iter().any(|(name, v)| {
            name == channel
                && v["type"] == "content_chunk"
                && v["text"].as_str().is_some_and(|t| {
                    t.contains("Attached files (read them):")
                        && t.contains("image-1: /abs/attachments/u__diagram.png")
                })
        });
        assert!(footered, "the adapter received the attachment footer");

        // The journal stores the CLEAN prompt + the structured attachment — never
        // the footer or raw paths in the prompt text.
        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        let records = switchboard_core::journal::read_records(&project.journal_path()).unwrap();
        match records.as_slice() {
            [
                switchboard_core::JournalRecord::Send {
                    prompt,
                    attachments,
                    ..
                },
            ] => {
                assert_eq!(prompt, "look at this", "journal keeps the clean prompt");
                assert_eq!(attachments, &vec![attachment]);
            }
            other => panic!("expected one clean Send, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_queued_message_round_trips_attachments() {
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, _project_id) = project_with_agent(&state, &tmp).await;

        // First send parks in flight (AwaitCancellation); the second queues behind it.
        send_msg(&state, agent.id, "in flight").await.unwrap();
        within(
            &emitter,
            "turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;

        let attachment = test_attachment(
            "text-1",
            switchboard_core::AttachmentKind::Text,
            "/abs/attachments/u__notes.txt",
        );
        let queued = send_message_impl(
            &state,
            agent.id,
            "queued",
            vec![attachment.clone()],
            Uuid::now_v7(),
        )
        .await
        .unwrap();

        let removed = remove_queued_message_impl(&state, agent.id, queued)
            .await
            .unwrap();
        assert_eq!(removed.prompt, "queued");
        assert_eq!(
            removed.attachments,
            vec![attachment],
            "dequeue restores the chips alongside the text"
        );
    }

    #[test]
    fn gc_removes_unreferenced_and_keeps_referenced() {
        let dir = TempDir::new().unwrap();
        let kept = dir.path().join("kept.png");
        let orphan = dir.path().join("orphan.png");
        std::fs::write(&kept, b"k").unwrap();
        std::fs::write(&orphan, b"o").unwrap();
        let referenced: HashSet<PathBuf> = [kept.clone()].into_iter().collect();

        gc_unreferenced_attachments(dir.path(), &referenced);

        assert!(kept.exists(), "referenced file survives");
        assert!(
            !orphan.exists(),
            "unreferenced (orphan drop) file is deleted"
        );
    }

    #[test]
    fn gc_missing_dir_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("attachments");
        gc_unreferenced_attachments(&missing, &HashSet::new());
        assert!(
            !missing.exists(),
            "GC does not create the dir it didn't find"
        );
    }

    #[test]
    fn collect_referenced_paths_reads_send_attachments() {
        let path = "/abs/attachments/u__a.png";
        let journal = vec![switchboard_core::JournalRecord::Send {
            send_id: Uuid::now_v7(),
            turn_id: Uuid::now_v7(),
            agent_id: Uuid::now_v7(),
            prompt: "p".to_owned(),
            attachments: vec![test_attachment(
                "image-1",
                switchboard_core::AttachmentKind::Image,
                path,
            )],
            at: chrono::Utc::now(),
        }];
        let refs = collect_referenced_attachment_paths(&journal);
        assert!(refs.contains(&PathBuf::from(path)));
    }

    #[tokio::test]
    async fn remove_queued_message_returns_payload_and_prevents_dispatch() {
        // Send twice to a busy agent: the first turn parks in flight
        // (AwaitCancellation), so the second send is queued behind it. Removing
        // the queued message by its MessageId returns its payload and ensures it
        // never dispatches.
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, _project_id) = project_with_agent(&state, &tmp).await;

        // First send starts the turn and parks it.
        send_msg(&state, agent.id, "blocker").await.unwrap();
        within(
            &emitter,
            "blocker turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;

        // Second send queues behind the in-flight turn.
        let queued_id = send_msg(&state, agent.id, "queued").await.unwrap();

        let removed = remove_queued_message_impl(&state, agent.id, queued_id)
            .await
            .expect("the queued message is removable");
        assert_eq!(removed.agent_id, agent.id);
        assert_eq!(removed.prompt, "queued");

        // Removing it again (now unknown) → QueuedMessageNotFound.
        let err = remove_queued_message_impl(&state, agent.id, queued_id)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::QueuedMessageNotFound(id) if id == queued_id));

        // Let the blocker finish; only the blocker's turn ever started.
        cancel_turn_impl(&state, agent.id);
        within(
            &emitter,
            "agent_idle",
            emitter.wait_for_type("agent_idle", 1),
        )
        .await;
        assert_eq!(
            count_type(&emitter.snapshot(), "turn_start"),
            1,
            "the removed message never dispatched"
        );
    }

    #[tokio::test]
    async fn remove_queued_message_unknown_id_errors() {
        // No actor for the agent (never dispatched) → NotQueued maps to
        // QueuedMessageNotFound.
        let (tmp, state, _emitter) = fresh_state_with_mock();
        let (agent, _project_id) = project_with_agent(&state, &tmp).await;
        let unknown = Uuid::now_v7();
        let err = remove_queued_message_impl(&state, agent.id, unknown)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::QueuedMessageNotFound(id) if id == unknown));
    }

    // ---- M4.6: multi-directory workspace ----

    #[tokio::test]
    async fn two_spellings_of_same_directory_collapse_to_one() {
        // Init the same directory under two spellings (a plain path and one
        // with a redundant `/./` component). `Directory::at` canonicalizes, so
        // both must collapse to a single `directories` entry, a single
        // workspace entry, and its projects must list exactly once.
        let (tmp, state, _) = fresh_state_with_mock();
        let plain = tmp.path().to_str().unwrap().to_owned();
        let dotted = format!("{plain}/./");

        init_directory_impl(&state, &plain).await.unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        init_directory_impl(&state, &dotted).await.unwrap();

        assert_eq!(
            lock(&state.directories).len(),
            1,
            "two spellings must collapse to one loaded directory"
        );
        assert_eq!(
            lock(&state.workspace).entries().len(),
            1,
            "two spellings must collapse to one workspace entry"
        );
        let listings = list_projects_impl(&state).unwrap();
        assert_eq!(listings.len(), 1, "the project lists exactly once");
        assert_eq!(listings[0].id, project.id);
        assert!(listings[0].available);
    }

    #[tokio::test]
    async fn remove_directory_drains_turns_releases_locks_and_preserves_disk() {
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, project_id) = project_with_agent(&state, &tmp).await;

        send_msg(&state, agent.id, "long task").await.unwrap();
        within(
            &emitter,
            "turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;
        assert!(lock(&state.project_locks).contains_key(&project_id));

        remove_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();

        // The in-flight turn was drained with source = shutdown, the lock
        // released, and all in-memory state for the directory dropped.
        let channel = format!("agent:{}", agent.id);
        let cancelled = emitter.snapshot().into_iter().any(|(name, v)| {
            name == channel
                && v["type"] == "turn_end"
                && v["outcome"]["status"] == "cancelled"
                && v["outcome"]["source"] == "shutdown"
        });
        assert!(
            cancelled,
            "remove drains the live turn with source = shutdown"
        );
        assert!(!lock(&state.project_locks).contains_key(&project_id));
        assert!(lock(&state.projects).is_empty());
        assert!(lock(&state.agents_by_id).is_empty());
        assert!(lock(&state.directories).is_empty());
        assert!(lock(&state.workspace).entries().is_empty());
        assert!(lock(&state.active_project_id).is_none());

        // `.switchboard/` was never deleted — re-init restores the project.
        assert!(tmp.path().join(".switchboard").is_dir());
        let info = init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].id, project_id);

        // Removing an absent/never-added directory is Ok (idempotent).
        let absent = TempDir::new().unwrap();
        remove_directory_impl(&state, absent.path().to_str().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_projects_aggregates_across_two_directories() {
        let (tmp_a, state, _) = fresh_state_with_mock();
        let tmp_b = TempDir::new().unwrap();

        init_directory_impl(&state, tmp_a.path().to_str().unwrap())
            .await
            .unwrap();
        let dir_a = lock(&state.directories)
            .keys()
            .next()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let alpha = create_project_impl(&state, "alpha", &dir_a).unwrap();

        let info_b = init_directory_impl(&state, tmp_b.path().to_str().unwrap())
            .await
            .unwrap();
        let dir_b = info_b.path.clone();
        let beta = create_project_impl(&state, "beta", &dir_b).unwrap();

        let listings = list_projects_impl(&state).unwrap();
        assert_eq!(listings.len(), 2, "both directories' projects aggregate");

        let alpha_row = listings.iter().find(|l| l.id == alpha.id).unwrap();
        let beta_row = listings.iter().find(|l| l.id == beta.id).unwrap();
        assert_eq!(alpha_row.directory, dir_a);
        assert_eq!(beta_row.directory, dir_b);
        assert!(alpha_row.available && beta_row.available);
    }

    #[tokio::test]
    async fn list_workspace_directories_includes_empty_directory_and_reports_persistable() {
        let dir = TempDir::new().unwrap();
        let ws_path = dir.path().join("workspace.yaml");
        let tmp = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        )
        .with_workspace(ws_path);

        // A directory with no projects must still appear in the switcher —
        // unlike `list_projects`, which emits zero rows for it.
        let info = init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(
            list_projects_impl(&state).unwrap().is_empty(),
            "no projects created yet"
        );

        let ws = list_workspace_directories_impl(&state);
        assert!(ws.persistable, "a readable workspace.yaml is persistable");
        assert_eq!(ws.directories.len(), 1);
        assert_eq!(ws.directories[0].path, info.path);
        assert!(
            ws.directories[0].available,
            "a freshly-initialized directory is loaded"
        );
    }

    #[tokio::test]
    async fn list_workspace_directories_marks_unloaded_directory_unavailable_and_not_persistable() {
        // `fresh_state_with_mock` builds state with no `workspace_path`, the
        // not-persistable case (an unreadable existing workspace.yaml).
        let (_tmp, state, _) = fresh_state_with_mock();
        // Register a directory in the registry without loading it into
        // `state.directories` — as if it were unmounted at startup.
        let phantom = PathBuf::from("/definitely/not/mounted");
        lock(&state.workspace).add(phantom.clone());

        let ws = list_workspace_directories_impl(&state);
        assert!(
            !ws.persistable,
            "state with no workspace_path is not persistable"
        );
        assert_eq!(ws.directories.len(), 1);
        assert_eq!(ws.directories[0].path, phantom.to_string_lossy());
        assert!(
            !ws.directories[0].available,
            "a registered-but-unloaded directory is unavailable"
        );
    }

    #[tokio::test]
    async fn codex_session_id_collision_rejected_across_two_directories() {
        // Codex ids are server-assigned + globally unique, so a collision
        // across two distinct directories must be rejected.
        let tmp_home = TempDir::new().unwrap();
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        let state = mock_app_state();
        let session_id = Uuid::now_v7();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        stage_codex_session_file(tmp_home.path(), date, &session_id.to_string());

        let info_a = init_directory_impl(&state, tmp_a.path().to_str().unwrap())
            .await
            .unwrap();
        let alpha = create_project_impl(&state, "alpha", &info_a.path).unwrap();
        set_active_project_impl(&state, alpha.id).unwrap();
        attach_agent_impl(
            &state,
            "a",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        // Second directory, attempt to attach the same Codex session id.
        let info_b = init_directory_impl(&state, tmp_b.path().to_str().unwrap())
            .await
            .unwrap();
        let beta = create_project_impl(&state, "beta", &info_b.path).unwrap();
        set_active_project_impl(&state, beta.id).unwrap();
        let err = attach_agent_impl(
            &state,
            "b",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, AppError::SessionAlreadyAttached { .. }),
            "Codex id collision across directories must be rejected, got {err:?}"
        );
    }

    #[tokio::test]
    async fn claude_same_session_id_in_two_directories_is_allowed() {
        // Claude ids are cwd-namespaced. The same UUID under two different
        // directories is a genuinely-distinct session (different on-disk file),
        // so the per-directory scan must allow it.
        let tmp_home = TempDir::new().unwrap();
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        let state = mock_app_state();
        let session_id = Uuid::now_v7();
        // Stage the same session id under each directory's cwd namespace.
        stage_claude_session_file(tmp_home.path(), tmp_a.path(), &session_id);
        stage_claude_session_file(tmp_home.path(), tmp_b.path(), &session_id);

        let info_a = init_directory_impl(&state, tmp_a.path().to_str().unwrap())
            .await
            .unwrap();
        let alpha = create_project_impl(&state, "alpha", &info_a.path).unwrap();
        set_active_project_impl(&state, alpha.id).unwrap();
        attach_agent_impl(
            &state,
            "a",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .unwrap();

        let info_b = init_directory_impl(&state, tmp_b.path().to_str().unwrap())
            .await
            .unwrap();
        let beta = create_project_impl(&state, "beta", &info_b.path).unwrap();
        set_active_project_impl(&state, beta.id).unwrap();
        attach_agent_impl(
            &state,
            "b",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
            None,
            None,
        )
        .expect("same Claude id under a different directory is a distinct session");
    }

    // ---- M4.6 hardening: directory-identity helper, list resilience ----

    #[tokio::test]
    async fn create_project_canonicalization_matches_init_directory_key() {
        // The dedup invariant the flat cross-directory list depends on: the key
        // `create_project_impl` resolves a directory by (`canonicalize_boundary`)
        // must equal the key `init_directory_impl` stored for that directory
        // (`Directory::at`'s canonical path). Feed `create_project` a noisy
        // spelling of the same path and assert it still finds the loaded
        // directory and creates the project there.
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let stored_key = lock(&state.directories).keys().next().unwrap().clone();

        let noisy = format!("{}/./", tmp.path().to_str().unwrap());
        let summary = create_project_impl(&state, "alpha", &noisy).unwrap();

        let project = lock(&state.projects).get(&summary.id).cloned().unwrap();
        assert_eq!(
            project.directory, stored_key,
            "create_project must resolve to the same canonical key init stored"
        );
        assert_eq!(
            lock(&state.directories).len(),
            1,
            "no second directory entry was created"
        );
    }

    #[tokio::test]
    async fn list_projects_with_no_changes_performs_no_write() {
        // Persist-on-change: a second list with nothing changed must not rewrite
        // workspace.yaml.
        let dir = TempDir::new().unwrap();
        let ws_path = dir.path().join("workspace.yaml");
        let tmp = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        )
        .with_workspace(ws_path.clone());

        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        create_project_in_only_dir(&state, "alpha");

        // First list refreshes the cache (project added since last persist) and
        // writes the file.
        list_projects_impl(&state).unwrap();
        let first_mtime = std::fs::metadata(&ws_path).unwrap().modified().unwrap();

        // Second list: nothing changed → no write, so mtime is unchanged.
        list_projects_impl(&state).unwrap();
        let second_mtime = std::fs::metadata(&ws_path).unwrap().modified().unwrap();
        assert_eq!(
            first_mtime, second_mtime,
            "an unchanged list must not rewrite workspace.yaml"
        );
    }

    #[tokio::test]
    async fn list_projects_unmounted_directory_falls_back_to_cache_without_write() {
        // A directory present in the workspace but not loaded (unmounted) serves
        // its cached snapshot as unavailable, and produces no write.
        let dir = TempDir::new().unwrap();
        let ws_path = dir.path().join("workspace.yaml");
        let tmp = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        )
        .with_workspace(ws_path.clone());

        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_in_only_dir(&state, "alpha");
        // Prime the cache + persist.
        list_projects_impl(&state).unwrap();

        // Simulate the directory becoming unavailable: drop the loaded handle.
        let key = lock(&state.projects)
            .get(&proj.id)
            .map(|p| p.directory.clone())
            .unwrap();
        lock(&state.directories).remove(&key);
        let before = std::fs::metadata(&ws_path).unwrap().modified().unwrap();

        let listings = list_projects_impl(&state).unwrap();
        assert_eq!(listings.len(), 1, "cached snapshot still lists the project");
        assert!(!listings[0].available, "unloaded directory is unavailable");
        let after = std::fs::metadata(&ws_path).unwrap().modified().unwrap();
        assert_eq!(before, after, "cache-fallback path must not rewrite");
    }

    #[tokio::test]
    async fn list_projects_corrupt_directory_does_not_fail_others_or_refresh_cache() {
        // A corrupt registry in directory A must not refresh/persist A's cache
        // and must not fail the listing of healthy directory B.
        let dir = TempDir::new().unwrap();
        let ws_path = dir.path().join("workspace.yaml");
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        let state = AppState::new(
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            Arc::clone(&mock),
            emitter as Arc<dyn EventEmitter>,
        )
        .with_workspace(ws_path.clone());

        let info_a = init_directory_impl(&state, tmp_a.path().to_str().unwrap())
            .await
            .unwrap();
        let alpha = create_project_impl(&state, "alpha", &info_a.path).unwrap();
        let info_b = init_directory_impl(&state, tmp_b.path().to_str().unwrap())
            .await
            .unwrap();
        let beta = create_project_impl(&state, "beta", &info_b.path).unwrap();
        // Prime caches for both.
        list_projects_impl(&state).unwrap();
        let cached_a_before: Vec<ProjectSummary> = lock(&state.workspace)
            .entries()
            .iter()
            .find(|e| e.path == tmp_a.path().canonicalize().unwrap())
            .map(|e| e.cached_projects.clone())
            .unwrap();

        // Corrupt A's projects index (Switchboard-owned JSONL).
        let index_a = tmp_a.path().join(".switchboard").join("projects.jsonl");
        std::fs::write(&index_a, "{ this is not valid json\n").unwrap();

        let listings = list_projects_impl(&state).unwrap();
        // B still lists; A degrades to its cached snapshot as unavailable.
        let beta_row = listings
            .iter()
            .find(|l| l.id == beta.id)
            .expect("healthy directory B still lists");
        assert!(beta_row.available);
        let alpha_row = listings
            .iter()
            .find(|l| l.id == alpha.id)
            .expect("corrupt directory A still lists from cache");
        assert!(
            !alpha_row.available,
            "corrupt directory degrades to unavailable"
        );

        // A's cache was NOT refreshed from the corrupt read.
        let cached_a_after: Vec<ProjectSummary> = lock(&state.workspace)
            .entries()
            .iter()
            .find(|e| e.path == tmp_a.path().canonicalize().unwrap())
            .map(|e| e.cached_projects.clone())
            .unwrap();
        assert_eq!(
            cached_a_before, cached_a_after,
            "corrupt read must not overwrite the last-good cached snapshot"
        );
    }

    #[tokio::test]
    async fn open_project_skips_corrupt_unrelated_directory() {
        // find_project_in_directories must skip-and-log a corrupt unrelated
        // directory (A) and still open a healthy project in directory B.
        let (tmp_a, state, _) = fresh_state_with_mock();
        let tmp_b = TempDir::new().unwrap();
        let info_a = init_directory_impl(&state, tmp_a.path().to_str().unwrap())
            .await
            .unwrap();
        // A has a project so its (now-corrupt) index would otherwise be read.
        create_project_impl(&state, "alpha", &info_a.path).unwrap();
        let info_b = init_directory_impl(&state, tmp_b.path().to_str().unwrap())
            .await
            .unwrap();
        let beta = create_project_impl(&state, "beta", &info_b.path).unwrap();

        // Evict B from the loaded set so open must locate it via directory scan.
        lock(&state.projects).remove(&beta.id);
        lock(&state.project_locks).remove(&beta.id);

        // Corrupt A's registry. HashMap iteration order is nondeterministic, so
        // A may be visited before B.
        let index_a = tmp_a.path().join(".switchboard").join("projects.jsonl");
        std::fs::write(&index_a, "{ corrupt\n").unwrap();

        let reopened = open_project_impl(&state, beta.id)
            .expect("open of a healthy project succeeds despite an unrelated corrupt directory");
        assert_eq!(reopened.id, beta.id);
    }

    // ---- M4.6 hardening: remove_directory teardown-race tests ----

    #[tokio::test]
    async fn remove_directory_races_send_no_second_turn_and_no_orphan_actor() {
        // (a) A `send` racing a `remove_directory` of its agent's directory must
        // not produce a second `turn_start` after removal begins, and no orphan
        // dispatcher actor may survive. The first send parks a turn in flight;
        // remove drains it while a concurrent late send is issued.
        let (tmp, state, emitter) =
            fresh_state_with_scenario(switchboard_harness::MockScenario::AwaitCancellation);
        let (agent, _project_id) = project_with_agent(&state, &tmp).await;
        let state = Arc::new(state);

        // First send starts + parks the turn.
        send_msg(&state, agent.id, "blocker").await.unwrap();
        within(
            &emitter,
            "blocker turn_start",
            emitter.wait_for_type("turn_start", 1),
        )
        .await;
        assert_eq!(count_type(&emitter.snapshot(), "turn_start"), 1);

        // Race: remove the directory concurrently with another send to the same
        // agent. Whichever interleaving wins, the cleared `agents_by_id` +
        // dispatcher `Closing` slot guarantee the late send never yields a
        // second turn.
        let path = tmp.path().to_str().unwrap().to_owned();
        let remove_state = Arc::clone(&state);
        let send_state = Arc::clone(&state);
        let agent_id = agent.id;
        let remover =
            tokio::spawn(async move { remove_directory_impl(&remove_state, &path).await });
        let sender = tokio::spawn(async move { send_msg(&send_state, agent_id, "late").await });
        remover.await.unwrap().unwrap();
        let _ = sender.await.unwrap();

        // No second turn ever started, and no actor/subprocess survives.
        assert_eq!(
            count_type(&emitter.snapshot(), "turn_start"),
            1,
            "the late send must not produce a second turn_start"
        );
        assert!(
            state.dispatcher.agent_slot_count() == 0,
            "no orphan dispatcher actor survives removal"
        );
        assert!(lock(&state.agents_by_id).is_empty());
        assert!(lock(&state.projects).is_empty());
        assert!(lock(&state.project_locks).is_empty());
        assert!(lock(&state.directories).is_empty());
    }

    #[tokio::test]
    async fn remove_directory_races_create_agent_and_create_project() {
        // (b) create_agent / create_project racing a remove of the same
        // directory must not strand state for a removed directory. After the
        // race settles, the directory is gone and no project/agent for it
        // survives in the routable maps.
        let (tmp, state, _emitter) = fresh_state_with_mock();
        let (_agent, _project_id) = project_with_agent(&state, &tmp).await;
        let state = Arc::new(state);

        let path = tmp.path().to_str().unwrap().to_owned();
        let dir_path = lock(&state.directories)
            .keys()
            .next()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let remove_state = Arc::clone(&state);
        let agent_state = Arc::clone(&state);
        let project_state = Arc::clone(&state);
        let remove_path = path.clone();
        let remover =
            tokio::spawn(async move { remove_directory_impl(&remove_state, &remove_path).await });
        // create_agent / create_project are synchronous; run them on blocking
        // tasks so they truly race the async remove.
        let agent_h = tokio::task::spawn_blocking(move || {
            create_agent_impl(&agent_state, "racer", HarnessKind::ClaudeCode, None, None)
        });
        let project_h = tokio::task::spawn_blocking(move || {
            create_project_impl(&project_state, "racer-proj", &dir_path)
        });
        remover.await.unwrap().unwrap();
        let _ = agent_h.await.unwrap();
        let _ = project_h.await.unwrap();

        // The directory is gone. `registry_write` serializes the racers against
        // remove, so any create that happened to land BEFORE remove is torn
        // down by it (its project lives under the removed directory), and any
        // that landed AFTER could not resolve the directory/active project.
        assert!(lock(&state.directories).is_empty());
        let no_survivor_under_removed = lock(&state.projects)
            .values()
            .all(|p| p.directory.to_string_lossy() != *path);
        assert!(
            no_survivor_under_removed,
            "no project for the removed directory survives in the routable map"
        );
        assert!(
            state.dispatcher.agent_slot_count() == 0,
            "no orphan dispatcher actor survives the race"
        );
    }

    #[tokio::test]
    async fn remove_directory_races_open_project() {
        // (c) open_project of one of the removed directory's projects, racing
        // the remove. Either the open wins (project loaded, then it is a
        // distinct directory's state — but here it's the same directory, so
        // remove tears it down) or remove wins (open fails to resolve). Either
        // way no post-remove project/lock/actor survives for the directory.
        let (tmp, state, _emitter) = fresh_state_with_mock();
        let (_agent, project_id) = project_with_agent(&state, &tmp).await;
        // Evict the project from the loaded set so open takes the first-open
        // (directory-scan) path and genuinely races the remove.
        let state = Arc::new(state);
        {
            lock(&state.projects).remove(&project_id);
            lock(&state.project_locks).remove(&project_id);
        }

        let path = tmp.path().to_str().unwrap().to_owned();
        let remove_state = Arc::clone(&state);
        let open_state = Arc::clone(&state);
        let remover =
            tokio::spawn(async move { remove_directory_impl(&remove_state, &path).await });
        let opener =
            tokio::task::spawn_blocking(move || open_project_impl(&open_state, project_id));
        remover.await.unwrap().unwrap();
        let _ = opener.await.unwrap();

        // The directory is removed; nothing routable for it survives.
        assert!(lock(&state.directories).is_empty());
        assert!(
            !lock(&state.projects).contains_key(&project_id),
            "no post-remove project survives for the removed directory"
        );
        assert!(
            !lock(&state.project_locks).contains_key(&project_id),
            "no post-remove lock survives"
        );
        assert!(
            state.dispatcher.agent_slot_count() == 0,
            "no orphan dispatcher actor survives the race"
        );
    }

    // ---- load_project_conversation: pure-merge unit tests (system-design §7) ----

    use chrono::{DateTime, TimeZone, Utc};
    use switchboard_core::JournalRecord;
    use switchboard_harness::{ContentKind, LoadedTranscript, Turn, TurnItem, TurnStatus};

    /// A fixed instant offset by `secs` seconds — deterministic timestamps so
    /// ordering assertions don't depend on wall-clock.
    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).single().unwrap()
    }

    fn send_record(
        send_id: SendId,
        turn_id: Uuid,
        agent_id: AgentId,
        prompt: &str,
        t: i64,
    ) -> JournalRecord {
        JournalRecord::Send {
            send_id,
            turn_id,
            agent_id,
            prompt: prompt.to_owned(),
            attachments: Vec::new(),
            at: at(t),
        }
    }

    fn outcome_record(
        send_id: SendId,
        turn_id: Uuid,
        agent_id: AgentId,
        outcome: serde_json::Value,
        t: i64,
    ) -> JournalRecord {
        JournalRecord::Outcome {
            send_id,
            turn_id,
            agent_id,
            outcome,
            started_at: at(t),
            ended_at: at(t + 1),
        }
    }

    fn agent_turn(turn_id: Uuid, agent_id: AgentId, text: &str, t: i64) -> Turn {
        Turn::Agent {
            turn_id,
            agent_id,
            started_at: at(t),
            ended_at: Some(at(t + 1)),
            status: TurnStatus::Complete,
            items: vec![TurnItem::Text {
                kind: ContentKind::Text,
                text: text.to_owned(),
            }],
            usage: None,
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
            stable_message_id: None,
        }
    }

    fn user_turn(turn_id: Uuid, agent_id: AgentId, text: &str, t: i64) -> Turn {
        Turn::User {
            turn_id,
            agent_id,
            started_at: at(t),
            text: text.to_owned(),
        }
    }

    fn transcript_of(turns: Vec<Turn>) -> LoadedTranscript {
        LoadedTranscript {
            turns,
            meta: None,
            last_rate_limit: None,
            last_rate_limit_as_of: None,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn merge_carries_hydration_key_onto_agent_turn_and_serializes_it() {
        // The stable hydration key must survive the project-conversation merge
        // AND reach the IPC wire — a parser-only implementation would pass the
        // per-agent serialization test while the project path silently dropped
        // it. Guards that trap.
        let agent = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let mut turn = agent_turn(turn_id, agent, "hi", 2);
        if let Turn::Agent { hydration_key, .. } = &mut turn {
            *hydration_key = Some("msg_disk01".to_owned());
        }
        let merged =
            merge_project_conversation(vec![], vec![(agent, transcript_of(vec![turn]), None)]);

        let key = merged
            .items
            .iter()
            .find_map(|i| match i {
                ConversationItem::AgentTurn { hydration_key, .. } => Some(hydration_key.clone()),
                _ => None,
            })
            .expect("an agent turn");
        assert_eq!(key.as_deref(), Some("msg_disk01"), "merge carries the key");

        let value = serde_json::to_value(&merged).unwrap();
        let item = value["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["kind"] == "agent_turn")
            .expect("agent_turn on the wire");
        assert_eq!(
            item["hydration_key"], "msg_disk01",
            "hydration_key must be present on the project-conversation wire shape"
        );
    }

    #[test]
    fn merge_single_completed_turn_drops_harness_user_role() {
        // §7 scenario 1: one Send + a harness transcript with a user-role copy
        // and an assistant reply → [UserMessage(from journal), AgentTurn].
        let send_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![send_record(send_id, turn_id, agent, "hello", 0)];
        let transcript = transcript_of(vec![
            user_turn(turn_id, agent, "hello", 1),
            agent_turn(turn_id, agent, "hi there", 2),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        assert_eq!(merged.items.len(), 2, "one user message + one agent turn");
        match &merged.items[0] {
            ConversationItem::UserMessage {
                agent_ids, text, ..
            } => {
                assert_eq!(text, "hello", "user text comes from the journal");
                assert_eq!(agent_ids, &vec![agent]);
            }
            other => panic!("expected user message first, got {other:?}"),
        }
        assert!(
            matches!(merged.items[1], ConversationItem::AgentTurn { .. }),
            "second item is the agent turn"
        );
        let user_count = merged
            .items
            .iter()
            .filter(|i| matches!(i, ConversationItem::UserMessage { .. }))
            .count();
        assert_eq!(user_count, 1, "harness user-role turn never duplicates it");
    }

    #[test]
    fn merge_fan_out_exposes_shared_attachment_once() {
        // One Send fanned out to two recipients references the SAME staged file
        // in both Send records (the compose bar snapshots one attachment list).
        // The grouped user message must surface that attachment exactly once.
        let send_id = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        let attachment = Attachment {
            label: "image-1".to_owned(),
            kind: switchboard_core::AttachmentKind::Image,
            path: "/abs/attachments/u__shared.png".to_owned(),
            original_name: "shared.png".to_owned(),
        };
        let send = |agent: AgentId| JournalRecord::Send {
            send_id,
            turn_id: Uuid::now_v7(),
            agent_id: agent,
            prompt: "compare".to_owned(),
            attachments: vec![attachment.clone()],
            at: at(0),
        };
        let merged = merge_project_conversation(vec![send(b), send(c)], vec![]);

        let users: Vec<(Vec<AgentId>, Vec<Attachment>)> = merged
            .items
            .iter()
            .filter_map(|item| match item {
                ConversationItem::UserMessage {
                    agent_ids,
                    attachments,
                    ..
                } => Some((agent_ids.clone(), attachments.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 1, "fan-out renders one grouped user message");
        assert_eq!(users[0].0, vec![b, c], "both recipients grouped");
        assert_eq!(
            users[0].1,
            vec![attachment],
            "the shared attachment surfaces exactly once"
        );
    }

    #[test]
    fn merge_groups_fan_out_by_order_when_journal_and_harness_turn_ids_differ() {
        // The journal's turn_id (dispatcher) is unrelated to the harness session
        // file's turn_id, so correlation is by ORDER, not id match. Both
        // recipients' first turn answers the one shared send.
        let send_id = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, Uuid::now_v7(), b, "go", 0),
            send_record(send_id, Uuid::now_v7(), c, "go", 0),
        ];
        // Harness turn ids are freshly minted, deliberately != the journal ids.
        let b_t = transcript_of(vec![agent_turn(Uuid::now_v7(), b, "b reply", 2)]);
        let c_t = transcript_of(vec![agent_turn(Uuid::now_v7(), c, "c reply", 3)]);

        let merged = merge_project_conversation(journal, vec![(b, b_t, None), (c, c_t, None)]);

        for item in &merged.items {
            if let ConversationItem::AgentTurn { send_id: sid, .. } = item {
                assert_eq!(
                    *sid,
                    Some(send_id),
                    "order-zip stamps the shared send_id despite mismatched turn ids"
                );
            }
        }
    }

    #[test]
    fn merge_end_aligns_recent_turns_when_session_has_pre_journaling_history() {
        // Regression for the drift bug: each agent's session file has an OLD turn
        // (predating journaling → no Send record) plus a RECENT fan-out turn
        // (journaled with a shared send_id). End-alignment pairs the most-recent
        // turns with the send; the older, unjournaled turns get no send_id. A
        // naive count-equality gate would refuse to correlate *any* of the
        // agent's turns here, breaking the recent fan-out.
        let send_id = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, Uuid::now_v7(), b, "Hello", 10),
            send_record(send_id, Uuid::now_v7(), c, "Hello", 10),
        ];
        let b_t = transcript_of(vec![
            agent_turn(Uuid::now_v7(), b, "old b", 0),
            agent_turn(Uuid::now_v7(), b, "recent b", 11),
        ]);
        let c_t = transcript_of(vec![
            agent_turn(Uuid::now_v7(), c, "old c", 1),
            agent_turn(Uuid::now_v7(), c, "recent c", 12),
        ]);

        let merged = merge_project_conversation(journal, vec![(b, b_t, None), (c, c_t, None)]);

        // Look up an agent turn's send_id by its start time.
        let send_at = |t: i64| {
            merged.items.iter().find_map(|i| match i {
                ConversationItem::AgentTurn {
                    started_at,
                    send_id,
                    ..
                } if *started_at == at(t) => Some(*send_id),
                _ => None,
            })
        };
        // Recent turns (10s+) carry the shared send_id; old turns (0/1s) don't.
        assert_eq!(send_at(11), Some(Some(send_id)), "recent b grouped");
        assert_eq!(send_at(12), Some(Some(send_id)), "recent c grouped");
        assert_eq!(send_at(0), Some(None), "old b un-grouped");
        assert_eq!(send_at(1), Some(None), "old c un-grouped");
    }

    #[test]
    fn merge_renders_pre_journaling_user_prompts_from_harness() {
        // Attaching an existing session: its history (user prompt + agent reply)
        // predates journaling, so the journal has no Send for it — the prompt lives
        // only in the harness file. A later turn is dispatched through Switchboard
        // (journaled). The pre-journaling prompt must survive restart (rendered from
        // the harness user turn); the journaled turn's prompt still comes from the
        // journal, and the harness user-role copy of it is NOT duplicated.
        let agent = Uuid::now_v7();
        let send_id = Uuid::now_v7();
        let journal = vec![send_record(send_id, Uuid::now_v7(), agent, "new prompt", 2)];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "old prompt", 0),
            agent_turn(Uuid::now_v7(), agent, "old reply", 1),
            user_turn(Uuid::now_v7(), agent, "new prompt", 2),
            agent_turn(Uuid::now_v7(), agent, "new reply", 3),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let user_texts: Vec<&str> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::UserMessage { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["old prompt", "new prompt"],
            "pre-journaling prompt survives from the harness file; the journaled \
             prompt appears once (its harness user-role copy is not duplicated)"
        );

        let send_at = |t: i64| {
            merged.items.iter().find_map(|i| match i {
                ConversationItem::AgentTurn {
                    started_at,
                    send_id,
                    ..
                } if *started_at == at(t) => Some(*send_id),
                _ => None,
            })
        };
        assert_eq!(
            send_at(1),
            Some(None),
            "pre-journaling agent turn un-grouped"
        );
        assert_eq!(
            send_at(3),
            Some(Some(send_id)),
            "journaled agent turn keeps its send_id"
        );
    }

    #[test]
    fn merge_pure_attach_with_empty_journal_renders_all_harness_prompts() {
        // The literal reported bug: attach an existing session and restart WITHOUT
        // ever dispatching through Switchboard. The journal is empty, so every turn
        // is pre-journaling — both user prompts must render from the harness file
        // (not just the assistant replies), each agent turn un-grouped.
        let agent = Uuid::now_v7();
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "first prompt", 0),
            agent_turn(Uuid::now_v7(), agent, "first reply", 1),
            user_turn(Uuid::now_v7(), agent, "second prompt", 2),
            agent_turn(Uuid::now_v7(), agent, "second reply", 3),
        ]);

        let merged = merge_project_conversation(vec![], vec![(agent, transcript, None)]);

        let user_texts: Vec<&str> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::UserMessage { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["first prompt", "second prompt"],
            "both imported prompts survive when nothing was journaled"
        );
        let all_ungrouped = merged.items.iter().all(|i| {
            !matches!(
                i,
                ConversationItem::AgentTurn {
                    send_id: Some(_),
                    ..
                }
            )
        });
        assert!(
            all_ungrouped,
            "no journal sends → every agent turn un-grouped"
        );
    }

    #[test]
    fn merge_imported_claude_session_keeps_prompts_through_real_parser() {
        // End-to-end through the real Claude parser (not hand-built Turns): an
        // imported turn that includes a tool call, plus one turn dispatched through
        // Switchboard. Proves the load-bearing assumption holds against real parser
        // output — the tool_result (a user-role array record) folds into the agent
        // turn rather than opening a spurious user turn, so genuine prompts stay 1:1
        // with agent turns, the imported prompt renders once, and the journaled
        // prompt is not duplicated.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let canonical_cwd = cwd.path().canonicalize().unwrap();
        let session_id = Uuid::now_v7();
        let agent = Uuid::now_v7();

        let lines = [
            serde_json::json!({"type":"user","message":{"role":"user","content":"fix the build"},"timestamp":"2026-05-14T04:43:15Z"}),
            serde_json::json!({"type":"assistant","message":{"model":"claude-sonnet-4-6","role":"assistant","content":[{"type":"text","text":"looking"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"make build"}}],"usage":{"input_tokens":1,"output_tokens":2}},"timestamp":"2026-05-14T04:43:16Z"}),
            serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok","is_error":false}]},"timestamp":"2026-05-14T04:43:17Z"}),
            serde_json::json!({"type":"assistant","message":{"model":"claude-sonnet-4-6","role":"assistant","content":[{"type":"text","text":"fixed"}]},"timestamp":"2026-05-14T04:43:18Z"}),
            serde_json::json!({"type":"user","message":{"role":"user","content":"now add tests"},"timestamp":"2026-05-14T04:43:20Z"}),
            serde_json::json!({"type":"assistant","message":{"model":"claude-sonnet-4-6","role":"assistant","content":[{"type":"text","text":"added"}]},"timestamp":"2026-05-14T04:43:21Z"}),
        ];
        let content = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let target =
            switchboard_harness::claude_session_file_path(home.path(), &canonical_cwd, &session_id);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, content).unwrap();

        let transcript = switchboard_harness::load_claude_transcript(
            home.path(),
            &canonical_cwd,
            session_id,
            agent,
        )
        .unwrap();

        // The journaled send carries a real 2026 timestamp so it sorts between the
        // imported turn and the journaled reply (the `at()` epoch helper would sort
        // a 2023 instant ahead of the 2026 parsed turns).
        let send_id = Uuid::now_v7();
        let journal = vec![JournalRecord::Send {
            send_id,
            turn_id: Uuid::now_v7(),
            agent_id: agent,
            prompt: "now add tests".to_owned(),
            attachments: Vec::new(),
            at: "2026-05-14T04:43:19Z".parse().unwrap(),
        }];

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let user_texts: Vec<&str> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::UserMessage { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["fix the build", "now add tests"],
            "imported prompt (with a tool call in its turn) survives; journaled prompt not doubled"
        );

        let agent_sends: Vec<Option<SendId>> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::AgentTurn { send_id, .. } => Some(*send_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            agent_sends.len(),
            2,
            "two agent turns (tool result folded in)"
        );
        assert!(
            agent_sends.contains(&None),
            "imported agent turn un-grouped"
        );
        assert!(
            agent_sends.contains(&Some(send_id)),
            "journaled agent turn grouped"
        );
    }

    #[test]
    fn merge_trailing_unanswered_imported_prompt_survives() {
        // An attached session that ends on a prompt with no reply yet (the CLI was
        // interrupted, or it was attached mid-turn). Empty journal → the trailing
        // prompt is pre-journaling and must render. Classifying user turns by
        // user-turn count (not by pairing each to a following agent turn) is what
        // keeps a trailing prompt that has no agent turn after it.
        let agent = Uuid::now_v7();
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "answered", 0),
            agent_turn(Uuid::now_v7(), agent, "reply", 1),
            user_turn(Uuid::now_v7(), agent, "dangling", 2),
        ]);

        let merged = merge_project_conversation(vec![], vec![(agent, transcript, None)]);

        let user_texts: Vec<&str> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::UserMessage { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["answered", "dangling"],
            "the trailing un-answered imported prompt is not dropped"
        );
    }

    #[test]
    fn merge_trailing_in_flight_prompt_comes_from_journal_not_duplicated() {
        // Same harness shape as the dangling case (trailing user turn, no reply),
        // but this prompt WAS dispatched through Switchboard and is in-flight
        // (`Send`, no `Outcome`, no agent turn yet). It must render once, from the
        // journal `Send`; the harness user-role copy is dropped so it isn't doubled.
        let agent = Uuid::now_v7();
        let s1 = Uuid::now_v7();
        let s2 = Uuid::now_v7();
        let journal = vec![
            send_record(s1, Uuid::now_v7(), agent, "answered", 0),
            send_record(s2, Uuid::now_v7(), agent, "in flight", 2),
        ];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "answered", 0),
            agent_turn(Uuid::now_v7(), agent, "reply", 1),
            user_turn(Uuid::now_v7(), agent, "in flight", 2),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let user_texts: Vec<&str> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::UserMessage { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["answered", "in flight"],
            "in-flight prompt rendered once (from the journal), harness copy dropped"
        );
    }

    #[test]
    fn merge_external_prompt_after_journaled_history_not_duplicated_or_dropped() {
        // A prompt dispatched through Switchboard (journaled, completed), then a
        // prompt run via the CLI directly in the same dir (no Send, no reply yet).
        // The journaled prompt must render ONCE (from the journal; harness copy
        // dropped) and the external dangling prompt must render (imported). A
        // suffix-count classification gets this wrong — it renders the journaled
        // prompt as imported (a duplicate of the journal's) and drops the external
        // one. Classifying by reply (the journaled prompt's reply is journaled; the
        // external prompt has no reply and no send to account for it) is correct.
        let agent = Uuid::now_v7();
        let s1 = Uuid::now_v7();
        let journal = vec![send_record(s1, Uuid::now_v7(), agent, "answered", 0)];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "answered", 0),
            agent_turn(Uuid::now_v7(), agent, "reply", 1),
            user_turn(Uuid::now_v7(), agent, "external", 2),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let user_msgs: Vec<(&str, Option<SendId>)> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::UserMessage { text, send_id, .. } => {
                    Some((text.as_str(), *send_id))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            user_msgs,
            vec![("answered", Some(s1)), ("external", None)],
            "journaled prompt once (grouped); external prompt kept (un-grouped)"
        );
    }

    #[test]
    fn merge_failed_send_does_not_duplicate_its_harness_prompt() {
        // A dispatched send that failed: the journal holds the Send (prompt) + a
        // failed Outcome, and the harness recorded the user turn but no agent reply.
        // The prompt must render ONCE — from the journal Send — not also from the
        // harness copy. The user-side count therefore has to include non-completed
        // sends (the journal renders a UserMessage for every Send), not only
        // completed ones.
        let agent = Uuid::now_v7();
        let s1 = Uuid::now_v7();
        let t1 = Uuid::now_v7();
        let journal = vec![
            send_record(s1, t1, agent, "do it", 0),
            outcome_record(
                s1,
                t1,
                agent,
                serde_json::json!({"status": "failed", "kind": "harness_error", "message": "boom"}),
                0,
            ),
        ];
        let transcript = transcript_of(vec![user_turn(Uuid::now_v7(), agent, "do it", 0)]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let user_count = merged
            .items
            .iter()
            .filter(|i| matches!(i, ConversationItem::UserMessage { text, .. } if text == "do it"))
            .count();
        assert_eq!(
            user_count, 1,
            "failed send's prompt renders once (journal), not duplicated by the harness copy"
        );
        assert!(
            merged.items.iter().any(|i| matches!(
                i,
                ConversationItem::Outcome {
                    status: OutcomeStatus::Failed,
                    ..
                }
            )),
            "the failed marker renders"
        );
    }

    #[test]
    fn merge_imported_dangling_before_journaled_dangling_is_misclassified() {
        // CHARACTERIZATION of a documented limitation (not desired behavior): when
        // an imported dangling prompt (bare CLI, no Send) precedes a journaled
        // dangling one (an in-flight Switchboard send) in the file, the front-to-
        // back dangling classification mis-assigns them. Order alone can't tell
        // which dangling turn owns the in-flight send; this only arises under the
        // discouraged "drive the same session from the bare CLI and Switchboard"
        // pattern. Pinned so a future fix (or regression) changes it consciously.
        //
        // CORRECT behavior would be: "external" rendered once (imported), "later"
        // rendered once (from the journal Send). What actually happens: "external"
        // is dropped and "later" is duplicated.
        let agent = Uuid::now_v7();
        let s1 = Uuid::now_v7();
        let journal = vec![send_record(s1, Uuid::now_v7(), agent, "later", 2)];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "external", 0),
            user_turn(Uuid::now_v7(), agent, "later", 2),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let count = |t: &str| {
            merged
                .items
                .iter()
                .filter(|i| matches!(i, ConversationItem::UserMessage { text, .. } if text == t))
                .count()
        };
        assert_eq!(
            count("external"),
            0,
            "documented limitation: imported prompt dropped"
        );
        assert_eq!(
            count("later"),
            2,
            "documented limitation: journaled prompt duplicated"
        );
    }

    #[test]
    fn merge_cancel_before_harness_flush_overcounts_and_drops_imported() {
        // CHARACTERIZATION of the second documented limitation: a send cancelled
        // before the harness recorded its prompt leaves no harness user turn, but
        // it still counts toward `dangling_journaled`, so a co-occurring imported
        // dangling prompt is absorbed (dropped). Pinned, not endorsed.
        //
        // CORRECT behavior would be: "imported" rendered once. What happens: it is
        // dropped (the phantom slot for the never-recorded cancelled prompt eats
        // it); only the journal's own "cancelled" prompt + marker render.
        let agent = Uuid::now_v7();
        let s1 = Uuid::now_v7();
        let t1 = Uuid::now_v7();
        let journal = vec![
            send_record(s1, t1, agent, "cancelled", 0),
            outcome_record(
                s1,
                t1,
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                0,
            ),
        ];
        let transcript = transcript_of(vec![user_turn(Uuid::now_v7(), agent, "imported", 1)]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let imported_count = merged
            .items
            .iter()
            .filter(
                |i| matches!(i, ConversationItem::UserMessage { text, .. } if text == "imported"),
            )
            .count();
        assert_eq!(
            imported_count, 0,
            "documented limitation: the imported prompt is absorbed by the phantom cancelled slot"
        );
    }

    #[test]
    fn merge_in_flight_send_does_not_mislabel_completed_turns() {
        // Regression: viewing a project while an agent is mid-response. The agent
        // has two COMPLETED turns (each answering an earlier send) plus a third
        // send that has started but not yet produced harness content (in-flight:
        // Send record, no Outcome, no agent turn). The in-flight send sits at the
        // BACK of the completed-sends list. End-aligning the sends would pair it
        // with the most-recent completed turn and shift every label by one;
        // front-aligning drops it so the two completed turns keep their own sends.
        let agent = Uuid::now_v7();
        let s1 = Uuid::now_v7();
        let s2 = Uuid::now_v7();
        let s3 = Uuid::now_v7(); // in-flight
        let t1 = Uuid::now_v7();
        let t2 = Uuid::now_v7();
        let journal = vec![
            send_record(s1, t1, agent, "first", 0),
            send_record(s2, t2, agent, "second", 2),
            send_record(s3, Uuid::now_v7(), agent, "third (in flight)", 4),
        ];
        let transcript = transcript_of(vec![
            agent_turn(t1, agent, "first reply", 1),
            agent_turn(t2, agent, "second reply", 3),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let send_at = |t: i64| {
            merged.items.iter().find_map(|i| match i {
                ConversationItem::AgentTurn {
                    started_at,
                    send_id,
                    ..
                } if *started_at == at(t) => Some(*send_id),
                _ => None,
            })
        };
        assert_eq!(send_at(1), Some(Some(s1)), "first reply keeps its own send");
        assert_eq!(
            send_at(3),
            Some(Some(s2)),
            "second reply keeps its own send, not the in-flight one"
        );
    }

    #[test]
    fn merge_partial_fan_out_failure_still_groups_the_completed_recipient() {
        // One recipient completes, the other fails. The completed turn is still
        // tagged with the shared send_id (so it groups), and the failure marker
        // carries the send_id too (so it routes into the failed recipient's
        // column). The failed send is excluded from the order-zip so it doesn't
        // misalign the completed recipient.
        let send_id = Uuid::now_v7();
        let b = Uuid::now_v7(); // completes
        let c = Uuid::now_v7(); // fails
        let journal = vec![
            send_record(send_id, Uuid::now_v7(), b, "go", 0),
            send_record(send_id, Uuid::now_v7(), c, "go", 0),
            outcome_record(
                send_id,
                Uuid::now_v7(),
                c,
                serde_json::json!({"status": "failed", "kind": "harness_error", "message": "x"}),
                1,
            ),
        ];
        let b_t = transcript_of(vec![agent_turn(Uuid::now_v7(), b, "b reply", 2)]);
        let c_t = transcript_of(Vec::new()); // c failed → no clean harness turn

        let merged = merge_project_conversation(journal, vec![(b, b_t, None), (c, c_t, None)]);

        let b_turn_send = merged
            .items
            .iter()
            .find_map(|i| match i {
                ConversationItem::AgentTurn {
                    agent_id, send_id, ..
                } if *agent_id == b => Some(*send_id),
                _ => None,
            })
            .expect("b's completed turn");
        assert_eq!(b_turn_send, Some(send_id), "completed recipient groups");

        let c_outcome_send = merged
            .items
            .iter()
            .find_map(|i| match i {
                ConversationItem::Outcome {
                    agent_id, send_id, ..
                } if *agent_id == c => Some(*send_id),
                _ => None,
            })
            .expect("c's failure marker");
        assert_eq!(c_outcome_send, send_id, "failure marker keeps the send_id");
    }

    #[test]
    fn merge_fan_out_both_complete_renders_user_message_once() {
        // §7 scenario 2: two Sends sharing one send_id + two agents each with an
        // agent turn → ONE UserMessage(agent_ids = [B, C]) then both turns.
        let send_id = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        let tb = Uuid::now_v7();
        let tc = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, tb, b, "status?", 0),
            send_record(send_id, tc, c, "status?", 0),
        ];
        let b_t = transcript_of(vec![agent_turn(tb, b, "b reply", 2)]);
        let c_t = transcript_of(vec![agent_turn(tc, c, "c reply", 3)]);

        let merged = merge_project_conversation(journal, vec![(b, b_t, None), (c, c_t, None)]);

        let user_msgs: Vec<&ConversationItem> = merged
            .items
            .iter()
            .filter(|i| matches!(i, ConversationItem::UserMessage { .. }))
            .collect();
        assert_eq!(user_msgs.len(), 1, "fan-out renders the user message once");
        match user_msgs[0] {
            ConversationItem::UserMessage { agent_ids, .. } => {
                assert_eq!(agent_ids, &vec![b, c], "both recipients, first-seen order");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            merged
                .items
                .iter()
                .filter(|i| matches!(i, ConversationItem::AgentTurn { .. }))
                .count(),
            2,
            "both agent turns present"
        );
        // Both responses recover the shared send_id (joined by turn_id against
        // the journal Sends), so the frontend groups them the same way it
        // groups a live fan-out.
        for item in &merged.items {
            if let ConversationItem::AgentTurn { send_id: sid, .. } = item {
                assert_eq!(
                    *sid,
                    Some(send_id),
                    "each historical fan-out response is tagged with the shared send_id"
                );
            }
        }
    }

    #[test]
    fn merge_agent_turn_without_a_matching_send_has_no_send_id() {
        // A harness turn with no journal Send record (e.g. pre-journal history
        // or a send whose record write failed) → send_id is None, not a panic.
        let agent = Uuid::now_v7();
        let orphan_turn = Uuid::now_v7();
        let transcript = transcript_of(vec![agent_turn(orphan_turn, agent, "reply", 1)]);

        let merged = merge_project_conversation(Vec::new(), vec![(agent, transcript, None)]);

        let tagged = merged
            .items
            .iter()
            .find_map(|i| match i {
                ConversationItem::AgentTurn { send_id, .. } => Some(*send_id),
                _ => None,
            })
            .expect("one agent turn");
        assert_eq!(tagged, None, "no journal Send ⇒ untagged, gracefully");
    }

    #[test]
    fn merge_failed_to_start_yields_failed_marker_no_orphan() {
        // §7 scenario 4: Send + Failed outcome, no harness content → marker.
        let send_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, turn_id, agent, "run build", 0),
            outcome_record(
                send_id,
                turn_id,
                agent,
                serde_json::json!({"status": "failed", "kind": "harness_error", "message": "spawn failed"}),
                1,
            ),
        ];

        let merged =
            merge_project_conversation(journal, vec![(agent, transcript_of(Vec::new()), None)]);

        assert_eq!(merged.items.len(), 2);
        assert!(matches!(
            merged.items[0],
            ConversationItem::UserMessage { .. }
        ));
        match &merged.items[1] {
            ConversationItem::Outcome { status, reason, .. } => {
                assert_eq!(*status, OutcomeStatus::Failed);
                assert_eq!(reason.as_deref(), Some("spawn failed"));
            }
            other => panic!("expected a failed outcome marker, got {other:?}"),
        }
    }

    #[test]
    fn merge_cancelled_mid_stream_yields_marker_only() {
        // §7 scenario 5 (Claude/Codex): Send + cancelled outcome, no harness
        // content → marker only, no partial agent turn.
        let send_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, turn_id, agent, "write a long essay", 0),
            outcome_record(
                send_id,
                turn_id,
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                1,
            ),
        ];

        let merged =
            merge_project_conversation(journal, vec![(agent, transcript_of(Vec::new()), None)]);

        assert!(
            !merged
                .items
                .iter()
                .any(|i| matches!(i, ConversationItem::AgentTurn { .. })),
            "no agent turn when the harness persisted nothing"
        );
        match merged.items.last() {
            Some(ConversationItem::Outcome { status, reason, .. }) => {
                assert_eq!(*status, OutcomeStatus::Cancelled);
                assert_eq!(reason.as_deref(), Some("user"), "reason from source");
            }
            other => panic!("expected a cancelled marker last, got {other:?}"),
        }
    }

    #[test]
    fn merge_mixed_fan_out_complete_and_cancelled_compose_by_timestamp() {
        // §7 closing paragraph: one send_id, B completes (harness turn), C is
        // cancelled (journal marker) → one UserMessage{B,C}, then B's turn, then
        // C's marker, ordered by timestamp.
        let send_id = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        let tb = Uuid::now_v7();
        let tc = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, tb, b, "do X", 0),
            send_record(send_id, tc, c, "do X", 0),
            outcome_record(
                send_id,
                tc,
                c,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                3,
            ),
        ];
        let b_t = transcript_of(vec![agent_turn(tb, b, "done", 2)]);

        let merged = merge_project_conversation(
            journal,
            vec![(b, b_t, None), (c, transcript_of(Vec::new()), None)],
        );

        assert_eq!(merged.items.len(), 3);
        match &merged.items[0] {
            ConversationItem::UserMessage { agent_ids, .. } => {
                assert_eq!(agent_ids, &vec![b, c]);
            }
            other => panic!("expected user message first, got {other:?}"),
        }
        assert!(
            matches!(&merged.items[1], ConversationItem::AgentTurn { agent_id, .. } if *agent_id == b),
            "B's completed turn comes before C's later cancel marker"
        );
        assert!(
            matches!(&merged.items[2], ConversationItem::Outcome { agent_id, status, .. } if *agent_id == c && *status == OutcomeStatus::Cancelled),
            "C's cancel marker last"
        );
    }

    /// An agent turn with an explicit status + a preceding shape — for the
    /// cancelled/failed-mid-turn fixtures where the harness persisted a partial
    /// turn (`Streaming` for Claude, `Failed` for Codex/Gemini/Antigravity) or,
    /// in the cancel-after-end race, a `Complete` one.
    fn agent_turn_status(
        turn_id: Uuid,
        agent_id: AgentId,
        text: &str,
        t: i64,
        status: TurnStatus,
    ) -> Turn {
        match agent_turn(turn_id, agent_id, text, t) {
            Turn::Agent {
                turn_id,
                agent_id,
                started_at,
                ended_at,
                items,
                usage,
                spend,
                model,
                effort,
                hydration_key,
                stable_message_id,
                ..
            } => Turn::Agent {
                turn_id,
                agent_id,
                started_at,
                ended_at,
                status,
                items,
                usage,
                spend,
                model,
                effort,
                hydration_key,
                stable_message_id,
            },
            other => other,
        }
    }

    fn user_messages(merged: &ProjectConversation) -> Vec<&ConversationItem> {
        merged
            .items
            .iter()
            .filter(|i| matches!(i, ConversationItem::UserMessage { .. }))
            .collect()
    }

    /// The headline bug: a send cancelled *after* the agent wrote content leaves a
    /// partial harness turn. Correlating against all sends (not completed-only)
    /// pairs that turn with its send, so its harness `Turn::User` prompt drops
    /// (the journal owns it) — no duplicate — and the turn groups under the send.
    /// The cancelled badge rides on the coexisting Outcome marker (render-both).
    #[test]
    fn merge_cancel_mid_turn_with_content_drops_prompt_and_groups() {
        let send_id = Uuid::now_v7();
        let dispatch_turn = Uuid::now_v7();
        let disk_turn = Uuid::now_v7();
        let disk_prompt = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, dispatch_turn, agent, "explore the repo", 0),
            outcome_record(
                send_id,
                dispatch_turn,
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                3,
            ),
        ];
        // Disk: the prompt + a partial (Streaming) agent turn the cancel left.
        let transcript = transcript_of(vec![
            user_turn(disk_prompt, agent, "explore the repo", 0),
            agent_turn_status(
                disk_turn,
                agent,
                "starting to look…",
                1,
                TurnStatus::Streaming,
            ),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        // Exactly one user message — the journal's, NOT an imported duplicate.
        let users = user_messages(&merged);
        assert_eq!(
            users.len(),
            1,
            "no duplicate/imported prompt, got {users:?}"
        );
        assert!(
            matches!(users[0], ConversationItem::UserMessage { send_id: Some(s), .. } if *s == send_id),
            "the surviving prompt is the journaled (grouped) one"
        );
        // The partial turn is grouped under the send (not orphaned send_id: None).
        assert!(
            merged.items.iter().any(|i| matches!(
                i,
                ConversationItem::AgentTurn { send_id: Some(s), .. } if *s == send_id
            )),
            "the cancelled partial turn groups under its send"
        );
        // The cancelled badge is carried by the coexisting Outcome marker.
        assert!(
            merged.items.iter().any(|i| matches!(
                i,
                ConversationItem::Outcome {
                    status: OutcomeStatus::Cancelled,
                    ..
                }
            )),
            "render-both: the Outcome marker is preserved"
        );
    }

    /// The cancel-after-end race: the model finished writing (disk turn reads
    /// `Complete`) before the kill, so the journal records cancelled. Status-blind
    /// correlation still pairs the `Complete` turn with its cancelled send — no
    /// duplicate prompt, no orphan. (This is the case a disk-status partition got
    /// wrong.)
    #[test]
    fn merge_cancel_after_end_turn_complete_on_disk_still_groups() {
        let send_id = Uuid::now_v7();
        let dispatch_turn = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, dispatch_turn, agent, "summarize", 0),
            outcome_record(
                send_id,
                dispatch_turn,
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                3,
            ),
        ];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "summarize", 0),
            agent_turn_status(
                Uuid::now_v7(),
                agent,
                "the full answer",
                1,
                TurnStatus::Complete,
            ),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        assert_eq!(user_messages(&merged).len(), 1, "no duplicate prompt");
        assert!(
            merged.items.iter().any(|i| matches!(
                i,
                ConversationItem::AgentTurn { send_id: Some(s), .. } if *s == send_id
            )),
            "the Complete-on-disk cancelled turn groups under its send"
        );
    }

    /// Cancel-then-retry on one agent: `[cancelled(partial), completed]`. Each turn
    /// pairs with its own send; both prompts drop; one user message per send.
    #[test]
    fn merge_cancel_then_retry_pairs_each_turn_to_its_send() {
        let s0 = Uuid::now_v7();
        let s1 = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(s0, Uuid::now_v7(), agent, "first try", 0),
            outcome_record(
                s0,
                Uuid::now_v7(),
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                2,
            ),
            send_record(s1, Uuid::now_v7(), agent, "second try", 3),
        ];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "first try", 0),
            agent_turn_status(Uuid::now_v7(), agent, "partial", 1, TurnStatus::Streaming),
            user_turn(Uuid::now_v7(), agent, "second try", 3),
            agent_turn(Uuid::now_v7(), agent, "done", 4),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        // Two grouped prompts (s0, s1), no imported duplicates.
        let users = user_messages(&merged);
        assert_eq!(users.len(), 2, "one prompt per send, no duplicates");
        assert!(
            users.iter().all(|u| matches!(
                u,
                ConversationItem::UserMessage {
                    send_id: Some(_),
                    ..
                }
            )),
            "both prompts journaled"
        );
        let agent_sends: Vec<Option<SendId>> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::AgentTurn { send_id, .. } => Some(*send_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            agent_sends,
            vec![Some(s0), Some(s1)],
            "turns pair to their own sends in order"
        );
    }

    /// Cancelled-before-output but the harness *did* record the prompt (a dangling
    /// user turn, no reply): the prompt still drops (journal owns it), the bare
    /// cancelled marker renders, no duplicate. The prompt-drop half's subtle shape.
    #[test]
    fn merge_cancel_before_output_with_recorded_prompt_drops_it() {
        let send_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, Uuid::now_v7(), agent, "do the thing", 0),
            outcome_record(
                send_id,
                Uuid::now_v7(),
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                2,
            ),
        ];
        // Prompt recorded, no agent turn (cancelled before any output).
        let transcript = transcript_of(vec![user_turn(Uuid::now_v7(), agent, "do the thing", 0)]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        assert_eq!(
            user_messages(&merged).len(),
            1,
            "the recorded prompt drops; only the journal's remains"
        );
        assert!(
            !merged
                .items
                .iter()
                .any(|i| matches!(i, ConversationItem::AgentTurn { .. })),
            "no agent turn (nothing produced)"
        );
        assert!(
            merged.items.iter().any(|i| matches!(
                i,
                ConversationItem::Outcome {
                    status: OutcomeStatus::Cancelled,
                    ..
                }
            )),
            "bare cancelled marker renders"
        );
    }

    /// Documented residual (NOT a correctness assertion): a cancelled-before-output
    /// send positioned *before* a content-bearing send shifts labels by one — the
    /// completed answer lands under the cancelled send's `send_id` (content
    /// mis-grouping). The prompt is still journal-owned (no duplication). Pins the
    /// known-bound so a future change to it is a conscious decision.
    #[test]
    fn merge_residual_leading_cancel_before_output_misgroups() {
        let s0 = Uuid::now_v7(); // cancelled before output (prompt recorded)
        let s1 = Uuid::now_v7(); // completed
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(s0, Uuid::now_v7(), agent, "p0", 0),
            outcome_record(
                s0,
                Uuid::now_v7(),
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                1,
            ),
            send_record(s1, Uuid::now_v7(), agent, "p1", 2),
        ];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "p0", 0),
            user_turn(Uuid::now_v7(), agent, "p1", 2),
            agent_turn(Uuid::now_v7(), agent, "answer to p1", 3),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        // No duplicated prompt (both journal-owned).
        let users = user_messages(&merged);
        assert!(
            users.iter().all(|u| matches!(
                u,
                ConversationItem::UserMessage {
                    send_id: Some(_),
                    ..
                }
            )),
            "no imported duplicate prompt"
        );
        // The residual: s1's answer is mis-grouped under s0.
        let answer = merged
            .items
            .iter()
            .find_map(|i| match i {
                ConversationItem::AgentTurn { send_id, .. } => Some(*send_id),
                _ => None,
            })
            .expect("one agent turn");
        assert_eq!(
            answer,
            Some(s0),
            "documented residual: leading cancel-before-output mis-groups the answer onto s0"
        );
    }

    /// Trailing interleave `[completed, cancel-after-end]` — both turns read
    /// `Complete` on disk (the second's process was killed only after the model
    /// finished writing). All-sends order-pairing crosses the completed/cancelled
    /// boundary correctly: the first `Complete` turn → completed send, the second
    /// `Complete` turn → cancelled send. A disk-status partition would route both
    /// to the completed bucket and reproduce the duplicate-prompt bug.
    #[test]
    fn merge_completed_then_cancel_after_end_pairs_across_the_boundary() {
        let s0 = Uuid::now_v7(); // completed
        let s1 = Uuid::now_v7(); // cancelled after end_turn (Complete on disk)
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(s0, Uuid::now_v7(), agent, "first", 0),
            send_record(s1, Uuid::now_v7(), agent, "second", 2),
            outcome_record(
                s1,
                Uuid::now_v7(),
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                4,
            ),
        ];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "first", 0),
            agent_turn_status(
                Uuid::now_v7(),
                agent,
                "first answer",
                1,
                TurnStatus::Complete,
            ),
            user_turn(Uuid::now_v7(), agent, "second", 2),
            agent_turn_status(
                Uuid::now_v7(),
                agent,
                "second answer",
                3,
                TurnStatus::Complete,
            ),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        assert_eq!(user_messages(&merged).len(), 2, "one prompt per send");
        let agent_sends: Vec<Option<SendId>> = merged
            .items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::AgentTurn { send_id, .. } => Some(*send_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            agent_sends,
            vec![Some(s0), Some(s1)],
            "completed turn → completed send, Complete-on-disk cancelled turn → cancelled send"
        );
    }

    /// A *completed* send whose last disk turn reads `Streaming` (the M2
    /// `eof_tail_status` running-vs-finished limitation: no `end_turn` written)
    /// with **no** Outcome marker. Order-pairing keys off the all-sends list, not
    /// the harness status, so the turn still pairs to its completed send — a
    /// disk-status partition would strand it in the non-completed bucket. Asserts
    /// **grouping** only; the residual `streaming` badge is out of scope here.
    #[test]
    fn merge_streaming_completed_tail_pairs_to_its_send() {
        let s0 = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![send_record(s0, Uuid::now_v7(), agent, "do it", 0)];
        let transcript = transcript_of(vec![
            user_turn(Uuid::now_v7(), agent, "do it", 0),
            agent_turn_status(
                Uuid::now_v7(),
                agent,
                "the answer",
                1,
                TurnStatus::Streaming,
            ),
        ]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        assert_eq!(user_messages(&merged).len(), 1, "no duplicate prompt");
        assert!(
            merged.items.iter().any(|i| matches!(
                i,
                ConversationItem::AgentTurn { send_id: Some(s), .. } if *s == s0
            )),
            "the Streaming-on-disk completed tail groups under its send"
        );
    }

    /// Switch-back re-runs the merge over the *same* disk state. The merge is a
    /// pure function of its inputs, so a second pass yields byte-identical items —
    /// no compounding duplicate prompts or stray user rows (the reported symptom
    /// was growth on every switch). Guards against any accidental statefulness.
    #[test]
    fn merge_cancel_mid_turn_is_idempotent_across_reopen() {
        let send_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        // Build the disk state once; both passes read the *same* records (a
        // switch-back does not regenerate the journal/session files).
        let journal = vec![
            send_record(send_id, Uuid::now_v7(), agent, "summarize", 0),
            outcome_record(
                send_id,
                Uuid::now_v7(),
                agent,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                3,
            ),
        ];
        let turns = vec![
            user_turn(Uuid::now_v7(), agent, "summarize", 0),
            agent_turn_status(Uuid::now_v7(), agent, "partial", 1, TurnStatus::Streaming),
        ];

        let first = merge_project_conversation(
            journal.clone(),
            vec![(agent, transcript_of(turns.clone()), None)],
        );
        let second = merge_project_conversation(journal, vec![(agent, transcript_of(turns), None)]);

        assert_eq!(
            first.items, second.items,
            "re-merge on switch-back yields identical items — no compounding"
        );
        assert_eq!(
            user_messages(&first).len(),
            1,
            "exactly one prompt, every pass"
        );
    }

    #[test]
    fn merge_lone_unjournaled_harness_user_turn_renders_as_imported() {
        // A harness user-role turn with no journal `Send` (pure pre-journaling —
        // an attached session never dispatched through Switchboard) surfaces as an
        // imported UserMessage: un-grouped, `send_id` None, keyed by the harness
        // turn_id. (A *journaled* turn's harness user copy is still dropped — see
        // `merge_single_completed_turn_drops_harness_user_role`. Dropping these
        // unconditionally, as before, lost the user's side of imported history.)
        let agent = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let transcript = transcript_of(vec![user_turn(turn_id, agent, "from harness", 0)]);

        let merged = merge_project_conversation(Vec::new(), vec![(agent, transcript, None)]);

        assert_eq!(merged.items.len(), 1, "the imported prompt renders");
        match &merged.items[0] {
            ConversationItem::UserMessage {
                id,
                send_id,
                agent_ids,
                text,
                ..
            } => {
                assert_eq!(text, "from harness");
                assert_eq!(*send_id, None, "imported prompt has no journal send_id");
                assert_eq!(*id, turn_id, "keyed by the harness turn_id");
                assert_eq!(agent_ids, &vec![agent]);
            }
            other => panic!("expected an imported UserMessage, got {other:?}"),
        }
    }

    #[test]
    fn merge_orders_all_kinds_strictly_ascending_by_timestamp() {
        // Interleave the three kinds out of order; the merge sorts by timestamp.
        let send_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let t_user = Uuid::now_v7();
        let t_turn = Uuid::now_v7();
        let t_out = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, t_user, agent, "msg", 0),
            outcome_record(
                send_id,
                t_out,
                agent,
                serde_json::json!({"status": "failed", "message": "x"}),
                4,
            ),
        ];
        let transcript = transcript_of(vec![agent_turn(t_turn, agent, "mid", 2)]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        let stamps: Vec<DateTime<Utc>> = merged
            .items
            .iter()
            .map(conversation_item_timestamp)
            .collect();
        let mut sorted = stamps.clone();
        sorted.sort();
        assert_eq!(stamps, sorted, "items are sorted ascending by timestamp");
        assert_eq!(stamps, vec![at(0), at(2), at(4)]);
    }

    #[test]
    fn merge_equal_timestamp_orders_user_message_before_its_outcome() {
        // The common failed-to-start/cancelled case: `Send.at == Outcome.started_at`.
        // The user message must still render before its own outcome marker.
        let send_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![
            send_record(send_id, turn_id, agent, "run build", 0),
            outcome_record(
                send_id,
                turn_id,
                agent,
                serde_json::json!({"status": "failed", "message": "spawn failed"}),
                0,
            ),
        ];

        let merged =
            merge_project_conversation(journal, vec![(agent, transcript_of(Vec::new()), None)]);

        assert_eq!(merged.items.len(), 2);
        assert!(
            matches!(merged.items[0], ConversationItem::UserMessage { .. }),
            "user message precedes the outcome at an equal instant"
        );
        assert!(matches!(
            merged.items[1],
            ConversationItem::Outcome {
                status: OutcomeStatus::Failed,
                ..
            }
        ));
    }

    #[test]
    fn merge_equal_timestamp_orders_user_message_before_agent_turn() {
        // An AgentTurn whose `started_at` equals a send's `at` → UserMessage first.
        let send_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let journal = vec![send_record(send_id, turn_id, agent, "hello", 0)];
        let transcript = transcript_of(vec![agent_turn(turn_id, agent, "hi", 0)]);

        let merged = merge_project_conversation(journal, vec![(agent, transcript, None)]);

        assert_eq!(merged.items.len(), 2);
        assert!(
            matches!(merged.items[0], ConversationItem::UserMessage { .. }),
            "user message precedes the agent turn at an equal instant"
        );
        assert!(matches!(
            merged.items[1],
            ConversationItem::AgentTurn { .. }
        ));
    }

    #[test]
    fn merge_same_turn_agent_turn_and_outcome_both_render_content_then_marker() {
        // A non-completed turn can produce BOTH a harness-persisted partial
        // AgentTurn (status Failed) AND a journal Outcome for the same turn_id.
        // The merge keeps both (no correlation/dedup), ordered content-then-marker.
        let send_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let agent = Uuid::now_v7();
        let failed_turn = Turn::Agent {
            turn_id,
            agent_id: agent,
            started_at: at(1),
            ended_at: None,
            status: TurnStatus::Failed,
            items: vec![TurnItem::Text {
                kind: ContentKind::Text,
                text: "partial output".to_owned(),
            }],
            usage: None,
            spend: None,
            model: None,
            effort: None,
            hydration_key: None,
            stable_message_id: None,
        };
        let journal = vec![
            send_record(send_id, turn_id, agent, "do work", 0),
            outcome_record(
                send_id,
                turn_id,
                agent,
                serde_json::json!({"status": "failed", "message": "died"}),
                1,
            ),
        ];

        let merged = merge_project_conversation(
            journal,
            vec![(agent, transcript_of(vec![failed_turn]), None)],
        );

        // UserMessage(0), AgentTurn(1), Outcome(1) — at equal t=1 the turn (rank 1)
        // precedes the marker (rank 2); neither is deduped against the other.
        assert_eq!(merged.items.len(), 3);
        assert!(matches!(
            merged.items[0],
            ConversationItem::UserMessage { .. }
        ));
        assert!(
            matches!(&merged.items[1], ConversationItem::AgentTurn { turn_id: tid, status, .. }
                if *tid == turn_id && *status == TurnStatus::Failed),
            "the partial failed turn is kept as content"
        );
        assert!(
            matches!(&merged.items[2], ConversationItem::Outcome { turn_id: tid, status, .. }
                if *tid == turn_id && *status == OutcomeStatus::Failed),
            "the journal marker for the same turn_id is also kept, after the content"
        );
    }

    #[test]
    fn merge_attributes_parse_warnings_to_their_agent() {
        // Two agents each with a distinct parse warning → each warning is
        // attributable to its own AgentConversationMeta.
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let warn = |reason: &str| switchboard_harness::ParseWarning {
            line_number: 1,
            reason: reason.to_owned(),
        };
        let a_t = LoadedTranscript {
            turns: Vec::new(),
            meta: None,
            last_rate_limit: None,
            last_rate_limit_as_of: None,
            warnings: vec![warn("a busted")],
        };
        let b_t = LoadedTranscript {
            turns: Vec::new(),
            meta: None,
            last_rate_limit: None,
            last_rate_limit_as_of: None,
            warnings: vec![warn("b busted")],
        };

        let merged = merge_project_conversation(Vec::new(), vec![(a, a_t, None), (b, b_t, None)]);

        let a_meta = merged.agents.iter().find(|m| m.agent_id == a).unwrap();
        let b_meta = merged.agents.iter().find(|m| m.agent_id == b).unwrap();
        assert_eq!(a_meta.warnings, vec![warn("a busted")]);
        assert_eq!(b_meta.warnings, vec![warn("b busted")]);
        assert!(a_meta.load_error.is_none());
        assert!(b_meta.load_error.is_none());
    }

    #[test]
    fn merge_carries_per_agent_load_error_without_dropping_others() {
        // One agent's transcript failed to load (empty transcript + load_error);
        // the journal items and the healthy agent's turn still render.
        let send_id = Uuid::now_v7();
        let healthy = Uuid::now_v7();
        let broken = Uuid::now_v7();
        let th = Uuid::now_v7();
        let journal = vec![send_record(send_id, th, healthy, "go", 0)];
        let healthy_t = transcript_of(vec![agent_turn(th, healthy, "ok", 1)]);

        let merged = merge_project_conversation(
            journal,
            vec![
                (healthy, healthy_t, None),
                (
                    broken,
                    transcript_of(Vec::new()),
                    Some("sidecar corrupt".to_owned()),
                ),
            ],
        );

        assert!(
            merged
                .items
                .iter()
                .any(|i| matches!(i, ConversationItem::UserMessage { .. })),
            "journal item still present"
        );
        assert!(
            merged
                .items
                .iter()
                .any(|i| matches!(i, ConversationItem::AgentTurn { agent_id, .. } if *agent_id == healthy)),
            "healthy agent's turn still present"
        );
        let broken_meta = merged.agents.iter().find(|m| m.agent_id == broken).unwrap();
        assert_eq!(broken_meta.load_error.as_deref(), Some("sidecar corrupt"));
    }

    #[test]
    fn parse_outcome_classifies_status_and_reason() {
        let (s, r) = parse_outcome(&serde_json::json!({"status": "cancelled", "source": "user"}));
        assert_eq!(s, OutcomeStatus::Cancelled);
        assert_eq!(r.as_deref(), Some("user"));

        let (s, r) = parse_outcome(&serde_json::json!({"status": "failed", "message": "boom"}));
        assert_eq!(s, OutcomeStatus::Failed);
        assert_eq!(r.as_deref(), Some("boom"));

        // Missing detail → None; an unknown status falls back to Failed.
        let (s, r) = parse_outcome(&serde_json::json!({"status": "weird"}));
        assert_eq!(s, OutcomeStatus::Failed);
        assert_eq!(r, None);
    }

    // ---- load_project_conversation_impl: command-level wiring ----

    #[tokio::test]
    async fn load_project_conversation_wires_journal_with_empty_transcripts() {
        // A real project + a Claude agent that never dispatched (no session ⇒
        // empty transcript). Append journal records directly, then assert the
        // command surfaces the journal-sourced items.
        let (tmp, state, _) = fresh_state_with_mock();
        let (agent, project_id) = project_with_agent(&state, &tmp).await;
        let project = lock(&state.projects).get(&project_id).cloned().unwrap();
        let send_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        switchboard_core::journal::append_record(
            &project.journal_path(),
            &send_record(send_id, turn_id, agent.id, "hi", 0),
        )
        .unwrap();
        switchboard_core::journal::append_record(
            &project.journal_path(),
            &outcome_record(
                send_id,
                turn_id,
                agent.id,
                serde_json::json!({"status": "cancelled", "source": "user"}),
                1,
            ),
        )
        .unwrap();

        let home = tmp.path().to_path_buf();
        let conv = load_project_conversation_impl(&state, project_id, &home)
            .await
            .unwrap();

        assert_eq!(
            conv.items.len(),
            2,
            "journal user message + cancel marker (empty transcript adds none)"
        );
        assert!(matches!(
            conv.items[0],
            ConversationItem::UserMessage { .. }
        ));
        assert!(matches!(
            conv.items[1],
            ConversationItem::Outcome {
                status: OutcomeStatus::Cancelled,
                ..
            }
        ));
        assert_eq!(conv.agents.len(), 1, "the one agent's meta is carried");
    }

    #[tokio::test]
    async fn load_project_conversation_missing_journal_has_no_user_items() {
        let (tmp, state, _) = fresh_state_with_mock();
        let (_agent, project_id) = project_with_agent(&state, &tmp).await;

        let home = tmp.path().to_path_buf();
        let conv = load_project_conversation_impl(&state, project_id, &home)
            .await
            .unwrap();

        assert!(
            conv.items.is_empty(),
            "no journal ⇒ no user/outcome items; empty transcript adds none"
        );
    }

    // --- Git view: registry, auto-sync, linking, aggregate ------------------

    /// Run `git` in `dir`, asserting success. Fixtures are built with the real
    /// CLI so they match on-disk repo shapes (worktree records, origin/HEAD).
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The full HEAD commit id of a git repo (for the commit-read tests).
    fn head_oid(dir: &Path) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A git repo with one commit on `main`, hermetic config.
    fn init_git_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@e.com"]);
        git(dir, &["config", "user.name", "T"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("README.md"), "hi\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
    }

    /// State with both registries pointed at temp files (so persistence is
    /// exercised without touching user-global state). Returns the temp dir
    /// holding the yaml files alongside the state.
    fn state_with_registries() -> (TempDir, AppState) {
        let cfg = TempDir::new().unwrap();
        let state = mock_app_state()
            .with_workspace(cfg.path().join("workspace.yaml"))
            .with_git_registry(cfg.path().join("git-view.yaml"));
        (cfg, state)
    }

    #[tokio::test]
    async fn init_directory_auto_adds_repo_root_to_git_registry() {
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());

        init_directory_impl(&state, repo.path().to_str().unwrap())
            .await
            .unwrap();

        let canonical = repo.path().canonicalize().unwrap();
        assert!(
            lock(&state.git_registry).contains(&canonical),
            "a git working directory auto-syncs its repo root into the git registry"
        );
    }

    #[tokio::test]
    async fn init_directory_does_not_track_a_non_git_directory() {
        let (_cfg, state) = state_with_registries();
        let plain = TempDir::new().unwrap(); // no `git init`

        init_directory_impl(&state, plain.path().to_str().unwrap())
            .await
            .unwrap();

        assert!(
            lock(&state.git_registry).roots().is_empty(),
            "a non-git directory must not be tracked (auto-sync is a no-op, not an error)"
        );
    }

    #[tokio::test]
    async fn auto_sync_dedups_subdirectory_and_worktree_to_one_root() {
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        git(repo.path(), &["branch", "feature"]);

        // Add the repo root, a subdirectory, and a linked worktree as separate
        // working directories — all must resolve to the one canonical root.
        let sub = repo.path().join("src/inner");
        std::fs::create_dir_all(&sub).unwrap();
        let wt = TempDir::new().unwrap();
        let wt_path = wt.path().join("feature-wt");
        git(
            repo.path(),
            &["worktree", "add", wt_path.to_str().unwrap(), "feature"],
        );

        init_directory_impl(&state, repo.path().to_str().unwrap())
            .await
            .unwrap();
        init_directory_impl(&state, sub.to_str().unwrap())
            .await
            .unwrap();
        init_directory_impl(&state, wt_path.to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(
            lock(&state.git_registry).roots().len(),
            1,
            "subdirectory + linked worktree of one repo dedup to a single tracked root"
        );
    }

    #[test]
    fn add_tracked_repo_accepts_subdirectory_and_rejects_non_git() {
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let sub = repo.path().join("nested");
        std::fs::create_dir_all(&sub).unwrap();

        // A subdirectory resolves to the root and is accepted.
        add_tracked_repo_impl(&state, sub.to_str().unwrap()).unwrap();
        let canonical = repo.path().canonicalize().unwrap();
        assert!(lock(&state.git_registry).contains(&canonical));

        // A second add (the root itself) dedups — still one entry.
        add_tracked_repo_impl(&state, repo.path().to_str().unwrap()).unwrap();
        assert_eq!(lock(&state.git_registry).roots().len(), 1);

        // A non-git path is rejected with the typed error for the inline UX.
        let plain = TempDir::new().unwrap();
        let err = add_tracked_repo_impl(&state, plain.path().to_str().unwrap()).unwrap_err();
        assert!(matches!(err, AppError::NotAGitRepo { .. }));
    }

    #[tokio::test]
    async fn remove_directory_leaves_repo_tracked_in_git_view() {
        // Decision 5: the git view is a superset — removing a working directory
        // does NOT untrack its repo.
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        init_directory_impl(&state, repo.path().to_str().unwrap())
            .await
            .unwrap();
        let canonical = repo.path().canonicalize().unwrap();
        assert!(lock(&state.git_registry).contains(&canonical));

        remove_directory_impl(&state, repo.path().to_str().unwrap())
            .await
            .unwrap();

        assert!(
            lock(&state.git_registry).contains(&canonical),
            "removing a working directory must leave the repo tracked in the git view"
        );
        assert!(
            !lock(&state.workspace).contains(&canonical),
            "but it is removed from the workspace"
        );
    }

    #[test]
    fn remove_tracked_repo_touches_only_the_registry() {
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        add_tracked_repo_impl(&state, repo.path().to_str().unwrap()).unwrap();

        remove_tracked_repo_impl(&state, repo.path().to_str().unwrap());

        assert!(lock(&state.git_registry).roots().is_empty());
        // Files on disk are untouched — the repo still exists.
        assert!(repo.path().join(".git").exists());
        assert!(repo.path().join("README.md").exists());
    }

    #[tokio::test]
    async fn fetch_repo_refuses_untracked_path() {
        // Fetch is the one Git-view command that spawns a subprocess, so it must
        // only run against roots the user has explicitly tracked — a path that
        // resolves outside the registry is refused before any `git` runs.
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());

        let err = fetch_repo_impl(&state, repo.path().to_str().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::RepoNotTracked { .. }));
    }

    #[tokio::test]
    async fn fetch_repo_runs_for_tracked_repo() {
        // A tracked repo (here with no remote) passes the membership gate; the
        // fetch itself is a no-op that succeeds, proving the guard lets real
        // tracked roots through. A subdirectory of the tracked root resolves to
        // it and is accepted too.
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        add_tracked_repo_impl(&state, repo.path().to_str().unwrap()).unwrap();
        let sub = repo.path().join("nested");
        std::fs::create_dir_all(&sub).unwrap();

        fetch_repo_impl(&state, sub.to_str().unwrap())
            .await
            .unwrap();
    }

    #[test]
    fn worktree_difftool_argv_matches_worktree_diff_for_tracked_changes() {
        assert_eq!(
            worktree_difftool_argv(
                "/repo/wt",
                "src/main.rs",
                switchboard_git::ChangeKind::Modified
            ),
            vec![
                "-C",
                "/repo/wt",
                "difftool",
                "--no-prompt",
                "HEAD",
                "--",
                "src/main.rs",
            ]
        );
    }

    #[test]
    fn worktree_difftool_argv_uses_no_index_for_untracked_files() {
        assert_eq!(
            worktree_difftool_argv(
                "/repo/wt",
                "new.txt",
                switchboard_git::ChangeKind::Untracked
            ),
            vec![
                "-C",
                "/repo/wt",
                "difftool",
                "--no-prompt",
                "--no-index",
                "--",
                "/dev/null",
                "new.txt",
            ]
        );
    }

    #[test]
    fn commit_difftool_argv_compares_parent_to_commit_for_one_file() {
        assert_eq!(
            commit_difftool_argv("/repo", "parent", "commit", "src/main.rs"),
            vec![
                "-C",
                "/repo",
                "difftool",
                "--no-prompt",
                "parent",
                "commit",
                "--",
                "src/main.rs",
            ]
        );
    }

    #[tokio::test]
    async fn difftool_refuses_untracked_paths_before_running_git() {
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());

        let worktree_err = open_worktree_file_difftool_impl(
            &state,
            repo.path().to_str().unwrap(),
            "README.md",
            switchboard_git::ChangeKind::Modified,
        )
        .await
        .unwrap_err();
        assert!(matches!(worktree_err, AppError::RepoNotTracked { .. }));

        let oid = head_oid(repo.path());
        let commit_err = open_commit_file_difftool_impl(
            &state,
            repo.path().to_str().unwrap(),
            &oid,
            "README.md",
        )
        .await
        .unwrap_err();
        assert!(matches!(commit_err, AppError::RepoNotTracked { .. }));
    }

    #[tokio::test]
    async fn commit_difftool_parent_resolution_handles_root_and_child_commits() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let root = head_oid(repo.path());
        assert_eq!(
            commit_first_parent_or_empty_tree(repo.path(), &root)
                .await
                .unwrap(),
            EMPTY_TREE_OID
        );

        std::fs::write(repo.path().join("README.md"), "second\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "second"]);
        let child = head_oid(repo.path());
        assert_eq!(
            commit_first_parent_or_empty_tree(repo.path(), &child)
                .await
                .unwrap(),
            root
        );
    }

    #[tokio::test]
    async fn commit_difftool_parent_resolution_surfaces_invalid_commit() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());

        let err = commit_first_parent_or_empty_tree(repo.path(), "not-a-commit")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::GitDifftool { .. }));
    }

    #[tokio::test]
    async fn git_difftool_failure_maps_stderr_to_app_error() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());

        let err = run_git_difftool(repo.path(), vec!["not-a-real-git-subcommand".to_owned()])
            .await
            .unwrap_err();
        match err {
            AppError::GitDifftool { message, .. } => {
                assert!(message.contains("not-a-real-git-subcommand"));
            }
            other => panic!("expected GitDifftool, got {other:?}"),
        }
    }

    /// The tracked-root set for a repo, as the command layer would snapshot it.
    fn roots_of(repo: &TempDir) -> Vec<PathBuf> {
        vec![repo.path().canonicalize().unwrap()]
    }

    #[test]
    fn changed_files_reports_staged_unstaged_and_untracked() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        // Tracked file modified (unstaged), a staged addition, and an untracked file.
        std::fs::write(repo.path().join("README.md"), "changed\n").unwrap();
        std::fs::write(repo.path().join("staged.txt"), "s\n").unwrap();
        git(repo.path(), &["add", "staged.txt"]);
        std::fs::write(repo.path().join("untracked.txt"), "u\n").unwrap();

        let files = changed_files_impl(&roots_of(&repo), repo.path().to_str().unwrap()).unwrap();
        let kind = |name: &str| files.iter().find(|f| f.path == name).map(|f| f.change);
        assert_eq!(
            kind("README.md"),
            Some(switchboard_git::ChangeKind::Modified)
        );
        assert_eq!(kind("staged.txt"), Some(switchboard_git::ChangeKind::Added));
        assert_eq!(
            kind("untracked.txt"),
            Some(switchboard_git::ChangeKind::Untracked)
        );
    }

    #[test]
    fn file_diff_returns_structured_hunks_through_the_command_layer() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        std::fs::write(repo.path().join("code.txt"), "a\nb\nc\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "add code"]);
        std::fs::write(repo.path().join("code.txt"), "a\nB\nc\n").unwrap();

        let diff =
            file_diff_impl(&roots_of(&repo), repo.path().to_str().unwrap(), "code.txt").unwrap();
        assert!(!diff.binary && !diff.truncated);
        let lines: Vec<_> = diff.hunks.iter().flat_map(|h| &h.lines).collect();
        assert!(
            lines
                .iter()
                .any(|l| l.origin == switchboard_git::DiffLineKind::Removed && l.content == "b"),
        );
        assert!(
            lines
                .iter()
                .any(|l| l.origin == switchboard_git::DiffLineKind::Added && l.content == "B"),
        );
    }

    #[test]
    fn diff_reads_refuse_an_untracked_worktree() {
        // The Git-view data reads honor the tracked set: a path whose repo root
        // isn't tracked (a stale panel after "Remove from view") yields the empty
        // non-error result, never live git data.
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        std::fs::write(repo.path().join("README.md"), "changed\n").unwrap();
        let untracked: &[PathBuf] = &[];

        let files = changed_files_impl(untracked, repo.path().to_str().unwrap()).unwrap();
        assert!(files.is_empty(), "untracked repo yields no changed files");

        let diff = file_diff_impl(untracked, repo.path().to_str().unwrap(), "README.md").unwrap();
        assert!(
            diff.hunks.is_empty() && !diff.binary,
            "untracked repo yields an empty diff"
        );
    }

    #[test]
    fn branch_commits_returns_ranges_through_the_command_layer() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path()); // "init" on main, no upstream → recent
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "second"]);

        let ranges = commit_ranges_impl(
            &roots_of(&repo),
            repo.path().to_str().unwrap(),
            switchboard_git::BranchKind::Local,
            "main",
        )
        .unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].kind, switchboard_git::CommitRangeKind::Recent);
        let subjects: Vec<_> = ranges[0]
            .commits
            .iter()
            .map(|c| c.subject.as_str())
            .collect();
        assert_eq!(subjects, vec!["second", "init"]);
    }

    #[test]
    fn commit_changed_files_and_diff_through_the_command_layer() {
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        std::fs::write(repo.path().join("code.txt"), "a\nb\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "add code"]);
        std::fs::write(repo.path().join("code.txt"), "a\nB\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "change code"]);
        let head = head_oid(repo.path());

        let changes =
            commit_changed_files_impl(&roots_of(&repo), repo.path().to_str().unwrap(), &head)
                .unwrap();
        assert!(changes.found);
        assert_eq!(changes.files.len(), 1);
        assert_eq!(changes.files[0].path, "code.txt");

        let diff = commit_file_diff_impl(
            &roots_of(&repo),
            repo.path().to_str().unwrap(),
            &head,
            "code.txt",
        )
        .unwrap();
        let lines: Vec<_> = diff.hunks.iter().flat_map(|h| &h.lines).collect();
        assert!(
            lines
                .iter()
                .any(|l| l.content == "B" && l.origin == switchboard_git::DiffLineKind::Added)
        );
    }

    #[test]
    fn commit_reads_reject_an_untracked_repo() {
        // Unlike the worktree reads (which degrade to empty), the commit reads are
        // invoked deliberately for a tracked branch, so an untracked root is a
        // stale reference and is rejected.
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let head = head_oid(repo.path());
        let untracked: &[PathBuf] = &[];
        let root = repo.path().to_str().unwrap();

        assert!(matches!(
            commit_ranges_impl(untracked, root, switchboard_git::BranchKind::Local, "main"),
            Err(AppError::RepoNotTracked { .. })
        ));
        assert!(matches!(
            commit_changed_files_impl(untracked, root, &head),
            Err(AppError::RepoNotTracked { .. })
        ));
        assert!(matches!(
            commit_file_diff_impl(untracked, root, &head, "README.md"),
            Err(AppError::RepoNotTracked { .. })
        ));
    }

    #[test]
    fn commit_reads_handle_missing_refs_without_a_worktree() {
        // A tracked repo, but the branch/oid don't resolve (a stale frontend
        // reference). Commits need no worktree, so this must degrade cleanly:
        // ranges empty, changes report `found: false`, diff empty — not an error.
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let root = repo.path().to_str().unwrap();
        let absent_oid = "0".repeat(40);

        let ranges = commit_ranges_impl(
            &roots_of(&repo),
            root,
            switchboard_git::BranchKind::Local,
            "no-such-branch",
        )
        .unwrap();
        assert!(ranges.is_empty());

        // A vanished commit is reported as not-found (distinct from a real empty
        // commit), calmly — not an error.
        let changes = commit_changed_files_impl(&roots_of(&repo), root, &absent_oid).unwrap();
        assert!(!changes.found);
        assert!(changes.files.is_empty());
        assert!(
            commit_file_diff_impl(&roots_of(&repo), root, &absent_oid, "README.md")
                .unwrap()
                .hunks
                .is_empty()
        );
    }

    #[test]
    fn editor_open_argv_uses_command_or_falls_back_to_os_open() {
        // A bare editor command runs against the path…
        assert_eq!(
            editor_open_argv(Some("cursor"), "/repo/wt"),
            vec![
                "/bin/zsh",
                "-lc",
                "exec \"$@\"",
                "switchboard-editor",
                "cursor",
                "/repo/wt"
            ]
        );
        // …a command with flags is shell-split into program + args + path…
        assert_eq!(
            editor_open_argv(Some("code --reuse-window"), "/repo/wt"),
            vec![
                "/bin/zsh",
                "-lc",
                "exec \"$@\"",
                "switchboard-editor",
                "code",
                "--reuse-window",
                "/repo/wt"
            ]
        );
        // …an absent command falls back to the OS folder-open…
        assert_eq!(editor_open_argv(None, "/repo/wt"), vec!["open", "/repo/wt"]);
        // …and a command with malformed quoting (splits to nothing) also falls
        // back rather than silently failing to spawn.
        assert_eq!(
            editor_open_argv(Some("\"unterminated"), "/repo/wt"),
            vec!["open", "/repo/wt"]
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn editor_open_argv_resolves_command_from_login_shell_path() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        let zdot_dir = tmp.path().join("zdot");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&zdot_dir).unwrap();

        let editor = bin_dir.join("fake-editor");
        let args_file = tmp.path().join("args.txt");
        std::fs::write(
            &editor,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$SWITCHBOARD_FAKE_EDITOR_ARGS\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&editor).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&editor, permissions).unwrap();
        std::fs::write(
            zdot_dir.join(".zprofile"),
            format!("export PATH=\"{}:$PATH\"\n", bin_dir.display()),
        )
        .unwrap();

        let argv = editor_open_argv(Some("fake-editor --reuse-window"), "/repo/work tree");
        let (program, rest) = argv.split_first().unwrap();
        let status = std::process::Command::new(program)
            .args(rest)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("ZDOTDIR", &zdot_dir)
            .env("SWITCHBOARD_FAKE_EDITOR_ARGS", &args_file)
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(args_file).unwrap(),
            "--reuse-window\n/repo/work tree\n"
        );
    }

    #[test]
    fn terminal_and_reveal_argv_are_macos_open_invocations() {
        assert_eq!(
            terminal_open_argv("iTerm", "/repo/wt"),
            vec!["open", "-a", "iTerm", "/repo/wt"]
        );
        assert_eq!(
            reveal_in_finder_argv("/repo/wt"),
            vec!["open", "-R", "/repo/wt"]
        );
    }

    #[tokio::test]
    async fn aggregate_links_project_to_its_worktree_and_is_partial_on_bad_repo() {
        let (_cfg, state) = state_with_registries();

        // A tracked repo hosting a Switchboard project at its main worktree.
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        init_directory_impl(&state, repo.path().to_str().unwrap())
            .await
            .unwrap();
        let project = create_project_in_only_dir(&state, "alpha");
        let canonical = repo.path().canonicalize().unwrap();

        // A second tracked repo whose path no longer exists → unavailable row.
        let gone = TempDir::new().unwrap();
        init_git_repo(gone.path());
        add_tracked_repo_impl(&state, gone.path().to_str().unwrap()).unwrap();
        let gone_root = gone.path().canonicalize().unwrap();
        drop(gone); // directory removed from disk

        let inputs = tracked_repos_inputs(&state);
        let listings = list_tracked_repos_from_inputs(&inputs);
        assert_eq!(
            listings.len(),
            2,
            "both tracked repos appear (partial success)"
        );

        let live = listings
            .iter()
            .find(|l| l.repo.root == canonical)
            .expect("the live repo is listed");
        assert!(live.repo.available);
        // Look up links by the main branch's actual worktree path (the same key
        // the frontend uses — `WorktreeView.path`).
        let main_wt = live
            .repo
            .local_branches
            .iter()
            .find(|b| b.name == "main")
            .and_then(|b| b.worktree.as_ref())
            .expect("main is checked out in a worktree");
        let key = main_wt.path.to_string_lossy().into_owned();
        let links = live
            .linked_projects
            .get(&key)
            .expect("the main worktree has linked projects");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].id, project.id);
        assert_eq!(links[0].name, "alpha");

        let dead = listings
            .iter()
            .find(|l| l.repo.root == gone_root)
            .expect("the vanished repo still appears, marked unavailable");
        assert!(
            !dead.repo.available,
            "a vanished repo degrades to an unavailable row, not a failed call"
        );
    }

    #[tokio::test]
    async fn aggregate_does_not_link_a_project_in_a_subfolder_of_a_worktree() {
        // Decision 7: linking is exact path-match. A project whose directory is a
        // *subfolder* of the worktree is not linked.
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let subdir = repo.path().join("packages/app");
        std::fs::create_dir_all(&subdir).unwrap();

        // Track the repo, but create the project in a subfolder of it.
        add_tracked_repo_impl(&state, repo.path().to_str().unwrap()).unwrap();
        init_directory_impl(&state, subdir.to_str().unwrap())
            .await
            .unwrap();
        create_project_impl(&state, "sub", subdir.to_str().unwrap()).unwrap();

        let inputs = tracked_repos_inputs(&state);
        let listings = list_tracked_repos_from_inputs(&inputs);
        let canonical = repo.path().canonicalize().unwrap();
        let repo_listing = listings
            .iter()
            .find(|l| l.repo.root == canonical)
            .expect("the repo is listed");
        assert!(
            repo_listing.linked_projects.is_empty(),
            "a project in a subfolder of the worktree is not linked (exact-match only)"
        );
    }

    #[test]
    fn read_tracked_repo_rejects_an_untracked_path() {
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        // Repo exists on disk but is NOT in the registry.
        let inputs = tracked_repos_inputs(&state);
        let listing = read_tracked_repo_from_inputs(repo.path().to_str().unwrap(), &inputs);
        assert!(
            !listing.repo.available,
            "an untracked path must not return live git data"
        );
    }

    #[test]
    fn read_tracked_repo_accepts_a_path_inside_a_tracked_repo() {
        let (_cfg, state) = state_with_registries();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        add_tracked_repo_impl(&state, repo.path().to_str().unwrap()).unwrap();
        let sub = repo.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();

        // A subdirectory of a tracked repo resolves to the tracked root and reads.
        let inputs = tracked_repos_inputs(&state);
        let listing = read_tracked_repo_from_inputs(sub.to_str().unwrap(), &inputs);
        assert!(listing.repo.available);
        assert_eq!(
            listing.repo.root.canonicalize().unwrap(),
            repo.path().canonicalize().unwrap()
        );
    }

    // --- Preferences (config.yaml) --------------------------------------------

    #[test]
    fn preferences_default_then_set_round_trips_through_state_and_disk() {
        let cfg = TempDir::new().unwrap();
        let path = cfg.path().join("config.yaml");
        let state = mock_app_state().with_preferences(path.clone());

        // Defaults until set.
        let defaults = get_preferences_impl(&state);
        assert_eq!(defaults.editor_command.as_deref(), Some("code"));
        assert_eq!(defaults.terminal_app, "Terminal");

        let prefs = Preferences {
            editor_command: Some("zed".to_owned()),
            terminal_app: "iTerm".to_owned(),
            diff_style: preferences::DiffStyle::Unified,
        };
        set_preferences_impl(&state, &prefs).unwrap();

        // In-memory reflects the change immediately...
        assert_eq!(get_preferences_impl(&state), prefs);
        // ...and a fresh load from disk sees the persisted value.
        assert_eq!(preferences::load(&path), prefs);
    }

    #[test]
    fn set_preferences_normalizes_blank_values_at_the_boundary() {
        // A client (or a future caller) sending blank strings must be normalized
        // by the backend, not stored verbatim — the open-actions consume this.
        let cfg = TempDir::new().unwrap();
        let path = cfg.path().join("config.yaml");
        let state = mock_app_state().with_preferences(path.clone());

        set_preferences_impl(
            &state,
            &Preferences {
                editor_command: Some("  ".to_owned()),
                terminal_app: String::new(),
                diff_style: preferences::DiffStyle::SideBySide,
            },
        )
        .unwrap();

        let got = get_preferences_impl(&state);
        assert_eq!(got.editor_command, None);
        assert_eq!(got.terminal_app, "Terminal");
        // ...and the normalized form is what hit disk.
        assert_eq!(preferences::load(&path), got);
    }

    #[test]
    fn set_preferences_with_no_path_updates_memory_only() {
        // No `with_preferences` → no path; set must still update the running
        // session (and not error) without touching any user-global file.
        let state = mock_app_state();
        let prefs = Preferences {
            editor_command: Some("code".to_owned()),
            terminal_app: "Terminal".to_owned(),
            diff_style: preferences::DiffStyle::SideBySide,
        };
        set_preferences_impl(&state, &prefs).unwrap();
        assert_eq!(get_preferences_impl(&state), prefs);
    }

    /// Build a state whose prompt service points at a fresh temp prompts dir
    /// (with `config.yaml` absent, so the default dir is used). Returns the dir
    /// so the test can drop prompt files into it.
    fn state_with_prompts() -> (TempDir, AppState) {
        let (tmp, state, _) = fresh_state_with_mock();
        let prompts_dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        let service = switchboard_prompts::PromptService::new(
            tmp.path().join("config.yaml"),
            prompts_dir,
            None,
            Arc::new(switchboard_prompts::InMemorySecretStore::new()),
        );
        (tmp, state.with_prompts(service))
    }

    const GREET_PROMPT: &str = "---\nname: greet\ndescription: Greeting.\narguments:\n  - name: who\n    required: true\n---\nHi {{ who }}\n";

    #[tokio::test]
    async fn list_prompts_surfaces_a_local_prompt_after_sync() {
        let (tmp, state) = state_with_prompts();
        std::fs::write(tmp.path().join("prompts").join("greet.md"), GREET_PROMPT).unwrap();

        // `list_prompts` reads the cache; it's empty until a sync runs.
        assert!(list_prompts_impl(&state).is_empty());
        state.prompts.sync().await;

        let prompts = list_prompts_impl(&state);
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].provider, "local");
        assert_eq!(prompts[0].name, "greet");
        assert_eq!(prompts[0].arguments.len(), 1);
        assert!(prompts[0].arguments[0].required);
    }

    #[tokio::test]
    async fn render_prompt_substitutes_arguments() {
        let (tmp, state) = state_with_prompts();
        std::fs::write(tmp.path().join("prompts").join("greet.md"), GREET_PROMPT).unwrap();

        let args = std::collections::BTreeMap::from([("who".to_owned(), "Ada".to_owned())]);
        // Render does not depend on the cache (no sync needed).
        let rendered = render_prompt_impl(&state, "local", "greet", &args)
            .await
            .unwrap();
        assert!(rendered.text.contains("Hi Ada"));
    }

    #[tokio::test]
    async fn render_prompt_missing_required_arg_is_error() {
        let (tmp, state) = state_with_prompts();
        std::fs::write(tmp.path().join("prompts").join("greet.md"), GREET_PROMPT).unwrap();

        let err = render_prompt_impl(&state, "local", "greet", &std::collections::BTreeMap::new())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Prompt(_)));
    }

    #[test]
    fn list_prompts_on_disabled_service_is_empty() {
        // A state without `with_prompts` keeps the disabled default; its cache
        // is empty without a sync.
        let (_tmp, state, _) = fresh_state_with_mock();
        assert!(list_prompts_impl(&state).is_empty());
    }

    #[tokio::test]
    async fn mcp_provider_add_list_remove_round_trip() {
        // Wiring check through the command impls (service logic is covered in the
        // prompts crate). Uses the state's in-memory secret store.
        let (_tmp, state) = state_with_prompts();

        add_mcp_provider_impl(&state, "team", "https://mcp.example.com", Some("tok")).unwrap();
        let providers = list_mcp_providers_impl(&state);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name, "team");
        assert!(providers[0].has_token);

        // Duplicate names are rejected at the command boundary.
        assert!(matches!(
            add_mcp_provider_impl(&state, "team", "https://other", None),
            Err(AppError::Prompt(_))
        ));

        remove_mcp_provider_impl(&state, "team").unwrap();
        assert!(list_mcp_providers_impl(&state).is_empty());
    }

    #[tokio::test]
    async fn sync_prompts_and_notify_emits_after_sync() {
        // Every sync path routes through this helper; the emit is bound to the
        // sync so a warm-cache draft restore can't get stuck waiting for an event
        // that only add/remove used to fire.
        let emitter = Arc::new(RecordingEmitter::new());
        sync_prompts_and_notify(
            PromptService::disabled(),
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
        )
        .await;

        let names: Vec<String> = emitter
            .snapshot()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, vec![PROMPTS_SYNCED_EVENT.to_owned()]);
    }
}
