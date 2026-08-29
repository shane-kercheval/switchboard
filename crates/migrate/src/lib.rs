//! The migration itself, separated from the binary so it can be exercised
//! against fixture directories.
//!
//! **Design: build a fresh store or nothing.** The tool refuses a target that
//! already holds anything, never merges, and never touches the legacy sources.
//! That one decision deletes the dangerous machinery a merging tool would need —
//! re-run idempotency records, write-once guards, cleanup flags, exclusive locks
//! against a running app — because the whole output is disposable: if anything
//! is wrong, delete the target and run again. The trade is that a re-run redoes
//! every directory, which for the intended audience (a handful of users, run
//! once) costs nothing.
//!
//! **Why Rust and not a shell script:** three files are *rewritten*, not
//! copied — each project's `config.yaml` gains the `directory_id` recovery
//! record, the index rows gain the same, and every journal `Send`'s attachment
//! paths move to the new location while `dispatched_path` preserves the
//! original (send↔turn correlation reconstructs the exact dispatched text,
//! footer paths included, so losing the original silently degrades old turns
//! to duplicates). Reusing `switchboard_core`'s own types means the round-trip
//! is the same code the app runs, and cannot drift from what the app expects.
//!
//! **The one failure that must not be silent is a missed path rewrite.** The
//! app garbage-collects a project's `attachments/` on every open, deleting any
//! file the journal does not name by its exact current path — so a migrated
//! file whose journal entry still points at the legacy location is *deleted the
//! first time the user opens the project*, quietly, while everything else
//! renders fine. Two consequences shape this tool: the rewrite is checked by
//! its exact negation immediately after it runs (no attachment may still
//! prefix-match a legacy root), and every path the rewrite deliberately left
//! alone is counted and reported per directory, so a legitimate miss is a line
//! the user can read rather than a silence.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use switchboard_core::{
    AgentId, Attachment, DirectoryId, JournalRecord, ProjectConfig, ProjectEntry, ProjectId, Store,
    append_jsonl, read_jsonl, read_yaml, write_yaml,
};

/// The legacy per-directory layout, named here and nowhere else.
/// `switchboard_core::paths` deliberately dropped this constant so no read path
/// could grow back into the store layer; the migration is the one thing that is
/// *about* the old layout.
const LEGACY_DIR: &str = ".switchboard";
const LEGACY_INDEX: &str = "projects.jsonl";
const LEGACY_PROJECTS_DIR: &str = "projects";
const INSTANCE_LOCK: &str = "instance.lock";
const JOURNAL_FILE: &str = "journal.jsonl";
const CONFIG_FILE: &str = "config.yaml";

/// A legacy index row. Redeclared rather than imported: the current
/// `ProjectEntry` *requires* `directory_id`, which legacy rows don't have —
/// that absence is the thing being migrated.
#[derive(Debug, Deserialize)]
struct LegacyProjectEntry {
    id: ProjectId,
    name: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// The slice of the old `workspace.yaml` the tool needs: directory paths only.
/// The old file also carried a per-directory project cache; the tool ignores it
/// and trusts the directories' own `.switchboard/projects.jsonl`, which was
/// always the source of truth the cache mirrored.
#[derive(Debug, Deserialize)]
struct LegacyWorkspace {
    #[serde(default)]
    entries: Vec<LegacyWorkspaceEntry>,
}

#[derive(Debug, Deserialize)]
struct LegacyWorkspaceEntry {
    path: PathBuf,
}

#[derive(Debug, Default)]
pub struct MigrationReport {
    pub migrated: Vec<MigratedDirectory>,
    /// Unavailable or unreadable sources, with the reason. Skipping is per the
    /// plan: an unplugged disk must not fail the whole run.
    pub skipped: Vec<(PathBuf, String)>,
    /// Directories with no legacy projects — listed so "why wasn't X migrated"
    /// has an answer in the report rather than a silence.
    pub empty: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct MigratedDirectory {
    pub directory: PathBuf,
    pub directory_id: DirectoryId,
    /// `(id, name)` — the id is what validation compares against the written
    /// index, so a report that only carried names could not detect a wrong row.
    pub projects: Vec<(ProjectId, String)>,
    /// Attachment paths rewritten into the store. **Per directory, not
    /// aggregate**: a single total can be non-zero while one directory silently
    /// contributed nothing, which is exactly the per-directory spelling
    /// mismatch the two-root matching exists to prevent.
    pub attachments_rewritten: usize,
    /// Attachment paths under *neither* spelling of this directory's legacy
    /// root, left untouched. Legitimate — a checkout that moved before
    /// migration records its old path — but the one ambiguous outcome, so it
    /// is printed rather than passed over.
    pub attachments_left: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error(
        "the target store at {0} is not empty — this tool never merges; \
         delete the target and re-run to migrate from scratch"
    )]
    TargetNotEmpty(PathBuf),
    #[error(
        "project {id} appears in two source directories ({first} and {second}) — likely a copied \
         checkout (cp -a, a restore) carrying the same .switchboard state. Migrating both would \
         make two index rows share one project directory, so the app would list one project \
         twice under two owners. Remove {LEGACY_DIR}/ from whichever copy is not the real one, \
         then re-run"
    )]
    DuplicateProjectId {
        id: ProjectId,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("core: {0}")]
    Core(#[from] switchboard_core::CoreError),
    #[error("io at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "validation failed after copying: {0} — the target store is incomplete; \
         delete it, fix the cause, and re-run (originals are untouched)"
    )]
    Validation(String),
}

fn io_err(path: &Path, source: std::io::Error) -> MigrateError {
    MigrateError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The installed app's store location — where the migrated data has to end up
/// for the app to see it. Resolved the same way the app resolves it in release.
#[must_use]
pub fn default_target_root() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "switchboard")
        .map(|dirs| dirs.config_dir().join("store"))
}

/// The directory paths the old `workspace.yaml` records.
pub fn workspace_directories(workspace_yaml: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    let workspace: LegacyWorkspace = read_yaml(workspace_yaml)?;
    Ok(workspace.entries.into_iter().map(|e| e.path).collect())
}

/// Make `target_root` absolute **without resolving symlinks**.
///
/// Both halves of that sentence are load-bearing, because journal attachment
/// paths are written under this root and the app's GC later compares them
/// *lexically* against files under the root the app opens:
///
/// - **Absolute**, because a relative target would write relative paths into
///   the journals, which can never match the absolute paths the app's
///   directory scan produces — so the GC would delete every migrated
///   attachment on first open.
/// - **Not canonicalized**, because the app opens the store through its
///   *configured* spelling (`ProjectDirs`, symlinks unresolved). Resolving
///   symlinks here would write journal paths in one spelling while the app
///   compares in another — the same deletion through a different door. The
///   default target and the app's own resolution agree by construction; a
///   `--target-root` override is the user's spelling, kept verbatim.
///
/// `.` components are stripped lexically; `..` is kept as-is (resolving it
/// lexically across a symlink would change meaning).
fn absolutize(target_root: &Path) -> Result<PathBuf, MigrateError> {
    let joined = if target_root.is_absolute() {
        target_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| io_err(target_root, e))?
            .join(target_root)
    };
    Ok(joined
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect())
}

/// One directory's readable legacy state, gathered before anything is written.
struct SourceScan {
    as_recorded: PathBuf,
    canonical: PathBuf,
    entries: Vec<LegacyProjectEntry>,
}

/// Migrate every available directory into a fresh store at `target_root`.
///
/// Reads everything first, writes second: the duplicate-project check has to
/// see *all* sources before the first byte lands, or the refusal arrives with
/// half a store already written.
pub fn migrate(
    directories: &[PathBuf],
    target_root: &Path,
) -> Result<MigrationReport, MigrateError> {
    let target_root = absolutize(target_root)?;
    let store = Store::open(&target_root)?;
    reject_non_empty_target(&store, &target_root)?;

    let mut report = MigrationReport::default();
    let sources = scan_sources(directories, &mut report);
    reject_duplicate_project_ids(&sources)?;

    for source in &sources {
        migrate_directory(&store, source, &mut report)?;
    }

    validate(&store, &report)?;
    Ok(report)
}

/// Refuse anything that could make this run a merge: index rows, catalog rows,
/// or any entry under `projects/`. Checking only the index would let a
/// partially-written target (a catalog row or an orphan project directory from
/// a crashed run) slip under "fresh" and leave residue behind a clean report.
fn reject_non_empty_target(store: &Store, target_root: &Path) -> Result<(), MigrateError> {
    let projects_dir = target_root.join(LEGACY_PROJECTS_DIR);
    let has_project_dirs =
        std::fs::read_dir(&projects_dir).is_ok_and(|mut entries| entries.next().is_some());
    if !store.list_projects()?.is_empty()
        || !store.list_directories()?.is_empty()
        || has_project_dirs
    {
        return Err(MigrateError::TargetNotEmpty(target_root.to_path_buf()));
    }
    Ok(())
}

fn scan_sources(directories: &[PathBuf], report: &mut MigrationReport) -> Vec<SourceScan> {
    let mut sources = Vec::new();
    for directory in directories {
        // Unavailable directory ⇒ skip with a reason, never fail the run.
        let canonical = match std::fs::canonicalize(directory) {
            Ok(canonical) => canonical,
            Err(e) => {
                report
                    .skipped
                    .push((directory.clone(), format!("unavailable: {e}")));
                continue;
            }
        };
        let legacy_index = canonical.join(LEGACY_DIR).join(LEGACY_INDEX);
        if !legacy_index.exists() {
            report.empty.push(directory.clone());
            continue;
        }
        // A *present but unreadable* index is a skip with a loud reason, not a
        // silent "empty": the directory demonstrably held projects.
        let entries: Vec<LegacyProjectEntry> = match read_jsonl(&legacy_index) {
            Ok(entries) => entries,
            Err(e) => {
                report
                    .skipped
                    .push((directory.clone(), format!("unreadable index: {e}")));
                continue;
            }
        };
        if entries.is_empty() {
            report.empty.push(directory.clone());
            continue;
        }
        sources.push(SourceScan {
            as_recorded: directory.clone(),
            canonical,
            entries,
        });
    }
    sources
}

/// Refuse a project id that appears in more than one source — including twice
/// in one index.
///
/// **The realistic trigger is a copied working directory, not corruption.**
/// `.switchboard/` is gitignored so clones don't carry it, but `cp -a`, a Time
/// Machine restore into a second location, or a duplicated worktree all do —
/// and then two catalogued directories legitimately list the same ids.
/// Unrefused, both would copy into the same `projects/<id>/` (second overwrites
/// first) while appending two index rows with different `directory_id`s: one
/// project directory, two rows claiming different owners, the same project
/// listed twice.
fn reject_duplicate_project_ids(sources: &[SourceScan]) -> Result<(), MigrateError> {
    let mut seen: HashMap<ProjectId, &Path> = HashMap::new();
    for source in sources {
        for entry in &source.entries {
            if let Some(first) = seen.insert(entry.id, &source.canonical) {
                return Err(MigrateError::DuplicateProjectId {
                    id: entry.id,
                    first: first.to_path_buf(),
                    second: source.canonical.clone(),
                });
            }
        }
    }
    Ok(())
}

fn migrate_directory(
    store: &Store,
    source: &SourceScan,
    report: &mut MigrationReport,
) -> Result<(), MigrateError> {
    // One catalog identity per directory, minted by the store itself so the
    // canonicalization and duplicate-path rules are the app's own.
    let directory_id = store.add_directory(&source.canonical)?.directory_id;

    let mut migrated = MigratedDirectory {
        directory: source.canonical.clone(),
        directory_id,
        projects: Vec::new(),
        attachments_rewritten: 0,
        attachments_left: Vec::new(),
    };
    for entry in &source.entries {
        copy_project(store, source, directory_id, entry, &mut migrated)?;
        migrated.projects.push((entry.id, entry.name.clone()));
    }
    report.migrated.push(migrated);
    Ok(())
}

fn copy_project(
    store: &Store,
    source: &SourceScan,
    directory_id: DirectoryId,
    entry: &LegacyProjectEntry,
    migrated: &mut MigratedDirectory,
) -> Result<(), MigrateError> {
    let legacy_project = |base: &Path| {
        base.join(LEGACY_DIR)
            .join(LEGACY_PROJECTS_DIR)
            .join(entry.id.to_string())
    };
    let source_root = legacy_project(&source.canonical);
    let target_project_root = store.project_root(entry.id);

    copy_tree(&source_root, &target_project_root)?;

    // config.yaml gains the directory_id recovery record. Read/modify/write
    // through the app's own type so the round-trip cannot drift.
    let config_path = target_project_root.join(CONFIG_FILE);
    let mut config: ProjectConfig = read_yaml(&config_path)?;
    config.directory_id = Some(directory_id);
    write_yaml(&config_path, &config)?;

    // **Both spellings of the source root, because journals record the path the
    // app saw, not the canonical one.** On macOS `/var` symlinks to
    // `/private/var`, so a journal written under `/var/...` does not
    // prefix-match the canonicalized directory — and any symlinked home or
    // checkout has the same mismatch. Matching only the canonical form made the
    // rewrite silently skip such paths; the end-to-end test caught it because
    // temp dirs on macOS live under exactly such a symlink.
    let source_roots = [source_root.clone(), legacy_project(&source.as_recorded)];
    rewrite_journal_attachment_paths(&source_roots, &target_project_root, migrated)?;

    // Index row last — the project is fully in place before it is listed, the
    // same commit ordering `create_project` uses.
    append_jsonl(
        &store.root().join(LEGACY_INDEX),
        &ProjectEntry {
            id: entry.id,
            name: entry.name.clone(),
            created_at: entry.created_at,
            directory_id,
        },
    )?;
    Ok(())
}

/// Copy a project tree, dropping `instance.lock`.
///
/// **Always a full re-copy; do not "optimise" into skip-if-exists.** Re-copying
/// is what makes running over a partially-written target safe in the crash
/// case: the copy restores the pristine legacy journal *before* the rewrite
/// runs, so a second rewrite cannot double-apply or clobber `dispatched_path`.
/// The copy, not a write-once guard, is the idempotency mechanism.
fn copy_tree(source: &Path, target: &Path) -> Result<(), MigrateError> {
    std::fs::create_dir_all(target).map_err(|e| io_err(target, e))?;
    for item in std::fs::read_dir(source).map_err(|e| io_err(source, e))? {
        let item = item.map_err(|e| io_err(source, e))?;
        let name = item.file_name();
        if name.to_str() == Some(INSTANCE_LOCK) {
            continue;
        }
        let from = item.path();
        let to = target.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| io_err(&from, e))?;
        }
    }
    Ok(())
}

/// Rewrite every journal `Send`'s attachment paths from the legacy project root
/// to the new one, preserving the original in `dispatched_path`.
///
/// **Checked by its exact negation before returning**: after the rewrite, no
/// attachment may still prefix-match either spelling of the legacy root. A path
/// that does means the rewrite should have fired and didn't — and the app's GC
/// would delete the migrated file on first open (see the module doc). A path
/// under *neither* root was never inside the legacy layout (or was recorded
/// under a since-moved directory) and is left alone but counted and reported.
///
/// `dispatched_path` is set only where `None`: legacy journals never set it,
/// and the full-re-copy in `copy_tree` is what makes re-runs safe (see there).
///
/// The write is deliberately **not** atomic (`std::fs::write`, where every
/// other JSONL write in the codebase is tmp-plus-rename): a crash mid-write
/// truncates a file in a target that is disposable by contract, and the
/// recovery — delete the target, re-run — is the same as for any other partial
/// run. Sources are never written.
fn rewrite_journal_attachment_paths(
    source_roots: &[PathBuf; 2],
    target_root: &Path,
    migrated: &mut MigratedDirectory,
) -> Result<(), MigrateError> {
    let journal = target_root.join(JOURNAL_FILE);
    if !journal.exists() {
        return Ok(());
    }
    let mut records: Vec<JournalRecord> = read_jsonl(&journal)?;
    for record in &mut records {
        if let JournalRecord::Send { attachments, .. } = record {
            for attachment in attachments {
                rewrite_attachment(attachment, source_roots, target_root, migrated);
            }
        }
    }
    for record in &records {
        if let JournalRecord::Send { attachments, .. } = record {
            for attachment in attachments {
                if let Some(root) = source_roots
                    .iter()
                    .find(|root| Path::new(&attachment.path).starts_with(root))
                {
                    return Err(MigrateError::Validation(format!(
                        "attachment {:?} still points into the legacy root {} after the rewrite — \
                         the app would delete the migrated copy on first open",
                        attachment.path,
                        root.display()
                    )));
                }
            }
        }
    }
    let mut lines = String::new();
    for record in &records {
        lines.push_str(
            &serde_json::to_string(record)
                .map_err(|e| MigrateError::Validation(format!("re-serializing journal: {e}")))?,
        );
        lines.push('\n');
    }
    std::fs::write(&journal, lines).map_err(|e| io_err(&journal, e))?;
    Ok(())
}

fn rewrite_attachment(
    attachment: &mut Attachment,
    source_roots: &[PathBuf; 2],
    target_root: &Path,
    migrated: &mut MigratedDirectory,
) {
    let old = PathBuf::from(&attachment.path);
    let Some(relative) = source_roots
        .iter()
        .find_map(|root| old.strip_prefix(root).ok())
    else {
        migrated.attachments_left.push(attachment.path.clone());
        return;
    };
    let new_path = target_root.join(relative);
    if attachment.dispatched_path.is_none() {
        attachment.dispatched_path = Some(attachment.path.clone());
    }
    attachment.path = new_path.to_string_lossy().into_owned();
    migrated.attachments_rewritten += 1;
}

/// Re-open everything just written through the app's own read paths — the same
/// code the app will run — so "the store validates" means "the app will load
/// it", not "the tool believes itself".
///
/// **Exact expected set, not counts.** The report carries every `(id, name)`
/// per directory; the written index must match it row for row (id, name,
/// `directory_id`, cardinality). A count can match while a duplicated or wrong
/// row hides inside it.
fn validate(store: &Store, report: &MigrationReport) -> Result<(), MigrateError> {
    let mut expected: HashMap<ProjectId, (&str, DirectoryId)> = HashMap::new();
    for migrated in &report.migrated {
        for (id, name) in &migrated.projects {
            expected.insert(*id, (name.as_str(), migrated.directory_id));
        }
    }

    let indexed = store.list_projects()?;
    if indexed.len() != expected.len() {
        return Err(MigrateError::Validation(format!(
            "index lists {} projects, migration copied {}",
            indexed.len(),
            expected.len()
        )));
    }
    // Cross-project agent-id uniqueness — the same invariant the app's
    // register cache enforces; a store that violates it fails on open there.
    let mut agent_ids: HashSet<AgentId> = HashSet::new();
    for entry in &indexed {
        let Some(&(name, directory_id)) = expected.get(&entry.id) else {
            return Err(MigrateError::Validation(format!(
                "index lists project {} that the migration never wrote",
                entry.id
            )));
        };
        if entry.name != name || entry.directory_id != directory_id {
            return Err(MigrateError::Validation(format!(
                "index row for {} disagrees with what was migrated \
                 (name {:?} vs {:?}, directory {} vs {})",
                entry.id, entry.name, name, entry.directory_id, directory_id
            )));
        }
        let project = store.open_project(entry.id)?;
        let config: ProjectConfig = read_yaml(&project.root.join(CONFIG_FILE))?;
        if config.directory_id != Some(entry.directory_id) {
            return Err(MigrateError::Validation(format!(
                "project {} config directory_id disagrees with its index row",
                entry.id
            )));
        }
        for agent in project.list_agents()? {
            if !agent_ids.insert(agent.id) {
                return Err(MigrateError::Validation(format!(
                    "agent {} appears in more than one migrated project",
                    agent.id
                )));
            }
        }
        let journal = project.root.join(JOURNAL_FILE);
        if journal.exists() {
            let _: Vec<JournalRecord> = read_jsonl(&journal)?;
        }
    }
    Ok(())
}
