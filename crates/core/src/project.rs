use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::{AgentRecord, SessionLocator};
use crate::error::{CoreError, Result};
use crate::harness::HarnessKind;
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_jsonl, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::paths::{CONFIG_FILE, JOURNAL_FILE, REGISTRY_FILE};

pub type ProjectId = Uuid;

const PROJECT_CONFIG_VERSION: u32 = 1;

/// One entry in `<directory>/.switchboard/projects.jsonl` — the directory-level
/// index of which projects exist under this directory. Append-only.
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
    pub fn register_agent(&self, name: &str, harness: HarnessKind) -> Result<AgentRecord> {
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
        self.register_agent_inner_with_id(name, harness, session_locator, Uuid::now_v7())
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
    ) -> Result<AgentRecord> {
        self.register_agent_inner_with_id(
            name,
            HarnessKind::ClaudeCode,
            Some(SessionLocator::Uuid(session_id)),
            Uuid::now_v7(),
        )
    }

    /// Register an attached **Codex** agent using a caller-supplied
    /// `agent_id`. The attach-flow callable (`attach_agent_impl`) uses this
    /// to **write the per-agent session-link sidecar before** committing
    /// the `AgentRecord` to the registry:
    ///
    /// 1. Mint `agent_id` upfront.
    /// 2. Compute the sidecar path from that id and write the link record.
    /// 3. Call this method to append the registry record with the **same**
    ///    id.
    ///
    /// **Why pre-generation is the public surface.** If the sidecar write
    /// happened *after* the registry append and failed, the `AgentRecord`
    /// would be orphaned: the adapter sees `prior.is_none()` on first
    /// dispatch and creates a brand-new Codex session (not the attached
    /// one), silently defeating the attach intent. The pre-generated-id
    /// ordering inverts the failure mode — at worst an orphan sidecar
    /// file with no `AgentRecord` pointing at it, invisible to dispatch
    /// and the collision scan. **No "register-without-id" Codex variant
    /// exists by design** — a parallel API that minted the id internally
    /// would be a trap for future callers who'd then need to compute the
    /// sidecar path post-register (the exact failure mode this method
    /// prevents).
    pub fn register_attached_codex_agent_with_id(
        &self,
        name: &str,
        agent_id: crate::agent::AgentId,
    ) -> Result<AgentRecord> {
        self.register_agent_inner_with_id(name, HarnessKind::Codex, None, agent_id)
    }

    /// Register an attached **Gemini** agent — one that wraps an
    /// already-existing Gemini session. Mirrors the Claude pattern
    /// (caller-controlled session UUID), not the Codex sidecar pattern.
    /// The provided `session_id` is the UUID embedded in the Gemini
    /// session-file filename's id8 prefix; the commands layer validates
    /// the file exists (and is unambiguous) before calling this method.
    pub fn register_attached_gemini_agent(
        &self,
        name: &str,
        session_id: Uuid,
    ) -> Result<AgentRecord> {
        self.register_agent_inner_with_id(
            name,
            HarnessKind::Gemini,
            Some(SessionLocator::Uuid(session_id)),
            Uuid::now_v7(),
        )
    }

    /// Register an attached **Antigravity** agent — one that wraps an existing
    /// server-assigned conversation. Now mirrors the Claude/Gemini
    /// caller-controlled-UUID pattern: the conversation UUID is the agent's
    /// session locator and is written straight onto the record, so there is no
    /// sidecar and no pre-generated-id ordering dance. The commands layer
    /// validates the conversation directory exists before calling this.
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
        )
    }

    /// Shared validation + JSONL append. Caller decides the `session_locator`
    /// strategy (create vs. attach, per-harness) and the `agent_id`
    /// (typically `Uuid::now_v7()` from the wrappers; the Codex attach flow
    /// pre-mints to coordinate with sidecar-first writing). Private to
    /// enforce the public surface invariants: create-path uses
    /// `register_agent`, attach-path uses the harness-specific
    /// `register_attached_*` methods, so a Claude attach without a
    /// `session_locator` (or a Codex attach with one) is unrepresentable at the
    /// API boundary.
    fn register_agent_inner_with_id(
        &self,
        name: &str,
        harness: HarnessKind,
        session_locator: Option<SessionLocator>,
        agent_id: Uuid,
    ) -> Result<AgentRecord> {
        validate_name(name)?;
        check_name_unique(&self.list_agents()?, name, None)?;

        let record = AgentRecord {
            id: agent_id,
            project_id: self.id,
            name: name.to_owned(),
            harness,
            session_locator,
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
            .register_agent("assistant", HarnessKind::ClaudeCode)
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
        let record = project.register_agent("g", HarnessKind::Gemini).unwrap();
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
        let record = project.register_agent("c", HarnessKind::Codex).unwrap();
        assert!(record.session_locator.is_none());
    }

    #[test]
    fn register_antigravity_agent_leaves_session_id_none() {
        // Antigravity assigns the conversation UUID server-side; the
        // adapter captures it post-spawn and writes the per-agent sidecar.
        // Mirrors Codex's pattern.
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent("a", HarnessKind::Antigravity)
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
            .register_agent("assistant", HarnessKind::ClaudeCode)
            .unwrap();
        let err = project
            .register_agent("assistant", HarnessKind::ClaudeCode)
            .unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
    }

    #[test]
    fn register_rejects_duplicate_under_hyphen_underscore_and_case() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("agent-a", HarnessKind::ClaudeCode)
            .unwrap();
        for collision in ["agent_a", "Agent-A", "AGENT_A"] {
            let err = project
                .register_agent(collision, HarnessKind::ClaudeCode)
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
            .register_agent("alpha", HarnessKind::ClaudeCode)
            .unwrap();
        let b = project.register_agent("beta", HarnessKind::Codex).unwrap();
        assert!(project.remove_agent(a.id).unwrap());
        assert_eq!(project.list_agents().unwrap(), vec![b]);
    }

    #[test]
    fn remove_agent_nonexistent_reports_not_removed() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("alpha", HarnessKind::ClaudeCode)
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
            .register_agent("alpha", HarnessKind::ClaudeCode)
            .unwrap();
        project.remove_agent(a.id).unwrap();
        project
            .register_agent("alpha", HarnessKind::Codex)
            .expect("name freed by removal");
    }

    #[test]
    fn rename_agent_changes_name_and_persists() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode)
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
            .register_agent("agent-a", HarnessKind::ClaudeCode)
            .unwrap();
        let updated = project.rename_agent(a.id, "Agent_A").unwrap();
        assert_eq!(updated.name, "Agent_A");
    }

    #[test]
    fn rename_agent_rejects_canonical_collision_with_another() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode)
            .unwrap();
        project.register_agent("beta", HarnessKind::Codex).unwrap();
        let err = project.rename_agent(a.id, "BETA").unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
        // The reject path leaves the registry untouched.
        assert_eq!(project.list_agents().unwrap()[0].name, "alpha");
    }

    #[test]
    fn rename_agent_rejects_invalid_name() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode)
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
            .register_agent("alpha", HarnessKind::ClaudeCode)
            .unwrap();
        let b = project.register_agent("beta", HarnessKind::Codex).unwrap();
        let c = project
            .register_agent("gamma", HarnessKind::Gemini)
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
            .register_agent("a", HarnessKind::Antigravity)
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
            .register_agent("c", HarnessKind::ClaudeCode)
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
        let codex = project.register_agent("x", HarnessKind::Codex).unwrap();
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
            .register_agent("agent.1", HarnessKind::ClaudeCode)
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidName { .. }));
    }

    #[test]
    fn register_attached_claude_persists_provided_session_id() {
        let (_tmp, project) = fresh_project();
        let provided = Uuid::now_v7();
        let record = project
            .register_attached_claude_agent("attached", provided)
            .unwrap();
        assert_eq!(record.harness, HarnessKind::ClaudeCode);
        assert_eq!(record.session_locator, Some(SessionLocator::Uuid(provided)));
        // Round-trips via the registry.
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn register_attached_codex_leaves_session_locator_none() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_attached_codex_agent_with_id("attached", Uuid::now_v7())
            .unwrap();
        assert_eq!(record.harness, HarnessKind::Codex);
        assert!(record.session_locator.is_none());
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
            .register_agent("agent-a", HarnessKind::ClaudeCode)
            .unwrap();
        let err = project
            .register_attached_claude_agent("agent_a", Uuid::now_v7())
            .unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
    }

    #[test]
    fn register_attached_validates_name() {
        let (_tmp, project) = fresh_project();
        let err = project
            .register_attached_claude_agent("bad.name", Uuid::now_v7())
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
            .register_agent("assistant", HarnessKind::ClaudeCode)
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
