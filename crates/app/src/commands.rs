//! Free-function implementations behind each Tauri command. The
//! `#[tauri::command]` wrappers in `lib.rs` are thin shims that adapt these
//! to Tauri's `State<'_, AppState>` / `String` conventions; the free
//! functions are what the unit tests target.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use switchboard_core::{
    AgentId, AgentRecord, CoreError, Directory, HarnessKind, Project, ProjectId, ProjectSummary,
    SendId,
};
use switchboard_dispatcher::{
    CancelOutcome, DispatchContextFactory, OnBusy, RemovedQueuedMessage, SendOutcome,
};
use switchboard_harness::{CancelSource, HarnessAdapter, MessageId};
use uuid::Uuid;

use crate::dispatch_context::ProjectDispatchContextFactory;
use crate::error::AppError;
use crate::state::{AppState, lock, persist_workspace};

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
                listings.push(ProjectListing {
                    id: s.id,
                    name: s.name,
                    created_at: s.created_at,
                    directory: dir_str.clone(),
                    available: true,
                    last_activity,
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
                listings.push(ProjectListing {
                    id: s.id,
                    name: s.name,
                    created_at: s.created_at,
                    directory: dir_str.clone(),
                    available: false,
                    last_activity: s.created_at,
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
) -> Result<AgentRecord, AppError> {
    // Same TOCTOU protection as create_project_impl — register_agent has
    // an internal read-check-then-append window that two concurrent IPC
    // calls could race through.
    let _write = lock(&state.registry_write);
    let active = lock(&state.active_project_id).ok_or(AppError::NoActiveProject)?;
    let project = lock(&state.projects)
        .get(&active)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(active))?;
    let record = project.register_agent(name, harness)?;
    lock(&state.agents_by_id).insert(record.id, record.clone());
    Ok(record)
}

/// Attach an existing harness session (Claude Code, Codex, Gemini, or
/// Antigravity) as a new Switchboard agent in the active project.
///
/// Validation order (all under the directory-level `registry_write` mutex
/// so the cross-project session-id check + register form one atomic step):
/// 1. Active project resolved.
/// 2. `existing_session_id` parses as UUID.
/// 3. Per-harness session existence under `home_dir`. Claude / Gemini check a
///    session file; Codex's discovery also returns the parsed `YYYY-MM-DD`
///    (the sidecar's `session_partition_date`); Antigravity checks that the
///    server-assigned conversation directory `brain/<uuid>/` exists (the
///    transcript inside may be absent — hydration degrades gracefully).
/// 4. Session-id collision scan (loaded or not — the scan walks projects on
///    disk). Scope differs by harness: Claude and Gemini scan only the active
///    project's **own directory** (`enumerate_directory_projects`) because
///    their session ids are caller-controlled and cwd-namespaced, so a widened
///    scan would false-reject a legitimately-distinct same-id-different-cwd
///    session. Codex and Antigravity scan **all loaded directories**
///    (`enumerate_all_projects`) because their ids are server-assigned and
///    globally unique. Claude and Gemini scan `AgentRecord.session_id`
///    (caller-controlled UUID); Codex and Antigravity scan every project's
///    `sessions/<agent_id>.*.jsonl` sidecar
///    (those agents leave `AgentRecord.session_id = None` — the id lives in
///    the sidecar). Two `AgentRecord`s pointing at the same harness session
///    is the same-session-parallel-invocation hazard
///    (`docs/research/same-session-parallel-invocation.md`); unloaded
///    projects could still be opened and dispatched concurrently later, so
///    loaded-only scope would miss the collision.
/// 5. Register via the harness-specific `register_attached_*` method.
/// 6. (Codex and Antigravity) Append the first sidecar record (Codex carries
///    the discovered `session_partition_date`; Antigravity the captured
///    `conversation_id`). Written **before** step 5 commits so a failed
///    sidecar write leaves an orphan sidecar, never an orphan agent.
/// 7. (Codex only) Insert the new `agent_id` into `needs_session_meta` so
///    every dispatch up to and including the one that observes `SessionMeta`
///    runs with `is_first_dispatch_after_attach: true` — forces `SessionMeta`
///    emission for the Codex sidebar. The per-dispatch emitter decorator
///    clears the flag once `session_meta` is genuinely observed on the wire.
///    Claude and Antigravity attaches do **not** populate this set: both emit
///    `SessionMeta` on every dispatch (Claude from `system/init`; Antigravity
///    constructs it post-terminal each turn), so the override has nothing to do.
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
) -> Result<AgentRecord, AppError> {
    let _write = lock(&state.registry_write);
    let active = lock(&state.active_project_id).ok_or(AppError::NoActiveProject)?;
    let project = lock(&state.projects)
        .get(&active)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(active))?;
    // The active project's owning directory — the cwd that namespaces Claude /
    // Gemini session files and the root of Codex / Antigravity sidecars.
    let directory = lock(&state.directories)
        .get(&project.directory)
        .cloned()
        .ok_or(AppError::NoDirectory)?;

    let session_uuid = parse_uuid(existing_session_id)?;

    let record = match harness {
        HarnessKind::ClaudeCode => {
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
            project.register_attached_claude_agent(name, session_uuid)?
        }
        HarnessKind::Codex => {
            let (_path, session_partition_date) =
                switchboard_harness::find_codex_session_file_for_attach(
                    home_dir,
                    existing_session_id,
                )
                .map_err(map_codex_attach_lookup_error(harness, home_dir))?;
            check_codex_session_id_unique(state, existing_session_id)?;
            // Pre-mint the AgentId so we can write the sidecar **before**
            // committing the registry record. If the sidecar write fails,
            // the registry stays untouched — at worst an orphan sidecar
            // file lands on disk, invisible to dispatch and the collision
            // scan (which walks AgentRecords → looks up *their* sidecars,
            // not the inverse). Inverted commit order, inverted blast
            // radius vs. registry-first.
            let new_agent_id = Uuid::now_v7();
            let sidecar_path = switchboard_harness::codex::sidecar::sidecar_path(
                &directory.path,
                project.id,
                new_agent_id,
            );
            let sidecar_record = switchboard_harness::codex::sidecar::SessionLinkRecord {
                session_id: existing_session_id.to_owned(),
                session_partition_date,
                started_at: chrono::Utc::now(),
            };
            switchboard_harness::codex::sidecar::append_record(&sidecar_path, &sidecar_record)?;
            let record = project.register_attached_codex_agent_with_id(name, new_agent_id)?;
            // Codex-only: force SessionMeta on subsequent dispatches until
            // one is genuinely observed. Claude attaches don't need this —
            // see step 7 docstring.
            lock(&state.needs_session_meta).insert(record.id);
            record
        }
        HarnessKind::Gemini => {
            let candidate = locate_gemini_candidate(home_dir, &directory.path, session_uuid)?;
            tracing::debug!(
                session_id = %session_uuid,
                path = %candidate.display(),
                "gemini attach: bound to candidate"
            );
            check_gemini_session_id_unique(state, &directory, &session_uuid)?;
            project.register_attached_gemini_agent(name, session_uuid)?
        }
        HarnessKind::Antigravity => {
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
            // Sidecar-before-registry, pre-minted id — same ordering and
            // inverted-blast-radius rationale as the Codex arm above.
            let new_agent_id = Uuid::now_v7();
            let sidecar_path = switchboard_harness::antigravity::sidecar::sidecar_path(
                &directory.path,
                project.id,
                new_agent_id,
            );
            let sidecar_record = switchboard_harness::antigravity::sidecar::SessionLinkRecord {
                conversation_id: session_uuid,
                captured_at: chrono::Utc::now(),
            };
            switchboard_harness::antigravity::sidecar::append_record(
                &sidecar_path,
                &sidecar_record,
            )?;
            project.register_attached_antigravity_agent_with_id(name, new_agent_id)?
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
            if agent.session_id == Some(*candidate) {
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

/// Cross-project Codex session-id collision check. Codex agents leave
/// `AgentRecord.session_id = None`; the session-link sidecar at
/// `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.jsonl`
/// is the system-of-record. Walks every project on disk in the bound
/// directory.
///
/// **Loud-fail on corrupt sidecar.** Sidecars are Switchboard-owned JSONL;
/// AGENTS.md's append-only-persistence invariant says Switchboard-owned
/// corruption surfaces (typed error), not skip-with-warning. Skipping
/// could let a duplicate attach through and violate same-session-uniqueness.
/// The error is wrapped in `AttachBlockedByCorruption` so the user sees
/// "the failure is about an *unrelated* agent's state, not your attach
/// target."
/// Per-directory Gemini session-id collision check. Gemini agents carry
/// `AgentRecord.session_id = Some(uuid)` (Claude shape). Walks every project on
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
            if agent.session_id == Some(*candidate) {
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

/// Cross-directory Codex session-id collision check. Codex session ids are
/// server-assigned and globally unique, so the scan spans **all loaded
/// directories** (`enumerate_all_projects`). Each project's sidecar lives under
/// its own owning directory (`project.directory`), so the sidecar path is
/// derived per-project, not from a single attach directory.
fn check_codex_session_id_unique(state: &AppState, candidate: &str) -> Result<(), AppError> {
    for project in enumerate_all_projects(state)? {
        for agent in project.list_agents()? {
            if agent.harness != HarnessKind::Codex {
                continue;
            }
            let sidecar = switchboard_harness::codex::sidecar::sidecar_path(
                &project.directory,
                project.id,
                agent.id,
            );
            let latest =
                switchboard_harness::codex::sidecar::read_latest(&sidecar).map_err(|source| {
                    AppError::AttachBlockedByCorruption {
                        path: sidecar.clone(),
                        source: Box::new(source),
                    }
                })?;
            if let Some(record) = latest
                && record.session_id == candidate
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
/// agent across **all loaded directories**. Mirrors the Codex check (the
/// conversation id lives in the per-agent sidecar, not the `AgentRecord`, so
/// the scan reads sidecars) and is likewise cross-directory: Antigravity
/// conversation ids are server-assigned and globally unique. The
/// same-session-parallel-invocation risk is identical: two agents resuming one
/// `--conversation <uuid>` would interleave server-side. Each project's sidecar
/// lives under its own owning directory (`project.directory`). Corrupt sidecars
/// fail loud rather than being skipped — a skipped sidecar could let a
/// duplicate attach through and violate the uniqueness contract.
fn check_antigravity_session_id_unique(state: &AppState, candidate: Uuid) -> Result<(), AppError> {
    for project in enumerate_all_projects(state)? {
        for agent in project.list_agents()? {
            if agent.harness != HarnessKind::Antigravity {
                continue;
            }
            let sidecar = switchboard_harness::antigravity::sidecar::sidecar_path(
                &project.directory,
                project.id,
                agent.id,
            );
            let latest = switchboard_harness::antigravity::sidecar::read_latest(&sidecar).map_err(
                |source| AppError::AttachBlockedByCorruption {
                    path: sidecar.clone(),
                    source: Box::new(source),
                },
            )?;
            if let Some(record) = latest
                && record.conversation_id == candidate
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

/// Resolves the agent (across all loaded projects) and accepts the send into
/// the dispatcher, returning the minted `MessageId` immediately. The turn's
/// `turn_id` and lifecycle flow over the per-agent event channel (the
/// correlated `TurnStart` carries this `message_id`); a failure before the turn
/// starts surfaces as a `MessageFailed` event. The `Result` carries only
/// **routing** failures (unknown agent, unsupported harness), resolved here
/// before the dispatcher is touched.
pub async fn send_message_impl(
    state: &AppState,
    agent_id: AgentId,
    prompt: &str,
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
    // — so `is_first_dispatch_after_attach` is read live, never frozen at
    // enqueue. See `crate::dispatch_context` and `AppState::needs_session_meta`.
    let factory: Arc<dyn DispatchContextFactory> = Arc::new(ProjectDispatchContextFactory::new(
        agent.clone(),
        project.directory.clone(),
        project.journal_path(),
        project.id,
        adapter,
        Arc::clone(&state.emitter),
        Arc::clone(&state.needs_session_meta),
    ));
    // `send_id` is minted by the frontend and shared across a fan-out's
    // recipients (one `send_message` call per recipient with the same id), so
    // hydration groups the user's message once. A single-recipient send is a
    // trivially-grouped 1-element fan-out with its own id.
    match state
        .dispatcher
        .send_message(agent_id, prompt, send_id, factory, OnBusy::Enqueue)
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
/// project-level conversation loader can reuse it per agent. Error scope and
/// missing-file/corruption behavior match the per-agent command: lookup-I/O
/// and per-harness defaults degrade to an empty transcript; corrupt
/// Switchboard-owned sidecars are fail-loud
/// ([`AppError::HydrationBlockedByCorruption`]).
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
    Ok(transcript)
}

/// Overlay a metadata sidecar's snapshot onto a freshly-loaded transcript.
///
/// **Fill-if-empty**: the sidecar fills `last_rate_limit` (+ its
/// `last_rate_limit_as_of` capture time) *only* when the loader left
/// `last_rate_limit` unset. A loader-provided value is a class-B source
/// (e.g. Codex's session-file rate-limit) that's already durable and
/// authoritative — it wins, and carries no `as_of` qualifier because it
/// isn't a stale snapshot. Mirrors the frontend reducer's hydrate fill-if-
/// empty semantics. A `None` sidecar (missing/corrupt) is a no-op.
fn apply_meta_sidecar_overlay(
    transcript: &mut switchboard_harness::LoadedTranscript,
    sidecar: Option<switchboard_harness::meta_sidecar::MetaSidecar>,
) {
    if transcript.last_rate_limit.is_some() {
        return;
    }
    if let Some(snapshot) = sidecar.and_then(|m| m.rate_limit) {
        transcript.last_rate_limit = Some(snapshot.payload);
        transcript.last_rate_limit_as_of = Some(snapshot.captured_at);
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
            let Some(session_id) = agent.session_id else {
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
            let sidecar_path = switchboard_harness::codex::sidecar::sidecar_path(
                &directory_path,
                project.id,
                agent.id,
            );
            // Fail loud on corrupt sidecars per AGENTS.md (our own JSONL is
            // never silently degraded). `Ok(None)` is the legitimate
            // never-dispatched case; only that path falls through to the
            // empty-transcript outcome.
            let latest = switchboard_harness::codex::sidecar::read_latest(&sidecar_path).map_err(
                |source| AppError::HydrationBlockedByCorruption {
                    path: sidecar_path.clone(),
                    source: Box::new(source),
                },
            )?;
            let (session_id, partition_date) = match latest {
                Some(record) => (record.session_id, Some(record.session_partition_date)),
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
            let Some(session_id) = agent.session_id else {
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
            // Antigravity agents carry `session_id: None` — the conversation
            // UUID is server-assigned and lives in the per-agent sidecar, so
            // this follows the Codex shape (sidecar lookup), not the Gemini
            // one (`agent.session_id`). Corrupt sidecar is fail-loud per the
            // Switchboard-owned-JSONL invariant; `Ok(None)` is the legitimate
            // never-dispatched case — passed through as `None` so the loader
            // still surfaces registry meta (matching the Codex arm).
            let sidecar_path = switchboard_harness::antigravity::sidecar::sidecar_path(
                &directory_path,
                project.id,
                agent.id,
            );
            let latest = switchboard_harness::antigravity::sidecar::read_latest(&sidecar_path)
                .map_err(|source| AppError::HydrationBlockedByCorruption {
                    path: sidecar_path.clone(),
                    source: Box::new(source),
                })?;
            Ok(switchboard_harness::load_antigravity_transcript(
                home_dir,
                &directory_path,
                latest.map(|record| record.conversation_id),
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
/// at least once (for Claude/Gemini that coincides with the file existing; for
/// Codex/Antigravity the id lives in a sidecar that's written post-dispatch, so
/// resume can be offered even if the local transcript file is absent).
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct AgentSessionInfo {
    /// Absolute path of the harness session file, present only if it exists.
    pub session_file: Option<String>,
    /// Full copy-ready resume command (`cd '<dir>' && <harness> …`), present
    /// only if the session can be resumed.
    pub resume_command: Option<String>,
}

/// Resolve the per-agent session actions ([`AgentSessionInfo`]). Mirrors
/// [`load_agent_transcript`]'s per-harness session-id resolution (Claude/Gemini
/// from `AgentRecord.session_id`; Codex/Antigravity from their sidecars — corrupt
/// sidecars fail loud, never-dispatched is the legitimate empty case). `home_dir`
/// is injected for testability; the Tauri shim reads `$HOME`.
pub fn agent_session_info_impl(
    state: &AppState,
    agent_id: AgentId,
    home_dir: &Path,
) -> Result<AgentSessionInfo, AppError> {
    let (project, agent) = lookup_agent(state, agent_id)?;
    let directory = project.directory.clone();

    // (session file if it exists, resume identifier if the agent can be resumed)
    let (session_file, resume_ref): (Option<PathBuf>, Option<String>) = match agent.harness {
        HarnessKind::ClaudeCode => match agent.session_id {
            Some(sid) => {
                let path =
                    switchboard_harness::claude_session_file_path(home_dir, &directory, &sid);
                let file = path.exists().then_some(path);
                let resume = file.as_ref().map(|_| sid.to_string());
                (file, resume)
            }
            None => (None, None),
        },
        HarnessKind::Gemini => match agent.session_id {
            Some(sid) => {
                let mut candidates =
                    switchboard_harness::gemini_session_file_candidates(home_dir, &directory, &sid);
                candidates.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
                let file = candidates.pop();
                let resume = file.as_ref().map(|_| sid.to_string());
                (file, resume)
            }
            None => (None, None),
        },
        HarnessKind::Codex => {
            let sidecar_path =
                switchboard_harness::codex::sidecar::sidecar_path(&directory, project.id, agent.id);
            let latest = switchboard_harness::codex::sidecar::read_latest(&sidecar_path).map_err(
                |source| AppError::HydrationBlockedByCorruption {
                    path: sidecar_path.clone(),
                    source: Box::new(source),
                },
            )?;
            match latest {
                Some(record) => {
                    let file = switchboard_harness::codex::session_file::locate_session_file(
                        home_dir,
                        record.session_partition_date,
                        &record.session_id,
                    );
                    (file, Some(record.session_id))
                }
                None => (None, None),
            }
        }
        HarnessKind::Antigravity => {
            let sidecar_path = switchboard_harness::antigravity::sidecar::sidecar_path(
                &directory, project.id, agent.id,
            );
            let latest = switchboard_harness::antigravity::sidecar::read_latest(&sidecar_path)
                .map_err(|source| AppError::HydrationBlockedByCorruption {
                    path: sidecar_path.clone(),
                    source: Box::new(source),
                })?;
            match latest {
                Some(record) => {
                    let path = switchboard_harness::antigravity::paths::transcript_path(
                        home_dir,
                        record.conversation_id,
                    );
                    let file = path.exists().then_some(path);
                    (file, Some(record.conversation_id.to_string()))
                }
                None => (None, None),
            }
        }
        _ => return Err(AppError::UnsupportedHarness),
    };

    let resume_command = resume_ref
        .and_then(|r| switchboard_harness::interactive_resume_command(agent.harness, &r))
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
    /// The user's side of a send, sourced from the journal. A fan-out renders
    /// once (grouped by `send_id`); `agent_ids` are the group's recipients in
    /// first-seen order. `text` is the prompt (identical across the group);
    /// `at` is the earliest `at` in the group.
    UserMessage {
        send_id: SendId,
        agent_ids: Vec<AgentId>,
        text: String,
        at: chrono::DateTime<chrono::Utc>,
    },
    /// One agent's completed (or harness-failed) turn content, sourced from the
    /// harness session file. Harness user-role turns are dropped — the journal
    /// is the canonical record of the user's words.
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
    let mut user_messages: Vec<(SendId, Vec<AgentId>, String, chrono::DateTime<chrono::Utc>)> =
        Vec::new();
    // The journal's `turn_id` is the dispatcher's, distinct from the harness
    // session file's own turn ids, so they can't be joined directly. Instead we
    // correlate each agent's harness turns to its sends by ORDER: the Nth
    // *completed* agent turn answers the Nth completed send dispatched to it
    // (the dispatcher runs an agent's turns FIFO and journals in that order).
    // `agent_sends` is each agent's sends in order; `non_completed` is the
    // (agent, send) pairs that failed/cancelled — those leave no clean harness
    // turn (only an Outcome marker, which already carries its own send_id), so
    // they're excluded from the order-zip below to keep it aligned.
    let mut agent_sends: HashMap<AgentId, Vec<SendId>> = HashMap::new();
    let mut non_completed: HashMap<AgentId, std::collections::HashSet<SendId>> = HashMap::new();
    for record in journal {
        match record {
            switchboard_core::JournalRecord::Send {
                send_id,
                agent_id,
                prompt,
                at,
                ..
            } => {
                agent_sends.entry(agent_id).or_default().push(send_id);
                if let Some(&i) = index_of.get(&send_id) {
                    let entry = &mut user_messages[i];
                    if !entry.1.contains(&agent_id) {
                        entry.1.push(agent_id);
                    }
                    if at < entry.3 {
                        entry.3 = at;
                    }
                } else {
                    // The prompt is shared across a fan-out's recipients (M4.2),
                    // so taking the first record's prompt is correct for M4; M6
                    // templated per-recipient prompts will need this revisited.
                    index_of.insert(send_id, user_messages.len());
                    user_messages.push((send_id, vec![agent_id], prompt, at));
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
                non_completed.entry(agent_id).or_default().insert(send_id);
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
    for (send_id, agent_ids, text, at) in user_messages {
        items.push(ConversationItem::UserMessage {
            send_id,
            agent_ids,
            text,
            at,
        });
    }

    // Per-agent FIFO of *completed* sends (non-completed excluded — they leave
    // no clean harness turn), drained in order to stamp each harness turn with
    // the send it answered.
    let mut sends_by_agent: HashMap<AgentId, Vec<SendId>> = HashMap::new();
    for (agent_id, sends) in agent_sends {
        let excluded = non_completed.get(&agent_id);
        let kept: Vec<SendId> = sends
            .into_iter()
            .filter(|s| !excluded.is_some_and(|e| e.contains(s)))
            .collect();
        sends_by_agent.insert(agent_id, kept);
    }

    // Agent turns ← each agent's harness transcript, keeping only `Turn::Agent`
    // (drop `Turn::User`). Warnings and any load error are agent-scoped: attach
    // them to this transcript's `AgentConversationMeta` so the unified view can
    // attribute them (one bad agent never blanks the project).
    let mut agents: Vec<AgentConversationMeta> = Vec::new();
    for (agent_id, transcript, load_error) in agent_transcripts {
        // This agent's Agent-role turns, in order.
        type HarnessTurn = (
            switchboard_harness::TurnId,
            AgentId,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
            switchboard_harness::TurnStatus,
            Vec<switchboard_harness::TurnItem>,
            Option<switchboard_harness::TurnUsage>,
        );
        let agent_turns: Vec<HarnessTurn> = transcript
            .turns
            .into_iter()
            .filter_map(|t| match t {
                switchboard_harness::Turn::Agent {
                    turn_id,
                    agent_id,
                    started_at,
                    ended_at,
                    status,
                    items,
                    usage,
                } => Some((
                    turn_id, agent_id, started_at, ended_at, status, items, usage,
                )),
                _ => None,
            })
            .collect();
        // Correlate turns to completed sends by order, accounting for an
        // asymmetry between the two lists' unpaired extras:
        //   - Extra *turns* sit at the FRONT — pre-journaling session history,
        //     older than the first journaled send. End-align turns so the most
        //     recent ones pair with sends; the leading unpaired turns get no
        //     send_id and render un-grouped.
        //   - Extra *sends* sit at the BACK — an in-flight send whose turn has
        //     started but not yet produced harness content. Front-align sends
        //     (`send_offset = 0`) so the trailing in-flight send is dropped
        //     rather than mislabeling a completed turn.
        let completed = sends_by_agent.get(&agent_id).map_or(&[][..], Vec::as_slice);
        let pairs = agent_turns.len().min(completed.len());
        let turn_offset = agent_turns.len() - pairs;
        for (j, (turn_id, a_id, started_at, ended_at, status, t_items, usage)) in
            agent_turns.into_iter().enumerate()
        {
            let send_id = if j >= turn_offset {
                completed.get(j - turn_offset).copied()
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
            });
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
/// - **No Claude equivalent.** Claude Code on macOS stores OAuth tokens in
///   the keychain; there's no on-disk file we can reliably probe. The plan
///   explicitly defers robust Claude auth detection to v2.
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
        let agent = create_agent_impl(state, "assistant", HarnessKind::ClaudeCode).unwrap();
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
        send_message_impl(state, agent_id, prompt, Uuid::now_v7()).await
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
        let agent = create_agent_impl(&state, "assistant", HarnessKind::ClaudeCode).unwrap();

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
    #[ignore = "requires agy authed via Antigravity desktop app — run with: make test-live"]
    fn live_antigravity_check_auth_finds_real_keychain_entry() {
        check_antigravity_auth_impl().expect(
            "Antigravity Keychain entry must live at service=gemini account=antigravity on a \
             logged-in machine; if this fails, Antigravity may have changed its keychain \
             naming or removed Keychain-based auth entirely",
        );
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
        create_agent_impl(&state, "a-agent", switchboard_core::HarnessKind::ClaudeCode).unwrap();
        set_active_project_impl(&state, proj_b.id).unwrap();
        create_agent_impl(&state, "b-agent", switchboard_core::HarnessKind::ClaudeCode).unwrap();

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
        let claude_agent = create_agent_impl(&state, "c1", HarnessKind::ClaudeCode).unwrap();
        let codex_agent = create_agent_impl(&state, "x1", HarnessKind::Codex).unwrap();
        let gemini_agent = create_agent_impl(&state, "g1", HarnessKind::Gemini).unwrap();
        let antigravity_agent = create_agent_impl(&state, "a1", HarnessKind::Antigravity).unwrap();

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
        let agent = create_agent_impl(&state, "a", HarnessKind::ClaudeCode).unwrap();
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
        let agent = create_agent_impl(&state, "a", HarnessKind::ClaudeCode).unwrap();
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
        let agent_default = create_agent_impl(&state, "a", HarnessKind::ClaudeCode).unwrap();
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
        let agent = create_agent_impl(&state, "a", HarnessKind::Codex).unwrap();
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
        )
        .unwrap();
        set_active_project_impl(&state, proj_b.id).unwrap();
        let agent_b = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
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
        )
        .unwrap();
        assert_eq!(record.session_id, Some(session_id));
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
        )
        .unwrap_err();
        assert!(matches!(err, AppError::InvalidUuid { .. }));
    }

    #[tokio::test]
    async fn attach_codex_succeeds_and_writes_sidecar() {
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let session_id = Uuid::now_v7();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        stage_codex_session_file(tmp_home.path(), date, &session_id.to_string());

        let record = attach_agent_impl(
            &state,
            "attached-codex",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
        )
        .unwrap();
        assert_eq!(
            record.session_id, None,
            "Codex AgentRecord.session_id stays None"
        );
        assert!(
            lock(&state.needs_session_meta).contains(&record.id),
            "Codex attach must populate needs_session_meta so first dispatch forces SessionMeta"
        );

        // Sidecar record exists with the discovered date.
        let sidecar = switchboard_harness::codex::sidecar::sidecar_path(
            tmp_workdir.path(),
            proj.id,
            record.id,
        );
        let latest = switchboard_harness::codex::sidecar::read_latest(&sidecar)
            .unwrap()
            .unwrap();
        assert_eq!(latest.session_id, session_id.to_string());
        assert_eq!(latest.session_partition_date, date);
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
        )
        .unwrap();

        set_active_project_impl(&state, beta.id).unwrap();
        let err = attach_agent_impl(
            &state,
            "attached",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
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
        )
        .unwrap();
        let err = attach_agent_impl(
            &state,
            "second",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
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
        )
        .unwrap();

        set_active_project_impl(&state, beta.id).unwrap();
        let err = attach_agent_impl(
            &state,
            "b",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
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
        create_agent_impl(&state, "taken", HarnessKind::ClaudeCode).unwrap();
        let session_id = Uuid::now_v7();
        stage_claude_session_file(tmp_home.path(), tmp_workdir.path(), &session_id);

        let err = attach_agent_impl(
            &state,
            "taken",
            HarnessKind::ClaudeCode,
            &session_id.to_string(),
            tmp_home.path(),
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

    /// The sidecar-first commit ordering's load-bearing invariant:
    /// when the registry append fails after the sidecar write succeeds,
    /// the result is an *orphan sidecar with no `AgentRecord`* — invisible
    /// to dispatch and to the collision scan — not an orphan `AgentRecord`
    /// pointing at the wrong session (the failure mode the ordering
    /// inverts). Without this test, a future regression that re-ordered
    /// the ops would only surface via the docstring contradicting the
    /// code.
    ///
    /// Trigger: name collision. The second attach uses a *different*
    /// `session_id` so the collision scan passes; the sidecar write
    /// (against a freshly-minted `AgentId`) succeeds; then
    /// `register_attached_codex_agent_with_id` fails on the duplicate
    /// name. Asserts: registry unchanged + an orphan sidecar exists on
    /// disk referencing the second `session_id`.
    #[tokio::test]
    async fn attach_codex_register_failure_after_sidecar_write_leaves_orphan_not_partial() {
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let canonical_workdir = tmp_workdir.path().canonicalize().unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();

        let first_session = Uuid::now_v7();
        stage_codex_session_file(tmp_home.path(), date, &first_session.to_string());
        attach_agent_impl(
            &state,
            "taken",
            HarnessKind::Codex,
            &first_session.to_string(),
            tmp_home.path(),
        )
        .unwrap();

        // Second attach: distinct session_id (collision scan passes) +
        // colliding name (register fails after sidecar write).
        let second_session = Uuid::now_v7();
        stage_codex_session_file(tmp_home.path(), date, &second_session.to_string());
        let err = attach_agent_impl(
            &state,
            "taken",
            HarnessKind::Codex,
            &second_session.to_string(),
            tmp_home.path(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::Core(switchboard_core::CoreError::DuplicateAgentName { .. })
        ));

        // Registry has exactly one "taken" — name uniqueness held.
        let agents = list_agents_impl(&state, None).unwrap();
        assert_eq!(
            agents.iter().filter(|a| a.name == "taken").count(),
            1,
            "registry must not double-add on name collision"
        );

        // Sidecar dir has TWO files: the legitimate first attach's sidecar
        // (pointing at first_session) and the orphan from the failed second
        // attach (pointing at second_session). The orphan is invisible to
        // dispatch (no AgentRecord with that id) and invisible to the
        // collision scan (which walks AgentRecords → looks up *their*
        // sidecars). Asserting both files exist pins the invariant.
        let sessions_dir = canonical_workdir
            .join(".switchboard")
            .join("projects")
            .join(proj.id.to_string())
            .join("sessions");
        let mut found_first = false;
        let mut found_orphan_for_second = false;
        for entry in std::fs::read_dir(&sessions_dir).unwrap().flatten() {
            let content = std::fs::read_to_string(entry.path()).unwrap();
            if content.contains(&first_session.to_string()) {
                found_first = true;
            }
            if content.contains(&second_session.to_string()) {
                found_orphan_for_second = true;
            }
        }
        assert!(found_first, "first attach's sidecar must remain on disk");
        assert!(
            found_orphan_for_second,
            "second attach's sidecar must remain as orphan after register failed (sidecar-first invariant)"
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
        )
        .unwrap();
        let err = attach_agent_impl(
            &state,
            "second",
            HarnessKind::Codex,
            &session_id.to_string(),
            tmp_home.path(),
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

    /// Corruption in a Switchboard-owned sidecar must surface as
    /// `AttachBlockedByCorruption`, not be silently skipped — otherwise the
    /// collision scan could miss a real binding and let a duplicate attach
    /// through. The error wrapping is intentional so the user sees that the
    /// failure is about an unrelated agent's state, not the session they
    /// were trying to attach.
    #[tokio::test]
    async fn attach_codex_fails_loud_on_corrupt_sidecar_in_other_project() {
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;

        // Plant a Codex agent in alpha with a corrupt sidecar. Use the
        // canonical bound-directory path (`Directory::at` canonicalizes;
        // macOS resolves `/var` → `/private/var`, so the sidecar collision
        // scan inside attach_agent_impl reads from the canonical path —
        // we must too, for the path equality assertion below.
        let canonical_workdir = tmp_workdir.path().canonicalize().unwrap();
        let other_agent = proj_handle(&state, proj.id)
            .register_attached_codex_agent_with_id("ghost", Uuid::now_v7())
            .unwrap();
        let bad_sidecar = switchboard_harness::codex::sidecar::sidecar_path(
            &canonical_workdir,
            proj.id,
            other_agent.id,
        );
        std::fs::create_dir_all(bad_sidecar.parent().unwrap()).unwrap();
        std::fs::write(&bad_sidecar, b"this is not json\n").unwrap();

        // Attempt an unrelated attach. Stage a real Codex session file so
        // the discovery phase passes — the failure must come from the
        // collision-scan corruption check, not the discovery miss.
        let new_session = Uuid::now_v7();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        stage_codex_session_file(tmp_home.path(), date, &new_session.to_string());

        let err = attach_agent_impl(
            &state,
            "newcomer",
            HarnessKind::Codex,
            &new_session.to_string(),
            tmp_home.path(),
        )
        .unwrap_err();
        match err {
            AppError::AttachBlockedByCorruption { path, .. } => {
                assert_eq!(path, bad_sidecar);
            }
            other => panic!("expected AttachBlockedByCorruption, got {other:?}"),
        }
    }

    /// Look up a loaded `Project` handle by id from `state.projects`.
    /// Test-only convenience for staging cross-project corruption without
    /// re-opening the project via the public command surface.
    fn proj_handle(state: &AppState, id: ProjectId) -> Project {
        lock(&state.projects).get(&id).cloned().unwrap()
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
        )
        .unwrap();

        let result = load_transcript_impl(&state, record.id, tmp_home.path()).unwrap();
        assert!(result.turns.is_empty());
        assert!(result.warnings.is_empty());
        // No metadata sidecar staged → both rate-limit fields stay None.
        assert!(result.last_rate_limit.is_none());
        assert!(result.last_rate_limit_as_of.is_none());
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
        // End-to-end: a Codex sidecar session id with shell metacharacters is
        // single-quoted in the rendered resume command.
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let agent = create_agent_impl(&state, "codex_evil", HarnessKind::Codex).unwrap();
        let canonical_workdir = tmp_workdir.path().canonicalize().unwrap();
        let sidecar = switchboard_harness::codex::sidecar::sidecar_path(
            &canonical_workdir,
            proj.id,
            agent.id,
        );
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        switchboard_harness::codex::sidecar::append_record(
            &sidecar,
            &switchboard_harness::codex::sidecar::SessionLinkRecord {
                session_id: "a;rm -rf".to_owned(),
                session_partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 25).unwrap(),
                started_at: chrono::Utc::now(),
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
        let record = create_agent_impl(&state, "codex_one", HarnessKind::Codex).unwrap();
        let info = agent_session_info_impl(&state, record.id, tmp_home.path()).unwrap();
        assert_eq!(info, AgentSessionInfo::default());
    }

    #[tokio::test]
    async fn agent_session_info_for_codex_resolves_resume_id_from_sidecar() {
        // Codex carries `session_id: None` on the record; the resume id lives in
        // the sidecar (written post-dispatch). Resume is offered from it even
        // when the local session file isn't present.
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let agent = create_agent_impl(&state, "codex_two", HarnessKind::Codex).unwrap();
        let canonical_workdir = tmp_workdir.path().canonicalize().unwrap();
        let sidecar = switchboard_harness::codex::sidecar::sidecar_path(
            &canonical_workdir,
            proj.id,
            agent.id,
        );
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        switchboard_harness::codex::sidecar::append_record(
            &sidecar,
            &switchboard_harness::codex::sidecar::SessionLinkRecord {
                session_id: "sess-xyz".to_owned(),
                session_partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 25).unwrap(),
                started_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        let info = agent_session_info_impl(&state, agent.id, tmp_home.path()).unwrap();
        assert!(
            info.session_file.is_none(),
            "no transcript file staged on disk"
        );
        let cmd = info.resume_command.expect("resume offered from sidecar id");
        assert!(cmd.contains("codex resume sess-xyz"), "got: {cmd}");
    }

    #[tokio::test]
    async fn load_transcript_for_codex_agent_without_sidecar_returns_meta_only_empty() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        // Create a Codex agent the normal way (no sidecar — no first dispatch).
        let record = create_agent_impl(&state, "codex_one", HarnessKind::Codex).unwrap();

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
    async fn load_transcript_for_codex_agent_with_corrupt_sidecar_returns_typed_error() {
        // Sidecars are Switchboard-owned JSONL: corruption must fail loud,
        // not silently degrade to "agent has no history." Parallel to the
        // attach-time AttachBlockedByCorruption surfacing in
        // check_codex_session_id_unique.
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let agent = create_agent_impl(&state, "codex_corrupt", HarnessKind::Codex).unwrap();

        // Plant a corrupt sidecar under the canonical bound directory
        // (macOS canonicalizes `/var` → `/private/var`; sidecar_path uses
        // the directory we pass in verbatim, so we must use the canonical
        // form to match what load_transcript_impl reads).
        let canonical_workdir = tmp_workdir.path().canonicalize().unwrap();
        let bad_sidecar = switchboard_harness::codex::sidecar::sidecar_path(
            &canonical_workdir,
            proj.id,
            agent.id,
        );
        std::fs::create_dir_all(bad_sidecar.parent().unwrap()).unwrap();
        std::fs::write(&bad_sidecar, b"this is not json\n").unwrap();

        let err = load_transcript_impl(&state, agent.id, tmp_home.path()).unwrap_err();
        match err {
            AppError::HydrationBlockedByCorruption { path, .. } => {
                assert_eq!(path, bad_sidecar);
            }
            other => panic!("expected HydrationBlockedByCorruption, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attach_antigravity_succeeds_and_writes_sidecar() {
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let conversation_id = Uuid::now_v7();
        stage_antigravity_conversation(tmp_home.path(), conversation_id, true);

        let record = attach_agent_impl(
            &state,
            "attached-agy",
            HarnessKind::Antigravity,
            &conversation_id.to_string(),
            tmp_home.path(),
        )
        .unwrap();
        assert_eq!(
            record.session_id, None,
            "Antigravity AgentRecord.session_id stays None (sidecar-carried)"
        );
        // Unlike Codex, Antigravity emits SessionMeta every turn, so attach
        // does not force it via needs_session_meta.
        assert!(
            !lock(&state.needs_session_meta).contains(&record.id),
            "Antigravity attach must not populate needs_session_meta"
        );

        let sidecar = switchboard_harness::antigravity::sidecar::sidecar_path(
            tmp_workdir.path(),
            proj.id,
            record.id,
        );
        let latest = switchboard_harness::antigravity::sidecar::read_latest(&sidecar)
            .unwrap()
            .unwrap();
        assert_eq!(latest.conversation_id, conversation_id);
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
        )
        .unwrap();
        let err = attach_agent_impl(
            &state,
            "agy-two",
            HarnessKind::Antigravity,
            &conversation_id.to_string(),
            tmp_home.path(),
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
    async fn attach_antigravity_fails_loud_on_corrupt_sidecar() {
        // The uniqueness scan must fail loud on a corrupt sidecar rather than
        // skip it — a skipped sidecar could let a duplicate conversation attach
        // through, putting two agents on one server-side conversation. Twin of
        // attach_codex_fails_loud_on_corrupt_sidecar_in_other_project.
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;

        // Plant an existing Antigravity agent with a corrupt sidecar. The
        // collision scan inside attach_agent_impl reads from the canonical
        // bound-directory path (Directory::at canonicalizes; macOS resolves
        // /var → /private/var), so plant — and assert — at that path.
        let canonical_workdir = tmp_workdir.path().canonicalize().unwrap();
        let ghost = proj_handle(&state, proj.id)
            .register_attached_antigravity_agent_with_id("ghost", Uuid::now_v7())
            .unwrap();
        let bad_sidecar = switchboard_harness::antigravity::sidecar::sidecar_path(
            &canonical_workdir,
            proj.id,
            ghost.id,
        );
        std::fs::create_dir_all(bad_sidecar.parent().unwrap()).unwrap();
        std::fs::write(&bad_sidecar, b"this is not json\n").unwrap();

        // Stage a valid brain dir for the newcomer so the brain-dir validation
        // passes and the failure comes from the collision-scan corruption
        // check, not the existence check.
        let new_conversation = Uuid::now_v7();
        stage_antigravity_conversation(tmp_home.path(), new_conversation, true);

        let err = attach_agent_impl(
            &state,
            "newcomer",
            HarnessKind::Antigravity,
            &new_conversation.to_string(),
            tmp_home.path(),
        )
        .unwrap_err();
        match err {
            AppError::AttachBlockedByCorruption { path, .. } => assert_eq!(path, bad_sidecar),
            other => panic!("expected AttachBlockedByCorruption, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_transcript_for_antigravity_agent_without_sidecar_returns_meta_only_empty() {
        let (_tmp_workdir, tmp_home, state, _proj) = fresh_state_with_active_project("alpha").await;
        // Antigravity agent never dispatched → no sidecar → empty turns, but
        // loader-derived registry meta still surfaces (mirrors the Codex arm)
        // so the sidebar populates the moment the agent is selected.
        let record = create_agent_impl(&state, "agy_one", HarnessKind::Antigravity).unwrap();
        let result = load_transcript_impl(&state, record.id, tmp_home.path()).unwrap();
        assert!(result.turns.is_empty());
        assert!(result.meta.is_some());
    }

    #[tokio::test]
    async fn load_transcript_for_antigravity_agent_with_corrupt_sidecar_returns_typed_error() {
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let agent = create_agent_impl(&state, "agy_corrupt", HarnessKind::Antigravity).unwrap();
        let canonical_workdir = tmp_workdir.path().canonicalize().unwrap();
        let bad_sidecar = switchboard_harness::antigravity::sidecar::sidecar_path(
            &canonical_workdir,
            proj.id,
            agent.id,
        );
        std::fs::create_dir_all(bad_sidecar.parent().unwrap()).unwrap();
        std::fs::write(&bad_sidecar, b"this is not json\n").unwrap();

        let err = load_transcript_impl(&state, agent.id, tmp_home.path()).unwrap_err();
        match err {
            AppError::HydrationBlockedByCorruption { path, .. } => assert_eq!(path, bad_sidecar),
            other => panic!("expected HydrationBlockedByCorruption, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_transcript_for_antigravity_agent_hydrates_prior_turns() {
        let (tmp_workdir, tmp_home, state, proj) = fresh_state_with_active_project("alpha").await;
        let agent = create_agent_impl(&state, "agy_hydrate", HarnessKind::Antigravity).unwrap();
        let canonical_workdir = tmp_workdir.path().canonicalize().unwrap();

        // Sidecar: the server-assigned conversation UUID captured at dispatch.
        let conversation_id = Uuid::new_v4();
        let sidecar = switchboard_harness::antigravity::sidecar::sidecar_path(
            &canonical_workdir,
            proj.id,
            agent.id,
        );
        switchboard_harness::antigravity::sidecar::append_record(
            &sidecar,
            &switchboard_harness::antigravity::sidecar::SessionLinkRecord {
                conversation_id,
                captured_at: chrono::Utc::now(),
            },
        )
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
        )
        .unwrap();
        assert_eq!(record.session_id, Some(session_id));
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
        )
        .unwrap();
        assert_eq!(record.session_id, Some(id_b));
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
        )
        .unwrap();

        let err = attach_agent_impl(
            &state,
            "second",
            HarnessKind::Gemini,
            &session_id.to_string(),
            tmp_home.path(),
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
        let agent = create_agent_impl(&state, "assistant", HarnessKind::ClaudeCode).unwrap();

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
        let agent = create_agent_impl(&state, "assistant", HarnessKind::ClaudeCode).unwrap();

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
        let agent_b = create_agent_impl(&state, "assistant-2", HarnessKind::ClaudeCode).unwrap();

        // One Send fanned out to both: same `send_id`, one call per recipient.
        let send_id = Uuid::now_v7();
        send_message_impl(&state, agent_a.id, "fan-out", send_id)
            .await
            .unwrap();
        send_message_impl(&state, agent_b.id, "fan-out", send_id)
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
            create_agent_impl(&agent_state, "racer", HarnessKind::ClaudeCode)
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

    #[test]
    fn merge_drops_harness_user_role_turns() {
        // A harness user-role turn never surfaces as a UserMessage — those come
        // only from the journal.
        let agent = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let transcript = transcript_of(vec![user_turn(turn_id, agent, "from harness", 0)]);

        let merged = merge_project_conversation(Vec::new(), vec![(agent, transcript, None)]);

        assert!(
            merged.items.is_empty(),
            "harness user-role turns produce no rendered items"
        );
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
}
