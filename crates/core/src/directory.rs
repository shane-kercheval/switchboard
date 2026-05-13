use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::project::{self, Project, ProjectId, ProjectSummary};

const DIRECTORY_CONFIG_VERSION: u32 = 1;
const SWITCHBOARD_DIR: &str = ".switchboard";
const CONFIG_FILE: &str = "config.yaml";
const PROJECTS_INDEX: &str = "projects.jsonl";
const PROJECTS_DIR: &str = "projects";
const WORKFLOWS_DIR: &str = "workflows";
const PROMPTS_DIR: &str = "prompts";

/// On-disk shape of `<directory>/.switchboard/config.yaml`. Mostly empty in v1;
/// placeholder for future MCP/harness config per system-design §6.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryConfig {
    pub version: u32,
}

/// A working directory the user has bound Switchboard to. Holds zero or more
/// projects under `<directory>/.switchboard/projects/`.
#[derive(Debug, Clone)]
pub struct Directory {
    pub path: PathBuf,
}

impl Directory {
    /// Wraps a path, canonicalizing symlinks and resolving to absolute. The path
    /// must exist and be a directory; `.switchboard/` need not exist (call `init`
    /// to create it).
    pub fn at(path: &Path) -> Result<Directory> {
        let canonical = std::fs::canonicalize(path).map_err(|e| CoreError::io(path, e))?;
        if !canonical.is_dir() {
            return Err(CoreError::NotADirectory { path: canonical });
        }
        Ok(Directory { path: canonical })
    }

    pub fn has_switchboard(&self) -> bool {
        self.path.join(SWITCHBOARD_DIR).is_dir()
    }

    /// Creates `<path>/.switchboard/{config.yaml, workflows/, prompts/,
    /// projects.jsonl, projects/}` if missing. Idempotent — calling twice on
    /// the same directory leaves existing structure intact.
    pub fn init(&self) -> Result<()> {
        let sb = self.switchboard_dir();
        create_dir_all(&sb).map_err(|e| CoreError::io(&sb, e))?;
        create_dir_all(sb.join(WORKFLOWS_DIR))
            .map_err(|e| CoreError::io(sb.join(WORKFLOWS_DIR), e))?;
        create_dir_all(sb.join(PROMPTS_DIR)).map_err(|e| CoreError::io(sb.join(PROMPTS_DIR), e))?;
        create_dir_all(sb.join(PROJECTS_DIR))
            .map_err(|e| CoreError::io(sb.join(PROJECTS_DIR), e))?;

        let config_path = sb.join(CONFIG_FILE);
        if !config_path.exists() {
            write_yaml(
                &config_path,
                &DirectoryConfig {
                    version: DIRECTORY_CONFIG_VERSION,
                },
            )?;
        }
        let index_path = sb.join(PROJECTS_INDEX);
        if !index_path.exists() {
            std::fs::write(&index_path, "").map_err(|e| CoreError::io(&index_path, e))?;
        }
        Ok(())
    }

    /// Lists all projects in this directory's index. Returns empty if `init` has
    /// not been called yet. If `.switchboard/` exists but `projects.jsonl` is
    /// missing, that's corruption — surfaces as `MissingAppendOnlyFile` rather
    /// than being silently reinterpreted as "no projects."
    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        let index = self.projects_index_path();
        if self.has_switchboard() && !index.exists() {
            return Err(CoreError::MissingAppendOnlyFile { path: index });
        }
        read_jsonl(&index)
    }

    /// Creates a new project under this directory. Validates the name (regex +
    /// per-directory uniqueness with hyphen↔underscore + case normalization).
    ///
    /// # Atomicity
    ///
    /// The project directory is created first, then the summary is appended to
    /// `projects.jsonl`. If the append fails, the freshly-created project
    /// directory is removed so `list_projects` and `open_project` remain
    /// consistent with the index. The original append error stays primary; a
    /// rollback failure (rare) is logged but does not replace the primary
    /// error.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the *same `Directory` instance* —
    /// the read-check-then-append sequence has a TOCTOU window. Callers must
    /// serialize access (the M1.4 dispatcher / `AppState` mutex does this).
    /// Concurrent calls against *different* `Directory` instances (different
    /// directories) are fine; M3's `instance.lock` provides cross-process
    /// serialization within one directory.
    pub fn create_project(&self, name: &str) -> Result<Project> {
        self.assert_initialized()?;
        validate_name(name)?;
        let canonical = canonicalize_for_uniqueness(name);
        for existing in self.list_projects()? {
            if canonicalize_for_uniqueness(&existing.name) == canonical {
                return Err(CoreError::DuplicateProjectName {
                    name: name.to_owned(),
                    existing: existing.name,
                });
            }
        }

        let projects_dir = self.projects_dir();
        let (summary, project) = project::create_on_disk(&self.path, &projects_dir, name)?;
        if let Err(append_err) = append_jsonl(&self.projects_index_path(), &summary) {
            if let Err(rollback_err) = std::fs::remove_dir_all(&project.root) {
                tracing_log_rollback_failure(&project.root, &rollback_err);
            }
            return Err(append_err);
        }
        Ok(project)
    }

    /// Loads a project by id. Errors if the project is not in this directory's
    /// index.
    ///
    /// # Concurrency
    ///
    /// Same instance-level serialization requirement as `create_project` — see
    /// that method's `# Concurrency` note.
    pub fn open_project(&self, id: ProjectId) -> Result<Project> {
        let summary = self
            .list_projects()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or(CoreError::ProjectNotFound(id))?;
        let root = self.projects_dir().join(summary.id.to_string());
        project::load(&self.path, summary.id, root)
    }

    /// Reads the directory-level config. Errors with `UnsupportedConfigVersion`
    /// if the on-disk `version` doesn't match this build.
    pub fn config(&self) -> Result<DirectoryConfig> {
        let path = self.config_path();
        let config = read_yaml::<DirectoryConfig>(&path)?;
        if config.version != DIRECTORY_CONFIG_VERSION {
            return Err(CoreError::UnsupportedConfigVersion {
                path,
                found: config.version,
                expected: DIRECTORY_CONFIG_VERSION,
            });
        }
        Ok(config)
    }

    fn switchboard_dir(&self) -> PathBuf {
        self.path.join(SWITCHBOARD_DIR)
    }
    fn projects_dir(&self) -> PathBuf {
        self.switchboard_dir().join(PROJECTS_DIR)
    }
    fn projects_index_path(&self) -> PathBuf {
        self.switchboard_dir().join(PROJECTS_INDEX)
    }
    fn config_path(&self) -> PathBuf {
        self.switchboard_dir().join(CONFIG_FILE)
    }

    fn assert_initialized(&self) -> Result<()> {
        if self.has_switchboard() {
            Ok(())
        } else {
            Err(CoreError::io(
                self.switchboard_dir(),
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    ".switchboard/ does not exist — call init() first",
                ),
            ))
        }
    }
}

// `tracing` is not yet a dep of this crate (deferred until M1.4 when the
// dispatcher introduces real logging). Until then, rollback failures go to
// stderr so they aren't completely silent. Replace with `tracing::warn!` when
// the dep lands.
fn tracing_log_rollback_failure(path: &Path, err: &std::io::Error) {
    eprintln!(
        "switchboard-core: failed to roll back project directory {} after index append failure: {err}",
        path.display(),
    );
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use tempfile::TempDir;

    #[test]
    fn at_requires_existing_directory() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        let err = Directory::at(&nonexistent).unwrap_err();
        assert!(matches!(err, CoreError::Io { .. }));
    }

    #[test]
    fn at_rejects_file_paths() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        // canonicalize() succeeds on files; we explicitly reject them.
        let err = Directory::at(&file).unwrap_err();
        assert!(matches!(err, CoreError::NotADirectory { .. }));
    }

    #[test]
    fn at_canonicalizes_symlinks() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let link = tmp.path().join("link");
        create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let directory = Directory::at(&link).unwrap();
        assert_eq!(directory.path, std::fs::canonicalize(&real).unwrap());
    }

    #[test]
    fn init_is_idempotent_and_preserves_projects() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        let project = directory.create_project("alpha").unwrap();

        // Second init must not wipe anything.
        directory.init().unwrap();

        let projects = directory.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, project.id);
    }

    #[test]
    fn list_projects_before_init_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        assert_eq!(directory.list_projects().unwrap(), vec![]);
    }

    #[test]
    fn list_projects_after_init_with_missing_index_is_typed_error() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        // Simulate the index being manually removed after init.
        std::fs::remove_file(directory.projects_index_path()).unwrap();
        let err = directory.list_projects().unwrap_err();
        assert!(matches!(err, CoreError::MissingAppendOnlyFile { .. }));
    }

    #[test]
    fn create_project_without_init_fails() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        let err = directory.create_project("alpha").unwrap_err();
        assert!(matches!(err, CoreError::Io { .. }));
    }

    #[test]
    fn open_project_unknown_id_fails() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        let err = directory.open_project(uuid::Uuid::now_v7()).unwrap_err();
        assert!(matches!(err, CoreError::ProjectNotFound(_)));
    }

    #[test]
    fn duplicate_project_name_rejected_under_canonicalization() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        directory.create_project("feature-a").unwrap();
        for collision in ["feature-a", "feature_a", "Feature-A", "FEATURE_A"] {
            let err = directory.create_project(collision).unwrap_err();
            assert!(
                matches!(err, CoreError::DuplicateProjectName { .. }),
                "{collision:?} should collide with 'feature-a'"
            );
        }
    }

    #[test]
    fn corrupt_projects_index_line_surfaces_typed_error() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        directory.create_project("alpha").unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(directory.switchboard_dir().join(PROJECTS_INDEX))
            .unwrap();
        writeln!(f, "{{garbage").unwrap();

        let err = directory.list_projects().unwrap_err();
        match err {
            CoreError::CorruptJsonl {
                line_number, line, ..
            } => {
                assert_eq!(line_number, 2);
                assert_eq!(line, "{garbage");
            }
            other => panic!("expected CorruptJsonl, got {other:?}"),
        }
    }

    #[test]
    fn create_project_rolls_back_directory_when_index_append_fails() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        // Make projects.jsonl unwritable by replacing it with a read-only directory
        // of the same name — the append call's open() will fail.
        let index = directory.projects_index_path();
        std::fs::remove_file(&index).unwrap();
        std::fs::create_dir(&index).unwrap();

        let projects_dir_before = std::fs::read_dir(directory.projects_dir()).unwrap().count();

        let err = directory.create_project("alpha").unwrap_err();
        assert!(
            matches!(err, CoreError::Io { .. }),
            "expected Io error, got {err:?}"
        );

        // Rollback must have removed the freshly-created project root.
        let projects_dir_after = std::fs::read_dir(directory.projects_dir()).unwrap().count();
        assert_eq!(
            projects_dir_before, projects_dir_after,
            "project directory not rolled back after index append failure"
        );
    }
}
