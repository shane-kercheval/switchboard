use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::project::{self, Project, ProjectId, ProjectSummary};

use crate::paths::{
    CONFIG_FILE, JOURNAL_FILE, PROJECTS_DIR, PROJECTS_INDEX, SWITCHBOARD_DIR, WORKFLOWS_DIR,
};

const DIRECTORY_CONFIG_VERSION: u32 = 1;

/// On-disk shape of `<directory>/.switchboard/config.yaml`. Mostly empty in v1;
/// placeholder for future directory-scoped config (it carries only a schema
/// version today). Prompt providers are user-global, not directory-scoped —
/// their config lives in the user-global `config.yaml` (system-design §6), not
/// here.
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

    /// Creates `<path>/.switchboard/{config.yaml, workflows/, projects.jsonl,
    /// projects/}` if missing. Idempotent — calling twice on the same directory
    /// leaves existing structure intact. (Prompts are user-global, not
    /// directory-scoped, so there is no per-directory `prompts/` dir — see §6.)
    pub fn init(&self) -> Result<()> {
        let sb = self.switchboard_dir();
        create_dir_all(&sb).map_err(|e| CoreError::io(&sb, e))?;
        create_dir_all(sb.join(WORKFLOWS_DIR))
            .map_err(|e| CoreError::io(sb.join(WORKFLOWS_DIR), e))?;
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
    /// `projects.jsonl`. If the append fails we **do not** delete the project
    /// directory: the append is the commit step, and (because `append_jsonl`
    /// fsyncs *after* writing) an append error does not prove the line is
    /// absent — it may already be on disk. Deleting the directory after a
    /// possible commit is exactly what would leave a dangling index entry
    /// pointing at a missing project. So on append failure we keep the
    /// directory and surface the error. The worst case is a benign orphan
    /// directory: it has no `projects.jsonl` entry, so `list_projects` never
    /// surfaces it and its UUID is unreachable; a retry mints a fresh UUID.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the *same `Directory` instance* —
    /// the read-check-then-append sequence has a TOCTOU window. Callers must
    /// serialize access (the dispatcher / `AppState` mutex does this).
    /// Concurrent calls against *different* `Directory` instances (different
    /// directories) are fine; cross-process serialization within one
    /// directory is future work.
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
        // No destructive rollback on append failure — see "Atomicity" above.
        // The directory stays; an orphan (no index entry) is benign.
        append_jsonl(&self.projects_index_path(), &summary)?;
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

    /// Best-effort "last activity" timestamp for a project, used to order the
    /// cross-directory project list by recency. Returns the later of the
    /// project's conversation-journal modification time and `fallback`
    /// (typically the project's `created_at`).
    ///
    /// The journal (`journal.jsonl`) is appended on every user send and every
    /// non-completed-turn outcome, so its mtime is a cheap recency proxy that
    /// needs no transcript parse — `O(1)` per project, safe to call for every
    /// project at startup. It reflects *send* time, not the eventual response
    /// time, for a completed turn; that's close enough for ordering. A missing
    /// or unreadable journal (never-dispatched project) yields `fallback`.
    pub fn project_last_activity(&self, id: ProjectId, fallback: DateTime<Utc>) -> DateTime<Utc> {
        let journal = self.projects_dir().join(id.to_string()).join(JOURNAL_FILE);
        let mtime = std::fs::metadata(&journal)
            .and_then(|m| m.modified())
            .ok()
            .map(DateTime::<Utc>::from);
        match mtime {
            Some(t) if t > fallback => t,
            _ => fallback,
        }
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
    fn project_last_activity_without_journal_returns_fallback() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        let project = directory.create_project("alpha").unwrap();

        // No dispatch has happened, so there is no journal file — the recency
        // key falls back to the supplied timestamp (the project's created_at).
        let fallback = DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            directory.project_last_activity(project.id, fallback),
            fallback
        );
    }

    #[test]
    fn project_last_activity_uses_journal_mtime_when_newer() {
        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        let project = directory.create_project("alpha").unwrap();
        // Touch the journal so it exists with a current mtime.
        std::fs::write(project.journal_path(), b"{}\n").unwrap();

        // A fallback far in the past must lose to the just-written journal's
        // mtime; a fallback far in the future must win (journal isn't newer).
        let past = DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let future = DateTime::parse_from_rfc3339("2999-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(directory.project_last_activity(project.id, past) > past);
        assert_eq!(directory.project_last_activity(project.id, future), future);
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

    // Unix-only: drives the commit-step failure via file permission bits
    // (the crate's durability hardening is itself `cfg(unix)`).
    #[cfg(unix)]
    #[test]
    fn create_project_keeps_directory_and_stays_index_consistent_when_index_append_fails() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let directory = Directory::at(tmp.path()).unwrap();
        directory.init().unwrap();
        // Exercise the *commit-step* failure: make projects.jsonl readable
        // (so the uniqueness pre-check `list_projects` succeeds and the
        // project dir does get created) but unwritable (so the subsequent
        // append fails). Replacing it with a directory — the prior approach —
        // instead failed the pre-check before any dir was created, so it never
        // tested the rollback path at all.
        let index = directory.projects_index_path();
        std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o444)).unwrap();

        let err = directory.create_project("alpha").unwrap_err();
        assert!(
            matches!(err, CoreError::Io { .. }),
            "expected Io error, got {err:?}"
        );

        // No destructive rollback: the created project directory is kept (the
        // append is the commit step; deleting after a possible commit is what
        // would leave a dangling index entry).
        let orphans = std::fs::read_dir(directory.projects_dir()).unwrap().count();
        assert_eq!(
            orphans, 1,
            "the created project directory must be kept, not rolled back"
        );

        // The orphan has no index entry, so it never surfaces; once the index
        // is writable again, list_projects ignores the orphan and a retry
        // succeeds with a fresh UUID.
        std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            directory.list_projects().unwrap().is_empty(),
            "orphan directory (no index entry) must not surface in list_projects"
        );
        let project = directory.create_project("alpha").unwrap();
        assert_eq!(project.config.name, "alpha");
        assert_eq!(directory.list_projects().unwrap().len(), 1);
    }
}
