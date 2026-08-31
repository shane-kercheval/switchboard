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

use std::collections::HashMap;
use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::AgentRecord;
use crate::error::{CoreError, Result};
use crate::ids::{DirectoryId, ProjectId};
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_jsonl, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::paths::{
    CONFIG_FILE, DIRECTORIES_CATALOG, JOURNAL_FILE, PROJECTS_DIR, PROJECTS_INDEX, REGISTRY_FILE,
    STORE_CONFIG_FILE,
};
use crate::project::{self, PROJECT_CONFIG_VERSION, Project, ProjectConfig};

/// Bumped only for a layout change old builds cannot read. Checked fail-loud on
/// every [`Store::open`], so a downgrade refuses rather than silently
/// misinterpreting `projects.jsonl` / `directories.jsonl`.
pub const STORE_VERSION: u32 = 1;

/// One line of `store.yaml`'s worth of state: the schema marker.
///
/// **One version for the whole store, not one per file.** `projects.jsonl` and
/// `directories.jsonl` are mutated independently by ordinary operations, but
/// they share a *schema* lifecycle — a layout change touches both — so two
/// markers could only ever disagree, and a reader trusting one while the other
/// was stale is exactly the failure a version check exists to prevent. (The plan
/// called for per-file markers; this is the same guarantee with one place to
/// check it.)
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

/// What a `directory_id` resolved to during one read of the catalog.
///
/// Three states, not two, because the two failures need opposite repairs and a
/// bare `Option` collapses them: a missing row is registered *zero* times and an
/// ambiguous one *twice*, so telling a user "not registered" when the truth is
/// "registered twice" points them at re-adding the directory, which mints a
/// third identity no project references. Every consumer that only cares
/// resolved-vs-not uses [`Self::path`] and ignores the distinction; the ones
/// that produce an error carry it through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryResolution {
    Resolved(PathBuf),
    /// The entry's `directory_id` has no catalog row — corruption, since the
    /// catalog has no delete API.
    Missing,
    /// More than one catalog row claims the id. See
    /// [`CoreError::AmbiguousDirectory`].
    Ambiguous,
}

impl DirectoryResolution {
    /// The path, for callers that only need resolved-vs-not (the listing's
    /// availability flag).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Resolved(path) => Some(path),
            Self::Missing | Self::Ambiguous => None,
        }
    }

    /// The path, or the error that names *which* failure this is.
    ///
    /// Total by construction: there is no branch for a case that "cannot
    /// happen", because an unreachable arm silently folded into a
    /// plausible-looking wrong error is the shape of the bug this type exists
    /// to prevent.
    fn require_path(&self, id: DirectoryId) -> Result<&Path> {
        match self {
            Self::Resolved(path) => Ok(path),
            Self::Missing => Err(CoreError::DirectoryNotFound(id)),
            Self::Ambiguous => Err(CoreError::AmbiguousDirectory(id)),
        }
    }
}

/// One project paired with the working directory it resolves to — a **snapshot**
/// taken during one read of the store.
///
/// Deliberately not named `ProjectListing`: the app already owns that name for
/// the enriched wire row it sends the frontend (availability, archived-ness,
/// recency). This is the layer below — index entry plus resolved path, nothing
/// derived.
///
/// Valid only within the caller's read. Holding one across a mutation (a
/// re-point, a create) leaves a stale path; the enumerate/scan sites are safe
/// because they already hold `registry_write` across snapshot **and** scan,
/// which is what makes the uniqueness checks atomic with the register that
/// follows them.
///
/// **Fields are private and the type is constructed only by
/// [`Store::list_projects_resolved`].** The catalog is the sole authority on
/// where a project runs; a publicly-buildable row would let any caller hand
/// [`Store::open_resolved`] a project id paired with an arbitrary path and get
/// back a `Project` whose dispatch cwd the catalog never approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    entry: ProjectEntry,
    directory: DirectoryResolution,
}

impl ResolvedProject {
    #[must_use]
    pub fn entry(&self) -> &ProjectEntry {
        &self.entry
    }

    #[must_use]
    pub fn directory(&self) -> &DirectoryResolution {
        &self.directory
    }
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
    /// Fails loud on a version mismatch, and — for an already-initialized store —
    /// on a missing index or catalog.
    ///
    /// **Nothing here is ever recreated over data.** The index and catalog are
    /// the record of what exists; recreating a lost one turns "this store is
    /// damaged" into "you have no projects", which is both false (every
    /// project's data is still under `projects/`) and self-worsening, because
    /// ordinary use then appends to the empty file while the real entries sit
    /// orphaned. The `MissingAppendOnlyFile` checks in [`Self::list_projects`]
    /// and [`Self::list_directories`] only mean anything if this method leaves
    /// the absence intact for them to find.
    ///
    /// That guarantee needs **two** gates, because the marker and the files it
    /// describes are lost by the same events. With a marker present, a missing
    /// index is refused outright. With the marker *also* gone, the layout is
    /// completed only when [`Self::holds_data`] proves there is nothing to
    /// destroy — an interrupted first launch heals, a store whose root-level
    /// files were taken together does not.
    ///
    /// **`store.yaml` is written last among the files whose absence is
    /// load-bearing** — the index and the catalog — as the initialization
    /// commit. A crash between the two would otherwise leave a valid version
    /// marker over a store with no index, indistinguishable on the next launch
    /// from a healthy empty one. And stamping a marker over surviving data would
    /// be wrong even if the index were intact: [`STORE_VERSION`] exists to be
    /// bumped, so "whatever version is running" is not a safe answer to "what
    /// layout is this data in".
    ///
    /// `projects/` is created *after* the marker, and its absence is neither
    /// checked nor refused. That is not a hole in the commit
    /// ordering: a missing `projects/` may well represent real data loss, but
    /// recreating the empty container cannot worsen it, because a directory
    /// holds no record that later writes append to. That is exactly what
    /// separates it from recreating an empty index, which is self-worsening —
    /// ordinary use appends to the new file while the real entries sit orphaned.
    /// Refusing to open on a missing `projects/` would be worse still: the data
    /// is already gone, and refusal would strand the user with index rows they
    /// cannot list or delete.
    pub fn open(root: &Path) -> Result<Store> {
        create_dir_all(root).map_err(|e| CoreError::io(root, e))?;
        let store = Store {
            root: root.to_path_buf(),
        };

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
            for path in [store.projects_index_path(), store.catalog_path()] {
                if !path.exists() {
                    return Err(CoreError::MissingAppendOnlyFile { path });
                }
            }
        } else if store.holds_data()? {
            return Err(CoreError::StoreDataWithoutVersionMarker {
                root: store.root.clone(),
                marker: config_path,
            });
        } else {
            // Nothing but empty scaffolding: a first launch, or an
            // initialization interrupted before its commit. Complete it.
            for path in [store.projects_index_path(), store.catalog_path()] {
                if !path.exists() {
                    std::fs::write(&path, "").map_err(|e| CoreError::io(&path, e))?;
                }
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

    /// Whether the root holds anything a legitimate pre-marker state could not.
    ///
    /// Nothing can write to the store before [`Self::open`] returns, so an
    /// interrupted initialization has empty scaffolding and nothing else. Any
    /// record in either index, or any project directory, therefore means the
    /// marker was lost from an initialized store rather than never written.
    ///
    /// Emptiness is judged by **content, not file size**: `read_jsonl` skips
    /// blank lines, so a file holding a stray newline is logically empty and a
    /// length check would refuse on it. A corrupt line surfaces as
    /// `CorruptJsonl` rather than the marker error — data with no marker
    /// either way, and the more specific diagnosis is the more useful one.
    ///
    /// `projects/` is the load-bearing half: it is a subdirectory while the
    /// three files are siblings at the root, so it is what survives the events
    /// that take the marker and the index together. The `is_dir()` filter on its
    /// scan is load-bearing too — a `.DS_Store` dropped in by Finder would
    /// otherwise read as data and refuse a legitimate first launch.
    ///
    /// **Revisit this predicate whenever the store gains another owned
    /// location** (locks, migration records), asking the same question of each:
    /// can anything under it still be attributed to a project once the indexes
    /// are gone? If not, it does not belong in the predicate. Attachments raised
    /// exactly this question and no longer do: they stage per-project, inside
    /// `projects/<id>/`, so the `projects/` scan already covers them and they
    /// need no clause of their own.
    fn holds_data(&self) -> Result<bool> {
        if !read_jsonl::<ProjectEntry>(&self.projects_index_path())?.is_empty()
            || !read_jsonl::<DirectoryEntry>(&self.catalog_path())?.is_empty()
        {
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
    ///
    /// **Refuses a destination another entry already holds**, upholding the same
    /// one-id-per-canonical-path invariant [`Self::add_directory`] enforces on
    /// the other writer. Without it, the ordinary recovery gesture produces a
    /// split identity: two worktrees are catalogued, one is deleted, its
    /// projects are re-pointed at the survivor — and now one folder has two ids,
    /// so every per-directory scope evaluated by id sees half of what it should.
    ///
    /// # Atomicity / partial-write contract
    ///
    /// `write_jsonl` renames the new catalog over the old and *then* fsyncs the
    /// parent directory, so a failure at that last step returns `Err` with the
    /// replacement already visible. **An `Err` from this method does not
    /// guarantee the catalog is unchanged** — the same contract
    /// [`Self::rename_project`] carries, and the same reason rollback is not
    /// attempted: undoing after a possible commit is what would actually lose
    /// the repair.
    ///
    /// **In-memory `Project`s are not updated.** `Project.directory` is
    /// snapshotted at [`Self::open_project`] and is the dispatch cwd, so a
    /// caller must quiesce any turn running against the old path and drain
    /// before calling, then re-read the catalog **on both outcomes** and rebuild
    /// project and actor state from the path it actually observes. Restoring the
    /// pre-call state on `Err` is the failure mode this contract exists to
    /// prevent: the app would keep dispatching into the old directory while the
    /// store resolves the id to the new one — the split re-pointing exists to
    /// end. If that re-read also fails, the affected projects are unloadable and
    /// dispatch must refuse; there is no safe fallback to the pre-call path. The
    /// situation that prompts a re-point is precisely the one where the project
    /// is already open. **Every project referencing `id` moves**, not just the
    /// one the user was looking at; the affordance repairs a directory identity,
    /// not a single project.
    ///
    /// # Collapsing a duplicated id
    ///
    /// Every row carrying `id` is replaced by **one** row at the new path.
    /// Rewriting only the first match would leave the duplicate behind, so
    /// [`Self::directory_map`] would still see the id as ambiguous: the call
    /// would return `Ok` while the projects stayed unresolved, and the user —
    /// having just been told the repair succeeded — would have no signal to stop
    /// trying, and no other exit ([`Self::add_directory`] keys on path and would
    /// mint a third id no project references). Collapsing is not a side effect
    /// but the repair: asserting "this id lives here" resolves the ambiguity by
    /// definition, and this is the only in-app way out of it.
    pub fn repoint_directory(&self, id: DirectoryId, new_path: &Path) -> Result<DirectoryEntry> {
        let canonical = std::fs::canonicalize(new_path).map_err(|e| CoreError::io(new_path, e))?;
        if !canonical.is_dir() {
            return Err(CoreError::NotADirectory { path: canonical });
        }
        let mut entries = self.list_directories()?;
        if !entries.iter().any(|e| e.directory_id == id) {
            return Err(CoreError::DirectoryNotFound(id));
        }
        // Rows carrying `id` are excluded, so a collapse can't collide with the
        // duplicate it is removing, and re-pointing an entry at the path it
        // already holds stays an idempotent success — except in an already-
        // broken catalog where a *different* id also holds that path, which
        // fails here rather than deepening the split.
        if let Some(other) = entries
            .iter()
            .find(|e| e.directory_id != id && e.path == canonical)
        {
            return Err(CoreError::DuplicateDirectoryPath {
                path: canonical,
                existing: other.directory_id,
            });
        }
        entries.retain(|e| e.directory_id != id);
        let updated = DirectoryEntry {
            directory_id: id,
            path: canonical,
        };
        entries.push(updated.clone());
        write_jsonl(&self.catalog_path(), &entries)?;
        Ok(updated)
    }

    /// Bind a `directory_id` that has **lost** its catalog row to a path.
    ///
    /// The narrow repair for a project whose `directory_id` resolves to nothing.
    /// It restores a mapping, it does not mint an identity: the id already
    /// exists and is already referenced by the projects being repaired — what
    /// was lost is only the row saying where it lives. (An earlier reading
    /// called this "inventing an identity"; that was wrong, and it is why this
    /// stayed unimplemented longer than it should have.)
    ///
    /// **Refuses an id that still has a row**, so it can never be a back door
    /// around [`Self::repoint_directory`]'s collapse semantics, and refuses a
    /// path another id holds, upholding the same one-id-per-canonical-path
    /// invariant as the other two writers.
    ///
    /// The state this repairs cannot arise from ordinary use — the catalog never
    /// deletes a row a project references — so the realistic sources are an
    /// external edit, a partial restore, or a sync conflict. Those can happen at
    /// any time, which is why an in-app repair is worth having and not merely a
    /// migration-tool concern.
    pub fn bind_directory(&self, id: DirectoryId, path: &Path) -> Result<DirectoryEntry> {
        let canonical = std::fs::canonicalize(path).map_err(|e| CoreError::io(path, e))?;
        if !canonical.is_dir() {
            return Err(CoreError::NotADirectory { path: canonical });
        }
        // Enforce the premise the method's whole justification rests on: this
        // restores a mapping for an identity that already exists *because
        // projects reference it*. Unenforced, a repair path could mint a row for
        // an id nothing points at — manufacturing the orphan state it was built
        // to fix.
        if !self.list_projects()?.iter().any(|p| p.directory_id == id) {
            return Err(CoreError::DirectoryNotFound(id));
        }
        let entries = self.list_directories()?;
        if entries.iter().any(|e| e.directory_id == id) {
            return Err(CoreError::DuplicateDirectoryId(id));
        }
        if let Some(other) = entries.iter().find(|e| e.path == canonical) {
            return Err(CoreError::DuplicateDirectoryPath {
                path: canonical,
                existing: other.directory_id,
            });
        }
        let entry = DirectoryEntry {
            directory_id: id,
            path: canonical,
        };
        append_jsonl(&self.catalog_path(), &entry)?;
        Ok(entry)
    }

    /// Every path the catalog currently associates with `id`.
    ///
    /// Plural because a duplicated id has more than one, and the repair has to
    /// retire **all** of them from view-state — [`Self::directory_path`] answers
    /// `Err` there, which would leave stale rows behind.
    pub fn directory_paths(&self, id: DirectoryId) -> Result<Vec<PathBuf>> {
        Ok(self
            .list_directories()?
            .into_iter()
            .filter(|entry| entry.directory_id == id)
            .map(|entry| entry.path)
            .collect())
    }

    /// The path a `directory_id` currently resolves to.
    ///
    /// **Catalog entries are never deleted while any project references them.**
    /// A dangling id would leave those projects unopenable with no way to
    /// re-point them — the id is the only handle the repair affordance has. So
    /// "the user is done with this directory" is expressed by hiding it in
    /// view-state, never by dropping the catalog row.
    pub fn directory_path(&self, id: DirectoryId) -> Result<PathBuf> {
        // Through `directory_map` so this and the listing cannot diverge on a
        // duplicated id: both see it as unresolvable, and this side turns that
        // into a refusal rather than a guessed path.
        // Bound to a local rather than chained: `require_path` borrows, and a
        // chain off the temporary compiles only while it stays one statement.
        let resolution = self
            .directory_map()?
            .remove(&id)
            .unwrap_or(DirectoryResolution::Missing);
        resolution.require_path(id).map(Path::to_path_buf)
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

    /// Every project paired with the working directory it resolves to — the
    /// listing shape, reading each file **once**.
    ///
    /// Use this instead of looping [`Self::open_project`]. That method resolves
    /// one project by re-reading the whole index *and* the whole catalog, so N
    /// of them is N full parses of each; here both are read once and every row
    /// resolves against an in-memory map. Same reason to prefer it even when N
    /// is small: it is the shape the call sites should be built on.
    ///
    /// **A dangling `directory_id` is one unresolved row, not a failed call.**
    /// Ambiguity and absence get different treatment throughout the store: a
    /// syntactically corrupt catalog still fails the whole read (nothing can be
    /// trusted), but a single project whose catalog row is gone must still list,
    /// so the user can see which project is broken and repair or delete it.
    /// Returning `Result<Vec<(ProjectEntry, PathBuf)>>` would hide every healthy
    /// project behind one damaged reference.
    pub fn list_projects_resolved(&self) -> Result<Vec<ResolvedProject>> {
        // **Index before catalog.** A project entry can only be appended after
        // its catalog row exists — `create_project` resolves the directory
        // first and fails without it — so reading in this order yields a
        // catalog at least as new as the index. The reverse can read an old
        // catalog and then a new index, reporting a freshly created project as
        // having a dangling directory.
        let projects = self.list_projects()?;
        let directories = self.directory_map()?;
        Ok(projects
            .into_iter()
            .map(|entry| ResolvedProject {
                directory: directories
                    .get(&entry.directory_id)
                    .cloned()
                    .unwrap_or(DirectoryResolution::Missing),
                entry,
            })
            .collect())
    }

    /// Open a project from a row [`Self::list_projects_resolved`] already
    /// resolved, without re-reading either file.
    ///
    /// The reason the batch read is worth having. Every caller that needs a
    /// *loaded* project — one with a dispatch cwd, which is what `Project`
    /// carries and what separates these callers from the roster reads — would
    /// otherwise discard the resolved row and call [`Self::open_project`], which
    /// parses the whole index and the whole catalog again for one project. N
    /// projects would still cost N full parses of each file, and the batch
    /// method would sit beside the loop instead of replacing it.
    ///
    /// Requiring a resolved directory is correct **here** and wrong for a
    /// registry read: see [`Self::read_project_registry`], which is why the
    /// session-id collision scans no longer come through this path.
    pub fn open_resolved(&self, resolved: &ResolvedProject) -> Result<Project> {
        let directory = resolved
            .directory
            .require_path(resolved.entry.directory_id)?;
        project::load(
            directory,
            resolved.entry.id,
            self.project_root(resolved.entry.id),
        )
    }

    /// Every agent in a project, read straight from its registry — for callers
    /// already holding the index row.
    ///
    /// **Needs no catalog resolution, by design.** `registry.jsonl` lives at
    /// `<store-root>/projects/<id>/`, so a project whose working directory is
    /// missing or ambiguous still has a readable roster. That is what keeps the
    /// session-id uniqueness scans whole when one catalog row is damaged: they
    /// need a registry and a display name (which the index entry carries), never
    /// a cwd. Going through [`Self::open_resolved`] instead would make one bad
    /// row fail every scan store-wide, and skipping the row would leave a hole
    /// in the guarantee — both are artefacts of a dependency the read does not
    /// have.
    ///
    /// **Takes the index row, not a bare id**, so that membership is visible at
    /// the call site instead of asserted in prose. The scans hold one anyway
    /// (they need `name` for the collision error and `directory_id` for
    /// scoping), so it costs them nothing. Deliberately **not**
    /// [`ResolvedProject`], whose construction reads the catalog and would put
    /// it back on this path's critical route. Callers holding only an id want
    /// [`Self::list_project_agents`].
    ///
    /// Same validation as [`Project::list_agents`]; they share one
    /// implementation so neither can drift into a laxer read.
    pub fn read_project_registry(&self, entry: &ProjectEntry) -> Result<Vec<AgentRecord>> {
        project::read_registry(&self.project_root(entry.id).join(REGISTRY_FILE), entry.id)
    }

    /// Every agent in a project, by id, **after confirming the project is in the
    /// index**.
    ///
    /// The read [`Self::read_project_registry`] skips: without it an id that
    /// names no project resolves to a registry path that doesn't exist, and a
    /// user-facing surface would render a stale or fabricated project as a valid
    /// empty one rather than reporting `ProjectNotFound`. Costs one index parse,
    /// which is why the scans use the other method.
    ///
    /// Still catalog-free: a project whose working directory no longer resolves
    /// lists its agents. Browsing a roster needs no cwd.
    pub fn list_project_agents(&self, id: ProjectId) -> Result<Vec<AgentRecord>> {
        let entry = self
            .list_projects()?
            .into_iter()
            .find(|e| e.id == id)
            .ok_or(CoreError::ProjectNotFound(id))?;
        self.read_project_registry(&entry)
    }

    /// The catalog as a lookup, resolving each id to a
    /// [`DirectoryResolution`].
    ///
    /// One definition shared by every resolution path. Built ad-hoc, the two
    /// disagreed: a `HashMap` collect is last-wins and a linear `find` is
    /// first-wins, so a duplicated `directory_id` made the project list show one
    /// working directory while dispatch ran the agent in another — the stable-id
    /// design failing at the one thing it exists to do.
    ///
    /// A duplicate degrades the affected id rather than failing the whole read.
    /// The file parses; every other row is exactly as trustworthy as before; so
    /// the "nothing can be trusted" case that justifies whole-read failure
    /// doesn't apply, and failing here would hide every healthy project behind
    /// one ambiguous one. Instead the affected projects list as unresolved while
    /// anything that would *run* an agent refuses — a refusal beats a guessed
    /// working directory. The state is repairable in-app because
    /// [`Self::repoint_directory`] collapses the duplicates; degrading without
    /// that repair would leave the user visibly broken and stuck.
    fn directory_map(&self) -> Result<HashMap<DirectoryId, DirectoryResolution>> {
        let mut map: HashMap<DirectoryId, DirectoryResolution> = HashMap::new();
        for entry in self.list_directories()? {
            map.entry(entry.directory_id)
                .and_modify(|slot| *slot = DirectoryResolution::Ambiguous)
                .or_insert(DirectoryResolution::Resolved(entry.path));
        }
        Ok(map)
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
    /// directory and surface the error. The worst case is an orphan directory:
    /// it has no index entry, so `list_projects` never surfaces it and a retry
    /// mints a fresh UUID. **Its unreachability is no longer absolute** — the
    /// project tree is self-describing now (`config.yaml` carries the name,
    /// creation time, and owning `directory_id`), so an index rebuild can
    /// resurrect an orphan this path deliberately abandoned. Harmless in itself
    /// (an empty registry, a real name), but it is why the repair tool must
    /// treat an unindexed root as ambiguous rather than as disposable.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the same store — the
    /// read-check-then-append sequence has a TOCTOU window. In-process, callers
    /// serialize (the app's `registry_write` mutex does this).
    ///
    /// Cross-process, the index is one file for the whole install where it used
    /// to be one per working directory, so a read-modify-write here can lose a
    /// whole project row rather than a preference. Three things already contain
    /// that: release builds are single-instance, and dev and release resolve
    /// disjoint store roots, so the only unguarded case is two debug builds
    /// launched without a per-instance config dir. That case is accepted, as it
    /// was before the store existed — but note the hazard is a **lost update**
    /// from interleaved read-modify-write, not a torn file: `write_jsonl` is
    /// tmp-plus-rename, so atomicity is not what makes it survivable.
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

        let (summary, project) =
            project::create_on_disk(&directory, Some(directory_id), &self.projects_dir(), name)?;
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
        // `version` is the current constant, and `created_at` and
        // `directory_id` both already live in the index entry, so a read would
        // only add a disk round-trip and a late failure window *after*
        // uniqueness already passed. **This shortcut holds only while every
        // `ProjectConfig` field is recoverable from the index entry** — a field
        // that isn't forces read-then-mutate here, or a rename silently drops
        // it. `directory_id` is the second field this guard has caught.
        let config_path = self.project_root(id).join(CONFIG_FILE);
        let config = ProjectConfig {
            version: PROJECT_CONFIG_VERSION,
            name: new_name.to_owned(),
            created_at: entries[idx].created_at,
            directory_id: Some(entries[idx].directory_id),
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
    /// Removal is scoped to the project root, so it takes whatever lives under
    /// it and nothing else. Staged attachment files are reclaimed by the
    /// reference-GC rather than here — it is the only component that can see
    /// whether another project's journal still references a given file.
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
    ///
    /// # A missing index refuses — a declared divergence from `Directory`
    ///
    /// `Directory::delete_project` tolerated a missing `projects.jsonl`: there
    /// was no entry to drop, so it skipped the rewrite and removed the
    /// directory anyway. This does not.
    ///
    /// [`Self::open`] makes the condition reachable only when the index vanishes
    /// **after** the store was opened, in a live session — and there the user is
    /// acting on a list loaded at startup, with no signal that anything is
    /// wrong. "They named this project explicitly" is not informed consent when
    /// they have not been told the store is damaged, and the project
    /// directories are the material a lost index would be rebuilt from: each
    /// project directory carries its id in its name, and its `config.yaml` the
    /// name, creation time, and owning [`ProjectConfig::directory_id`] — enough
    /// to reconstruct the index and to see which projects share a directory
    /// identity. (It is *not* enough
    /// to reconstruct the catalog's `directory_id -> path` mapping — those paths
    /// need migration records or the user re-pointing each id — so the material
    /// is recoverable, not self-restoring.) An error tells the user the truth;
    /// proceeding destroys that material while looking like an ordinary delete.
    ///
    /// Only genuine absence reaches this — an unreadable-but-present index
    /// surfaces as `CoreError::Io` and propagates regardless. Deliberate
    /// orphan cleanup belongs to an explicit repair operation, not to this one.
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
    fn reopening_an_initialized_store_refuses_a_missing_index_and_recreates_nothing() {
        // The production path. `open` recreating the file would make the guard
        // above unreachable and turn "this store is damaged" into "you have no
        // projects" — while every project's data still sits under `projects/`.
        for missing in [PROJECTS_INDEX, DIRECTORIES_CATALOG] {
            let (root, _cwd, _store, _id) = store_with_dir();
            let path = root.path().join(missing);
            std::fs::remove_file(&path).unwrap();

            let err = Store::open(root.path()).unwrap_err();
            assert!(
                matches!(err, CoreError::MissingAppendOnlyFile { .. }),
                "expected MissingAppendOnlyFile for {missing}, got {err:?}"
            );
            assert!(
                !path.exists(),
                "{missing} must be left absent, not healed back into existence"
            );
        }
    }

    #[test]
    fn an_interrupted_initialization_completes_on_the_next_open() {
        // `store.yaml` is written last, so a crash mid-setup leaves empty
        // scaffolding and no marker. Nothing can write to the store before
        // `open` returns, so that state provably holds no data and must heal —
        // refusing would wedge a fresh install that was force-quit.
        for pre_created in [
            vec![],
            vec![PROJECTS_INDEX],
            vec![PROJECTS_INDEX, DIRECTORIES_CATALOG],
        ] {
            let root = TempDir::new().unwrap();
            for name in &pre_created {
                std::fs::write(root.path().join(name), "").unwrap();
            }
            // A stray newline is logically empty: `read_jsonl` skips blank
            // lines, so a byte-length test would refuse here.
            if pre_created.contains(&PROJECTS_INDEX) {
                std::fs::write(root.path().join(PROJECTS_INDEX), "\n").unwrap();
            }

            let store = Store::open(root.path()).unwrap();
            assert!(root.path().join(STORE_CONFIG_FILE).exists());
            assert!(store.list_projects().unwrap().is_empty());
        }
    }

    #[test]
    fn a_markerless_store_that_holds_data_refuses_instead_of_being_blessed() {
        // The marker, the index, and the catalog are siblings at the root while
        // project data is a subdirectory — a selective restore or a sync
        // conflict takes the files together and leaves the data. Completing
        // initialization there would recreate the index empty and present a
        // store full of projects as empty, which is the failure this whole
        // guard exists to prevent. It is also how a future schema version would
        // silently stamp its marker over another version's data.
        let (root, cwd, store, id) = store_with_dir();
        store.create_project(id, "alpha").unwrap();
        let project_dirs = || {
            std::fs::read_dir(root.path().join(PROJECTS_DIR))
                .unwrap()
                .count()
        };
        assert_eq!(project_dirs(), 1);

        // Marker and both indexes taken together; only `projects/` survives.
        for name in [STORE_CONFIG_FILE, PROJECTS_INDEX, DIRECTORIES_CATALOG] {
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
        drop(cwd);
    }

    #[test]
    fn a_markerless_store_refuses_on_a_surviving_index_or_catalog_too() {
        // `projects/` is the signal that survives losing both files, but either
        // index holding a record is equally proof the marker was lost from an
        // initialized store rather than never written.
        for keep in [PROJECTS_INDEX, DIRECTORIES_CATALOG] {
            let (root, _cwd, store, id) = store_with_dir();
            store.create_project(id, "alpha").unwrap();
            std::fs::remove_dir_all(root.path().join(PROJECTS_DIR)).unwrap();
            std::fs::remove_file(root.path().join(STORE_CONFIG_FILE)).unwrap();
            for name in [PROJECTS_INDEX, DIRECTORIES_CATALOG] {
                if name != keep {
                    std::fs::remove_file(root.path().join(name)).unwrap();
                }
            }

            let err = Store::open(root.path()).unwrap_err();
            assert!(
                matches!(err, CoreError::StoreDataWithoutVersionMarker { .. }),
                "expected refusal with {keep} surviving, got {err:?}"
            );
        }
    }

    #[test]
    fn a_refusal_leaves_no_trace_in_the_root_it_declined() {
        // `create_dir_all` for `projects/` sits below both refusal branches so a declined root is left exactly as found. Nothing
        // else pins that placement: moving those calls above the branch would
        // otherwise pass every test while stamping our layout onto a store we
        // just refused to open.
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
    fn a_crash_between_the_marker_and_the_indexes_cannot_look_like_an_empty_store() {
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
    fn repointing_a_directory_moves_every_project_in_it_on_disk() {
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
    fn repointing_onto_a_directory_that_is_already_registered_is_refused() {
        // The ordinary recovery gesture: two worktrees catalogued, one deleted,
        // its projects re-pointed at the survivor. Allowing it would give one
        // folder two ids, and every per-directory scope evaluated by id — most
        // consequentially the Claude session-id collision scan — would then see
        // half the agents that share the cwd namespace.
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let a = store.add_directory(one.path()).unwrap().directory_id;
        let b = store.add_directory(two.path()).unwrap().directory_id;

        let err = store.repoint_directory(b, one.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::DuplicateDirectoryPath { existing, .. } if existing == a),
            "expected DuplicateDirectoryPath naming the holder, got {err:?}"
        );
        // Refused means unchanged, not partially applied.
        assert_eq!(
            store.directory_path(b).unwrap(),
            std::fs::canonicalize(two.path()).unwrap()
        );
    }

    #[test]
    fn repointing_an_entry_at_its_own_current_path_stays_a_success() {
        let (_root, cwd, store, id) = store_with_dir();
        // Self-excluded from the collision check, so the operation is idempotent.
        assert_eq!(
            store.repoint_directory(id, cwd.path()).unwrap().path,
            std::fs::canonicalize(cwd.path()).unwrap()
        );
    }

    #[test]
    fn resolved_listing_agrees_with_per_id_resolution_and_keeps_broken_rows() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let a = store.add_directory(one.path()).unwrap().directory_id;
        let b = store.add_directory(two.path()).unwrap().directory_id;
        let p1 = store.create_project(a, "alpha").unwrap();
        let p2 = store.create_project(b, "beta").unwrap();

        let listed = store.list_projects_resolved().unwrap();
        assert_eq!(listed.len(), 2);
        for listing in &listed {
            assert_eq!(
                listing.directory.path(),
                Some(
                    store
                        .directory_path(listing.entry.directory_id)
                        .unwrap()
                        .as_path()
                ),
                "resolved listing must agree with per-id resolution"
            );
        }

        // Drop the catalog row `beta` depends on. `alpha` must still list — one
        // damaged reference cannot hide every healthy project.
        let kept: Vec<DirectoryEntry> = store
            .list_directories()
            .unwrap()
            .into_iter()
            .filter(|e| e.directory_id == a)
            .collect();
        crate::io::write_jsonl(&root.path().join(DIRECTORIES_CATALOG), &kept).unwrap();

        let listed = store.list_projects_resolved().unwrap();
        assert_eq!(listed.len(), 2);
        let alpha = listed.iter().find(|l| l.entry.id == p1.id).unwrap();
        let beta = listed.iter().find(|l| l.entry.id == p2.id).unwrap();
        assert!(alpha.directory.path().is_some());
        assert_eq!(
            beta.directory,
            DirectoryResolution::Missing,
            "a dangling id is one unresolved row"
        );
    }

    #[test]
    fn a_duplicated_directory_id_is_unresolvable_everywhere_rather_than_guessed() {
        // Built ad-hoc, the two resolution paths disagreed — a map collect is
        // last-wins and a linear find is first-wins — so the list showed one
        // working directory while dispatch used another. Both now see it as
        // unresolvable: the project still lists (visible, repairable) and
        // anything that would run an agent refuses.
        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        let elsewhere = TempDir::new().unwrap();
        let dup = DirectoryEntry {
            directory_id: id,
            path: std::fs::canonicalize(elsewhere.path()).unwrap(),
        };
        crate::io::append_jsonl(&root.path().join(DIRECTORIES_CATALOG), &dup).unwrap();

        // Ambiguous, not missing: the two need opposite repairs, and telling
        // the user this id isn't registered would send them to `add_directory`,
        // which mints a third id no project references.
        assert!(matches!(
            store.directory_path(id).unwrap_err(),
            CoreError::AmbiguousDirectory(_)
        ));
        assert!(matches!(
            store.open_project(project.id).unwrap_err(),
            CoreError::AmbiguousDirectory(_)
        ));
        let listed = store.list_projects_resolved().unwrap();
        assert_eq!(listed.len(), 1, "the project still lists");
        assert_eq!(listed[0].directory, DirectoryResolution::Ambiguous);
    }

    #[test]
    fn a_duplicated_id_does_not_hide_healthy_projects() {
        let root = TempDir::new().unwrap();
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let a = store.add_directory(one.path()).unwrap().directory_id;
        let b = store.add_directory(two.path()).unwrap().directory_id;
        let healthy = store.create_project(b, "beta").unwrap();
        store.create_project(a, "alpha").unwrap();
        let elsewhere = TempDir::new().unwrap();
        crate::io::append_jsonl(
            &root.path().join(DIRECTORIES_CATALOG),
            &DirectoryEntry {
                directory_id: a,
                path: std::fs::canonicalize(elsewhere.path()).unwrap(),
            },
        )
        .unwrap();

        // Whole-read failure would hide `beta` behind `alpha`'s ambiguity, and
        // the user's only recovery would be hand-editing a JSONL file.
        let listed = store.list_projects_resolved().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .iter()
                .find(|l| l.entry.id == healthy.id)
                .unwrap()
                .directory
                .path()
                .is_some()
        );
        store.open_project(healthy.id).unwrap();
    }

    #[test]
    fn open_resolved_matches_open_project_without_re_reading() {
        let (_root, _cwd, store, id) = store_with_dir();
        let created = store.create_project(id, "alpha").unwrap();

        let listed = store.list_projects_resolved().unwrap();
        let opened = store.open_resolved(&listed[0]).unwrap();
        assert_eq!(opened.id, created.id);
        assert_eq!(opened.root, store.open_project(created.id).unwrap().root);
        assert_eq!(opened.directory, created.directory);
    }

    #[test]
    fn open_resolved_refuses_a_row_whose_directory_never_resolved() {
        // Built through the real path rather than by hand: a fabricated
        // `ResolvedProject` would keep passing even if `list_projects_resolved`
        // stopped producing unresolved rows at all, which is the behaviour under
        // test.
        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        crate::io::write_jsonl::<DirectoryEntry>(&root.path().join(DIRECTORIES_CATALOG), &[])
            .unwrap();

        let listed = store.list_projects_resolved().unwrap();
        let row = listed.iter().find(|l| l.entry.id == project.id).unwrap();
        assert!(matches!(
            store.open_resolved(row).unwrap_err(),
            CoreError::DirectoryNotFound(_)
        ));
    }

    #[test]
    fn open_resolved_names_ambiguity_rather_than_absence() {
        // The dispatch-refusal surface: an ambiguous row must not report the
        // repair for a missing one.
        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        let elsewhere = TempDir::new().unwrap();
        crate::io::append_jsonl(
            &root.path().join(DIRECTORIES_CATALOG),
            &DirectoryEntry {
                directory_id: id,
                path: std::fs::canonicalize(elsewhere.path()).unwrap(),
            },
        )
        .unwrap();

        let listed = store.list_projects_resolved().unwrap();
        let row = listed.iter().find(|l| l.entry.id == project.id).unwrap();
        assert!(matches!(
            store.open_resolved(row).unwrap_err(),
            CoreError::AmbiguousDirectory(_)
        ));
    }

    #[test]
    fn repointing_a_duplicated_id_collapses_it_and_actually_repairs_the_projects() {
        // The affordance the degradation design points at. Rewriting only the
        // first matching row returns Ok and changes nothing observable: the id
        // stays ambiguous, the project stays unresolved, and the user — just
        // told the repair succeeded — has no signal to stop trying and no other
        // exit, since `add_directory` keys on path and would mint a third id.
        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        let stale = TempDir::new().unwrap();
        crate::io::append_jsonl(
            &root.path().join(DIRECTORIES_CATALOG),
            &DirectoryEntry {
                directory_id: id,
                path: std::fs::canonicalize(stale.path()).unwrap(),
            },
        )
        .unwrap();
        assert!(matches!(
            store.directory_path(id).unwrap_err(),
            CoreError::AmbiguousDirectory(_)
        ));

        let moved = TempDir::new().unwrap();
        let canonical = std::fs::canonicalize(moved.path()).unwrap();
        let updated = store.repoint_directory(id, moved.path()).unwrap();
        assert_eq!(updated.path, canonical);

        let rows: Vec<_> = store
            .list_directories()
            .unwrap()
            .into_iter()
            .filter(|e| e.directory_id == id)
            .collect();
        assert_eq!(rows.len(), 1, "the duplicate must be collapsed away");
        assert_eq!(store.directory_path(id).unwrap(), canonical);
        assert_eq!(
            store.open_project(project.id).unwrap().directory,
            canonical,
            "the repair must actually make the project dispatchable"
        );
    }

    #[test]
    fn binding_restores_a_lost_mapping_without_minting_an_identity() {
        // The repair for a catalog that lost a row. The id is unchanged — the
        // projects referencing it resolve again — which is what distinguishes
        // restoring a mapping from inventing an identity.
        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        crate::io::write_jsonl::<DirectoryEntry>(&root.path().join(DIRECTORIES_CATALOG), &[])
            .unwrap();
        assert!(matches!(
            store.directory_path(id).unwrap_err(),
            CoreError::DirectoryNotFound(_)
        ));

        let home = TempDir::new().unwrap();
        let canonical = std::fs::canonicalize(home.path()).unwrap();
        assert_eq!(
            store.bind_directory(id, home.path()).unwrap().path,
            canonical
        );

        assert_eq!(store.directory_path(id).unwrap(), canonical);
        assert_eq!(store.open_project(project.id).unwrap().directory, canonical);
    }

    #[test]
    fn binding_an_id_no_project_references_is_refused() {
        // The method's justification is that binding restores a mapping for an
        // identity projects already reference. Unenforced, a repair path could
        // mint a row for an id nothing points at — manufacturing the orphan
        // state it exists to fix.
        let (root, _cwd, store, id) = store_with_dir();
        crate::io::write_jsonl::<DirectoryEntry>(&root.path().join(DIRECTORIES_CATALOG), &[])
            .unwrap();
        let elsewhere = TempDir::new().unwrap();

        assert!(matches!(
            store.bind_directory(id, elsewhere.path()).unwrap_err(),
            CoreError::DirectoryNotFound(_)
        ));
    }

    #[test]
    fn binding_an_id_that_still_has_a_row_is_refused() {
        // Otherwise the repair for an ambiguous identity would be the thing that
        // creates one. Re-pointing is the operation for an id that resolves.
        let (_root, _cwd, store, id) = store_with_dir();
        store.create_project(id, "alpha").unwrap();
        let elsewhere = TempDir::new().unwrap();
        assert!(matches!(
            store.bind_directory(id, elsewhere.path()).unwrap_err(),
            CoreError::DuplicateDirectoryId(_)
        ));
    }

    #[test]
    fn binding_refuses_a_path_another_identity_holds() {
        let (root, _cwd, store, id) = store_with_dir();
        store.create_project(id, "alpha").unwrap();
        let taken = TempDir::new().unwrap();
        let other = store.add_directory(taken.path()).unwrap();
        let kept: Vec<DirectoryEntry> = store
            .list_directories()
            .unwrap()
            .into_iter()
            .filter(|entry| entry.directory_id == other.directory_id)
            .collect();
        crate::io::write_jsonl(&root.path().join(DIRECTORIES_CATALOG), &kept).unwrap();

        assert!(matches!(
            store.bind_directory(id, taken.path()).unwrap_err(),
            CoreError::DuplicateDirectoryPath { existing, .. } if existing == other.directory_id
        ));
    }

    #[test]
    fn directory_paths_reports_every_row_for_an_ambiguous_id() {
        // `directory_path` errors on ambiguity, so a repair that used it to find
        // what to retire left the surplus rows behind.
        let (root, cwd, store, id) = store_with_dir();
        let extra = TempDir::new().unwrap();
        let extra_path = std::fs::canonicalize(extra.path()).unwrap();
        crate::io::append_jsonl(
            &root.path().join(DIRECTORIES_CATALOG),
            &DirectoryEntry {
                directory_id: id,
                path: extra_path.clone(),
            },
        )
        .unwrap();

        let mut paths = store.directory_paths(id).unwrap();
        paths.sort();
        let mut expected = vec![std::fs::canonicalize(cwd.path()).unwrap(), extra_path];
        expected.sort();
        assert_eq!(paths, expected);
    }

    #[test]
    fn collapsing_a_duplicated_id_still_refuses_a_path_another_id_holds() {
        // The collapse must not become a way around the one-id-per-path
        // invariant: the rows being removed carry the id under repair, never
        // another one.
        let (root, _cwd, store, id) = store_with_dir();
        let taken = TempDir::new().unwrap();
        let other = store.add_directory(taken.path()).unwrap();
        let stale = TempDir::new().unwrap();
        crate::io::append_jsonl(
            &root.path().join(DIRECTORIES_CATALOG),
            &DirectoryEntry {
                directory_id: id,
                path: std::fs::canonicalize(stale.path()).unwrap(),
            },
        )
        .unwrap();

        let err = store.repoint_directory(id, taken.path()).unwrap_err();
        assert!(
            matches!(err, CoreError::DuplicateDirectoryPath { existing, .. } if existing == other.directory_id),
            "expected DuplicateDirectoryPath, got {err:?}"
        );
        assert_eq!(
            store.list_directories().unwrap().len(),
            3,
            "a refused repair must not collapse anything"
        );
    }

    #[test]
    fn agents_are_readable_for_a_project_whose_directory_does_not_resolve() {
        // The registry lives under the store root, keyed by project id, so the
        // session-uniqueness scans keep working when a catalog row is damaged.
        // Without this the scans would either fail store-wide on one bad row or
        // skip it and leave a hole in the guarantee.
        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        let agent = project
            .register_agent("one", crate::harness::HarnessKind::ClaudeCode, None, None)
            .unwrap();
        crate::io::write_jsonl::<DirectoryEntry>(&root.path().join(DIRECTORIES_CATALOG), &[])
            .unwrap();

        assert!(matches!(
            store.open_project(project.id).unwrap_err(),
            CoreError::DirectoryNotFound(_)
        ));
        // Both entry points: the scans' parse-free read and the checked one a
        // user-facing roster uses.
        let entry = ProjectEntry {
            id: project.id,
            name: "alpha".to_owned(),
            created_at: Utc::now(),
            directory_id: id,
        };
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
        let (_root, _cwd, store, _id) = store_with_dir();
        assert!(matches!(
            store.list_project_agents(Uuid::now_v7()).unwrap_err(),
            CoreError::ProjectNotFound(_)
        ));
    }

    #[test]
    fn a_project_whose_registry_vanished_refuses_rather_than_reporting_no_agents() {
        // `create_on_disk` makes `registry.jsonl` with `create_new`, so absence
        // is corruption. Reporting an empty roster would let the session-id
        // uniqueness scan pass over agents it cannot see — the guarantee the
        // catalog-free read exists to keep whole.
        let (_root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
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
    fn a_created_project_records_its_owning_directory_in_its_own_config() {
        // Without this the project tree cannot say where any project belongs,
        // so losing both root indexes leaves every project's data intact and
        // homeless. Rename must preserve it — `rename_project` builds the config
        // from scratch, so a dropped field would be silent.
        let (root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        let config_path = store.project_root(project.id).join(CONFIG_FILE);
        assert_eq!(
            read_yaml::<ProjectConfig>(&config_path)
                .unwrap()
                .directory_id,
            Some(id)
        );

        store.rename_project(project.id, "beta").unwrap();
        assert_eq!(
            read_yaml::<ProjectConfig>(&config_path)
                .unwrap()
                .directory_id,
            Some(id),
            "a rename must preserve the ownership record"
        );
        drop(root);
    }

    #[test]
    fn a_config_without_an_owning_directory_still_loads() {
        // The legacy `<directory>/.switchboard/` layout implied ownership by
        // path, so its configs carry no id and must keep opening.
        let (_root, _cwd, store, id) = store_with_dir();
        let project = store.create_project(id, "alpha").unwrap();
        let config_path = store.project_root(project.id).join(CONFIG_FILE);
        let config = read_yaml::<ProjectConfig>(&config_path).unwrap();
        write_yaml(
            &config_path,
            &ProjectConfig {
                directory_id: None,
                ..config
            },
        )
        .unwrap();

        assert_eq!(
            store.open_project(project.id).unwrap().config.directory_id,
            None
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
    fn delete_refuses_while_the_index_is_missing() {
        // Declared divergence from `Directory`, which removed the directory
        // anyway. Reachable only when the index vanishes mid-session, where the
        // user is acting on a list loaded at startup and has no idea the store
        // is damaged — and the project directories are what a lost index would
        // be rebuilt from.
        let (root, _cwd, store, id) = store_with_dir();
        let a = store.create_project(id, "alpha").unwrap();
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
}
