use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::{AgentRecord, SessionLocator, normalize_selection};
use crate::error::{CoreError, Result};
use crate::harness::{HarnessKind, SelectionAxis};
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_jsonl, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::paths::{ATTACHMENTS_DIR, CONFIG_FILE, JOURNAL_FILE, REGISTRY_FILE};

pub type ProjectId = Uuid;

/// `pub(crate)` so `Directory::rename_project` can stamp the current version
/// when rewriting `config.yaml` without a redundant read-back.
pub(crate) const PROJECT_CONFIG_VERSION: u32 = 1;

/// One entry in `<directory>/.switchboard/projects.jsonl` — the directory-level
/// index of which projects exist under this directory. Appended on
/// `create_project`; rewritten in place on rename/delete (see
/// `Directory::rename_project` / `Directory::delete_project`), exactly as
/// `registry.jsonl` is for agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// On-disk shape of `<directory>/.switchboard/projects/<id>/config.yaml`. This
/// is the canonical source of truth for a project's identity; the matching
/// entry in `projects.jsonl` is denormalized for fast listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub version: u32,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// A task-scoped project within a working directory. Holds agents in its registry.
#[derive(Debug, Clone)]
pub struct Project {
    pub id: ProjectId,
    pub directory: PathBuf,
    pub config: ProjectConfig,
    pub root: PathBuf,
    pub registry_path: PathBuf,
}

impl Project {
    /// User-facing project name, sourced from `config.yaml` (the canonical
    /// record; the `projects.jsonl` summary's `name` is a denormalized copy).
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Path to this project's conversation journal (`journal.jsonl`) — the
    /// Switchboard-owned record of user sends + non-completed-turn outcomes
    /// (see [`crate::journal`]). Runtime data; `.gitignore`d like the rest of
    /// `projects/`.
    pub fn journal_path(&self) -> PathBuf {
        self.root.join(JOURNAL_FILE)
    }

    /// Directory holding this project's staged attachment files
    /// (`projects/<id>/attachments/`). Sited inside the per-project metadata dir
    /// so a staged file resolves under every harness's sandbox (the dir is under
    /// the user's working tree). Runtime data; `.gitignore`d like the rest of
    /// `projects/`. Created lazily on first stage; absent until then.
    pub fn attachments_dir(&self) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR)
    }

    /// Append a new agent to this project's registry. Validates the name (regex +
    /// per-project uniqueness with hyphen↔underscore + case normalization), generates
    /// a UUID v7 `AgentId`, and (for Claude Code) pre-generates a UUID v7
    /// `SessionLocator::Uuid` the adapter will pass via `--session-id <uuid>`.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the *same `Project` instance* — the
    /// read-check-then-append sequence has a TOCTOU window. Callers must
    /// serialize access (the dispatcher / `AppState` mutex does this).
    /// Concurrent calls against *different* `Project` instances (in the same
    /// or different directories) are fine; cross-process serialization within
    /// one directory is future work.
    ///
    /// # Durability
    ///
    /// On the rare path where `append_jsonl` reports a post-write durability
    /// (fsync) failure, this returns `Err` even though the record may already
    /// be on disk (`append_jsonl` syncs after writing). The caller must not
    /// treat that as "nothing happened": a subsequent retry can hit
    /// `DuplicateAgentName` because the record is visible, and the agent will
    /// appear on the next `list_agents`. There is no destructive cleanup to
    /// undo here (unlike `Directory::create_project`), so no rollback applies.
    ///
    /// `model` / `effort` are the user-selected per-agent settings (`None` =
    /// harness default). A selection on a harness that can't apply it (a model
    /// on Antigravity, an effort on Gemini/Antigravity) is rejected at the
    /// persistence boundary — see `register_agent_inner_with_id`. This generic
    /// create path can't constrain that in its signature the way the attach
    /// variants do (Gemini takes no effort, Antigravity takes neither), so it
    /// relies on that shared chokepoint. The commands layer also validates
    /// first to return a friendlier error, but `core` is the backstop that
    /// keeps an inapplicable selection out of the registry regardless of
    /// caller.
    pub fn register_agent(
        &self,
        name: &str,
        harness: HarnessKind,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<AgentRecord> {
        // Harness-asymmetry rule (which harnesses can pre-generate their
        // session locator at registration vs. learn it at runtime):
        // - Claude Code pre-generates a UUID v7 locator; passed via
        //   `--session-id`/`--resume`.
        // - Gemini pre-generates a UUID **v4** locator. Gemini's session
        //   filename embeds the first 8 hex chars of the session ID, and
        //   UUID v7s minted in the same millisecond share their first 8
        //   chars — concurrent Gemini dispatches in one cwd would interleave
        //   on disk. v4's first 8 chars are random across 32 bits, so the
        //   collision probability is ~1/2^32. Localized to Gemini.
        // - Codex and Antigravity leave it `None`: their session id is
        //   assigned by the harness at runtime (Codex's `thread_id` from
        //   `thread.started`; Antigravity's server-assigned conversation
        //   UUID), so it isn't knowable at registration time. The adapter
        //   captures it on first dispatch and it's persisted to this record's
        //   `session_locator` via `set_session_locator`.
        let session_locator = match harness {
            HarnessKind::ClaudeCode => Some(SessionLocator::Uuid(Uuid::now_v7())),
            HarnessKind::Gemini => Some(SessionLocator::Uuid(Uuid::new_v4())),
            HarnessKind::Codex | HarnessKind::Antigravity => None,
        };
        self.register_agent_inner_with_id(
            name,
            harness,
            session_locator,
            Uuid::now_v7(),
            model,
            effort,
        )
    }

    /// Register an attached **Claude Code** agent — one that wraps an
    /// already-existing harness session (e.g., a session the user started
    /// outside Switchboard). The provided `session_id` is the existing
    /// `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` filename. Caller
    /// (commands layer) is responsible for validating the session file
    /// exists; this method only persists the record.
    pub fn register_attached_claude_agent(
        &self,
        name: &str,
        session_id: Uuid,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<AgentRecord> {
        self.register_agent_inner_with_id(
            name,
            HarnessKind::ClaudeCode,
            Some(SessionLocator::Uuid(session_id)),
            Uuid::now_v7(),
            model,
            effort,
        )
    }

    /// Register an attached **Codex** agent — one that wraps an existing Codex
    /// session. The `thread_id` and partition-date (parsed from the existing
    /// rollout file's name and directory) are the agent's session locator and
    /// are written straight onto the record — no sidecar, no pre-generated-id
    /// ordering. The commands layer locates and validates the rollout file
    /// before calling this.
    pub fn register_attached_codex_agent(
        &self,
        name: &str,
        thread_id: String,
        partition_date: chrono::NaiveDate,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<AgentRecord> {
        self.register_agent_inner_with_id(
            name,
            HarnessKind::Codex,
            Some(SessionLocator::Codex {
                thread_id,
                partition_date,
            }),
            Uuid::now_v7(),
            model,
            effort,
        )
    }

    /// Register an attached **Gemini** agent — one that wraps an
    /// already-existing Gemini session. Mirrors the Claude pattern
    /// (caller-controlled session UUID), not the Codex sidecar pattern.
    /// The provided `session_id` is the UUID embedded in the Gemini
    /// session-file filename's id8 prefix; the commands layer validates
    /// the file exists (and is unambiguous) before calling this method.
    ///
    /// Takes `model` but no `effort`: Gemini supports model selection but not
    /// effort selection (`supports_effort_selection` is `false`), so the
    /// capability invariant is encoded in the signature rather than asserted.
    pub fn register_attached_gemini_agent(
        &self,
        name: &str,
        session_id: Uuid,
        model: Option<String>,
    ) -> Result<AgentRecord> {
        self.register_agent_inner_with_id(
            name,
            HarnessKind::Gemini,
            Some(SessionLocator::Uuid(session_id)),
            Uuid::now_v7(),
            model,
            None,
        )
    }

    /// Register an attached **Antigravity** agent — one that wraps an existing
    /// server-assigned conversation. Now mirrors the Claude/Gemini
    /// caller-controlled-UUID pattern: the conversation UUID is the agent's
    /// session locator and is written straight onto the record, so there is no
    /// sidecar and no pre-generated-id ordering dance. The commands layer
    /// validates the conversation directory exists before calling this.
    ///
    /// Takes neither `model` nor `effort`: Antigravity supports neither (its
    /// model is harness-owned global config, and effort is folded into the
    /// model name), so both invariants are encoded in the signature.
    pub fn register_attached_antigravity_agent(
        &self,
        name: &str,
        conversation_id: Uuid,
    ) -> Result<AgentRecord> {
        self.register_agent_inner_with_id(
            name,
            HarnessKind::Antigravity,
            Some(SessionLocator::Uuid(conversation_id)),
            Uuid::now_v7(),
            None,
            None,
        )
    }

    /// Shared validation + JSONL append. Caller decides the `session_locator`
    /// strategy (create vs. attach, per-harness); every caller mints the
    /// `agent_id` inline as `Uuid::now_v7()`. Private to enforce the public
    /// surface invariants: create-path uses `register_agent`, attach-path uses
    /// the harness-specific `register_attached_*` methods, so a Claude attach
    /// without a `session_locator` (or a Codex attach with one) is
    /// unrepresentable at the API boundary.
    ///
    /// This is also the single chokepoint that enforces the model/effort
    /// capability invariant at the persistence boundary — mirroring
    /// [`Self::set_session_locator`]'s `is_valid_for` guard. The attach
    /// variants already make unsupported combinations unrepresentable in their
    /// signatures; this catches the generic create path (and any future caller)
    /// so an unsupported selection can never reach `registry.jsonl`, regardless
    /// of whether a higher layer remembered to check.
    fn register_agent_inner_with_id(
        &self,
        name: &str,
        harness: HarnessKind,
        session_locator: Option<SessionLocator>,
        agent_id: Uuid,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<AgentRecord> {
        validate_name(name)?;
        // Normalize **before** the capability check: a blank selection means
        // "unset," which is allowed on any harness — it must not trip the
        // capability error (e.g. a whitespace effort on Gemini is "no effort,"
        // not an unsupported effort).
        let model = normalize_selection(model);
        let effort = normalize_selection(effort);
        if model.is_some() && !harness.supports_model_selection() {
            return Err(CoreError::SelectionUnsupported {
                harness,
                axis: SelectionAxis::Model,
            });
        }
        if effort.is_some() && !harness.supports_effort_selection() {
            return Err(CoreError::SelectionUnsupported {
                harness,
                axis: SelectionAxis::Effort,
            });
        }
        check_name_unique(&self.list_agents()?, name, None)?;

        let record = AgentRecord {
            id: agent_id,
            project_id: self.id,
            name: name.to_owned(),
            harness,
            session_locator,
            model,
            effort,
            created_at: Utc::now(),
        };

        append_jsonl(&self.registry_path, &record)?;
        Ok(record)
    }

    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        read_jsonl(&self.registry_path)
    }

    /// Remove an agent from the registry by id, rewriting `registry.jsonl`
    /// without the record. Returns whether a record was actually removed, so a
    /// stale or double remove is detectable rather than a silent no-op. Touches
    /// only the registry — the caller owns sidecar cleanup and any actor
    /// teardown.
    pub fn remove_agent(&self, agent_id: crate::agent::AgentId) -> Result<bool> {
        let mut agents = self.list_agents()?;
        let before = agents.len();
        agents.retain(|a| a.id != agent_id);
        if agents.len() == before {
            return Ok(false);
        }
        write_jsonl(&self.registry_path, &agents)?;
        Ok(true)
    }

    /// Rename an agent in the registry. Validates the new name's format and its
    /// canonicalized uniqueness against the *other* agents (self excluded, so
    /// re-saving the same name — or a case/hyphen variant — is allowed), then
    /// rewrites `registry.jsonl`. Returns the updated record.
    pub fn rename_agent(
        &self,
        agent_id: crate::agent::AgentId,
        new_name: &str,
    ) -> Result<AgentRecord> {
        validate_name(new_name)?;
        let mut agents = self.list_agents()?;
        let idx = agents
            .iter()
            .position(|a| a.id == agent_id)
            .ok_or(CoreError::AgentNotFound(agent_id))?;
        check_name_unique(&agents, new_name, Some(agent_id))?;
        new_name.clone_into(&mut agents[idx].name);
        let updated = agents[idx].clone();
        write_jsonl(&self.registry_path, &agents)?;
        Ok(updated)
    }

    /// Set one agent's `session_locator` in place, rewriting `registry.jsonl`
    /// with the new value and every other record (and their order) preserved.
    /// Returns the updated record.
    ///
    /// This is the registry's only in-place field mutation beyond `rename_agent`.
    /// It exists for the runtime-capture path: Codex/Antigravity learn their
    /// session locator on first dispatch (and Antigravity can re-learn it on a
    /// fork-and-heal), and the captured locator is identity that belongs on the
    /// record. Same atomic full-rewrite + concurrency contract as
    /// `remove_agent`/`rename_agent` — callers serialize via the app's
    /// `registry_write` mutex. Deliberately *not* a generic update API; this is
    /// the one mutation the capture path needs.
    pub fn set_session_locator(
        &self,
        agent_id: crate::agent::AgentId,
        locator: SessionLocator,
    ) -> Result<AgentRecord> {
        let mut agents = self.list_agents()?;
        let idx = agents
            .iter()
            .position(|a| a.id == agent_id)
            .ok_or(CoreError::AgentNotFound(agent_id))?;
        // Reject a locator whose shape doesn't match the agent's harness (e.g.
        // a Codex locator on a Claude agent). This is the persistence-boundary
        // guard: an adapter capture bug would otherwise durably store a record
        // that silently fails to resume. The enum makes intra-variant invalid
        // states unrepresentable; this closes the harness↔variant gap.
        let harness = agents[idx].harness;
        if !locator.is_valid_for(harness) {
            return Err(CoreError::SessionLocatorHarnessMismatch { agent_id, harness });
        }
        agents[idx].session_locator = Some(locator);
        let updated = agents[idx].clone();
        write_jsonl(&self.registry_path, &agents)?;
        Ok(updated)
    }

    /// Set (or clear, with `None`) an agent's selected `model` in place,
    /// rewriting `registry.jsonl`. Mirrors `rename_agent` / `set_session_locator`
    /// — same atomic full-rewrite + `registry_write`-serialized contract. The
    /// value is normalized (blank → unset) and the model-selection capability is
    /// re-checked at this persistence boundary, so both dispatch-safety
    /// invariants — "no blank selection" and "no inapplicable selection" reaches
    /// the registry — hold on the mutation path too, not just at registration.
    /// Takes `Option<String>` so clearing back to the harness default is
    /// expressible. Returns the updated record.
    pub fn set_agent_model(
        &self,
        agent_id: crate::agent::AgentId,
        model: Option<String>,
    ) -> Result<AgentRecord> {
        let model = normalize_selection(model);
        let mut agents = self.list_agents()?;
        let idx = agents
            .iter()
            .position(|a| a.id == agent_id)
            .ok_or(CoreError::AgentNotFound(agent_id))?;
        let harness = agents[idx].harness;
        if model.is_some() && !harness.supports_model_selection() {
            return Err(CoreError::SelectionUnsupported {
                harness,
                axis: SelectionAxis::Model,
            });
        }
        agents[idx].model = model;
        let updated = agents[idx].clone();
        write_jsonl(&self.registry_path, &agents)?;
        Ok(updated)
    }

    /// Set (or clear, with `None`) an agent's selected reasoning `effort` in
    /// place. The effort-axis counterpart to [`Self::set_agent_model`]; same
    /// contract, normalization, and capability re-check.
    pub fn set_agent_effort(
        &self,
        agent_id: crate::agent::AgentId,
        effort: Option<String>,
    ) -> Result<AgentRecord> {
        let effort = normalize_selection(effort);
        let mut agents = self.list_agents()?;
        let idx = agents
            .iter()
            .position(|a| a.id == agent_id)
            .ok_or(CoreError::AgentNotFound(agent_id))?;
        let harness = agents[idx].harness;
        if effort.is_some() && !harness.supports_effort_selection() {
            return Err(CoreError::SelectionUnsupported {
                harness,
                axis: SelectionAxis::Effort,
            });
        }
        agents[idx].effort = effort;
        let updated = agents[idx].clone();
        write_jsonl(&self.registry_path, &agents)?;
        Ok(updated)
    }
}

/// Canonicalized-uniqueness check shared by register (`exclude` = `None`) and
/// rename (`exclude` = the renamed agent's id, so it doesn't collide with
/// itself). Per system-design §4, names collide case-insensitively and with
/// hyphens treated as underscores.
fn check_name_unique(
    agents: &[AgentRecord],
    name: &str,
    exclude: Option<crate::agent::AgentId>,
) -> Result<()> {
    let canonical = canonicalize_for_uniqueness(name);
    for existing in agents {
        if Some(existing.id) == exclude {
            continue;
        }
        if canonicalize_for_uniqueness(&existing.name) == canonical {
            return Err(CoreError::DuplicateAgentName {
                name: name.to_owned(),
                existing: existing.name.clone(),
            });
        }
    }
    Ok(())
}

/// Load a `Project` from disk. Reads the per-project config.yaml; the caller has
/// already located the project root (e.g., via `Directory::open_project`).
pub(crate) fn load(directory: &Path, id: ProjectId, root: PathBuf) -> Result<Project> {
    let config_path = root.join(CONFIG_FILE);
    let config = read_yaml::<ProjectConfig>(&config_path)?;
    if config.version != PROJECT_CONFIG_VERSION {
        return Err(CoreError::UnsupportedConfigVersion {
            path: config_path,
            found: config.version,
            expected: PROJECT_CONFIG_VERSION,
        });
    }
    let registry_path = root.join(REGISTRY_FILE);
    Ok(Project {
        id,
        directory: directory.to_owned(),
        config,
        root,
        registry_path,
    })
}

/// Create a new project's on-disk artifacts (config.yaml + empty registry.jsonl).
/// The caller (`Directory`) is responsible for appending the `ProjectSummary` to
/// projects.jsonl — and for rolling back the directory if that append fails.
pub(crate) fn create_on_disk(
    directory: &Path,
    projects_dir: &Path,
    name: &str,
) -> Result<(ProjectSummary, Project)> {
    let id = Uuid::now_v7();
    let root = projects_dir.join(id.to_string());
    create_dir_all(&root).map_err(|e| CoreError::io(&root, e))?;

    let created_at = Utc::now();
    let config = ProjectConfig {
        version: PROJECT_CONFIG_VERSION,
        name: name.to_owned(),
        created_at,
    };
    write_yaml(&root.join(CONFIG_FILE), &config)?;

    // Touch registry.jsonl so the file exists even before any agents are
    // registered. `create_new` (not `create`) so we fail fast if a stale
    // registry already sits at this path — that would only happen if a
    // prior `create_project` partially succeeded and rollback failed to
    // remove the project dir; under that condition we want a hard error,
    // not silent truncation of a registry that might still have data.
    let registry_path = root.join(REGISTRY_FILE);
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&registry_path)
        .map_err(|e| CoreError::io(&registry_path, e))?;

    let summary = ProjectSummary {
        id,
        name: name.to_owned(),
        created_at,
    };
    let project = Project {
        id,
        directory: directory.to_owned(),
        config,
        root,
        registry_path,
    };
    Ok((summary, project))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use tempfile::TempDir;

    fn fresh_project() -> (TempDir, Project) {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path().join("projects");
        create_dir_all(&projects_dir).unwrap();
        let (_summary, project) =
            create_on_disk(tmp.path(), &projects_dir, "test-project").unwrap();
        (tmp, project)
    }

    #[test]
    fn register_then_list_agent_roundtrips() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        assert_eq!(record.name, "assistant");
        assert_eq!(record.project_id, project.id);
        assert!(record.session_locator.is_some()); // ClaudeCode pre-generates a UUID locator.

        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn register_gemini_agent_mints_uuid_v4_session_id() {
        // Load-bearing: v7 caused the on-disk session-file interleave
        // hazard against Gemini's 8-char-prefix filename. If a future
        // refactor accidentally swaps this back to `Uuid::now_v7()`,
        // concurrent dispatches in one cwd corrupt transcripts.
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent("g", HarnessKind::Gemini, None, None)
            .unwrap();
        let SessionLocator::Uuid(session_id) = record
            .session_locator
            .expect("Gemini pre-generates a UUID locator")
        else {
            panic!("Gemini locator must be the Uuid variant");
        };
        assert_eq!(
            session_id.get_version_num(),
            4,
            "Gemini session_id must be UUID v4, got: {session_id} (version {})",
            session_id.get_version_num()
        );
    }

    #[test]
    fn register_codex_agent_leaves_session_id_none() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent("c", HarnessKind::Codex, None, None)
            .unwrap();
        assert!(record.session_locator.is_none());
    }

    #[test]
    fn register_antigravity_agent_leaves_session_locator_none() {
        // Antigravity assigns the conversation UUID server-side; the adapter
        // captures it post-spawn and the dispatcher persists it onto the
        // registry record. Mirrors Codex's pattern.
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent("a", HarnessKind::Antigravity, None, None)
            .unwrap();
        assert!(record.session_locator.is_none());
    }

    #[test]
    fn project_name_delegates_to_config() {
        let (_tmp, project) = fresh_project();
        assert_eq!(project.name(), "test-project");
        assert_eq!(project.name(), project.config.name);
    }

    #[test]
    fn register_rejects_duplicate_verbatim() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let err = project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
    }

    #[test]
    fn register_rejects_duplicate_under_hyphen_underscore_and_case() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("agent-a", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        for collision in ["agent_a", "Agent-A", "AGENT_A"] {
            let err = project
                .register_agent(collision, HarnessKind::ClaudeCode, None, None)
                .unwrap_err();
            assert!(
                matches!(err, CoreError::DuplicateAgentName { .. }),
                "{collision:?} should collide with 'agent-a'"
            );
        }
    }

    #[test]
    fn remove_agent_drops_target_and_keeps_others() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("beta", HarnessKind::Codex, None, None)
            .unwrap();
        assert!(project.remove_agent(a.id).unwrap());
        assert_eq!(project.list_agents().unwrap(), vec![b]);
    }

    #[test]
    fn remove_agent_nonexistent_reports_not_removed() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        assert!(!project.remove_agent(Uuid::now_v7()).unwrap());
        assert_eq!(project.list_agents().unwrap().len(), 1);
    }

    #[test]
    fn removed_name_is_reusable() {
        // Uniqueness is checked against the live registry, so freeing a name by
        // removal lets it be registered again.
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        project.remove_agent(a.id).unwrap();
        project
            .register_agent("alpha", HarnessKind::Codex, None, None)
            .expect("name freed by removal");
    }

    #[test]
    fn rename_agent_changes_name_and_persists() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let updated = project.rename_agent(a.id, "renamed").unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.id, a.id);
        let listed = project.list_agents().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "renamed");
    }

    #[test]
    fn rename_agent_to_own_name_variant_succeeds() {
        // Self is excluded from the uniqueness check, so a case/hyphen variant
        // of the agent's own name is allowed.
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("agent-a", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let updated = project.rename_agent(a.id, "Agent_A").unwrap();
        assert_eq!(updated.name, "Agent_A");
    }

    #[test]
    fn rename_agent_rejects_canonical_collision_with_another() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        project
            .register_agent("beta", HarnessKind::Codex, None, None)
            .unwrap();
        let err = project.rename_agent(a.id, "BETA").unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
        // The reject path leaves the registry untouched.
        assert_eq!(project.list_agents().unwrap()[0].name, "alpha");
    }

    #[test]
    fn rename_agent_rejects_invalid_name() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let err = project.rename_agent(a.id, "bad name").unwrap_err();
        assert!(matches!(err, CoreError::InvalidName { .. }));
    }

    #[test]
    fn rename_agent_nonexistent_returns_not_found() {
        let (_tmp, project) = fresh_project();
        let err = project.rename_agent(Uuid::now_v7(), "x").unwrap_err();
        assert!(matches!(err, CoreError::AgentNotFound(_)));
    }

    #[test]
    fn set_session_locator_updates_only_target_and_preserves_order() {
        let (_tmp, project) = fresh_project();
        // Three agents in a known order; Codex starts with no locator.
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("beta", HarnessKind::Codex, None, None)
            .unwrap();
        let c = project
            .register_agent("gamma", HarnessKind::Gemini, None, None)
            .unwrap();
        assert!(b.session_locator.is_none());

        let locator = SessionLocator::Codex {
            thread_id: "thread-xyz".to_owned(),
            partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        };
        let updated = project.set_session_locator(b.id, locator.clone()).unwrap();
        assert_eq!(updated.id, b.id);
        assert_eq!(updated.session_locator, Some(locator.clone()));

        let listed = project.list_agents().unwrap();
        // Order preserved: alpha, beta, gamma.
        assert_eq!(
            listed.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![a.id, b.id, c.id]
        );
        // Only beta changed.
        assert_eq!(listed[0].session_locator, a.session_locator);
        assert_eq!(listed[1].session_locator, Some(locator));
        assert_eq!(listed[2].session_locator, c.session_locator);
    }

    #[test]
    fn set_session_locator_overwrites_an_existing_locator() {
        // Fork-and-heal shape: a locator already present is replaced.
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("a", HarnessKind::Antigravity, None, None)
            .unwrap();
        let first = SessionLocator::Uuid(Uuid::new_v4());
        project.set_session_locator(a.id, first).unwrap();
        let healed = SessionLocator::Uuid(Uuid::new_v4());
        let updated = project.set_session_locator(a.id, healed.clone()).unwrap();
        assert_eq!(updated.session_locator, Some(healed.clone()));
        assert_eq!(
            project.list_agents().unwrap()[0].session_locator,
            Some(healed)
        );
    }

    #[test]
    fn set_session_locator_nonexistent_returns_not_found() {
        let (_tmp, project) = fresh_project();
        let err = project
            .set_session_locator(Uuid::now_v7(), SessionLocator::Uuid(Uuid::new_v4()))
            .unwrap_err();
        assert!(matches!(err, CoreError::AgentNotFound(_)));
    }

    #[test]
    fn set_session_locator_rejects_harness_shape_mismatch() {
        // A Codex locator on a Claude agent must be refused (it would never
        // resume) — and the registry left untouched.
        let (_tmp, project) = fresh_project();
        let claude = project
            .register_agent("c", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let before = project.list_agents().unwrap();
        let err = project
            .set_session_locator(
                claude.id,
                SessionLocator::Codex {
                    thread_id: "t".to_owned(),
                    partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
                },
            )
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::SessionLocatorHarnessMismatch { .. }
        ));
        assert_eq!(project.list_agents().unwrap(), before);

        // The inverse: a Uuid locator on a Codex agent is likewise refused.
        let codex = project
            .register_agent("x", HarnessKind::Codex, None, None)
            .unwrap();
        let err = project
            .set_session_locator(codex.id, SessionLocator::Uuid(Uuid::new_v4()))
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::SessionLocatorHarnessMismatch { .. }
        ));
    }

    #[test]
    fn register_rejects_invalid_name() {
        let (_tmp, project) = fresh_project();
        let err = project
            .register_agent("agent.1", HarnessKind::ClaudeCode, None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidName { .. }));
    }

    #[test]
    fn register_attached_claude_persists_provided_session_id() {
        let (_tmp, project) = fresh_project();
        let provided = Uuid::now_v7();
        let record = project
            .register_attached_claude_agent("attached", provided, None, None)
            .unwrap();
        assert_eq!(record.harness, HarnessKind::ClaudeCode);
        assert_eq!(record.session_locator, Some(SessionLocator::Uuid(provided)));
        // Round-trips via the registry.
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn register_agent_persists_model_and_effort_in_one_step() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent(
                "assistant",
                HarnessKind::ClaudeCode,
                Some("opus".to_owned()),
                Some("max".to_owned()),
            )
            .unwrap();
        assert_eq!(record.model.as_deref(), Some("opus"));
        assert_eq!(record.effort.as_deref(), Some("max"));
        // Durable: the values are on the appended record, not set by a
        // follow-up call.
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn register_attached_claude_persists_model_and_effort() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_attached_claude_agent(
                "attached",
                Uuid::now_v7(),
                Some("sonnet".to_owned()),
                Some("low".to_owned()),
            )
            .unwrap();
        assert_eq!(record.model.as_deref(), Some("sonnet"));
        assert_eq!(record.effort.as_deref(), Some("low"));
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn register_attached_gemini_persists_model_with_no_effort() {
        // Gemini supports model but not effort; the signature structurally
        // forbids an effort, so the stored record always has `effort: None`.
        let (_tmp, project) = fresh_project();
        let record = project
            .register_attached_gemini_agent(
                "attached",
                Uuid::now_v7(),
                Some("gemini-2.5-pro".to_owned()),
            )
            .unwrap();
        assert_eq!(record.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(record.effort, None);
    }

    #[test]
    fn register_attached_antigravity_carries_no_model_or_effort() {
        // Antigravity supports neither axis; both are structurally None.
        let (_tmp, project) = fresh_project();
        let record = project
            .register_attached_antigravity_agent("attached", Uuid::new_v4())
            .unwrap();
        assert_eq!(record.model, None);
        assert_eq!(record.effort, None);
    }

    #[test]
    fn register_agent_rejects_model_on_unsupporting_harness() {
        // The generic create path is harness-agnostic, so the capability
        // invariant is enforced at the persistence boundary (the single
        // chokepoint), not just at the app layer. An Antigravity model never
        // reaches the registry.
        let (_tmp, project) = fresh_project();
        let err = project
            .register_agent(
                "a",
                HarnessKind::Antigravity,
                Some("whatever".to_owned()),
                None,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::SelectionUnsupported {
                harness: HarnessKind::Antigravity,
                axis: SelectionAxis::Model
            }
        ));
        // Rejected *before* the append — no orphan record.
        assert!(project.list_agents().unwrap().is_empty());
    }

    #[test]
    fn register_agent_rejects_effort_on_unsupporting_harness() {
        let (_tmp, project) = fresh_project();
        let err = project
            .register_agent("g", HarnessKind::Gemini, None, Some("high".to_owned()))
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::SelectionUnsupported {
                harness: HarnessKind::Gemini,
                axis: SelectionAxis::Effort
            }
        ));
        assert!(project.list_agents().unwrap().is_empty());
    }

    #[test]
    fn set_agent_model_and_effort_update_persist_and_clear() {
        let (_tmp, project) = fresh_project();
        let agent = project
            .register_agent("a", HarnessKind::ClaudeCode, None, None)
            .unwrap();

        let updated = project
            .set_agent_model(agent.id, Some("opus".to_owned()))
            .unwrap();
        assert_eq!(updated.model.as_deref(), Some("opus"));
        project
            .set_agent_effort(agent.id, Some("high".to_owned()))
            .unwrap();
        // Durable across a fresh read of the registry.
        let reloaded = &project.list_agents().unwrap()[0];
        assert_eq!(reloaded.model.as_deref(), Some("opus"));
        assert_eq!(reloaded.effort.as_deref(), Some("high"));

        // Clearing back to the harness default persists `None`.
        project.set_agent_model(agent.id, None).unwrap();
        project.set_agent_effort(agent.id, None).unwrap();
        let cleared = &project.list_agents().unwrap()[0];
        assert_eq!(cleared.model, None);
        assert_eq!(cleared.effort, None);
    }

    #[test]
    fn set_agent_model_rejects_unsupported_harness_without_mutating() {
        let (_tmp, project) = fresh_project();
        let agent = project
            .register_agent("a", HarnessKind::Antigravity, None, None)
            .unwrap();
        let err = project
            .set_agent_model(agent.id, Some("x".to_owned()))
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::SelectionUnsupported {
                harness: HarnessKind::Antigravity,
                axis: SelectionAxis::Model
            }
        ));
        // The rejected write left the record untouched.
        assert_eq!(project.list_agents().unwrap()[0].model, None);
    }

    #[test]
    fn set_agent_effort_rejects_unsupported_harness_without_mutating() {
        let (_tmp, project) = fresh_project();
        let agent = project
            .register_agent("g", HarnessKind::Gemini, None, None)
            .unwrap();
        let err = project
            .set_agent_effort(agent.id, Some("high".to_owned()))
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::SelectionUnsupported {
                harness: HarnessKind::Gemini,
                axis: SelectionAxis::Effort
            }
        ));
        assert_eq!(project.list_agents().unwrap()[0].effort, None);
    }

    #[test]
    fn set_agent_model_unknown_agent_errors() {
        let (_tmp, project) = fresh_project();
        let err = project
            .set_agent_model(Uuid::now_v7(), Some("x".to_owned()))
            .unwrap_err();
        assert!(matches!(err, CoreError::AgentNotFound(_)));
    }

    #[test]
    fn core_normalizes_blank_selection_regardless_of_caller() {
        // The persistence boundary, not just the IPC layer, drops a blank
        // selection — so a direct-core caller can't persist a dispatch-breaking
        // `Some("")` either.
        let (_tmp, project) = fresh_project();
        let agent = project
            .register_agent(
                "a",
                HarnessKind::ClaudeCode,
                Some("  ".to_owned()),
                Some(String::new()),
            )
            .unwrap();
        assert_eq!(agent.model, None);
        assert_eq!(agent.effort, None);

        project
            .set_agent_model(agent.id, Some("   ".to_owned()))
            .unwrap();
        project
            .set_agent_effort(agent.id, Some(" ".to_owned()))
            .unwrap();
        let reloaded = &project.list_agents().unwrap()[0];
        assert_eq!(reloaded.model, None);
        assert_eq!(reloaded.effort, None);
    }

    #[test]
    fn blank_selection_on_unsupporting_harness_is_unset_not_an_error() {
        // Normalize-before-capability-check: a blank value means "clear the
        // field," which is allowed on any harness. It must NOT surface as
        // `SelectionUnsupported` just because the harness lacks that axis —
        // clearing an effort on Gemini is "no effort," not "illegal effort."
        let (_tmp, project) = fresh_project();
        let gemini = project
            .register_agent("g", HarnessKind::Gemini, None, None)
            .unwrap();
        let updated = project
            .set_agent_effort(gemini.id, Some("   ".to_owned()))
            .expect("blank effort on Gemini is a no-op clear, not an error");
        assert_eq!(updated.effort, None);

        // Same at registration: a blank effort on Gemini registers fine.
        let agent = project
            .register_agent("g2", HarnessKind::Gemini, None, Some("  ".to_owned()))
            .expect("blank effort at registration is unset, not unsupported");
        assert_eq!(agent.effort, None);
    }

    #[test]
    fn register_attached_codex_persists_thread_id_and_date() {
        let (_tmp, project) = fresh_project();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        let record = project
            .register_attached_codex_agent("attached", "thread-abc".to_owned(), date, None, None)
            .unwrap();
        assert_eq!(record.harness, HarnessKind::Codex);
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Codex {
                thread_id: "thread-abc".to_owned(),
                partition_date: date,
            })
        );
    }

    #[test]
    fn register_attached_antigravity_persists_conversation_uuid() {
        let (_tmp, project) = fresh_project();
        let conversation_id = Uuid::new_v4();
        let record = project
            .register_attached_antigravity_agent("attached", conversation_id)
            .unwrap();
        assert_eq!(record.harness, HarnessKind::Antigravity);
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Uuid(conversation_id))
        );
    }

    #[test]
    fn register_attached_enforces_name_uniqueness_across_create_and_attach() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("agent-a", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let err = project
            .register_attached_claude_agent("agent_a", Uuid::now_v7(), None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
    }

    #[test]
    fn register_attached_validates_name() {
        let (_tmp, project) = fresh_project();
        let err = project
            .register_attached_claude_agent("bad.name", Uuid::now_v7(), None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidName { .. }));
    }

    #[test]
    fn unsupported_config_version_surfaces_typed_error() {
        let (_tmp, project) = fresh_project();
        // Write a bad version to the project's config.yaml.
        std::fs::write(
            project.root.join(CONFIG_FILE),
            "version: 99\nname: x\ncreated_at: 2026-05-12T00:00:00Z\n",
        )
        .unwrap();
        let err = load(&project.directory, project.id, project.root.clone()).unwrap_err();
        assert!(matches!(
            err,
            CoreError::UnsupportedConfigVersion {
                found: 99,
                expected: 1,
                ..
            }
        ));
    }

    #[test]
    fn corrupt_registry_line_surfaces_typed_error_with_line_number() {
        let (_tmp, project) = fresh_project();
        // Append a valid record then a malformed line.
        project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&project.registry_path)
            .unwrap();
        writeln!(f, "this is not json").unwrap();

        let err = project.list_agents().unwrap_err();
        match err {
            CoreError::CorruptJsonl {
                line_number, line, ..
            } => {
                assert_eq!(line_number, 2);
                assert_eq!(line, "this is not json");
            }
            other => panic!("expected CorruptJsonl, got {other:?}"),
        }
    }
}
