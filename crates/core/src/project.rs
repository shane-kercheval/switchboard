use std::fs::{File, create_dir_all};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::AgentRecord;
use crate::error::{CoreError, Result};
use crate::harness::HarnessKind;
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};

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
    /// the M1.3 adapter will pass via `--session-id <uuid>`.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the *same `Project` instance* — the
    /// read-check-then-append sequence has a TOCTOU window. Callers must
    /// serialize access (the M1.4 dispatcher / `AppState` mutex does this).
    /// Concurrent calls against *different* `Project` instances (in the same
    /// or different directories) are fine; M3's `instance.lock` provides
    /// cross-process serialization.
    pub fn register_agent(&self, name: &str, harness: HarnessKind) -> Result<AgentRecord> {
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

        let session_id = match harness {
            HarnessKind::ClaudeCode => Some(Uuid::now_v7()),
        };

        let record = AgentRecord {
            id: Uuid::now_v7(),
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
    let config_path = root.join("config.yaml");
    let config = read_yaml::<ProjectConfig>(&config_path)?;
    if config.version != PROJECT_CONFIG_VERSION {
        return Err(CoreError::UnsupportedConfigVersion {
            path: config_path,
            found: config.version,
            expected: PROJECT_CONFIG_VERSION,
        });
    }
    let registry_path = root.join("registry.jsonl");
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
    write_yaml(&root.join("config.yaml"), &config)?;

    // Touch registry.jsonl so the file exists even before any agents are registered.
    let registry_path = root.join("registry.jsonl");
    File::create(&registry_path).map_err(|e| CoreError::io(&registry_path, e))?;

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
    fn unsupported_config_version_surfaces_typed_error() {
        let (_tmp, project) = fresh_project();
        // Write a bad version to the project's config.yaml.
        std::fs::write(
            project.root.join("config.yaml"),
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
