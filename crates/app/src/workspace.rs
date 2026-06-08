//! User-global workspace registry — the ordered set of working directories the
//! app knows about, plus a cached snapshot of each directory's projects.
//!
//! This is the source for the flat cross-directory project list. The cached
//! snapshot lets the UI list projects from a directory that is currently
//! unavailable (unmounted, on a disconnected volume) without reading its
//! `.switchboard/` state. It persists to a user-global `workspace.yaml`
//! resolved via the `directories` crate.
//!
//! The registry is **convenience state, not load-bearing**: directory-local
//! `.switchboard/` state remains the source of truth for what projects exist.
//! A missing or corrupt `workspace.yaml` degrades to an empty registry rather
//! than failing app startup.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use switchboard_core::{CoreError, ProjectId, ProjectSummary};

use crate::error::AppError;

/// One known working directory plus the last-seen snapshot of its projects.
/// This is exactly the shape that persists to `workspace.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub path: PathBuf,
    pub cached_projects: Vec<ProjectSummary>,
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
}

impl Workspace {
    /// Add a directory to the registry. Idempotent: a second add of an
    /// already-known path is a no-op that preserves the existing entry's
    /// position and `cached_projects`. New entries start with no cached
    /// projects (populated later via `refresh_cache`).
    ///
    /// Paths are compared as-given. Callers that want canonicalized identity
    /// (matching `Directory::at`) should canonicalize before adding; we do not
    /// canonicalize here because canonicalization requires the path to exist on
    /// disk and the registry must be able to hold currently-unavailable
    /// directories.
    pub fn add(&mut self, path: PathBuf) {
        if self.contains(&path) {
            return;
        }
        self.entries.push(DirectoryEntry {
            path,
            cached_projects: Vec::new(),
        });
    }

    /// Drop the entry for `path`. Returns whether an entry was removed.
    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.path != path);
        self.entries.len() != before
    }

    /// Replace the cached project snapshot for `path`. No-op if `path` is not a
    /// known entry (we only cache projects for directories the user added).
    /// Returns whether the snapshot actually changed — callers on hot read
    /// paths (`list_projects`) use this to persist `workspace.yaml` only when
    /// something changed, avoiding a write storm on every project switch.
    pub fn refresh_cache(&mut self, path: &Path, projects: Vec<ProjectSummary>) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
            if entry.cached_projects == projects {
                return false;
            }
            entry.cached_projects = projects;
            return true;
        }
        false
    }

    /// Drop a single project id from `path`'s cached snapshot. Used on a delete
    /// whose post-delete index re-read isn't available (e.g. the index file
    /// vanished out-of-band), so the deleted project can't resurrect as a stale
    /// cached row the next time `list_projects` falls back to the cache. No-op
    /// (returns `false`) if `path` is unknown or the id wasn't cached. This is the
    /// targeted form for when the owning directory is known; use
    /// [`Workspace::remove_cached_project_by_id`] when it can't be resolved.
    pub fn remove_cached_project(&mut self, path: &Path, id: ProjectId) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
            let before = entry.cached_projects.len();
            entry.cached_projects.retain(|p| p.id != id);
            return entry.cached_projects.len() != before;
        }
        false
    }

    /// Drop a project id from whichever directory entry caches it, without
    /// needing to know the owning path. Used when deleting a project whose
    /// directory is unreachable (its folder/volume is gone): the persisted
    /// `workspace.yaml` cache is then the only remaining reference, so it must be
    /// pruned by id alone or the unavailable row resurrects on the next list /
    /// restart. Returns whether anything changed.
    pub fn remove_cached_project_by_id(&mut self, id: ProjectId) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            let before = entry.cached_projects.len();
            entry.cached_projects.retain(|p| p.id != id);
            changed |= entry.cached_projects.len() != before;
        }
        changed
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

    /// Whether any known directory's cached snapshot lists this project — i.e.
    /// the project is one the workspace actually knows about. Used to reject
    /// archiving a bogus/typo'd id so it can't accumulate junk in the archived
    /// set. Every project a UI row can target is in a cached snapshot
    /// (`list_projects` / `create_project` refresh it), including projects whose
    /// directory is currently unavailable — which is exactly the set archive
    /// must still work for.
    pub fn knows_project(&self, id: ProjectId) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.cached_projects.iter().any(|p| p.id == id))
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
    use chrono::Utc;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;

    fn summary(name: &str) -> ProjectSummary {
        ProjectSummary {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            created_at: Utc::now(),
        }
    }

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
        workspace.refresh_cache(Path::new("/a"), vec![summary("alpha"), summary("beta")]);

        save(&path, &workspace).unwrap();
        let outcome = load(&path);
        assert_eq!(outcome.workspace, workspace);
        assert!(outcome.persistable);
    }

    #[test]
    fn add_is_idempotent_and_preserves_order_and_cache() {
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));
        workspace.add(PathBuf::from("/b"));
        workspace.refresh_cache(Path::new("/a"), vec![summary("alpha")]);

        workspace.add(PathBuf::from("/a"));

        let paths: Vec<&Path> = workspace
            .entries()
            .iter()
            .map(|e| e.path.as_path())
            .collect();
        assert_eq!(paths, vec![Path::new("/a"), Path::new("/b")]);
        assert_eq!(workspace.entries()[0].cached_projects.len(), 1);
    }

    #[test]
    fn remove_drops_entry_and_reports() {
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));

        assert!(workspace.remove(Path::new("/a")));
        assert!(!workspace.contains(Path::new("/a")));
        assert!(!workspace.remove(Path::new("/a")));
    }

    #[test]
    fn refresh_cache_replaces_and_is_noop_for_unknown() {
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));

        workspace.refresh_cache(Path::new("/a"), vec![summary("one")]);
        workspace.refresh_cache(Path::new("/a"), vec![summary("two"), summary("three")]);
        assert_eq!(workspace.entries()[0].cached_projects.len(), 2);

        workspace.refresh_cache(Path::new("/unknown"), vec![summary("x")]);
        assert_eq!(workspace.entries().len(), 1);
    }

    #[test]
    fn refresh_cache_reports_whether_snapshot_changed() {
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));

        let one = summary("one");
        assert!(
            workspace.refresh_cache(Path::new("/a"), vec![one.clone()]),
            "first non-empty snapshot is a change"
        );
        assert!(
            !workspace.refresh_cache(Path::new("/a"), vec![one.clone()]),
            "an identical snapshot is not a change"
        );
        assert!(
            workspace.refresh_cache(Path::new("/a"), vec![one, summary("two")]),
            "a differing snapshot is a change"
        );
        assert!(
            !workspace.refresh_cache(Path::new("/unknown"), vec![summary("x")]),
            "an unknown path is never a change"
        );
    }

    #[test]
    fn remove_cached_project_drops_one_id() {
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));
        let keep = summary("keep");
        let drop = summary("drop");
        workspace.refresh_cache(Path::new("/a"), vec![keep.clone(), drop.clone()]);

        assert!(workspace.remove_cached_project(Path::new("/a"), drop.id));
        assert_eq!(workspace.entries()[0].cached_projects, vec![keep]);
        // Removing an absent id, or operating on an unknown path, is a no-op.
        assert!(!workspace.remove_cached_project(Path::new("/a"), drop.id));
        assert!(!workspace.remove_cached_project(Path::new("/unknown"), keep_id(&workspace)));
    }

    fn keep_id(workspace: &Workspace) -> uuid::Uuid {
        workspace.entries()[0].cached_projects[0].id
    }

    #[test]
    fn remove_cached_project_by_id_drops_it_from_any_entry() {
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));
        workspace.add(PathBuf::from("/b"));
        let keep = summary("keep");
        let drop = summary("drop");
        workspace.refresh_cache(Path::new("/a"), vec![keep.clone()]);
        workspace.refresh_cache(Path::new("/b"), vec![drop.clone()]);

        // Removes the id without being told which directory holds it.
        assert!(workspace.remove_cached_project_by_id(drop.id));
        assert!(!workspace.knows_project(drop.id));
        assert!(workspace.knows_project(keep.id));
        // A second removal of the now-absent id is a no-op.
        assert!(!workspace.remove_cached_project_by_id(drop.id));
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
    fn knows_project_checks_cached_snapshots() {
        let mut workspace = Workspace::default();
        workspace.add(PathBuf::from("/a"));
        let known = summary("known");
        workspace.refresh_cache(Path::new("/a"), vec![known.clone()]);

        assert!(workspace.knows_project(known.id));
        assert!(!workspace.knows_project(Uuid::new_v4()));
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
