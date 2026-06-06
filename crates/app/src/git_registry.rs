//! User-global tracked-repo registry for the Git view — the ordered set of git
//! repository roots the user wants to see.
//!
//! A twin of [`crate::workspace`] in shape and persistence (a user-global YAML
//! file, graceful degradation on a bad read), but distinct in purpose: this is
//! the set of repos tracked for *visibility*, a superset of the directories that
//! host Switchboard projects. It stores **paths only** — never any git state.
//! Branch/worktree/status data is always read live (cheap to recompute, and
//! dangerous to show stale).
//!
//! Entries are **canonical main-repo roots** (resolved via
//! `switchboard_git::resolve_repo_root`): a subdirectory or a linked worktree of
//! a repo all dedup to the one root. A missing or corrupt `git-view.yaml`
//! degrades to an empty registry rather than failing app startup.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use switchboard_core::CoreError;

use crate::error::AppError;

/// The ordered set of tracked repo roots. Order is insertion order — the UI
/// renders repos in the sequence the user added them. This is exactly the shape
/// that persists to `git-view.yaml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRegistry {
    roots: Vec<PathBuf>,
}

impl GitRegistry {
    /// Add a repo root. Idempotent: a second add of an already-tracked path is a
    /// no-op preserving its position. Callers pass an already-resolved canonical
    /// root (so subdirectories / linked worktrees of one repo collapse to a
    /// single entry); we compare as-given, mirroring [`crate::workspace::Workspace`].
    pub fn add(&mut self, root: PathBuf) {
        if self.contains(&root) {
            return;
        }
        self.roots.push(root);
    }

    /// Drop the entry for `root`. Returns whether an entry was removed.
    pub fn remove(&mut self, root: &Path) -> bool {
        let before = self.roots.len();
        self.roots.retain(|r| r != root);
        self.roots.len() != before
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn contains(&self, root: &Path) -> bool {
        self.roots.iter().any(|r| r == root)
    }
}

/// Outcome of reading the registry: the value to use this session plus whether
/// persisting *over the file we read* is safe.
pub struct LoadOutcome {
    pub registry: GitRegistry,
    /// `false` only when the file exists but the **read itself** failed (I/O):
    /// the file may hold a real registry we couldn't parse, so the session must
    /// not overwrite it. A missing file (fresh install) and a corrupt-YAML file
    /// are both `true` — there's nothing recoverable on disk to clobber. Same
    /// three-case contract as [`crate::workspace::load`].
    pub persistable: bool,
}

/// Read the registry from `path`. Never fails: a bad file degrades to empty
/// rather than aborting startup. See [`LoadOutcome::persistable`] for the
/// unreadable-vs-corrupt distinction.
pub fn load(path: &Path) -> LoadOutcome {
    if !path.exists() {
        return LoadOutcome {
            registry: GitRegistry::default(),
            persistable: true,
        };
    }
    match switchboard_core::read_yaml::<GitRegistry>(path) {
        Ok(registry) => LoadOutcome {
            registry,
            persistable: true,
        },
        Err(e @ CoreError::CorruptYaml { .. }) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "git-view.yaml is corrupt — resetting to an empty registry; a fresh save will replace it"
            );
            LoadOutcome {
                registry: GitRegistry::default(),
                persistable: true,
            }
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "git-view.yaml could not be read — persistence disabled this session to avoid overwriting it"
            );
            LoadOutcome {
                registry: GitRegistry::default(),
                persistable: false,
            }
        }
    }
}

/// Persist the registry to `path`, creating the parent directory if needed.
/// Atomic temp-write + rename via `switchboard_core::write_yaml`.
pub fn save(path: &Path, registry: &GitRegistry) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AppError::GitRegistryPersist {
            path: path.to_owned(),
            source,
        })?;
    }
    switchboard_core::write_yaml(path, registry)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn add_is_idempotent_and_preserves_order() {
        let mut reg = GitRegistry::default();
        reg.add(PathBuf::from("/a"));
        reg.add(PathBuf::from("/b"));
        reg.add(PathBuf::from("/a"));

        let roots: Vec<&Path> = reg.roots().iter().map(PathBuf::as_path).collect();
        assert_eq!(roots, vec![Path::new("/a"), Path::new("/b")]);
    }

    #[test]
    fn remove_drops_entry_and_reports() {
        let mut reg = GitRegistry::default();
        reg.add(PathBuf::from("/a"));

        assert!(reg.remove(Path::new("/a")));
        assert!(!reg.contains(Path::new("/a")));
        assert!(!reg.remove(Path::new("/a")));
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("git-view.yaml");

        let mut reg = GitRegistry::default();
        reg.add(PathBuf::from("/a"));
        reg.add(PathBuf::from("/b"));

        save(&path, &reg).unwrap();
        let outcome = load(&path);
        assert_eq!(outcome.registry, reg);
        assert!(outcome.persistable);
    }

    #[test]
    fn load_missing_file_returns_empty_and_persistable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("git-view.yaml");
        let outcome = load(&path);
        assert!(outcome.registry.roots().is_empty());
        assert!(outcome.persistable, "a fresh install must be persistable");
    }

    #[test]
    fn corrupt_file_loads_empty_but_persistable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("git-view.yaml");
        std::fs::write(&path, "this: is: not: valid: yaml: [").unwrap();

        let outcome = load(&path);
        assert!(outcome.registry.roots().is_empty());
        assert!(outcome.persistable);
    }

    #[test]
    fn unreadable_file_loads_empty_and_not_persistable() {
        // A path that exists but is a directory forces an I/O read error rather
        // than a parse error — the dangerous case the registry must never
        // overwrite.
        let dir = tempdir().unwrap();
        let path = dir.path().join("git-view.yaml");
        std::fs::create_dir(&path).unwrap();

        let outcome = load(&path);
        assert!(outcome.registry.roots().is_empty());
        assert!(
            !outcome.persistable,
            "an unreadable existing file must disable persistence"
        );
    }
}
