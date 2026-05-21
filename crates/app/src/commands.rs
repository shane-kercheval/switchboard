//! Free-function implementations behind each Tauri command. The
//! `#[tauri::command]` wrappers in `lib.rs` are thin shims that adapt these
//! to Tauri's `State<'_, AppState>` / `String` conventions; the free
//! functions are what the unit tests target.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use switchboard_core::{
    AgentId, AgentRecord, Directory, HarnessKind, Project, ProjectId, ProjectSummary,
};
use switchboard_dispatcher::{DispatchHandle, EventEmitter};
use switchboard_harness::HarnessAdapter;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::{AppState, lock};

/// Returned by `init_directory_impl` — gives the caller everything it needs
/// to render the directory header (path) and project list in one round trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryInfo {
    pub path: String,
    pub has_switchboard: bool,
    pub projects: Vec<ProjectSummary>,
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

/// Idempotent for the same path: creates `.switchboard/` if missing and
/// binds the directory in `AppState`. Re-binding to a *different* canonical
/// path clears the loaded-project cache and the active project, so the
/// frontend can't subsequently dispatch to an agent from the previous
/// directory (which would resolve to a now-stale `project.directory`).
///
/// In-flight dispatches on agents from the prior directory keep running
/// (their `AgentIdleGuard` and event channels are dispatcher-owned and
/// agent-scoped) — graceful cleanup of those is future work.
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
    let info = DirectoryInfo {
        path: directory.path.to_string_lossy().into_owned(),
        has_switchboard: directory.has_switchboard(),
        projects,
    };

    // Atomic-ish: if the new path differs from the current one, clear the
    // project caches before swapping in the new directory binding so the
    // loaded-projects/active-project state never references a directory we
    // are no longer bound to.
    {
        let mut current = lock(&state.directory);
        let rebinding = matches!(current.as_ref(), Some(d) if d.path != directory.path);
        if rebinding {
            lock(&state.projects).clear();
            *lock(&state.active_project_id) = None;
            // A pending one-shot from a prior directory's attach must not
            // leak into the newly-bound directory's first dispatch — the
            // agent_id wouldn't even resolve.
            lock(&state.needs_session_meta).clear();
        }
        *current = Some(directory);
    }
    Ok(info)
}

pub fn list_projects_impl(state: &AppState) -> Result<Vec<ProjectSummary>, AppError> {
    let directory = bound_directory(state)?;
    Ok(directory.list_projects()?)
}

pub fn create_project_impl(state: &AppState, name: &str) -> Result<ProjectSummary, AppError> {
    // Serialize the uniqueness check + JSONL append against concurrent
    // `create_project` / `register_agent` / `init_directory` calls. Without
    // this, two concurrent IPC calls could both pass the canonical-name
    // uniqueness check (which reads disk) and then both append colliding
    // records (which write disk).
    let _write = lock(&state.registry_write);
    let directory = bound_directory(state)?;
    let project = directory.create_project(name)?;
    let summary = ProjectSummary {
        id: project.id,
        name: project.config.name.clone(),
        created_at: project.config.created_at,
    };
    lock(&state.projects).insert(project.id, project);
    Ok(summary)
}

pub fn open_project_impl(
    state: &AppState,
    project_id: ProjectId,
) -> Result<ProjectSummary, AppError> {
    if let Some(loaded) = lock(&state.projects).get(&project_id) {
        return Ok(ProjectSummary {
            id: loaded.id,
            name: loaded.config.name.clone(),
            created_at: loaded.config.created_at,
        });
    }
    let directory = bound_directory(state)?;
    let project = directory.open_project(project_id)?;
    let summary = ProjectSummary {
        id: project.id,
        name: project.config.name.clone(),
        created_at: project.config.created_at,
    };
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
    Ok(project.register_agent(name, harness)?)
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
/// 4. Session-id collision scan across **all projects in the bound
///    directory** (loaded or not — `enumerate_all_projects` walks every
///    project on disk via `directory.list_projects()`). Claude and Gemini
///    scan `AgentRecord.session_id` (caller-controlled UUID); Codex and
///    Antigravity scan every project's `sessions/<agent_id>.*.jsonl` sidecar
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
    let directory = bound_directory(state)?;

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
            check_claude_session_id_unique(state, &session_uuid)?;
            project.register_attached_claude_agent(name, session_uuid)?
        }
        HarnessKind::Codex => {
            let (_path, session_partition_date) =
                switchboard_harness::find_codex_session_file_for_attach(
                    home_dir,
                    existing_session_id,
                )
                .map_err(map_codex_attach_lookup_error(harness, home_dir))?;
            check_codex_session_id_unique(state, existing_session_id, &directory.path)?;
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
            check_gemini_session_id_unique(state, &session_uuid)?;
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
            check_antigravity_session_id_unique(state, session_uuid, &directory.path)?;
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

/// Enumerate every project on disk under the bound directory, preferring
/// the in-memory `state.projects` entry for already-loaded projects (avoids
/// a redundant disk read of the same `config.yaml`). Unloaded projects are
/// constructed via `directory.open_project(id)`, which is a pure read —
/// it does **not** mutate `state.projects` or register any listeners.
///
/// Used by the attach-flow collision scans. A v1 directory typically holds
/// a handful of projects so the disk cost is small; if a directory ever
/// grows to dozens of projects, attach latency may become visible — flag
/// as a future optimization (cache `Project` handles + invalidate on
/// rebind), not a current concern.
fn enumerate_all_projects(state: &AppState) -> Result<Vec<Project>, AppError> {
    let directory = bound_directory(state)?;
    let loaded = lock(&state.projects);
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

/// Cross-project Claude session-id collision check. Walks every project on
/// disk in the bound directory — not just `state.projects` — because an
/// unloaded project's `AgentRecord` could still be opened later and
/// dispatched concurrently, which is the same-session-parallel-invocation
/// hazard the invariant is defending against. Held under `registry_write`
/// so it's atomic with the subsequent register.
fn check_claude_session_id_unique(state: &AppState, candidate: &Uuid) -> Result<(), AppError> {
    for project in enumerate_all_projects(state)? {
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
/// Cross-project Gemini session-id collision check. Gemini agents carry
/// `AgentRecord.session_id = Some(uuid)` (Claude shape). Walks every
/// project on disk in the bound directory and rejects if any agent
/// already attached to the same UUID.
fn check_gemini_session_id_unique(state: &AppState, candidate: &Uuid) -> Result<(), AppError> {
    for project in enumerate_all_projects(state)? {
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

fn check_codex_session_id_unique(
    state: &AppState,
    candidate: &str,
    directory: &Path,
) -> Result<(), AppError> {
    for project in enumerate_all_projects(state)? {
        for agent in project.list_agents()? {
            if agent.harness != HarnessKind::Codex {
                continue;
            }
            let sidecar =
                switchboard_harness::codex::sidecar::sidecar_path(directory, project.id, agent.id);
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
/// agent in the directory. Mirrors the Codex check (the conversation id lives
/// in the per-agent sidecar, not the `AgentRecord`, so the scan reads
/// sidecars). The same-session-parallel-invocation risk is identical: two
/// agents resuming one `--conversation <uuid>` would interleave server-side.
/// Corrupt sidecars fail loud rather than being skipped — a skipped sidecar
/// could let a duplicate attach through and violate the uniqueness contract.
fn check_antigravity_session_id_unique(
    state: &AppState,
    candidate: Uuid,
    directory: &Path,
) -> Result<(), AppError> {
    for project in enumerate_all_projects(state)? {
        for agent in project.list_agents()? {
            if agent.harness != HarnessKind::Antigravity {
                continue;
            }
            let sidecar = switchboard_harness::antigravity::sidecar::sidecar_path(
                directory, project.id, agent.id,
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
    Ok(project.list_agents()?)
}

/// Resolves the agent (across all loaded projects) and dispatches the turn
/// through the dispatcher. Returns the `DispatchHandle` (`turn_id` + drain
/// `JoinHandle`) as soon as the adapter's `dispatch()` returns and
/// `TurnStart` has been emitted — the drain task continues in the
/// background.
///
/// The Tauri command shim discards the `JoinHandle`, returning just the
/// `TurnId` to the frontend; tests `.await` the handle for deterministic
/// drain completion (instead of polling agent status).
pub async fn send_message_impl(
    state: &AppState,
    agent_id: AgentId,
    prompt: &str,
) -> Result<DispatchHandle, AppError> {
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
    let adapter: &dyn HarnessAdapter = match agent.harness {
        HarnessKind::ClaudeCode => state.claude_adapter.as_ref(),
        HarnessKind::Codex => state.codex_adapter.as_ref(),
        HarnessKind::Gemini => state.gemini_adapter.as_ref(),
        HarnessKind::Antigravity => state.antigravity_adapter.as_ref(),
        _ => return Err(AppError::UnsupportedHarness),
    };
    // Read (don't drain) the attach-flow flag. The per-dispatch emitter
    // decorator clears the flag if-and-only-if a `session_meta` event is
    // observed on the wire. Pre-stream errors and mid-stream failures both
    // leave the flag intact, so the next retry still forces SessionMeta.
    // See `AppState::needs_session_meta` and `crate::emitter` for the full
    // contract; the four-dispatch test below pins the invariant.
    let is_first_dispatch_after_attach = lock(&state.needs_session_meta).contains(&agent_id);
    let options = switchboard_harness::DispatchOptions {
        is_first_dispatch_after_attach,
    };
    let observing_emitter: Arc<dyn EventEmitter> =
        Arc::new(crate::emitter::SessionMetaObservingEmitter::new(
            Arc::clone(&state.emitter),
            Arc::clone(&state.needs_session_meta),
            agent_id,
        ));
    Ok(state
        .dispatcher
        .send_message(
            &agent,
            &project.directory,
            prompt,
            adapter,
            observing_emitter,
            options,
        )
        .await?)
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
    let directory = bound_directory(state)?;
    match agent.harness {
        HarnessKind::ClaudeCode => {
            let Some(session_id) = agent.session_id else {
                return Ok(switchboard_harness::LoadedTranscript::default());
            };
            Ok(switchboard_harness::load_claude_transcript(
                home_dir,
                &directory.path,
                session_id,
                agent.id,
            )?)
        }
        HarnessKind::Codex => {
            let sidecar_path = switchboard_harness::codex::sidecar::sidecar_path(
                &directory.path,
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
                &directory.path,
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
                &directory.path,
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
                &directory.path,
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
                &directory.path,
                latest.map(|record| record.conversation_id),
                agent.id,
            )?)
        }
        _ => Err(AppError::UnsupportedHarness),
    }
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
/// `docs/research/antigravity-cli-observed.md` line 99.
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

fn bound_directory(state: &AppState) -> Result<Directory, AppError> {
    lock(&state.directory)
        .as_ref()
        .cloned()
        .ok_or(AppError::NoDirectory)
}

fn lookup_agent(state: &AppState, agent_id: AgentId) -> Result<(Project, AgentRecord), AppError> {
    // Clone the project handles out so we don't hold the projects lock
    // while doing disk I/O via `list_agents`.
    let candidates: Vec<Project> = lock(&state.projects).values().cloned().collect();
    for project in candidates {
        let agents = project.list_agents()?;
        if let Some(agent) = agents.into_iter().find(|a| a.id == agent_id) {
            return Ok((project, agent));
        }
    }
    Err(AppError::AgentNotFound(agent_id))
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
    use switchboard_dispatcher::RecordingEmitter;
    use switchboard_harness::{ClaudeCodeAdapter, HarnessAdapter, MockHarnessAdapter};
    use tempfile::TempDir;

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
    async fn init_directory_to_different_path_clears_projects_and_unbinds_old_agents() {
        // First directory: create a project + agent.
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
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "assistant", HarnessKind::ClaudeCode).unwrap();

        // Simulate a stale attach-flow one-shot from the old directory.
        lock(&state.needs_session_meta).insert(agent.id);

        // Rebind to a different directory.
        let info_b = init_directory_impl(&state, tmp_b.path().to_str().unwrap())
            .await
            .unwrap();

        // Loaded-project state was cleared, active project unset, and the
        // attach-flow flag drained — a stale agent_id from a previous
        // directory's attach must not leak into the new binding.
        assert_eq!(info_b.projects.len(), 0);
        assert!(lock(&state.projects).is_empty());
        assert!(lock(&state.active_project_id).is_none());
        assert!(lock(&state.needs_session_meta).is_empty());

        // The actual user-visible bug guard: sending to the old agent ID
        // now returns AgentNotFound (not a silent dispatch against the old
        // project's cwd).
        let err = send_message_impl(&state, agent.id, "should fail")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn init_directory_is_idempotent() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        // Second call must succeed and preserve any created projects.
        create_project_impl(&state, "alpha").unwrap();
        let info = init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "alpha");
    }

    #[test]
    fn list_projects_without_init_errors() {
        let (_tmp, state, _) = fresh_state_with_mock();
        let err = list_projects_impl(&state).unwrap_err();
        assert!(matches!(err, AppError::NoDirectory));
    }

    #[tokio::test]
    async fn create_open_set_active_round_trip() {
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let summary = create_project_impl(&state, "alpha").unwrap();
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
        let project = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, project.id).unwrap();
        let agent = create_agent_impl(
            &state,
            "assistant",
            switchboard_core::HarnessKind::ClaudeCode,
        )
        .unwrap();

        let DispatchHandle { turn_id, join } =
            send_message_impl(&state, agent.id, "hello").await.unwrap();
        join.await.unwrap();

        let events = emitter.snapshot();
        assert!(!events.is_empty(), "expected events to be emitted");
        let channel = format!("agent:{}", agent.id);
        for (name, _) in &events {
            assert_eq!(name, &channel);
        }
        assert_eq!(events[0].1["type"], "turn_start");
        assert_eq!(events[0].1["turn_id"], turn_id.to_string());
        assert_eq!(
            state.dispatcher.agent_status(agent.id),
            Some(switchboard_dispatcher::AgentStatus::Idle)
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
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let state_a = Arc::clone(&state);
        let state_b = Arc::clone(&state);
        // Run on real threads so the mutex contention is real (not
        // single-threaded cooperative scheduling). The work inside
        // create_project_impl is synchronous once it enters the locked
        // section.
        let a = tokio::task::spawn_blocking(move || create_project_impl(&state_a, "shared-name"));
        let b = tokio::task::spawn_blocking(move || create_project_impl(&state_b, "shared-name"));
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
        let project = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, project.id).unwrap();
        let err = send_message_impl(&state, Uuid::now_v7(), "hi")
            .await
            .unwrap_err();
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
    fn live_check_codex_auth_finds_real_auth_file() {
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
    fn live_check_antigravity_auth_finds_real_keychain_entry() {
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
    fn live_check_antigravity_binary_finds_real_agy_on_path() {
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
    fn live_check_gemini_auth_finds_real_settings_file() {
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
        let proj_a = create_project_impl(&state, "alpha").unwrap();
        let proj_b = create_project_impl(&state, "beta").unwrap();
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
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let claude_agent = create_agent_impl(&state, "c1", HarnessKind::ClaudeCode).unwrap();
        let codex_agent = create_agent_impl(&state, "x1", HarnessKind::Codex).unwrap();
        let gemini_agent = create_agent_impl(&state, "g1", HarnessKind::Gemini).unwrap();
        let antigravity_agent = create_agent_impl(&state, "a1", HarnessKind::Antigravity).unwrap();

        let claude_handle = send_message_impl(&state, claude_agent.id, "hi")
            .await
            .unwrap();
        claude_handle.join.await.unwrap();
        let codex_handle = send_message_impl(&state, codex_agent.id, "hi")
            .await
            .unwrap();
        codex_handle.join.await.unwrap();
        let gemini_handle = send_message_impl(&state, gemini_agent.id, "hi")
            .await
            .unwrap();
        gemini_handle.join.await.unwrap();
        let antigravity_handle = send_message_impl(&state, antigravity_agent.id, "hi")
            .await
            .unwrap();
        antigravity_handle.join.await.unwrap();

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
        let (tmp, state, _) = fresh_state_with_mock();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "a", HarnessKind::ClaudeCode).unwrap();
        lock(&state.needs_session_meta).insert(agent.id);

        let handle = send_message_impl(&state, agent.id, "hi").await.unwrap();
        handle.join.await.unwrap();

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
        // Pre-stream Err paths (binary missing, spawn failure) also leave
        // the flag set: read-don't-drain means there's nothing to "restore"
        // — the flag was never moved.
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
            emitter as Arc<dyn EventEmitter>,
        );
        let tmp = TempDir::new().unwrap();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "a", HarnessKind::ClaudeCode).unwrap();
        lock(&state.needs_session_meta).insert(agent.id);

        let err = send_message_impl(&state, agent.id, "hi").await.unwrap_err();
        assert!(matches!(err, AppError::Dispatcher(_)));
        assert!(
            lock(&state.needs_session_meta).contains(&agent.id),
            "flag must persist through pre-stream Err so a retry still forces SessionMeta"
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
            emitter as Arc<dyn EventEmitter>,
        );
        let tmp = TempDir::new().unwrap();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let agent_default = create_agent_impl(&state, "a", HarnessKind::ClaudeCode).unwrap();
        let handle = send_message_impl(&state, agent_default.id, "hi")
            .await
            .unwrap();
        handle.join.await.unwrap();
        assert!(
            !saw_flag.load(Ordering::SeqCst),
            "default send must pass is_first_dispatch_after_attach=false"
        );

        // Now stash the flag and re-send for the same agent — adapter must see true.
        lock(&state.needs_session_meta).insert(agent_default.id);
        let handle = send_message_impl(&state, agent_default.id, "again")
            .await
            .unwrap();
        handle.join.await.unwrap();
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
            emitter as Arc<dyn EventEmitter>,
        );
        let tmp = TempDir::new().unwrap();
        init_directory_impl(&state, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let proj = create_project_impl(&state, "alpha").unwrap();
        set_active_project_impl(&state, proj.id).unwrap();
        let agent = create_agent_impl(&state, "a", HarnessKind::Codex).unwrap();
        // Simulate the Codex-attach state: the flag is set on a real attach,
        // but `create_agent_impl` doesn't trigger that path, so set it
        // directly to isolate the read-don't-drain behavior under test.
        lock(&state.needs_session_meta).insert(agent.id);

        // Run four dispatches sequentially. Each completes before the next.
        for _ in 0..4 {
            let handle = send_message_impl(&state, agent.id, "hi").await.unwrap();
            handle.join.await.unwrap();
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
        let proj_a = create_project_impl(&state, "alpha").unwrap();
        let proj_b = create_project_impl(&state, "beta").unwrap();

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

        let (handle_a, handle_b) = tokio::join!(
            send_message_impl(&state, agent_a.id, "A's prompt"),
            send_message_impl(&state, agent_b.id, "B's prompt"),
        );
        let handle_a = handle_a.unwrap();
        let handle_b = handle_b.unwrap();
        handle_a.join.await.unwrap();
        handle_b.join.await.unwrap();

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
        create_project_impl(&state, "alpha").unwrap();

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
        let proj = create_project_impl(&state, name).unwrap();
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
        let beta = create_project_impl(&state, "beta").unwrap();
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
        let beta = create_project_impl(&state, "beta").unwrap();
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
            let proj_a = create_project_impl(&state_a, "alpha").unwrap();
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
        let proj_b = create_project_impl(&state_b, "beta").unwrap();
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
            let proj_a = create_project_impl(&state_a, "alpha").unwrap();
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
        let proj_b = create_project_impl(&state_b, "beta").unwrap();
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

        let proj_b = create_project_impl(&state, "beta").unwrap();
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
}
