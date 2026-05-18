use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::AgentRecord;
use crate::error::{CoreError, Result};
use crate::harness::HarnessKind;
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::paths::{CONFIG_FILE, REGISTRY_FILE};

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

    /// Append a new agent to this project's registry. Validates the name (regex +
    /// per-project uniqueness with hyphen↔underscore + case normalization), generates
    /// a UUID v7 `AgentId`, and (for Claude Code) pre-generates a UUID v7 `session_id`
    /// the adapter will pass via `--session-id <uuid>`.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the *same `Project` instance* — the
    /// read-check-then-append sequence has a TOCTOU window. Callers must
    /// serialize access (the dispatcher / `AppState` mutex does this).
    /// Concurrent calls against *different* `Project` instances (in the same
    /// or different directories) are fine; cross-process serialization within
    /// one directory is future work.
    pub fn register_agent(&self, name: &str, harness: HarnessKind) -> Result<AgentRecord> {
        // Harness-asymmetry rule:
        // - Claude Code pre-generates session_id (UUID v7) at registration
        //   time; passed via `--session-id`/`--resume`.
        // - Gemini pre-generates session_id (UUID **v4**) at registration
        //   time; passed via `--session-id`/`--resume`. Gemini's session
        //   filename embeds the first 8 hex chars of the session ID, and
        //   UUID v7s minted in the same millisecond share their first 8
        //   chars — concurrent Gemini dispatches in one cwd would interleave
        //   on disk. v4's first 8 chars are random across 32 bits, so the
        //   collision probability is ~1/2^32. Localized to Gemini.
        // - Codex leaves it None and relies on the per-agent session-link
        //   sidecar populated from `thread.started` on first dispatch.
        let session_id = match harness {
            HarnessKind::ClaudeCode => Some(Uuid::now_v7()),
            HarnessKind::Gemini => Some(Uuid::new_v4()),
            HarnessKind::Codex => None,
        };
        self.register_agent_inner_with_id(name, harness, session_id, Uuid::now_v7())
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
            Some(session_id),
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

    /// Shared validation + JSONL append. Caller decides the `session_id`
    /// strategy (create vs. attach, per-harness) and the `agent_id`
    /// (typically `Uuid::now_v7()` from the wrappers; the Codex attach flow
    /// pre-mints to coordinate with sidecar-first writing). Private to
    /// enforce the public surface invariants: create-path uses
    /// `register_agent`, attach-path uses the harness-specific
    /// `register_attached_*` methods, so a Claude attach without a
    /// `session_id` (or a Codex attach with one) is unrepresentable at the
    /// API boundary.
    fn register_agent_inner_with_id(
        &self,
        name: &str,
        harness: HarnessKind,
        session_id: Option<Uuid>,
        agent_id: Uuid,
    ) -> Result<AgentRecord> {
        validate_name(name)?;
        let canonical = canonicalize_for_uniqueness(name);
        for existing in self.list_agents()? {
            if canonicalize_for_uniqueness(&existing.name) == canonical {
                return Err(CoreError::DuplicateAgentName {
                    name: name.to_owned(),
                    existing: existing.name,
                });
            }
        }

        let record = AgentRecord {
            id: agent_id,
            project_id: self.id,
            name: name.to_owned(),
            harness,
            session_id,
            created_at: Utc::now(),
        };

        append_jsonl(&self.registry_path, &record)?;
        Ok(record)
    }

    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        read_jsonl(&self.registry_path)
    }
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
        assert!(record.session_id.is_some()); // ClaudeCode pre-generates session UUID.

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
        let session_id = record.session_id.expect("Gemini pre-generates session_id");
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
        assert!(record.session_id.is_none());
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
        assert_eq!(record.session_id, Some(provided));
        // Round-trips via the registry.
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn register_attached_codex_leaves_session_id_none() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_attached_codex_agent_with_id("attached", Uuid::now_v7())
            .unwrap();
        assert_eq!(record.harness, HarnessKind::Codex);
        assert!(record.session_id.is_none());
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
