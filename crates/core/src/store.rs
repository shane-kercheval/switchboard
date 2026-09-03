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
//! So the store is user-global and a project's *working directory* is a path
//! recorded on its index row. Changing that path is what lets a project outlive
//! a moved or deleted checkout.
//!
//! # Why the path lives on the project, not in a catalog
//!
//! Version 1 of this layout kept a second file, `directories.jsonl`, mapping a
//! minted directory id to a path, and projects referenced the id. The stated
//! reason — re-pointing a moved folder updates one row instead of every project
//! sharing it — bought nothing: the index is one file rewritten whole and
//! atomically, so updating three rows costs the same as updating one. What the
//! indirection did cost was a whole vocabulary of failure (a row whose id had no
//! catalog entry, an id with two entries) that ordinary use never produced but
//! every reader had to handle, and a repair that moved projects the user had
//! not asked about. A project now says where it lives; a directory is nothing
//! more than the paths projects name.
//!
//! # Layout
//!
//! ```text
//! <store-root>/
//!   store.yaml            schema version (fail-loud)
//!   projects.jsonl        the global project index: { id, name, created_at, directory }
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

use std::collections::{HashMap, HashSet};
use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::AgentRecord;
use crate::error::{CoreError, Result};
use crate::ids::ProjectId;
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_jsonl, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::paths::{
    CONFIG_FILE, DIRECTORIES_CATALOG_V1, DIRECTORIES_CATALOG_V1_BACKUP, JOURNAL_FILE, PROJECTS_DIR,
    PROJECTS_INDEX, REGISTRY_FILE, STORE_CONFIG_FILE,
};
use crate::project::{self, PROJECT_CONFIG_VERSION, Project, ProjectConfig};

/// Bumped only for a layout change old builds cannot read. Checked fail-loud on
/// every [`Store::open`], so a downgrade refuses rather than silently
/// misinterpreting `projects.jsonl`.
///
/// History: 1 kept a `directories.jsonl` catalog and referenced it by id from
/// each project row; 2 records the path on the row. [`Store::open`] migrates 1
/// to 2 in place.
pub const STORE_VERSION: u32 = 2;

/// The last layout that used a directory catalog; the one [`Store::open`]
/// knows how to migrate from.
const STORE_VERSION_WITH_CATALOG: u32 = 1;

/// One line of `store.yaml`'s worth of state: the schema marker.
///
/// **One version for the whole store, not one per file.** The index and the
/// per-project trees share a *schema* lifecycle — a layout change touches
/// both — so per-file markers could only ever disagree, and a reader trusting
/// one while the other was stale is exactly the failure a version check exists
/// to prevent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreConfig {
    pub version: u32,
}

/// One line of the global project index.
///
/// `directory` is the project's working directory — the agent spawn cwd —
/// canonical at the time it was recorded and **not revalidated on read**. A
/// project whose directory has since been deleted must still list, rename,
/// archive, and delete; whether the path currently exists is a per-call
/// question for whoever needs to dispatch into it.
///
/// A distinct type from [`crate::ProjectSummary`] rather than that type plus an
/// `Option<PathBuf>`. A store entry *always* has a working directory, and an
/// optional field would force every read site to handle a case that cannot
/// occur; the legacy `.switchboard/projects.jsonl` entries that genuinely lack
/// one keep their own type, which is also what the migration tool reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEntry {
    pub id: ProjectId,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub directory: PathBuf,
}

/// A version-1 index row: the directory is an id into the retired catalog.
/// Read only by the migration. `directory` is accepted too so a migration
/// interrupted after the index rewrite but before the marker re-runs cleanly.
#[derive(Debug, Deserialize)]
struct ProjectEntryV1 {
    id: ProjectId,
    name: String,
    created_at: DateTime<Utc>,
    #[serde(default)]
    directory_id: Option<Uuid>,
    #[serde(default)]
    directory: Option<PathBuf>,
}

/// A version-1 catalog row. Read only by the migration.
#[derive(Debug, Deserialize)]
struct DirectoryEntryV1 {
    directory_id: Uuid,
    path: PathBuf,
}

/// The `config.yaml` shape version 1 wrote: the recovery copy was the catalog
/// id. Read only by the migration, to carry the rest of the file across.
#[derive(Debug, Deserialize)]
struct ProjectConfigV1 {
    name: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open the store at `root`, creating the layout if it does not exist and
    /// migrating a version-1 store in place.
    ///
    /// Idempotent: an existing store is left intact and only its version is
    /// checked. Unlike `Directory::at`/`init` there is no separate "is it
    /// initialized" question — there is exactly one store and the app must be
    /// able to create it on first launch, so splitting the two would only
    /// produce a state no caller wants to be in.
    ///
    /// Fails loud on a version this build cannot read, and — for an
    /// already-initialized store — on a missing index.
    ///
    /// **Nothing here is ever recreated over data.** The index is the record of
    /// what exists; recreating a lost one turns "this store is damaged" into
    /// "you have no projects", which is both false (every project's data is
    /// still under `projects/`) and self-worsening, because ordinary use then
    /// appends to the empty file while the real entries sit orphaned. The
    /// `MissingAppendOnlyFile` check in [`Self::list_projects`] only means
    /// anything if this method leaves the absence intact for it to find.
    ///
    /// That guarantee needs **two** gates, because the marker and the index are
    /// lost by the same events. With a marker present, a missing index is
    /// refused outright. With the marker *also* gone, the layout is completed
    /// only when [`Self::holds_data`] proves there is nothing to destroy — an
    /// interrupted first launch heals, a store whose root-level files were
    /// taken together does not.
    ///
    /// **`store.yaml` is written last** — after the index — as the
    /// initialization commit. A crash between the two would otherwise leave a
    /// valid version marker over a store with no index, indistinguishable on
    /// the next launch from a healthy empty one. And stamping a marker over
    /// surviving data would be wrong even if the index were intact:
    /// [`STORE_VERSION`] exists to be bumped, so "whatever version is running"
    /// is not a safe answer to "what layout is this data in".
    ///
    /// `projects/` is created *after* the marker, and its absence is neither
    /// checked nor refused. A missing `projects/` may well represent real data
    /// loss, but recreating the empty container cannot worsen it, because a
    /// directory holds no record that later writes append to. Refusing to open
    /// on a missing `projects/` would be worse still: the data is already gone,
    /// and refusal would strand the user with index rows they cannot list or
    /// delete.
    pub fn open(root: &Path) -> Result<Store> {
        create_dir_all(root).map_err(|e| CoreError::io(root, e))?;
        let store = Store {
            root: root.to_path_buf(),
        };

        let config_path = store.config_path();
        if config_path.exists() {
            let config = read_yaml::<StoreConfig>(&config_path)?;
            match config.version {
                STORE_VERSION => {}
                STORE_VERSION_WITH_CATALOG => store.migrate_from_catalog()?,
                found => {
                    return Err(CoreError::UnsupportedConfigVersion {
                        path: config_path,
                        found,
                        expected: STORE_VERSION,
                    });
                }
            }
            let index = store.projects_index_path();
            if !index.exists() {
                return Err(CoreError::MissingAppendOnlyFile { path: index });
            }
        } else if store.holds_data()? {
            return Err(CoreError::StoreDataWithoutVersionMarker {
                root: store.root.clone(),
                marker: config_path,
            });
        } else {
            // Nothing but empty scaffolding: a first launch, or an
            // initialization interrupted before its commit. Complete it.
            let index = store.projects_index_path();
            if !index.exists() {
                std::fs::write(&index, "").map_err(|e| CoreError::io(&index, e))?;
            }
            write_yaml(
                &config_path,
                &StoreConfig {
                    version: STORE_VERSION,
                },
            )?;
        }

        // Below both refusals, so neither leaves a trace in a root it declined
        // to open. Both success paths still need them.
        create_dir_all(store.projects_dir()).map_err(|e| CoreError::io(store.projects_dir(), e))?;
        Ok(store)
    }

    /// Carry a version-1 store (directory catalog, id on each row) to version 2
    /// (path on each row). Runs inside [`Self::open`]; the user does nothing.
    ///
    /// # Ordering, and why it can be interrupted anywhere
    ///
    /// 1. Read the index and the catalog; join each row's id to its path.
    /// 2. Rewrite the index atomically with the path on every row.
    /// 3. Rewrite each project's `config.yaml` with the path as its recovery
    ///    copy.
    /// 4. Write the marker as version 2 — **the commit**.
    /// 5. Rename the catalog aside (best-effort; it is now unreferenced).
    ///
    /// The marker stays at 1 until step 4, so a crash before it re-runs the
    /// migration on the next launch. That re-run is safe because step 1 accepts
    /// rows in either shape: a row that already carries `directory` is taken as
    /// is, and only rows that still carry an id consult the catalog. Steps 2
    /// and 3 are idempotent rewrites.
    ///
    /// # What it refuses, and what it deliberately carries across
    ///
    /// Refused: a row whose id has no catalog entry, or more than one; a
    /// project id listed twice; two directory ids resolving to one path. None
    /// was ever produced by the app — the catalog never deleted a referenced
    /// row, never minted a duplicate, and kept one id per canonical path; the
    /// index minted fresh ids — so each means the files were edited or partially
    /// restored, and guessing would bind a project to the wrong folder (or, for
    /// the shared path, merge two Claude session namespaces whose agents may
    /// already share a session id). The error names the project and the file so
    /// the user can repair by hand; nothing is written before every row has
    /// resolved.
    ///
    /// Carried across unchanged: two projects with the same name at one path.
    /// The app tolerates that at rest — both list, and only creating or
    /// renaming *into* the collision is refused — so blocking every launch over
    /// it would be worse than the collision. Do not add a name check here.
    fn migrate_from_catalog(&self) -> Result<()> {
        let index_path = self.projects_index_path();
        if !index_path.exists() {
            return Err(CoreError::MissingAppendOnlyFile { path: index_path });
        }
        let rows: Vec<ProjectEntryV1> = read_jsonl(&index_path)?;
        let catalog_path = self.catalog_v1_path();
        let catalog: Vec<DirectoryEntryV1> = if catalog_path.exists() {
            read_jsonl(&catalog_path)?
        } else {
            Vec::new()
        };
        let entries = join_v1_rows(rows, catalog, &index_path, &catalog_path)?;
        write_jsonl(&index_path, &entries)?;
        for entry in &entries {
            let config_path = self.project_root(entry.id).join(CONFIG_FILE);
            // The old config is only consulted for the fields the index does
            // not carry — and today it carries all of them, so a config that
            // cannot be read is rebuilt from the row rather than blocking the
            // migration on a file that is itself only a recovery copy.
            let (name, created_at) = match read_yaml::<ProjectConfigV1>(&config_path) {
                Ok(config) => (config.name, config.created_at),
                Err(_) => (entry.name.clone(), entry.created_at),
            };
            write_yaml(
                &config_path,
                &ProjectConfig {
                    version: PROJECT_CONFIG_VERSION,
                    name,
                    created_at,
                    directory: Some(entry.directory.clone()),
                },
            )?;
        }
        write_yaml(
            &self.config_path(),
            &StoreConfig {
                version: STORE_VERSION,
            },
        )?;
        if catalog_path.exists() {
            // Unreferenced from here on. Kept as a backup for one release
            // rather than deleted; a rename failure is not worth refusing a
            // store that is already fully migrated.
            let _ = std::fs::rename(&catalog_path, self.root.join(DIRECTORIES_CATALOG_V1_BACKUP));
        }
        Ok(())
    }

    /// Whether the root holds anything a legitimate pre-marker state could not.
    ///
    /// Nothing can write to the store before [`Self::open`] returns, so an
    /// interrupted initialization has empty scaffolding and nothing else. Any
    /// record in the index, any row in a surviving version-1 catalog, or any
    /// project directory therefore means the marker was lost from an
    /// initialized store rather than never written.
    ///
    /// Emptiness is judged by **content, not file size**: `read_jsonl` skips
    /// blank lines, so a file holding a stray newline is logically empty and a
    /// length check would refuse on it. A corrupt line surfaces as
    /// `CorruptJsonl` rather than the marker error — data with no marker
    /// either way, and the more specific diagnosis is the more useful one.
    ///
    /// `projects/` is the load-bearing half: it is a subdirectory while the
    /// files are siblings at the root, so it is what survives the events that
    /// take the marker and the index together. The `is_dir()` filter on its
    /// scan is load-bearing too — a `.DS_Store` dropped in by Finder would
    /// otherwise read as data and refuse a legitimate first launch.
    ///
    /// **Revisit this predicate whenever the store gains another owned
    /// location** (locks, migration records), asking the same question of each:
    /// can anything under it still be attributed to a project once the index is
    /// gone? If not, it does not belong in the predicate.
    fn holds_data(&self) -> Result<bool> {
        if !read_jsonl::<serde_json::Value>(&self.projects_index_path())?.is_empty() {
            return Ok(true);
        }
        let catalog_v1 = self.catalog_v1_path();
        if catalog_v1.exists() && !read_jsonl::<serde_json::Value>(&catalog_v1)?.is_empty() {
            return Ok(true);
        }
        // Runs before `projects/` is created, so absence is the fresh-root case
        // and must read as "no projects" — propagating it would refuse every
        // first launch, the exact failure this predicate exists to avoid.
        let entries = match std::fs::read_dir(self.projects_dir()) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(CoreError::io(self.projects_dir(), e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| CoreError::io(self.projects_dir(), e))?;
            if entry.path().is_dir() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The store root, for callers that need to site something beside it (the
    /// session-lock root, a migration report).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    // ---- projects ------------------------------------------------------

    /// Every project in the store, across all working directories.
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

    /// Every agent in a project, read straight from its registry — for callers
    /// already holding the index row.
    ///
    /// **Needs no working directory, by design.** `registry.jsonl` lives at
    /// `<store-root>/projects/<id>/`, so a project whose working directory is
    /// missing still has a readable roster. That is what keeps the session-id
    /// uniqueness scans whole when a checkout is gone: they need a registry and
    /// a display name (which the index entry carries), never a cwd.
    ///
    /// **Takes the index row, not a bare id**, so that membership is visible at
    /// the call site instead of asserted in prose. Callers holding only an id
    /// want [`Self::list_project_agents`].
    ///
    /// Same validation as [`Project::list_agents`]; they share one
    /// implementation so neither can drift into a laxer read.
    pub fn read_project_registry(&self, entry: &ProjectEntry) -> Result<Vec<AgentRecord>> {
        project::read_registry(&self.project_root(entry.id).join(REGISTRY_FILE), entry.id)
    }

    /// Every agent in a project by id, without opening it.
    ///
    /// The checked counterpart of [`Self::read_project_registry`]: membership is
    /// verified against the index, so an unknown id is `ProjectNotFound` rather
    /// than a silently empty roster. Same validation as [`Project::list_agents`].
    pub fn list_project_agents(&self, id: ProjectId) -> Result<Vec<AgentRecord>> {
        let entry = self
            .list_projects()?
            .into_iter()
            .find(|e| e.id == id)
            .ok_or(CoreError::ProjectNotFound(id))?;
        self.read_project_registry(&entry)
    }

    /// Create a project whose working directory is `directory`.
    ///
    /// The path is canonicalized here — **this is the identity boundary** — so
    /// two spellings of one folder (a symlink, a `/./`, a relative path) record
    /// the same directory, and per-directory rules see them as one. It must
    /// exist: a project cannot be created in a folder that isn't there.
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
    /// directory and surface the error. The worst case is an orphan directory:
    /// it has no index entry, so `list_projects` never surfaces it and a retry
    /// mints a fresh UUID. The project tree is self-describing (`config.yaml`
    /// carries the name, creation time, and working directory), so an index
    /// rebuild can resurrect an orphan this path deliberately abandoned —
    /// harmless in itself, but it is why a repair tool must treat an unindexed
    /// root as ambiguous rather than as disposable.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the same store — the
    /// read-check-then-append sequence has a TOCTOU window. In-process, callers
    /// serialize (the app's `registry_write` mutex does this).
    ///
    /// Cross-process, the index is one file for the whole install, so a
    /// read-modify-write here can lose a whole project row. Release builds are
    /// single-instance, and dev and release resolve disjoint store roots, so the
    /// only unguarded case is two debug builds launched without a per-instance
    /// config dir. That case is accepted — but note the hazard is a **lost
    /// update** from interleaved read-modify-write, not a torn file:
    /// `write_jsonl` is tmp-plus-rename, so atomicity is not what makes it
    /// survivable.
    pub fn create_project(&self, directory: &Path, name: &str) -> Result<Project> {
        let directory = canonical_existing_directory(directory)?;
        validate_name(name)?;
        let canonical = canonicalize_for_uniqueness(name);
        for existing in self.list_projects()? {
            if existing.directory == directory
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
            directory,
        };
        append_jsonl(&self.projects_index_path(), &entry)?;
        Ok(project)
    }

    /// Load a project by id.
    ///
    /// **Does not require the working directory to exist.** That is the whole
    /// point of the move: a project whose checkout was deleted still opens, so
    /// it can be listed, renamed, archived, deleted, or pointed at a new folder.
    /// Only dispatch (and the cwd-dependent features around it) needs a live
    /// path, and that is checked where it matters, not here.
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
        project::load(&entry.directory, id, self.project_root(id))
    }

    /// Point a project at a new working directory — the repair for a moved or
    /// recreated checkout. Moves **this project only**; a sibling that pointed
    /// at the same old folder is untouched, and repaired separately.
    ///
    /// The destination is canonicalized and must exist. The project's name must
    /// be unique among the projects already at the destination, by the same
    /// per-directory rule [`Self::create_project`] applies — otherwise the move
    /// would create the collision that rule exists to prevent.
    ///
    /// # Atomicity / partial-write contract
    ///
    /// Same shape as [`Self::rename_project`]: `config.yaml` (canonical) is
    /// rewritten first, then the index (the commit). An `Err` therefore does
    /// **not** guarantee nothing changed, and `write_jsonl` fsyncs the parent
    /// *after* the rename, so an `Err` from that last step returns with the new
    /// index already visible. Callers must re-read rather than assume.
    ///
    /// **In-memory `Project`s are not updated.** `Project.directory` is
    /// snapshotted at [`Self::open_project`] and is the dispatch cwd, so a
    /// caller must quiesce any turn running against the old path and drain
    /// before calling, then re-read and rebuild project and actor state from
    /// what it observes. Restoring the pre-call state on `Err` is the failure
    /// mode this contract exists to prevent: the app would keep dispatching into
    /// the old directory while the index names the new one.
    ///
    /// # Concurrency
    ///
    /// Same serialization requirement as [`Self::create_project`].
    pub fn set_project_directory(&self, id: ProjectId, new_path: &Path) -> Result<ProjectEntry> {
        let directory = canonical_existing_directory(new_path)?;
        let mut entries = self.list_projects()?;
        let idx = entries
            .iter()
            .position(|e| e.id == id)
            .ok_or(CoreError::ProjectNotFound(id))?;
        let canonical = canonicalize_for_uniqueness(&entries[idx].name);
        for (i, existing) in entries.iter().enumerate() {
            if i == idx || existing.directory != directory {
                continue;
            }
            if canonicalize_for_uniqueness(&existing.name) == canonical {
                return Err(CoreError::DuplicateProjectName {
                    name: entries[idx].name.clone(),
                    existing: existing.name.clone(),
                });
            }
        }

        write_yaml(
            &self.project_root(id).join(CONFIG_FILE),
            &ProjectConfig {
                version: PROJECT_CONFIG_VERSION,
                name: entries[idx].name.clone(),
                created_at: entries[idx].created_at,
                directory: Some(directory.clone()),
            },
        )?;
        entries[idx].directory = directory;
        let updated = entries[idx].clone();
        write_jsonl(&self.projects_index_path(), &entries)?;
        Ok(updated)
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
        let canonical = canonicalize_for_uniqueness(new_name);
        for (i, existing) in entries.iter().enumerate() {
            if i == idx || existing.directory != entries[idx].directory {
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
        // `version` is the current constant, and `created_at` and `directory`
        // both already live in the index entry, so a read would only add a disk
        // round-trip and a late failure window *after* uniqueness already
        // passed. **This shortcut holds only while every `ProjectConfig` field
        // is recoverable from the index entry** — a field that isn't forces
        // read-then-mutate here, or a rename silently drops it.
        let config_path = self.project_root(id).join(CONFIG_FILE);
        let config = ProjectConfig {
            version: PROJECT_CONFIG_VERSION,
            name: new_name.to_owned(),
            created_at: entries[idx].created_at,
            directory: Some(entries[idx].directory.clone()),
        };
        write_yaml(&config_path, &config)?;

        new_name.clone_into(&mut entries[idx].name);
        let updated = entries[idx].clone();
        write_jsonl(&self.projects_index_path(), &entries)?;
        Ok(updated)
    }

    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        // Any read failure propagates, absence included — we must not rewrite an
        // index we couldn't read (losing sibling entries), nor remove a project
        // root on the authority of a source of truth we couldn't consult.
        let mut entries = self.list_projects()?;
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
    fn catalog_v1_path(&self) -> PathBuf {
        self.root.join(DIRECTORIES_CATALOG_V1)
    }
    fn config_path(&self) -> PathBuf {
        self.root.join(STORE_CONFIG_FILE)
    }
}

/// The join at the heart of the version-1 → 2 migration: every index row gets
/// the path its directory id named, or the migration is refused. See
/// [`Store::migrate_from_catalog`] for what is refused and what is carried.
fn join_v1_rows(
    rows: Vec<ProjectEntryV1>,
    catalog: Vec<DirectoryEntryV1>,
    index_path: &Path,
    catalog_path: &Path,
) -> Result<Vec<ProjectEntry>> {
    let mut paths_by_id: HashMap<Uuid, Vec<PathBuf>> = HashMap::new();
    for entry in catalog {
        paths_by_id
            .entry(entry.directory_id)
            .or_default()
            .push(entry.path);
    }

    let mut entries = Vec::with_capacity(rows.len());
    let mut seen_ids: HashSet<ProjectId> = HashSet::new();
    // Which directory id first reached each path. Version 1's writers kept
    // one id per canonical path; two ids on one path means the catalog was
    // edited, and collapsing them would merge two Claude session
    // namespaces that were kept apart — agents already carrying the same
    // session id under the two ids would then drive one session at once.
    let mut id_for_path: HashMap<PathBuf, Uuid> = HashMap::new();
    for row in rows {
        if !seen_ids.insert(row.id) {
            return Err(CoreError::StoreMigrationBlocked {
                project: row.id,
                name: row.name,
                index: index_path.to_path_buf(),
                reason: "is listed more than once".to_owned(),
            });
        }
        let directory = match (row.directory, row.directory_id) {
            (Some(directory), _) => directory,
            (None, Some(directory_id)) => match paths_by_id.get(&directory_id) {
                Some(paths) if paths.len() == 1 => {
                    let path = paths[0].clone();
                    if let Some(other) = id_for_path.insert(path.clone(), directory_id)
                        && other != directory_id
                    {
                        return Err(CoreError::StoreMigrationBlocked {
                            project: row.id,
                            name: row.name,
                            index: index_path.to_path_buf(),
                            reason: format!(
                                "its directory id {directory_id} and {other} both resolve to {} in {}",
                                path.display(),
                                catalog_path.display()
                            ),
                        });
                    }
                    path
                }
                Some(paths) if paths.is_empty() => {
                    unreachable!("a catalog id is inserted with at least one path")
                }
                Some(_) => {
                    return Err(CoreError::StoreMigrationBlocked {
                        project: row.id,
                        name: row.name,
                        index: index_path.to_path_buf(),
                        reason: format!(
                            "its directory id {directory_id} has more than one row in {}",
                            catalog_path.display()
                        ),
                    });
                }
                None => {
                    return Err(CoreError::StoreMigrationBlocked {
                        project: row.id,
                        name: row.name,
                        index: index_path.to_path_buf(),
                        reason: format!(
                            "its directory id {directory_id} has no row in {}",
                            catalog_path.display()
                        ),
                    });
                }
            },
            (None, None) => {
                return Err(CoreError::StoreMigrationBlocked {
                    project: row.id,
                    name: row.name,
                    index: index_path.to_path_buf(),
                    reason: "its row names neither a directory nor a directory id".to_owned(),
                });
            }
        };
        entries.push(ProjectEntry {
            id: row.id,
            name: row.name,
            created_at: row.created_at,
            directory,
        });
    }
    Ok(entries)
}

/// Canonicalize a working-directory path that must exist and be a directory.
/// The one place a path becomes a project's recorded directory, so every
/// writer identifies a folder the same way.
fn canonical_existing_directory(path: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(path).map_err(|e| CoreError::io(path, e))?;
    if !canonical.is_dir() {
        return Err(CoreError::NotADirectory { path: canonical });
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::io::Write;

    use super::*;
    use tempfile::TempDir;

    /// A store plus one working directory — the shape almost every test needs.
    fn store_with_dir() -> (TempDir, TempDir, Store) {
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        (root, cwd, store)
    }

    fn canonical(dir: &TempDir) -> PathBuf {
        std::fs::canonicalize(dir.path()).unwrap()
    }

    #[test]
    fn open_creates_the_layout_and_is_idempotent() {
        let (root, cwd, store) = store_with_dir();
        store.create_project(cwd.path(), "alpha").unwrap();

        // Re-opening an existing store must leave it intact, not reinitialize it.
        let reopened = Store::open(root.path()).unwrap();
        assert_eq!(reopened.list_projects().unwrap().len(), 1);
        assert_eq!(
            read_yaml::<StoreConfig>(&root.path().join(STORE_CONFIG_FILE))
                .unwrap()
                .version,
            STORE_VERSION
        );
    }

    #[test]
    fn open_rejects_a_store_written_by_a_newer_schema() {
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
        let (root, _cwd, store) = store_with_dir();
        std::fs::remove_file(root.path().join(PROJECTS_INDEX)).unwrap();

        let err = store.list_projects().unwrap_err();
        assert!(
            matches!(err, CoreError::MissingAppendOnlyFile { .. }),
            "expected MissingAppendOnlyFile, got {err:?}"
        );
    }

    #[test]
    fn reopening_an_initialized_store_refuses_a_missing_index_and_recreates_nothing() {
        // The production path. `open` recreating the file would make the guard
        // above unreachable and turn "this store is damaged" into "you have no
        // projects" — while every project's data still sits under `projects/`.
        let (root, _cwd, _store) = store_with_dir();
        let path = root.path().join(PROJECTS_INDEX);
        std::fs::remove_file(&path).unwrap();

        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::MissingAppendOnlyFile { .. }),
            "expected MissingAppendOnlyFile, got {err:?}"
        );
        assert!(
            !path.exists(),
            "the index must be left absent, not healed back into existence"
        );
    }

    #[test]
    fn an_interrupted_initialization_completes_on_the_next_open() {
        // `store.yaml` is written last, so a crash mid-setup leaves empty
        // scaffolding and no marker. Nothing can write to the store before
        // `open` returns, so that state provably holds no data and must heal —
        // refusing would wedge a fresh install that was force-quit.
        for pre_created in [false, true] {
            let root = TempDir::new().unwrap();
            if pre_created {
                // A stray newline is logically empty: `read_jsonl` skips blank
                // lines, so a byte-length test would refuse here.
                std::fs::write(root.path().join(PROJECTS_INDEX), "\n").unwrap();
            }

            let store = Store::open(root.path()).unwrap();
            assert!(root.path().join(STORE_CONFIG_FILE).exists());
            assert!(store.list_projects().unwrap().is_empty());
        }
    }

    #[test]
    fn a_markerless_store_that_holds_data_refuses_instead_of_being_blessed() {
        // The marker and the index are siblings at the root while project data
        // is a subdirectory — a selective restore or a sync conflict takes the
        // files together and leaves the data. Completing initialization there
        // would recreate the index empty and present a store full of projects
        // as empty, which is the failure this whole guard exists to prevent. It
        // is also how a future schema version would silently stamp its marker
        // over another version's data.
        let (root, cwd, store) = store_with_dir();
        store.create_project(cwd.path(), "alpha").unwrap();
        let project_dirs = || {
            std::fs::read_dir(root.path().join(PROJECTS_DIR))
                .unwrap()
                .count()
        };
        assert_eq!(project_dirs(), 1);

        for name in [STORE_CONFIG_FILE, PROJECTS_INDEX] {
            std::fs::remove_file(root.path().join(name)).unwrap();
        }
        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::StoreDataWithoutVersionMarker { .. }),
            "expected StoreDataWithoutVersionMarker, got {err:?}"
        );
        assert!(
            !root.path().join(STORE_CONFIG_FILE).exists(),
            "a refusal must not stamp a marker"
        );
        assert!(
            !root.path().join(PROJECTS_INDEX).exists(),
            "a refusal must not recreate the index"
        );
        assert_eq!(project_dirs(), 1, "the surviving data is untouched");
    }

    #[test]
    fn a_markerless_store_refuses_on_a_surviving_index_too() {
        // `projects/` is the signal that survives losing the index, but an
        // index holding a record is equally proof the marker was lost from an
        // initialized store rather than never written.
        let (root, cwd, store) = store_with_dir();
        store.create_project(cwd.path(), "alpha").unwrap();
        std::fs::remove_dir_all(root.path().join(PROJECTS_DIR)).unwrap();
        std::fs::remove_file(root.path().join(STORE_CONFIG_FILE)).unwrap();

        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::StoreDataWithoutVersionMarker { .. }),
            "expected refusal with the index surviving, got {err:?}"
        );
    }

    #[test]
    fn a_markerless_store_refuses_on_a_surviving_version_1_catalog() {
        // A version-1 store that lost its marker and index but kept the catalog
        // is still data from an initialized store; blessing it would leave the
        // catalog beside a fresh index that never gets migrated.
        let root = TempDir::new().unwrap();
        std::fs::write(
            root.path().join(DIRECTORIES_CATALOG_V1),
            format!(
                "{{\"directory_id\":\"{}\",\"path\":\"/somewhere\"}}\n",
                Uuid::now_v7()
            ),
        )
        .unwrap();

        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::StoreDataWithoutVersionMarker { .. }),
            "expected refusal with the catalog surviving, got {err:?}"
        );
    }

    #[test]
    fn a_refusal_leaves_no_trace_in_the_root_it_declined() {
        // `create_dir_all` for `projects/` sits below both refusal branches so
        // a declined root is left exactly as found.
        let root = TempDir::new().unwrap();
        let projects = root.path().join(PROJECTS_DIR);
        std::fs::create_dir_all(projects.join(Uuid::now_v7().to_string())).unwrap();

        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::StoreDataWithoutVersionMarker { .. }),
            "expected StoreDataWithoutVersionMarker, got {err:?}"
        );
        let created: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            created,
            vec![std::ffi::OsString::from(PROJECTS_DIR)],
            "a refusal must add nothing to the root"
        );
    }

    #[test]
    fn a_crash_between_the_marker_and_the_index_cannot_look_like_an_empty_store() {
        // The ordering this test pins: if the marker were written first, this
        // state would be reachable and would read as a healthy empty store on
        // the next launch. Simulated directly, since the real window no longer
        // exists.
        let root = TempDir::new().unwrap();
        Store::open(root.path()).unwrap();
        std::fs::remove_file(root.path().join(PROJECTS_INDEX)).unwrap();

        assert!(matches!(
            Store::open(root.path()).unwrap_err(),
            CoreError::MissingAppendOnlyFile { .. }
        ));
    }

    #[test]
    fn open_rejects_an_unparseable_marker() {
        let root = TempDir::new().unwrap();
        Store::open(root.path()).unwrap();
        std::fs::write(root.path().join(STORE_CONFIG_FILE), "{{{ not yaml").unwrap();

        // Corruption in the marker is not "no marker" — it must not fall through
        // to the initialize path and stamp a fresh one over a live store.
        assert!(Store::open(root.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join(STORE_CONFIG_FILE)).unwrap(),
            "{{{ not yaml",
            "the damaged marker must survive, not be overwritten"
        );
    }

    #[test]
    fn corrupt_index_line_surfaces_typed_error() {
        let (root, cwd, store) = store_with_dir();
        store.create_project(cwd.path(), "alpha").unwrap();
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

    // ---- version-1 migration ------------------------------------------

    /// Lay down a version-1 store by hand: marker at 1, index rows carrying
    /// `directory_id`, a catalog, and a `config.yaml` per project in the old
    /// shape. Returns the project ids in `rows` order.
    fn write_v1_store(root: &Path, rows: &[(&str, Uuid)], catalog: &[(Uuid, &Path)]) -> Vec<Uuid> {
        std::fs::create_dir_all(root.join(PROJECTS_DIR)).unwrap();
        write_yaml(
            &root.join(STORE_CONFIG_FILE),
            &StoreConfig {
                version: STORE_VERSION_WITH_CATALOG,
            },
        )
        .unwrap();
        let mut index = String::new();
        let mut ids = Vec::new();
        for (name, directory_id) in rows {
            let id = Uuid::now_v7();
            ids.push(id);
            writeln!(
                index,
                "{{\"id\":\"{id}\",\"name\":\"{name}\",\"created_at\":\"2026-08-01T00:00:00Z\",\"directory_id\":\"{directory_id}\"}}"
            )
            .unwrap();
            let project_root = root.join(PROJECTS_DIR).join(id.to_string());
            std::fs::create_dir_all(&project_root).unwrap();
            std::fs::write(
                project_root.join(CONFIG_FILE),
                format!(
                    "version: 1\nname: {name}\ncreated_at: 2026-08-01T00:00:00Z\ndirectory_id: {directory_id}\n"
                ),
            )
            .unwrap();
            std::fs::write(project_root.join(REGISTRY_FILE), "").unwrap();
        }
        std::fs::write(root.join(PROJECTS_INDEX), index).unwrap();
        let mut catalog_lines = String::new();
        for (directory_id, path) in catalog {
            writeln!(
                catalog_lines,
                "{{\"directory_id\":\"{directory_id}\",\"path\":{}}}",
                serde_json::to_string(path).unwrap()
            )
            .unwrap();
        }
        std::fs::write(root.join(DIRECTORIES_CATALOG_V1), catalog_lines).unwrap();
        ids
    }

    #[test]
    fn opening_a_version_1_store_migrates_it_to_paths_on_the_rows() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let dir_a = Uuid::now_v7();
        let dir_b = Uuid::now_v7();
        let ids = write_v1_store(
            root.path(),
            &[("alpha", dir_a), ("beta", dir_a), ("gamma", dir_b)],
            &[(dir_a, one.path()), (dir_b, two.path())],
        );

        let store = Store::open(root.path()).unwrap();

        let entries = store.list_projects().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].directory, one.path());
        assert_eq!(entries[1].directory, one.path());
        assert_eq!(entries[2].directory, two.path());
        assert_eq!(entries[0].name, "alpha");
        // The project opens against the migrated path, and its config carries
        // the path as its recovery copy.
        let gamma = store.open_project(ids[2]).unwrap();
        assert_eq!(gamma.directory, two.path());
        assert_eq!(gamma.config.directory.as_deref(), Some(two.path()));
        assert_eq!(gamma.config.name, "gamma");
        // The commit: marker at 2, the catalog set aside, not deleted.
        assert_eq!(
            read_yaml::<StoreConfig>(&root.path().join(STORE_CONFIG_FILE))
                .unwrap()
                .version,
            STORE_VERSION
        );
        assert!(!root.path().join(DIRECTORIES_CATALOG_V1).exists());
        assert!(root.path().join(DIRECTORIES_CATALOG_V1_BACKUP).exists());
        // Reopening is an ordinary version-2 open.
        assert_eq!(
            Store::open(root.path())
                .unwrap()
                .list_projects()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn migration_refuses_a_row_whose_directory_id_has_no_catalog_entry() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let known = Uuid::now_v7();
        let lost = Uuid::now_v7();
        let ids = write_v1_store(
            root.path(),
            &[("alpha", known), ("beta", lost)],
            &[(known, one.path())],
        );

        let err = Store::open(root.path()).unwrap_err();
        match err {
            CoreError::StoreMigrationBlocked { project, name, .. } => {
                assert_eq!(project, ids[1]);
                assert_eq!(name, "beta");
            }
            other => panic!("expected StoreMigrationBlocked, got {other:?}"),
        }
        // Nothing written: the marker still says 1 and the catalog is in place,
        // so a repaired store migrates on the next launch.
        assert_eq!(
            read_yaml::<StoreConfig>(&root.path().join(STORE_CONFIG_FILE))
                .unwrap()
                .version,
            STORE_VERSION_WITH_CATALOG
        );
        assert!(root.path().join(DIRECTORIES_CATALOG_V1).exists());
        let raw = std::fs::read_to_string(root.path().join(PROJECTS_INDEX)).unwrap();
        assert!(raw.contains("directory_id"), "the index must be untouched");
    }

    #[test]
    fn migration_refuses_a_project_id_listed_twice() {
        // `open_project` would pick the first row and `delete_project` remove
        // both; refusing here, while the index is being rewritten anyway, is the
        // one moment the cause and the failure are next to each other.
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let dir = Uuid::now_v7();
        let ids = write_v1_store(root.path(), &[("alpha", dir)], &[(dir, one.path())]);
        let index = root.path().join(PROJECTS_INDEX);
        let mut raw = std::fs::read_to_string(&index).unwrap();
        raw.push_str(&raw.clone());
        std::fs::write(&index, raw).unwrap();

        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::StoreMigrationBlocked { project, ref reason, .. } if project == ids[0] && reason.contains("more than once")),
            "expected StoreMigrationBlocked for the duplicated id, got {err:?}"
        );
        assert_eq!(
            read_yaml::<StoreConfig>(&root.path().join(STORE_CONFIG_FILE))
                .unwrap()
                .version,
            STORE_VERSION_WITH_CATALOG
        );
    }

    #[test]
    fn migration_refuses_two_directory_ids_resolving_to_one_path() {
        // Collapsing them would merge two Claude session namespaces that
        // version 1 kept apart; agents already sharing a session id under the
        // two ids would then drive one session concurrently.
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        write_v1_store(
            root.path(),
            &[("alpha", a), ("beta", b)],
            &[(a, one.path()), (b, one.path())],
        );

        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::StoreMigrationBlocked { ref reason, .. } if reason.contains("both resolve to")),
            "expected StoreMigrationBlocked for the shared path, got {err:?}"
        );
        assert!(root.path().join(DIRECTORIES_CATALOG_V1).exists());
    }

    #[test]
    fn migration_carries_a_deleted_working_directory_across_verbatim() {
        // The store's defining promise: a project whose checkout was deleted
        // keeps its history. The migration must never grow an existence check.
        let root = TempDir::new().unwrap();
        let gone = TempDir::new().unwrap();
        let gone_path = gone.path().to_path_buf();
        drop(gone);
        assert!(!gone_path.exists());
        let dir = Uuid::now_v7();
        let ids = write_v1_store(root.path(), &[("alpha", dir)], &[(dir, &gone_path)]);

        let store = Store::open(root.path()).unwrap();
        let opened = store.open_project(ids[0]).unwrap();
        assert_eq!(opened.directory, gone_path);
        assert_eq!(store.list_projects().unwrap()[0].directory, gone_path);
    }

    #[test]
    fn migration_refusal_names_the_lossless_repair_and_warns_against_deleting_the_project() {
        let root = TempDir::new().unwrap();
        let lost = Uuid::now_v7();
        let ids = write_v1_store(root.path(), &[("alpha", lost)], &[]);

        let message = Store::open(root.path()).unwrap_err().to_string();
        assert!(message.contains("projects.jsonl"), "{message}");
        assert!(message.contains("add \"directory\""), "{message}");
        assert!(
            message.contains(&format!("Do not delete projects/{}/", ids[0])),
            "{message}"
        );
    }

    #[test]
    fn migration_refuses_a_directory_id_with_two_catalog_entries() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let dup = Uuid::now_v7();
        write_v1_store(
            root.path(),
            &[("alpha", dup)],
            &[(dup, one.path()), (dup, two.path())],
        );

        let err = Store::open(root.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::StoreMigrationBlocked { ref reason, .. } if reason.contains("more than one row")),
            "expected StoreMigrationBlocked naming the duplicate, got {err:?}"
        );
    }

    #[test]
    fn a_migration_interrupted_before_its_commit_completes_on_the_next_open() {
        // Simulate a crash after the index rewrite (step 2) but before the
        // marker (step 4): the rows already carry `directory`, the marker still
        // says 1, the catalog is still there. The re-run must accept the rows
        // as they are and finish.
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let dir = Uuid::now_v7();
        let ids = write_v1_store(root.path(), &[("alpha", dir)], &[(dir, one.path())]);
        write_jsonl(
            &root.path().join(PROJECTS_INDEX),
            &[ProjectEntry {
                id: ids[0],
                name: "alpha".to_owned(),
                created_at: Utc::now(),
                directory: one.path().to_path_buf(),
            }],
        )
        .unwrap();

        let store = Store::open(root.path()).unwrap();
        assert_eq!(store.open_project(ids[0]).unwrap().directory, one.path());
        assert_eq!(
            read_yaml::<StoreConfig>(&root.path().join(STORE_CONFIG_FILE))
                .unwrap()
                .version,
            STORE_VERSION
        );
        assert!(!root.path().join(DIRECTORIES_CATALOG_V1).exists());
    }

    #[test]
    fn migration_rebuilds_an_unreadable_config_from_the_index_row() {
        // The config is a recovery copy; an unreadable one must not block the
        // migration of a project whose index row is intact.
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let dir = Uuid::now_v7();
        let ids = write_v1_store(root.path(), &[("alpha", dir)], &[(dir, one.path())]);
        let config_path = root
            .path()
            .join(PROJECTS_DIR)
            .join(ids[0].to_string())
            .join(CONFIG_FILE);
        std::fs::write(&config_path, "{{{ not yaml").unwrap();

        let store = Store::open(root.path()).unwrap();
        let opened = store.open_project(ids[0]).unwrap();
        assert_eq!(opened.config.name, "alpha");
        assert_eq!(opened.config.directory.as_deref(), Some(one.path()));
    }

    // ---- projects ------------------------------------------------------

    #[test]
    fn creating_a_project_canonicalizes_its_directory() {
        // The identity boundary: a symlinked spelling must record the same
        // path a direct one does, or per-directory rules split one folder in two.
        let root = TempDir::new().unwrap();
        let real = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let store = Store::open(root.path()).unwrap();

        let direct = store.create_project(real.path(), "alpha").unwrap();
        let via_link = store.create_project(&link, "beta").unwrap();
        assert_eq!(direct.directory, via_link.directory);
        assert_eq!(direct.directory, canonical(&real));
    }

    #[test]
    fn creating_a_project_in_a_missing_directory_is_refused() {
        let (_root, cwd, store) = store_with_dir();
        let missing = cwd.path().join("nope");
        assert!(matches!(
            store.create_project(&missing, "alpha").unwrap_err(),
            CoreError::Io { .. }
        ));
        let file = cwd.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(
            store.create_project(&file, "alpha").unwrap_err(),
            CoreError::NotADirectory { .. }
        ));
        assert!(store.list_projects().unwrap().is_empty());
    }

    #[test]
    fn set_project_directory_moves_one_project_and_leaves_its_sibling() {
        let (_root, cwd, store) = store_with_dir();
        let alpha = store.create_project(cwd.path(), "alpha").unwrap();
        let beta = store.create_project(cwd.path(), "beta").unwrap();
        let moved = TempDir::new().unwrap();

        let updated = store.set_project_directory(alpha.id, moved.path()).unwrap();
        assert_eq!(updated.directory, canonical(&moved));

        let reopened = store.open_project(alpha.id).unwrap();
        assert_eq!(reopened.directory, canonical(&moved));
        assert_eq!(
            reopened.config.directory.as_deref(),
            Some(canonical(&moved).as_path()),
            "the config's recovery copy follows the move"
        );
        assert_eq!(
            store.open_project(beta.id).unwrap().directory,
            canonical(&cwd),
            "a sibling at the old path is untouched"
        );
    }

    #[test]
    fn set_project_directory_refuses_a_missing_destination_and_an_unknown_project() {
        let (_root, cwd, store) = store_with_dir();
        let alpha = store.create_project(cwd.path(), "alpha").unwrap();
        assert!(matches!(
            store
                .set_project_directory(alpha.id, &cwd.path().join("nope"))
                .unwrap_err(),
            CoreError::Io { .. }
        ));
        let elsewhere = TempDir::new().unwrap();
        assert!(matches!(
            store
                .set_project_directory(Uuid::now_v7(), elsewhere.path())
                .unwrap_err(),
            CoreError::ProjectNotFound(_)
        ));
        assert_eq!(
            store.open_project(alpha.id).unwrap().directory,
            canonical(&cwd),
            "a refusal changes nothing"
        );
    }

    #[test]
    fn set_project_directory_refuses_a_name_collision_at_the_destination() {
        // Moving `api` into a folder that already has an `api` would create the
        // very collision the per-directory rule exists to prevent.
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.create_project(one.path(), "api").unwrap();
        let mover = store.create_project(two.path(), "API").unwrap();

        let err = store
            .set_project_directory(mover.id, one.path())
            .unwrap_err();
        assert!(
            matches!(err, CoreError::DuplicateProjectName { .. }),
            "expected DuplicateProjectName, got {err:?}"
        );
        assert_eq!(
            store.open_project(mover.id).unwrap().directory,
            canonical(&two)
        );
    }

    #[test]
    fn set_project_directory_to_its_current_path_is_an_idempotent_success() {
        let (_root, cwd, store) = store_with_dir();
        let alpha = store.create_project(cwd.path(), "alpha").unwrap();
        assert_eq!(
            store
                .set_project_directory(alpha.id, cwd.path())
                .unwrap()
                .directory,
            canonical(&cwd)
        );
    }

    #[test]
    fn agents_are_readable_for_a_project_whose_directory_is_gone() {
        // The registry lives under the store root, keyed by project id, so the
        // session-uniqueness scans keep working when a checkout is deleted.
        let (_root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha").unwrap();
        let agent = project
            .register_agent(
                "one",
                crate::harness::HarnessKind::ClaudeCode,
                crate::agent::AgentSelection::default(),
            )
            .unwrap();
        drop(cwd);

        let entry = store.list_projects().unwrap().remove(0);
        for agents in [
            store.read_project_registry(&entry).unwrap(),
            store.list_project_agents(project.id).unwrap(),
        ] {
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].id, agent.id);
        }
    }

    #[test]
    fn listing_agents_for_an_unknown_project_refuses_rather_than_reporting_none() {
        // A registry path derives from any UUID, and a missing file reads as an
        // empty list — so without the membership check a bogus or stale id
        // renders as a valid project with no agents.
        let (_root, _cwd, store) = store_with_dir();
        assert!(matches!(
            store.list_project_agents(Uuid::now_v7()).unwrap_err(),
            CoreError::ProjectNotFound(_)
        ));
    }

    #[test]
    fn a_project_whose_registry_vanished_refuses_rather_than_reporting_no_agents() {
        // `create_on_disk` makes `registry.jsonl` with `create_new`, so absence
        // is corruption. Reporting an empty roster would let the session-id
        // uniqueness scan pass over agents it cannot see.
        let (_root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha").unwrap();
        std::fs::remove_file(store.project_root(project.id).join(REGISTRY_FILE)).unwrap();

        assert!(matches!(
            store.list_project_agents(project.id).unwrap_err(),
            CoreError::MissingAppendOnlyFile { .. }
        ));
        assert!(
            matches!(
                project.list_agents().unwrap_err(),
                CoreError::MissingAppendOnlyFile { .. }
            ),
            "both roster paths share one implementation and must not diverge"
        );
    }

    #[test]
    fn a_created_project_records_its_directory_in_its_own_config() {
        // Without this the project tree cannot say where any project belongs,
        // so losing the index leaves every project's data intact and homeless.
        // Rename must preserve it — `rename_project` builds the config from
        // scratch, so a dropped field would be silent.
        let (_root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha").unwrap();
        let config_path = store.project_root(project.id).join(CONFIG_FILE);
        assert_eq!(
            read_yaml::<ProjectConfig>(&config_path).unwrap().directory,
            Some(canonical(&cwd))
        );

        store.rename_project(project.id, "beta").unwrap();
        assert_eq!(
            read_yaml::<ProjectConfig>(&config_path).unwrap().directory,
            Some(canonical(&cwd)),
            "a rename must preserve the recovery record"
        );
    }

    #[test]
    fn a_config_without_a_directory_still_loads() {
        // The legacy `<directory>/.switchboard/` layout implied the directory by
        // path, so its configs carry none and must keep opening.
        let (_root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha").unwrap();
        let config_path = store.project_root(project.id).join(CONFIG_FILE);
        let config = read_yaml::<ProjectConfig>(&config_path).unwrap();
        write_yaml(
            &config_path,
            &ProjectConfig {
                directory: None,
                ..config
            },
        )
        .unwrap();

        assert_eq!(
            store.open_project(project.id).unwrap().config.directory,
            None
        );
    }

    #[test]
    fn a_project_opens_after_its_working_directory_is_deleted() {
        // The entire point of the move: state outlives the checkout. Listing,
        // renaming, and deleting must all keep working so the user can repair
        // or clean up.
        let (_root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha").unwrap();

        // Compare against the canonical form the index stored, not the raw temp
        // path (macOS resolves /var -> /private/var).
        let gone = canonical(&cwd);
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
    fn project_names_are_unique_per_directory_not_store_wide() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();

        store.create_project(one.path(), "api").unwrap();
        // Two unrelated checkouts each having an `api` project is ordinary, and
        // the pre-store layout allowed it. Widening to store-wide uniqueness
        // would reject names users already have.
        store.create_project(two.path(), "api").unwrap();

        // Within one directory it still collides, under canonicalization
        // (case-folded, hyphen and underscore equivalent).
        store.create_project(one.path(), "web-ui").unwrap();
        let err = store.create_project(one.path(), "Web_UI").unwrap_err();
        assert!(matches!(err, CoreError::DuplicateProjectName { .. }));
    }

    #[test]
    fn renaming_collides_only_within_the_owning_directory() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.create_project(one.path(), "alpha").unwrap();
        let other = store.create_project(two.path(), "beta").unwrap();

        // A sibling directory's `alpha` is not a collision.
        assert_eq!(
            store.rename_project(other.id, "alpha").unwrap().name,
            "alpha"
        );
    }

    #[test]
    fn renaming_to_a_variant_of_its_own_name_is_allowed() {
        let (_root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha-one").unwrap();
        assert_eq!(
            store.rename_project(project.id, "Alpha_One").unwrap().name,
            "Alpha_One"
        );
    }

    #[test]
    fn rename_persists_to_both_config_and_index() {
        let (_root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha").unwrap();

        let updated = store.rename_project(project.id, "renamed").unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(
            updated.directory,
            canonical(&cwd),
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
        let (_root, _cwd, store) = store_with_dir();
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

        let (root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha").unwrap();

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

        let (root, cwd, store) = store_with_dir();
        // Exercise the *commit-step* failure: the index stays readable (so the
        // uniqueness pre-check succeeds and the project dir does get created)
        // but unwritable, so the subsequent append fails.
        let index = root.path().join(PROJECTS_INDEX);
        std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o444)).unwrap();

        let err = store.create_project(cwd.path(), "alpha").unwrap_err();
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
        let project = store.create_project(cwd.path(), "alpha").unwrap();
        assert_ne!(
            project.root,
            root.path().join(PROJECTS_DIR).join("orphan"),
            "the retry mints a fresh id"
        );
        assert_eq!(store.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn delete_drops_the_entry_and_removes_the_dir_keeping_siblings() {
        let (_root, cwd, store) = store_with_dir();
        let a = store.create_project(cwd.path(), "alpha").unwrap();
        let b = store.create_project(cwd.path(), "beta").unwrap();

        store.delete_project(a.id).unwrap();

        assert!(!a.root.exists());
        assert!(b.root.exists());
        let remaining = store.list_projects().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, b.id);
    }

    #[test]
    fn delete_is_idempotent_and_an_unknown_id_is_a_noop() {
        let (_root, cwd, store) = store_with_dir();
        let a = store.create_project(cwd.path(), "alpha").unwrap();

        store.delete_project(a.id).unwrap();
        store.delete_project(a.id).unwrap();
        store.delete_project(Uuid::now_v7()).unwrap();
        assert!(store.list_projects().unwrap().is_empty());
    }

    #[test]
    fn delete_refuses_while_the_index_is_missing() {
        // Reachable only when the index vanishes mid-session, where the user is
        // acting on a list loaded at startup and has no idea the store is
        // damaged — and the project directories are what a lost index would be
        // rebuilt from.
        let (root, cwd, store) = store_with_dir();
        let a = store.create_project(cwd.path(), "alpha").unwrap();
        std::fs::remove_file(root.path().join(PROJECTS_INDEX)).unwrap();

        assert!(matches!(
            store.delete_project(a.id).unwrap_err(),
            CoreError::MissingAppendOnlyFile { .. }
        ));
        assert!(
            a.root.exists(),
            "recoverable state must survive the refusal"
        );
    }

    // Unix-only, same rationale as the other permission-driven tests.
    #[cfg(unix)]
    #[test]
    fn delete_rmdir_failure_still_commits_the_index_and_leaves_an_orphan() {
        use std::os::unix::fs::PermissionsExt;

        let (root, cwd, store) = store_with_dir();
        let a = store.create_project(cwd.path(), "alpha").unwrap();

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
        let (_root, cwd, store) = store_with_dir();
        let project = store.create_project(cwd.path(), "alpha").unwrap();
        let fallback = Utc::now();

        assert_eq!(store.project_last_activity(project.id, fallback), fallback);

        std::fs::write(project.root.join(JOURNAL_FILE), "{}\n").unwrap();
        let old_fallback = fallback - chrono::Duration::days(1);
        assert!(
            store.project_last_activity(project.id, old_fallback) > old_fallback,
            "a journal written now must beat a day-old fallback"
        );
    }
}
