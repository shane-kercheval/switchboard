//! The user-global project store.
//!
//! # Why projects moved out of working directories
//!
//! Switchboard originally kept each project's state at
//! `<working-directory>/.switchboard/`, on the theory that a self-contained
//! directory travels with its repo. Two things falsified that:
//!
//! 1. **It doesn't survive the directory.** Deleting a worktree — routine, and
//!    the normal end of a short-lived branch — destroyed the journal, the agent
//!    registry, and the pins for every project in it, with no warning and no
//!    recovery. The state the user cares about outlived the checkout it happened
//!    to be created in.
//! 2. **The original rationale is gone.** Directory-scoped state made sense when
//!    prompts and workflows lived there too. Both are user-global now
//!    (system-design §3/§6), so all that remained in `.switchboard/` was runtime
//!    data every repo had to `.gitignore` — a per-repo tax for something the
//!    repo never wanted.
//!
//! So the store is user-global and the *working directory* becomes a reference:
//! a catalog entry mapping a stable [`DirectoryId`] to a path. Re-pointing that
//! path is what lets a project outlive a moved or deleted checkout, and it is
//! why the id is minted rather than derived from the path.
//!
//! # Layout
//!
//! ```text
//! <store-root>/
//!   store.yaml            schema version (fail-loud)
//!   projects.jsonl        the global project index
//!   directories.jsonl     the catalog: directory_id -> path
//!   attachments/          store-wide staged attachment files
//!   projects/<id>/        config.yaml, registry.jsonl, journal.jsonl, pins.jsonl, sessions/, runs/
//! ```
//!
//! # Injected root
//!
//! [`Store::open`] takes the root explicitly; core never resolves the OS config
//! dir itself (the same posture as `switchboard_prompts`). That is what lets
//! tests point at a `TempDir`, the migration tool target an explicit `--target-root`,
//! and the app decide between its release and dev roots — a decision core has no
//! business making.

use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{CoreError, Result};
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_jsonl, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::paths::{
    ATTACHMENTS_DIR, CONFIG_FILE, DIRECTORIES_CATALOG, JOURNAL_FILE, PROJECTS_DIR, PROJECTS_INDEX,
    STORE_CONFIG_FILE,
};
use crate::project::{self, PROJECT_CONFIG_VERSION, Project, ProjectConfig, ProjectId};

/// Bumped only for a layout change old builds cannot read. Checked fail-loud on
/// every [`Store::open`], so a downgrade refuses rather than silently
/// misinterpreting `projects.jsonl` / `directories.jsonl`.
pub const STORE_VERSION: u32 = 1;

/// Stable identity for a working directory, minted on first registration.
///
/// **Not derived from the path**, which is the entire point: the path is
/// mutable state (a repo gets moved, a worktree gets recreated elsewhere) and
/// the id is what projects reference, so re-pointing is a one-line catalog
/// rewrite instead of a rewrite of every project entry.
pub type DirectoryId = Uuid;

/// One line of `store.yaml`'s worth of state: the schema marker.
///
/// **One version for the whole store, not one per file.** `projects.jsonl` and
/// `directories.jsonl` are written by the same code and always evolve together,
/// so two markers could only ever disagree — and a reader that trusted one while
/// the other was stale is exactly the failure a version check exists to prevent.
/// (The plan called for per-file markers; this is the same guarantee with one
/// place to check it.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreConfig {
    pub version: u32,
}

/// A working directory the user works in, as the catalog records it.
///
/// The `path` is canonical at the time it was added or re-pointed, and is **not
/// revalidated on read** — a project whose directory has since been deleted must
/// still list, rename, archive, and delete. Whether the path currently resolves
/// is a per-call question for whoever needs to dispatch into it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub directory_id: DirectoryId,
    pub path: PathBuf,
}

/// One line of the global project index.
///
/// A distinct type from [`crate::ProjectSummary`] rather than that type plus an
/// `Option<DirectoryId>`. A store entry *always* has an owning directory, and an
/// optional field would force every read site to handle a case that cannot
/// occur; the legacy `.switchboard/projects.jsonl` entries that genuinely lack
/// one keep their own type, which is also what the migration tool reads. The
/// conversion happens once, at migration, instead of at every call site forever.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEntry {
    pub id: ProjectId,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub directory_id: DirectoryId,
}

/// The user-global store. Cheap to construct and holds no cached state, so
/// callers may keep one or build one per operation.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open the store at `root`, creating the layout if it does not exist.
    ///
    /// Idempotent: an existing store is left intact and only its version is
    /// checked. Unlike `Directory::at`/`init` there is no separate "is it
    /// initialized" question — there is exactly one store and the app must be
    /// able to create it on first launch, so splitting the two would only
    /// produce a state no caller wants to be in.
    ///
    /// Fails loud on a version mismatch. A store written by a newer build is not
    /// read with today's assumptions; a corrupt `store.yaml` surfaces rather
    /// than being reinterpreted as a fresh store, which would shadow the user's
    /// real projects behind an empty list.
    pub fn open(root: &Path) -> Result<Store> {
        create_dir_all(root).map_err(|e| CoreError::io(root, e))?;
        let store = Store {
            root: root.to_path_buf(),
        };
        create_dir_all(store.projects_dir()).map_err(|e| CoreError::io(store.projects_dir(), e))?;
        create_dir_all(store.attachments_dir())
            .map_err(|e| CoreError::io(store.attachments_dir(), e))?;

        let config_path = store.config_path();
        if config_path.exists() {
            let config = read_yaml::<StoreConfig>(&config_path)?;
            if config.version != STORE_VERSION {
                return Err(CoreError::UnsupportedConfigVersion {
                    path: config_path,
                    found: config.version,
                    expected: STORE_VERSION,
                });
            }
        } else {
            write_yaml(
                &config_path,
                &StoreConfig {
                    version: STORE_VERSION,
                },
            )?;
        }
        // Touch both append-only files so a missing one later is corruption
        // rather than ambiguity — the same distinction `Directory::list_projects`
        // draws between "not initialized" and "index vanished".
        for path in [store.projects_index_path(), store.catalog_path()] {
            if !path.exists() {
                std::fs::write(&path, "").map_err(|e| CoreError::io(&path, e))?;
            }
        }
        Ok(store)
    }

    /// The store root, for callers that need to site something beside it (the
    /// session-lock root, a migration report).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store-wide staging for attachment files.
    ///
    /// **Store-wide, not per-project**: the same file is routinely dropped into
    /// sends in more than one project, and a per-project copy would duplicate it
    /// per project while making cross-project reference counting impossible for
    /// the GC. Reference-GC is what makes one shared directory safe.
    #[must_use]
    pub fn attachments_dir(&self) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR)
    }

    // ---- catalog -------------------------------------------------------

    /// Every registered working directory.
    pub fn list_directories(&self) -> Result<Vec<DirectoryEntry>> {
        let path = self.catalog_path();
        if !path.exists() {
            return Err(CoreError::MissingAppendOnlyFile { path });
        }
        read_jsonl(&path)
    }

    /// Register a working directory, or return the existing entry if this path
    /// is already catalogued.
    ///
    /// Idempotent **by canonical path**, so re-adding a directory the user
    /// already works in cannot mint a second id — which would split one
    /// directory's projects across two catalog entries and make a later
    /// re-point fix only half of them. The path must exist to be added
    /// (canonicalization requires it); a directory that disappears *later*
    /// keeps its entry.
    pub fn add_directory(&self, path: &Path) -> Result<DirectoryEntry> {
        let canonical = std::fs::canonicalize(path).map_err(|e| CoreError::io(path, e))?;
        if !canonical.is_dir() {
            return Err(CoreError::NotADirectory { path: canonical });
        }
        if let Some(existing) = self
            .list_directories()?
            .into_iter()
            .find(|e| e.path == canonical)
        {
            return Ok(existing);
        }
        let entry = DirectoryEntry {
            directory_id: Uuid::now_v7(),
            path: canonical,
        };
        append_jsonl(&self.catalog_path(), &entry)?;
        Ok(entry)
    }

    /// Point an existing catalog entry at a new path — the affordance stable ids
    /// exist for. This is how a project whose checkout was moved or recreated
    /// becomes dispatchable again without touching a single project record.
    ///
    /// Rewrite-on-mutate over the otherwise append-only catalog, matching
    /// [`Self::rename_project`]'s posture on the index.
    pub fn repoint_directory(&self, id: DirectoryId, new_path: &Path) -> Result<DirectoryEntry> {
        let canonical = std::fs::canonicalize(new_path).map_err(|e| CoreError::io(new_path, e))?;
        if !canonical.is_dir() {
            return Err(CoreError::NotADirectory { path: canonical });
        }
        let mut entries = self.list_directories()?;
        let idx = entries
            .iter()
            .position(|e| e.directory_id == id)
            .ok_or(CoreError::DirectoryNotFound(id))?;
        entries[idx].path = canonical;
        let updated = entries[idx].clone();
        write_jsonl(&self.catalog_path(), &entries)?;
        Ok(updated)
    }

    /// The path a `directory_id` currently resolves to.
    ///
    /// **Catalog entries are never deleted while any project references them.**
    /// A dangling id would leave those projects unopenable with no way to
    /// re-point them — the id is the only handle the repair affordance has. So
    /// "the user is done with this directory" is expressed by hiding it in
    /// view-state, never by dropping the catalog row.
    pub fn directory_path(&self, id: DirectoryId) -> Result<PathBuf> {
        self.list_directories()?
            .into_iter()
            .find(|e| e.directory_id == id)
            .map(|e| e.path)
            .ok_or(CoreError::DirectoryNotFound(id))
    }

    // ---- projects ------------------------------------------------------

    /// Every project in the store, across all directories.
    ///
    /// A missing index file is corruption (`MissingAppendOnlyFile`), not "no
    /// projects" — [`Store::open`] creates it, so its absence means it was
    /// removed out of band, and reporting an empty list would invite the caller
    /// to overwrite it.
    pub fn list_projects(&self) -> Result<Vec<ProjectEntry>> {
        let path = self.projects_index_path();
        if !path.exists() {
            return Err(CoreError::MissingAppendOnlyFile { path });
        }
        read_jsonl(&path)
    }

    /// Create a project owned by `directory_id`.
    ///
    /// Name uniqueness is **per directory**, not store-wide: two unrelated
    /// checkouts each having an `api` project is ordinary, and the old
    /// directory-scoped layout allowed it. Widening to store-wide here would
    /// reject names users already have.
    ///
    /// # Atomicity
    ///
    /// The project directory is created first, then the entry is appended to
    /// `projects.jsonl`. If the append fails we **do not** delete the project
    /// directory: the append is the commit step, and (because `append_jsonl`
    /// fsyncs *after* writing) an append error does not prove the line is
    /// absent — it may already be on disk. Deleting the directory after a
    /// possible commit is exactly what would leave a dangling index entry
    /// pointing at a missing project. So on append failure we keep the
    /// directory and surface the error. The worst case is a benign orphan
    /// directory: it has no index entry, so `list_projects` never surfaces it
    /// and its UUID is unreachable; a retry mints a fresh UUID.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the same store — the
    /// read-check-then-append sequence has a TOCTOU window. Callers serialize
    /// (the app's `registry_write` mutex does this).
    pub fn create_project(&self, directory_id: DirectoryId, name: &str) -> Result<Project> {
        let directory = self.directory_path(directory_id)?;
        validate_name(name)?;
        let canonical = canonicalize_for_uniqueness(name);
        for existing in self.list_projects()? {
            if existing.directory_id == directory_id
                && canonicalize_for_uniqueness(&existing.name) == canonical
            {
                return Err(CoreError::DuplicateProjectName {
                    name: name.to_owned(),
                    existing: existing.name,
                });
            }
        }

        let (summary, project) = project::create_on_disk(&directory, &self.projects_dir(), name)?;
        // No destructive rollback on append failure — see "Atomicity" above.
        let entry = ProjectEntry {
            id: summary.id,
            name: summary.name,
            created_at: summary.created_at,
            directory_id,
        };
        append_jsonl(&self.projects_index_path(), &entry)?;
        Ok(project)
    }

    /// Load a project by id, resolving its working directory through the catalog.
    ///
    /// **Does not require the working directory to exist.** That is the whole
    /// point of the move: a project whose checkout was deleted still opens, so
    /// it can be listed, renamed, archived, deleted, or re-pointed. Only
    /// dispatch (and the cwd-dependent features around it) needs a live path,
    /// and that is checked where it matters, not here.
    ///
    /// # Concurrency
    ///
    /// Same serialization requirement as [`Self::create_project`].
    pub fn open_project(&self, id: ProjectId) -> Result<Project> {
        let entry = self
            .list_projects()?
            .into_iter()
            .find(|e| e.id == id)
            .ok_or(CoreError::ProjectNotFound(id))?;
        let directory = self.directory_path(entry.directory_id)?;
        project::load(&directory, id, self.project_root(id))
    }

    /// Rename a project: same directory, new display name. Validates the new
    /// name's format and its per-directory canonicalized uniqueness against the
    /// *other* projects (self excluded, so re-saving the same name — or a
    /// case/hyphen variant — is allowed), then **dual-writes** both copies of
    /// the project's identity: the canonical `config.yaml` and the denormalized
    /// index entry.
    ///
    /// # Atomicity / partial-write contract
    ///
    /// The two writes can't be made atomic without a journal, so the order is
    /// deliberate: rewrite `config.yaml` (canonical) **first**, then
    /// `projects.jsonl` (the index every UI surface reads) as the **commit**.
    /// If the index write fails — `Err` returned, *or* a crash between the two —
    /// `config.yaml` is left "ahead" with the new name while the index still
    /// holds the old one. This is benign and parallels [`Self::create_project`]'s
    /// orphan-directory note: the index is what lists/renders, so the user sees
    /// the old name (consistent with the failure they were shown), and the next
    /// successful rename of the same project reconciles both files. Hence an
    /// `Err` from this method does **not** guarantee nothing changed on disk —
    /// callers must not treat it as "no-op". We deliberately do **not** roll back
    /// `config.yaml`: rolling back after a possible commit is the exact
    /// anti-pattern `io::append_jsonl` warns against, and `write_jsonl`'s
    /// post-rename dir-fsync window means an `Err` doesn't even prove the index
    /// is still old.
    ///
    /// # Concurrency
    ///
    /// Same serialization requirement as [`Self::create_project`].
    pub fn rename_project(&self, id: ProjectId, new_name: &str) -> Result<ProjectEntry> {
        validate_name(new_name)?;
        let mut entries = self.list_projects()?;
        let idx = entries
            .iter()
            .position(|e| e.id == id)
            .ok_or(CoreError::ProjectNotFound(id))?;
        let owner = entries[idx].directory_id;
        let canonical = canonicalize_for_uniqueness(new_name);
        for (i, existing) in entries.iter().enumerate() {
            if i == idx || existing.directory_id != owner {
                continue;
            }
            if canonicalize_for_uniqueness(&existing.name) == canonical {
                return Err(CoreError::DuplicateProjectName {
                    name: new_name.to_owned(),
                    existing: existing.name.clone(),
                });
            }
        }

        // Rewrite the canonical config.yaml first, then the denormalized index
        // (the commit). Build the config directly rather than reading it back:
        // `version` is the current constant and `created_at` already lives in
        // the index entry, so a read would only add a disk round-trip and a late
        // failure window *after* uniqueness already passed. (If `ProjectConfig`
        // ever gains a field a rename must preserve, switch back to
        // read-then-mutate here.)
        let config_path = self.project_root(id).join(CONFIG_FILE);
        let config = ProjectConfig {
            version: PROJECT_CONFIG_VERSION,
            name: new_name.to_owned(),
            created_at: entries[idx].created_at,
        };
        write_yaml(&config_path, &config)?;

        new_name.clone_into(&mut entries[idx].name);
        let updated = entries[idx].clone();
        write_jsonl(&self.projects_index_path(), &entries)?;
        Ok(updated)
    }

    /// Permanently delete a project's Switchboard state: drop its index entry,
    /// then recursively remove `projects/<id>/` (config, registry, journal,
    /// pins, sessions, runs). **Scoped to the store's projects dir** — never the
    /// working directory, never a sibling project, never a harness-native
    /// session file (`~/.claude/…`, `~/.codex/…`, …).
    ///
    /// Note what this no longer does: attachments are store-wide now, so there
    /// is no per-project attachments directory to remove. Orphaned files are
    /// reclaimed by the reference-GC, which is the only component that can see
    /// whether another project still references them.
    ///
    /// # Atomicity / ordering / failure model
    ///
    /// Index-rewrite first (**the commit**), then `remove_dir_all`. Dropping the
    /// index entry is the point at which the project stops existing — once it
    /// returns, the project no longer lists. The directory removal that follows
    /// is **best-effort**: a leftover directory with no index entry is a benign,
    /// unreachable orphan, exactly the tolerated state [`Self::create_project`]
    /// leaves when its post-directory index append fails. So a failed removal is
    /// **not** an error here — surfacing one would imply "nothing was deleted,"
    /// but the listing is already gone. The reverse order (rmdir then index) is
    /// what we avoid: a removed directory with a surviving index entry *would*
    /// surface as a broken listing.
    ///
    /// The **only** failures this returns are reading or rewriting the index
    /// (the steps that actually change what lists).
    ///
    /// # Idempotency
    ///
    /// A missing project is a benign no-op: if `id` isn't in the index the
    /// rewrite is skipped, and a missing directory is ignored. A double-delete
    /// (or deleting a project removed out-of-band) returns `Ok(())`.
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        // A genuine read failure (I/O, corruption) propagates — we must not
        // rewrite an index we couldn't read, or we'd lose sibling entries.
        let mut entries = match self.list_projects() {
            Ok(entries) => entries,
            Err(CoreError::MissingAppendOnlyFile { .. }) => Vec::new(),
            Err(e) => return Err(e),
        };
        let before = entries.len();
        entries.retain(|e| e.id != id);
        // Rewrite only when an entry was actually dropped — a double-delete must
        // not churn the index, and a never-existent id must not recreate it.
        // This is the commit: once it returns Ok, the project no longer lists.
        if entries.len() != before {
            write_jsonl(&self.projects_index_path(), &entries)?;
        }
        // Best-effort directory removal (see "failure model" above).
        let _ = std::fs::remove_dir_all(self.project_root(id));
        Ok(())
    }

    /// Best-effort "last activity" timestamp for a project, used to order the
    /// cross-directory project list by recency. Returns the later of the
    /// project's conversation-journal modification time and `fallback`
    /// (typically the project's `created_at`).
    ///
    /// The journal is appended on every user send and every non-completed-turn
    /// outcome, so its mtime is a cheap recency proxy that needs no transcript
    /// parse — `O(1)` per project, safe to call for every project at startup. It
    /// reflects *send* time, not the eventual response time, for a completed
    /// turn; that's close enough for ordering. A missing or unreadable journal
    /// (never-dispatched project) yields `fallback`.
    #[must_use]
    pub fn project_last_activity(&self, id: ProjectId, fallback: DateTime<Utc>) -> DateTime<Utc> {
        let journal = self.project_root(id).join(JOURNAL_FILE);
        let mtime = std::fs::metadata(&journal)
            .and_then(|m| m.modified())
            .ok()
            .map(DateTime::<Utc>::from);
        match mtime {
            Some(t) if t > fallback => t,
            _ => fallback,
        }
    }

    /// Where a project's metadata lives. Public because the harness crate needs
    /// the project root to site its sidecars, and re-deriving the layout there
    /// is what this move exists to stop.
    #[must_use]
    pub fn project_root(&self, id: ProjectId) -> PathBuf {
        self.projects_dir().join(id.to_string())
    }

    fn projects_dir(&self) -> PathBuf {
        self.root.join(PROJECTS_DIR)
    }
    fn projects_index_path(&self) -> PathBuf {
        self.root.join(PROJECTS_INDEX)
    }
    fn catalog_path(&self) -> PathBuf {
        self.root.join(DIRECTORIES_CATALOG)
    }
    fn config_path(&self) -> PathBuf {
        self.root.join(STORE_CONFIG_FILE)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use tempfile::TempDir;

    /// A store plus one catalogued working directory — the shape almost every
    /// test needs, since a project cannot exist without an owning directory.
    fn store_with_dir() -> (TempDir, TempDir, Store, DirectoryId) {
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let dir = store.add_directory(cwd.path()).unwrap();
        (root, cwd, store, dir.directory_id)
    }

    #[test]
    fn open_creates_the_layout_and_is_idempotent() {
        let root = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let (_cwd, id) = {
            let cwd = TempDir::new().unwrap();
            let entry = store.add_directory(cwd.path()).unwrap();
            (cwd, entry.directory_id)
        };
        store.create_project(id, "alpha").unwrap();

        // Re-opening an existing store must leave it intact, not reinitialize it.
        let reopened = Store::open(root.path()).unwrap();
        assert_eq!(reopened.list_projects().unwrap().len(), 1);
        assert_eq!(reopened.list_directories().unwrap().len(), 1);
    }

    #[test]
    fn open_rejects_a_store_written_by_another_schema() {
        let root = TempDir::new().unwrap();
        Store::open(root.path()).unwrap();
        write_yaml(
            &root.path().join(STORE_CONFIG_FILE),
            &StoreConfig {
                version: STORE_VERSION + 1,
            },
        )
        .unwrap();

        // Fail loud rather than reinterpreting a newer layout with today's
        // assumptions — an empty-and-overwrite would shadow the user's projects.
        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::UnsupportedConfigVersion { .. }),
            "expected a version error, got {err:?}"
        );
    }

    #[test]
    fn missing_index_is_corruption_not_an_empty_store() {
        let (root, _cwd, store, _id) = store_with_dir();
        std::fs::remove_file(root.path().join(PROJECTS_INDEX)).unwrap();

        let err = store.list_projects().unwrap_err();
        assert!(
            matches!(err, CoreError::MissingAppendOnlyFile { .. }),
            "expected MissingAppendOnlyFile, got {err:?}"
        );
    }

    #[test]
    fn corrupt_index_line_surfaces_typed_error() {
        let (root, _cwd, store, id) = store_with_dir();
        store.create_project(id, "alpha").unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(root.path().join(PROJECTS_INDEX))
            .unwrap();
        writeln!(f, "{{garbage").unwrap();

        match store.list_projects().unwrap_err() {
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
    fn corrupt_catalog_line_surfaces_typed_error() {
        let (root, _cwd, store, _id) = store_with_dir();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(root.path().join(DIRECTORIES_CATALOG))
            .unwrap();
        writeln!(f, "{{garbage").unwrap();

        assert!(
            matches!(
                store.list_directories().unwrap_err(),
                CoreError::CorruptJsonl { .. }
            ),
            "the catalog gets the same fail-loud treatment as the index"
        );
    }

    #[test]
    fn adding_the_same_directory_twice_reuses_its_id() {
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();

        let first = store.add_directory(cwd.path()).unwrap();
        let second = store.add_directory(cwd.path()).unwrap();

        // A second id would split one directory's projects across two catalog
        // entries, so a later re-point would fix only half of them.
        assert_eq!(first.directory_id, second.directory_id);
        assert_eq!(store.list_directories().unwrap().len(), 1);
    }

    #[test]
    fn adding_a_directory_canonicalizes_so_a_symlink_is_not_a_second_entry() {
        let root = TempDir::new().unwrap();
        let real = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let store = Store::open(root.path()).unwrap();

        let direct = store.add_directory(real.path()).unwrap();
        let via_link = store.add_directory(&link).unwrap();
        assert_eq!(direct.directory_id, via_link.directory_id);
    }

    #[test]
    fn repointing_a_directory_moves_every_project_in_it_at_once() {
        let (_root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        let moved = TempDir::new().unwrap();

        store.repoint_directory(id, moved.path()).unwrap();

        // The project records nothing about the path, so one catalog write is
        // the whole repair — that is what the stable id buys.
        let reopened = store.open_project(project.id).unwrap();
        assert_eq!(
            reopened.directory,
            std::fs::canonicalize(moved.path()).unwrap()
        );
    }

    #[test]
    fn repointing_an_unknown_directory_is_a_typed_error() {
        let (_root, _cwd, store, _id) = store_with_dir();
        let elsewhere = TempDir::new().unwrap();
        assert!(matches!(
            store
                .repoint_directory(Uuid::now_v7(), elsewhere.path())
                .unwrap_err(),
            CoreError::DirectoryNotFound(_)
        ));
    }

    #[test]
    fn a_project_opens_after_its_working_directory_is_deleted() {
        // The entire point of the move: state outlives the checkout. Listing,
        // renaming, and deleting must all keep working so the user can re-point
        // or clean up.
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let id = store.add_directory(cwd.path()).unwrap().directory_id;
        let project = store.create_project(id, "alpha").unwrap();

        // Compare against the canonical form the catalog stored, not the raw
        // temp path (macOS resolves /var -> /private/var).
        let gone = std::fs::canonicalize(cwd.path()).unwrap();
        drop(cwd);
        assert!(!gone.exists());

        assert_eq!(store.list_projects().unwrap().len(), 1);
        let opened = store.open_project(project.id).unwrap();
        assert_eq!(opened.config.name, "alpha");
        assert_eq!(opened.directory, gone);
        store.rename_project(project.id, "renamed").unwrap();
        store.delete_project(project.id).unwrap();
        assert!(store.list_projects().unwrap().is_empty());
    }

    #[test]
    fn a_dangling_directory_id_surfaces_cleanly_rather_than_panicking() {
        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        // Catalog entries are never dropped while a project references them, so
        // this state is corruption — it must surface as a typed error, not a
        // panic or a silently absent project.
        std::fs::write(root.path().join(DIRECTORIES_CATALOG), "").unwrap();

        assert!(matches!(
            store.open_project(project.id).unwrap_err(),
            CoreError::DirectoryNotFound(_)
        ));
        // The project still *lists* — listing reads only the index, so the user
        // can see what is broken.
        assert_eq!(store.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn project_names_are_unique_per_directory_not_store_wide() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let a = store.add_directory(one.path()).unwrap().directory_id;
        let b = store.add_directory(two.path()).unwrap().directory_id;

        store.create_project(a, "api").unwrap();
        // Two unrelated checkouts each having an `api` project is ordinary, and
        // the pre-store layout allowed it. Widening to store-wide uniqueness
        // would reject names users already have.
        store.create_project(b, "api").unwrap();

        // Within one directory it still collides, under canonicalization
        // (case-folded, hyphen and underscore equivalent).
        store.create_project(a, "web-ui").unwrap();
        let err = store.create_project(a, "Web_UI").unwrap_err();
        assert!(matches!(err, CoreError::DuplicateProjectName { .. }));
    }

    #[test]
    fn renaming_collides_only_within_the_owning_directory() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let a = store.add_directory(one.path()).unwrap().directory_id;
        let b = store.add_directory(two.path()).unwrap().directory_id;
        store.create_project(a, "alpha").unwrap();
        let other = store.create_project(b, "beta").unwrap();

        // A sibling directory's `alpha` is not a collision.
        assert_eq!(
            store.rename_project(other.id, "alpha").unwrap().name,
            "alpha"
        );
    }

    #[test]
    fn renaming_to_a_variant_of_its_own_name_is_allowed() {
        let (_root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha-one").unwrap();
        assert_eq!(
            store.rename_project(project.id, "Alpha_One").unwrap().name,
            "Alpha_One"
        );
    }

    #[test]
    fn rename_persists_to_both_config_and_index() {
        let (_root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();

        let updated = store.rename_project(project.id, "renamed").unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(
            updated.directory_id, id,
            "rename must not re-home a project"
        );
        assert_eq!(store.list_projects().unwrap()[0].name, "renamed");
        assert_eq!(
            store.open_project(project.id).unwrap().config.name,
            "renamed"
        );
    }

    #[test]
    fn rename_of_an_unknown_project_is_not_found() {
        let (_root, _cwd, store, _id) = store_with_dir();
        assert!(matches!(
            store.rename_project(Uuid::now_v7(), "x").unwrap_err(),
            CoreError::ProjectNotFound(_)
        ));
    }

    // Unix-only: drives the commit-step failure via file permission bits (the
    // crate's durability hardening is itself `cfg(unix)`).
    #[cfg(unix)]
    #[test]
    fn rename_index_write_failure_leaves_config_ahead_and_retry_heals() {
        use std::os::unix::fs::PermissionsExt;

        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();

        // `write_jsonl` writes `<index>.tmp` then renames over the index, so a
        // read-only index *file* wouldn't block it (rename needs write on the
        // *directory*, not the target). Make the store root read-only so the
        // index tmp can't be created — config.yaml lives in the separate, still
        // writable `projects/<id>/` subdir, so it is rewritten before the index
        // write fails.
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let err = store.rename_project(project.id, "renamed").unwrap_err();
        assert!(
            matches!(err, CoreError::Io { .. }),
            "expected Io, got {err:?}"
        );

        // Partial state: canonical config is "ahead" (new name); the index is
        // stale (old name), so list/UI still show the old name.
        assert_eq!(
            store.open_project(project.id).unwrap().config.name,
            "renamed"
        );
        assert_eq!(store.list_projects().unwrap()[0].name, "alpha");

        // Retry once the root is writable again: the same rename reconciles both
        // files (uniqueness still passes — nothing else is named "renamed").
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            store.rename_project(project.id, "renamed").unwrap().name,
            "renamed"
        );
        assert_eq!(store.list_projects().unwrap()[0].name, "renamed");
    }

    // Unix-only, same rationale as above.
    #[cfg(unix)]
    #[test]
    fn create_keeps_the_directory_and_stays_index_consistent_when_the_append_fails() {
        use std::os::unix::fs::PermissionsExt;

        let (root, _cwd, store, id) = store_with_dir();
        // Exercise the *commit-step* failure: the index stays readable (so the
        // uniqueness pre-check succeeds and the project dir does get created)
        // but unwritable, so the subsequent append fails.
        let index = root.path().join(PROJECTS_INDEX);
        std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o444)).unwrap();

        let err = store.create_project(id, "alpha").unwrap_err();
        assert!(
            matches!(err, CoreError::Io { .. }),
            "expected Io, got {err:?}"
        );

        // No destructive rollback: the created project directory is kept (the
        // append is the commit step; deleting after a possible commit is what
        // would leave a dangling index entry).
        let orphans = std::fs::read_dir(root.path().join(PROJECTS_DIR))
            .unwrap()
            .count();
        assert_eq!(
            orphans, 1,
            "the created project directory must be kept, not rolled back"
        );

        // The orphan has no index entry, so it never surfaces; once the index is
        // writable again a retry succeeds with a fresh UUID.
        std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            store.list_projects().unwrap().is_empty(),
            "an orphan directory (no index entry) must not surface in list_projects"
        );
        let project = store.create_project(id, "alpha").unwrap();
        assert_ne!(
            project.root,
            root.path().join(PROJECTS_DIR).join("orphan"),
            "the retry mints a fresh id"
        );
        assert_eq!(store.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn delete_drops_the_entry_and_removes_the_dir_keeping_siblings() {
        let (_root, _cwd, store, id) = store_with_dir();
        let a = store.create_project(id, "alpha").unwrap();
        let b = store.create_project(id, "beta").unwrap();

        store.delete_project(a.id).unwrap();

        assert!(!a.root.exists());
        assert!(b.root.exists());
        let remaining = store.list_projects().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, b.id);
    }

    #[test]
    fn delete_is_idempotent_and_an_unknown_id_is_a_noop() {
        let (_root, _cwd, store, id) = store_with_dir();
        let a = store.create_project(id, "alpha").unwrap();

        store.delete_project(a.id).unwrap();
        store.delete_project(a.id).unwrap();
        store.delete_project(Uuid::now_v7()).unwrap();
        assert!(store.list_projects().unwrap().is_empty());
    }

    #[test]
    fn delete_with_a_missing_index_still_removes_the_directory() {
        let (root, _cwd, store, id) = store_with_dir();
        let a = store.create_project(id, "alpha").unwrap();
        std::fs::remove_file(root.path().join(PROJECTS_INDEX)).unwrap();

        // No entry to drop, but the project directory must still go.
        store.delete_project(a.id).unwrap();
        assert!(!a.root.exists());
    }

    // Unix-only, same rationale as the other permission-driven tests.
    #[cfg(unix)]
    #[test]
    fn delete_rmdir_failure_still_commits_the_index_and_leaves_an_orphan() {
        use std::os::unix::fs::PermissionsExt;

        let (root, _cwd, store, id) = store_with_dir();
        let a = store.create_project(id, "alpha").unwrap();

        // Make `projects/` read-only so the final unlink fails; the index lives
        // in the writable store root, so the commit still succeeds.
        let projects_dir = root.path().join(PROJECTS_DIR);
        std::fs::set_permissions(&projects_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Best-effort removal failure is not surfaced.
        store.delete_project(a.id).unwrap();

        std::fs::set_permissions(&projects_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(store.list_projects().unwrap().is_empty());
        assert!(
            a.root.exists(),
            "a failed rmdir leaves the directory in place"
        );
    }

    #[test]
    fn last_activity_falls_back_without_a_journal_and_prefers_a_newer_one() {
        let (_root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        let fallback = Utc::now();

        assert_eq!(store.project_last_activity(project.id, fallback), fallback);

        std::fs::write(project.root.join(JOURNAL_FILE), "{}\n").unwrap();
        let old_fallback = fallback - chrono::Duration::days(1);
        assert!(
            store.project_last_activity(project.id, old_fallback) > old_fallback,
            "a journal written now must beat a day-old fallback"
        );
    }

    #[test]
    fn attachments_are_store_wide_not_per_project() {
        let (root, _cwd, store, id) = store_with_dir();
        let a = store.create_project(id, "alpha").unwrap();
        let b = store.create_project(id, "beta").unwrap();

        assert_eq!(store.attachments_dir(), root.path().join(ATTACHMENTS_DIR));
        // One shared directory is what lets the same dropped file be referenced
        // from sends in both projects without a per-project copy.
        assert!(!a.root.join(ATTACHMENTS_DIR).exists());
        assert!(!b.root.join(ATTACHMENTS_DIR).exists());
    }
}
