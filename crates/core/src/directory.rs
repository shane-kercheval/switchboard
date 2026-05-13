use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::project::{
    self, Project, ProjectId, ProjectSummary, append_jsonl, read_jsonl, read_yaml, write_yaml,
};

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
    /// not been called yet.
    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        read_jsonl(&self.projects_index_path())
    }

    /// Creates a new project under this directory. Validates the name (regex +
    /// per-directory uniqueness with hyphen↔underscore + case normalization).
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
        append_jsonl(&self.projects_index_path(), &summary)?;
        Ok(project)
    }

    /// Loads a project by id. Errors if the project is not in this directory's
    /// index.
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
            // Standard NotFound surfaces correctly via the typed Io variant.
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
}
