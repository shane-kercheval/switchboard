//! User-global workspace **view-state** — how the user has arranged the
//! directories and projects the store holds.
//!
//! Three things live here and nothing else: the ordered set of known directory
//! paths, which of those the user has hidden, and which projects they have
//! archived. All three are presentation choices with no on-disk counterpart,
//! which is why they are user-global rather than stored beside the data.
//!
//! **Nothing here is load-bearing, and that is what makes best-effort loading
//! safe.** The store's `projects.jsonl` and `directories.jsonl` are the record
//! of what exists; a missing or corrupt `workspace.yaml` costs the user their
//! ordering and their hidden/archived choices, never a project. This file used
//! to also cache each directory's project list so an unavailable directory
//! could still be listed — the store makes that redundant, because the index
//! lives under the store root and is readable whether or not any working
//! directory is. Removing the cache removes the one thing in here that *was*
//! load-bearing, and with it the class of bug where a stale snapshot
//! resurrected a deleted project.
//!
//! `serde` ignores the removed `cached_projects` key, so an existing
//! `workspace.yaml` loads unchanged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use switchboard_core::{CoreError, ProjectId};

use crate::error::AppError;

/// One known working directory, as `workspace.yaml` persists it.
///
/// Path only. The projects that live in it come from the store index, keyed by
/// `directory_id`, not from anything recorded here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub path: PathBuf,
}

/// The ordered set of known directories. Order is insertion order — the UI
/// renders directories in the sequence the user added them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    entries: Vec<DirectoryEntry>,
    /// Projects the user has archived (hidden from the default view). This is
    /// **user-global view-state**, not on-disk project state: it lives here so
    /// archive works even when a project's directory is offline, and so it never
    /// touches the project's own files. `BTreeSet` keeps `workspace.yaml`
    /// deterministically ordered. `#[serde(default)]` so an older file without
    /// the field loads as "nothing archived" — no migration.
    #[serde(default)]
    archived: BTreeSet<ProjectId>,
    /// Directories the user has hidden from their list.
    ///
    /// **Hiding is what "remove directory" now does.** Under the store, a
    /// directory's catalog entry is referenced by every project in it and can
    /// never be deleted while any of them exists, so "remove" cannot mean
    /// "forget" without orphaning those projects. It means "stop showing me
    /// this", which is view-state and belongs here. Hiding stops nothing:
    /// projects in a hidden directory keep running, and unhiding is lossless.
    ///
    /// Keyed by path so the entry and its hidden flag share one key.
    /// `#[serde(default)]` so an older file loads as "nothing hidden".
    #[serde(default)]
    hidden: BTreeSet<PathBuf>,
}

impl Workspace {
    /// Add a directory to the registry. Idempotent: a second add of an
    /// already-known path is a no-op that preserves the existing entry's
    /// position. Adding a hidden directory unhides it — the user asking for it
    /// again is the same gesture as unhiding.
    ///
    /// Paths are compared as-given. Callers that want canonicalized identity
    /// (matching `Directory::at`) should canonicalize before adding; we do not
    /// canonicalize here because canonicalization requires the path to exist on
    /// disk and the registry must be able to hold currently-unavailable
    /// directories.
    pub fn add(&mut self, path: PathBuf) {
        self.hidden.remove(&path);
        if self.contains(&path) {
            return;
        }
        self.entries.push(DirectoryEntry { path });
    }

    /// Hide (or unhide) a directory. Returns whether the set changed.
    pub fn set_hidden(&mut self, path: &Path, hidden: bool) -> bool {
        if hidden {
            self.hidden.insert(path.to_path_buf())
        } else {
            self.hidden.remove(path)
        }
    }

    pub fn is_hidden(&self, path: &Path) -> bool {
        self.hidden.contains(path)
    }

    pub fn entries(&self) -> &[DirectoryEntry] {
        &self.entries
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.entries.iter().any(|entry| entry.path == path)
    }

    /// Set (or clear) a project's archived flag. Returns whether the set
    /// actually changed, so callers persist `workspace.yaml` only on a real
    /// change (mirrors `refresh_cache`).
    pub fn set_archived(&mut self, id: ProjectId, archived: bool) -> bool {
        if archived {
            self.archived.insert(id)
        } else {
            self.archived.remove(&id)
        }
    }

    pub fn is_archived(&self, id: ProjectId) -> bool {
        self.archived.contains(&id)
    }
}

/// Outcome of reading the workspace registry: the registry to use this session
/// plus whether persisting *over the file we read* is safe.
pub struct LoadOutcome {
    pub workspace: Workspace,
    /// `false` only when the file exists but the **read itself** failed
    /// (permissions, transient filesystem error). The file may hold a real
    /// registry we simply couldn't parse, so the session must not overwrite it:
    /// persistence is disabled (`workspace_path` left `None`) and the
    /// established on-disk set is preserved for the next launch. A missing file
    /// (fresh install) and a corrupt-YAML file are both `true` — there is
    /// nothing recoverable on disk to clobber.
    pub persistable: bool,
}

/// Read the workspace registry from `path`. Never fails: the registry is
/// convenience state, so a bad file degrades to empty rather than aborting
/// startup. The three cases are distinguished deliberately (see
/// [`LoadOutcome::persistable`]) — note the intentional tradeoff that an
/// unreadable file yields an empty *non-persistable* session, so any directory
/// the user adds during that session is dropped on save (a no-op) rather than
/// overwriting the directory set we failed to read; losing one session's
/// additions is strictly better than nuking the user's whole established set.
pub fn load(path: &Path) -> LoadOutcome {
    if !path.exists() {
        return LoadOutcome {
            workspace: Workspace::default(),
            persistable: true,
        };
    }
    match switchboard_core::read_yaml::<Workspace>(path) {
        Ok(workspace) => LoadOutcome {
            workspace,
            persistable: true,
        },
        // Corrupt YAML is unrecoverable garbage — degrade to empty and allow a
        // fresh write to replace it. Logged loudly so the reset is diagnosable.
        Err(e @ CoreError::CorruptYaml { .. }) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "workspace.yaml is corrupt — resetting to an empty registry; a fresh save will replace it"
            );
            LoadOutcome {
                workspace: Workspace::default(),
                persistable: true,
            }
        }
        // The file exists but could not be read (I/O). It may hold a real
        // registry — show empty this session but disable persistence so we never
        // overwrite directories we merely failed to read.
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "workspace.yaml could not be read — persistence disabled this session to avoid overwriting it"
            );
            LoadOutcome {
                workspace: Workspace::default(),
                persistable: false,
            }
        }
    }
}

/// Persist the workspace registry to `path`, creating the parent directory if
/// needed. Atomic temp-write + rename via `switchboard_core::write_yaml`.
pub fn save(path: &Path, workspace: &Workspace) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AppError::WorkspacePersist {
            path: path.to_owned(),
            source,
        })?;
    }
    switchboard_core::write_yaml(path, workspace)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn load_missing_file_returns_empty_and_persistable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        let outcome = load(&path);
        assert!(outcome.workspace.entries().is_empty());
        assert!(outcome.persistable, "a fresh install must be persistable");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("workspace.yaml");

        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));
        workspace.add(PathBuf::from("/b"));

        save(&path, &workspace).unwrap();
        let outcome = load(&path);
        assert_eq!(outcome.workspace, workspace);
        assert!(outcome.persistable);
    }

    #[test]
    fn add_is_idempotent_and_preserves_order() {
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));
        workspace.add(PathBuf::from("/b"));

        workspace.add(PathBuf::from("/a"));

        let paths: Vec<&Path> = workspace
            .entries()
            .iter()
            .map(|e| e.path.as_path())
            .collect();
        assert_eq!(paths, vec![Path::new("/a"), Path::new("/b")]);
    }

    #[test]
    fn set_archived_reports_change_and_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        let mut workspace = Workspace::default();
        let id = Uuid::new_v4();

        assert!(
            !workspace.is_archived(id),
            "unknown id defaults to not archived"
        );
        assert!(workspace.set_archived(id, true), "archiving is a change");
        assert!(
            !workspace.set_archived(id, true),
            "re-archiving is not a change"
        );
        assert!(workspace.is_archived(id));

        save(&path, &workspace).unwrap();
        assert!(
            load(&path).workspace.is_archived(id),
            "archived state survives a round-trip"
        );

        assert!(workspace.set_archived(id, false), "unarchiving is a change");
        assert!(!workspace.is_archived(id));
        assert!(
            !workspace.set_archived(id, false),
            "clearing an absent id is not a change"
        );
    }

    #[test]
    fn archived_defaults_empty_for_old_workspace_yaml() {
        // A pre-archive `workspace.yaml` has no `archived` key; it must load as
        // "nothing archived" (serde default), not fail.
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        std::fs::write(&path, "entries: []\n").unwrap();

        let workspace = load(&path).workspace;
        assert!(!workspace.is_archived(Uuid::new_v4()));
    }

    #[test]
    fn hiding_a_directory_keeps_its_entry_and_adding_it_back_unhides() {
        // "Remove directory" is a view-state gesture now: the catalog entry is
        // referenced by every project in the directory and cannot be dropped, so
        // hiding must be lossless and reversible by the ordinary add path.
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));

        assert!(workspace.set_hidden(Path::new("/a"), true));
        assert!(workspace.is_hidden(Path::new("/a")));
        assert!(
            workspace.contains(Path::new("/a")),
            "hiding must not drop the entry"
        );
        assert!(!workspace.set_hidden(Path::new("/a"), true));

        workspace.add(PathBuf::from("/a"));
        assert!(!workspace.is_hidden(Path::new("/a")));
        assert_eq!(workspace.entries().len(), 1, "unhiding is not a second add");
    }

    #[test]
    fn hidden_defaults_empty_for_an_older_workspace_yaml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        std::fs::write(&path, "entries: []\n").unwrap();

        assert!(!load(&path).workspace.is_hidden(Path::new("/a")));
    }

    #[test]
    fn a_workspace_yaml_holding_the_removed_project_cache_still_loads() {
        // The cache key is gone from the struct; serde must ignore it rather
        // than fail, or an existing install loses its directory list.
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        std::fs::write(
            &path,
            "entries:\n- path: /a\n  cached_projects:\n  - id: 0192f0c0-0000-7000-8000-000000000000\n    name: alpha\n    created_at: 2026-01-01T00:00:00Z\n",
        )
        .unwrap();

        let workspace = load(&path).workspace;
        assert!(workspace.contains(Path::new("/a")));
    }

    #[test]
    fn corrupt_file_loads_empty_but_persistable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        std::fs::write(&path, "this: is: not: valid: yaml: [").unwrap();

        let outcome = load(&path);
        assert!(outcome.workspace.entries().is_empty());
        // Corrupt → unrecoverable, so a fresh save may replace it.
        assert!(outcome.persistable);
    }

    #[test]
    fn unreadable_file_loads_empty_and_not_persistable() {
        // A path that exists but isn't a regular file (a directory) forces an
        // I/O read error rather than a parse error — the dangerous case the
        // registry must never overwrite.
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        std::fs::create_dir(&path).unwrap();

        let outcome = load(&path);
        assert!(outcome.workspace.entries().is_empty());
        assert!(
            !outcome.persistable,
            "an unreadable existing file must disable persistence so it is never clobbered"
        );
    }
}
