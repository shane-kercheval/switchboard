//! User-global workspace **view-state** — which projects the user has
//! archived.
//!
//! That is the only thing that lives here. It is a presentation choice with no
//! on-disk counterpart in the project itself, which is why it is user-global
//! rather than stored beside the data.
//!
//! **Nothing here is load-bearing, and that is what makes best-effort loading
//! safe.** The store's `projects.jsonl` is the record of what exists; a missing
//! or corrupt `workspace.yaml` costs the user their archived choices, never a
//! project.
//!
//! This file used to also carry the ordered list of working directories (and,
//! before that, a cached copy of each directory's projects). Both are gone: the
//! store records each project's directory on its own row, so the list had
//! nothing left to say. `serde` ignores the retired `entries` and
//! `cached_projects` keys, so an existing `workspace.yaml` loads unchanged.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use switchboard_core::{CoreError, ProjectId};

use crate::error::AppError;

/// The archived set, as `workspace.yaml` persists it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
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
    /// Set (or clear) a project's archived flag. Returns whether the set
    /// actually changed, so callers persist `workspace.yaml` only on a real
    /// change.
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
        assert_eq!(outcome.workspace, Workspace::default());
        assert!(outcome.persistable, "a fresh install must be persistable");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("workspace.yaml");

        let mut workspace = Workspace::default();
        workspace.set_archived(Uuid::new_v4(), true);
        workspace.set_archived(Uuid::new_v4(), true);

        save(&path, &workspace).unwrap();
        let outcome = load(&path);
        assert_eq!(outcome.workspace, workspace);
        assert!(outcome.persistable);
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
    fn a_workspace_yaml_carrying_retired_keys_still_loads_its_archived_set() {
        // The directory list, its project cache, and directory hiding were all
        // removed after shipping, so a real file can still carry every one of
        // those keys. The struct does not deny unknown fields, so they are
        // ignored and dropped on the next write — a stale key must never cost
        // the user their archived choices.
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        let archived = Uuid::new_v4();
        std::fs::write(
            &path,
            format!(
                "entries:\n- path: /a\n  cached_projects:\n  - id: 0192f0c0-0000-7000-8000-000000000000\n    name: alpha\n    created_at: 2026-01-01T00:00:00Z\nhidden:\n- /a\narchived:\n- {archived}\n"
            ),
        )
        .unwrap();

        let loaded = load(&path).workspace;
        assert!(loaded.is_archived(archived));
        let mut expected = Workspace::default();
        expected.set_archived(archived, true);
        assert_eq!(loaded, expected);
    }

    #[test]
    fn corrupt_file_loads_empty_but_persistable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("workspace.yaml");
        std::fs::write(&path, "this: is: not: valid: yaml: [").unwrap();

        let outcome = load(&path);
        assert_eq!(outcome.workspace, Workspace::default());
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
        assert_eq!(outcome.workspace, Workspace::default());
        assert!(
            !outcome.persistable,
            "an unreadable existing file must disable persistence so it is never clobbered"
        );
    }
}
