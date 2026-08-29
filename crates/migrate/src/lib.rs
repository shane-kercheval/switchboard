//! The migration itself, separated from the binary so it can be exercised
//! against fixture directories.
//!
//! **Design: build a fresh store or nothing.** The tool refuses a target that
//! already holds projects, never merges, and never touches the legacy sources.
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

use std::path::{Path, PathBuf};

use serde::Deserialize;
use switchboard_core::{
    Attachment, DirectoryId, JournalRecord, ProjectConfig, ProjectEntry, Store, append_jsonl,
    read_jsonl, read_yaml, write_yaml,
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
    id: switchboard_core::ProjectId,
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
    pub projects: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error(
        "the target store at {0} already contains projects — this tool never merges; \
         delete the target and re-run to migrate from scratch"
    )]
    TargetNotEmpty(PathBuf),
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

/// Migrate every available directory into a fresh store at `target_root`.
///
/// Journal attachment paths are rewritten to the new location **with the
/// original preserved in `dispatched_path`** (only where it is not already
/// set — a legacy journal never sets it, but write-once is the contract).
/// `instance.lock` files are dropped: they are advisory-lock tokens for a
/// process that is not running.
pub fn migrate(
    directories: &[PathBuf],
    target_root: &Path,
) -> Result<MigrationReport, MigrateError> {
    let store = Store::open(target_root)?;
    if !store.list_projects()?.is_empty() {
        return Err(MigrateError::TargetNotEmpty(target_root.to_path_buf()));
    }

    let mut report = MigrationReport::default();
    for directory in directories {
        migrate_directory(&store, target_root, directory, &mut report)?;
    }

    validate(&store, &report)?;
    Ok(report)
}

fn migrate_directory(
    store: &Store,
    target_root: &Path,
    directory: &Path,
    report: &mut MigrationReport,
) -> Result<(), MigrateError> {
    // Unavailable directory ⇒ skip with a reason, never fail the run.
    let canonical = match std::fs::canonicalize(directory) {
        Ok(canonical) => canonical,
        Err(e) => {
            report
                .skipped
                .push((directory.to_path_buf(), format!("unavailable: {e}")));
            return Ok(());
        }
    };
    let legacy_index = canonical.join(LEGACY_DIR).join(LEGACY_INDEX);
    if !legacy_index.exists() {
        report.empty.push(directory.to_path_buf());
        return Ok(());
    }
    // A *present but unreadable* index is a skip with a loud reason, not a
    // silent "empty": the directory demonstrably held projects.
    let entries: Vec<LegacyProjectEntry> = match read_jsonl(&legacy_index) {
        Ok(entries) => entries,
        Err(e) => {
            report
                .skipped
                .push((directory.to_path_buf(), format!("unreadable index: {e}")));
            return Ok(());
        }
    };
    if entries.is_empty() {
        report.empty.push(directory.to_path_buf());
        return Ok(());
    }

    // One catalog identity per directory, minted by the store itself so the
    // canonicalization and duplicate-path rules are the app's own.
    let directory_id = store.add_directory(&canonical)?.directory_id;

    let mut migrated = MigratedDirectory {
        directory: canonical.clone(),
        directory_id,
        projects: Vec::new(),
    };
    for entry in entries {
        copy_project(
            store,
            target_root,
            &canonical,
            directory,
            directory_id,
            &entry,
        )?;
        migrated.projects.push(entry.name);
    }
    report.migrated.push(migrated);
    Ok(())
}

fn copy_project(
    store: &Store,
    target_root: &Path,
    directory: &Path,
    directory_as_recorded: &Path,
    directory_id: DirectoryId,
    entry: &LegacyProjectEntry,
) -> Result<(), MigrateError> {
    let legacy_project = |base: &Path| {
        base.join(LEGACY_DIR)
            .join(LEGACY_PROJECTS_DIR)
            .join(entry.id.to_string())
    };
    let source_root = legacy_project(directory);
    let target_project_root = store.project_root(entry.id);

    copy_tree(&source_root, &target_project_root)?;

    // config.yaml gains the directory_id recovery record. Read/modify/write
    // through the app's own type so the round-trip cannot drift.
    let config_path = target_project_root.join(CONFIG_FILE);
    let mut config: ProjectConfig = read_yaml(&config_path)?;
    config.directory_id = Some(directory_id);
    write_yaml(&config_path, &config)?;

    // **Both spellings of the source root, because journals record the path
    // the app saw, not the canonical one.** On macOS `/var` symlinks to
    // `/private/var`, so a journal written under `/var/...` does not
    // prefix-match the canonicalized directory — and any symlinked home or
    // checkout has the same mismatch. Matching only the canonical form made
    // the rewrite silently skip such paths, leaving journals pointing at the
    // legacy location; the end-to-end test caught it because temp dirs on
    // macOS live under exactly such a symlink.
    let source_roots = [source_root.clone(), legacy_project(directory_as_recorded)];
    rewrite_journal_attachment_paths(&source_roots, &target_project_root)?;

    // Index row last — the project is fully in place before it is listed, the
    // same commit ordering `create_project` uses.
    append_jsonl(
        &target_root.join("projects.jsonl"),
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
/// Only paths under the legacy project root are rewritten — an attachment that
/// somehow points elsewhere is left alone (its file was never under
/// `.switchboard/`, so the move does not affect it). `dispatched_path` is set
/// only where `None`: legacy journals never set it, but write-once is the
/// field's contract and this is the one writer.
fn rewrite_journal_attachment_paths(
    source_roots: &[PathBuf],
    target_root: &Path,
) -> Result<(), MigrateError> {
    let journal = target_root.join(JOURNAL_FILE);
    if !journal.exists() {
        return Ok(());
    }
    let mut records: Vec<JournalRecord> = read_jsonl(&journal)?;
    for record in &mut records {
        if let JournalRecord::Send { attachments, .. } = record {
            for attachment in attachments {
                rewrite_attachment(attachment, source_roots, target_root);
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

fn rewrite_attachment(attachment: &mut Attachment, source_roots: &[PathBuf], target_root: &Path) {
    let old = PathBuf::from(&attachment.path);
    let Some(relative) = source_roots
        .iter()
        .find_map(|root| old.strip_prefix(root).ok())
    else {
        // Not under the legacy project root under either spelling: the file was
        // never inside `.switchboard/`, so the move does not affect it.
        return;
    };
    let new_path = target_root.join(relative);
    if attachment.dispatched_path.is_none() {
        attachment.dispatched_path = Some(attachment.path.clone());
    }
    attachment.path = new_path.to_string_lossy().into_owned();
}

/// Re-open everything just written through the app's own read paths — the same
/// code the app will run — so "the store validates" means "the app will load
/// it", not "the tool believes itself".
fn validate(store: &Store, report: &MigrationReport) -> Result<(), MigrateError> {
    let indexed = store.list_projects()?;
    let expected: usize = report.migrated.iter().map(|m| m.projects.len()).sum();
    if indexed.len() != expected {
        return Err(MigrateError::Validation(format!(
            "index lists {} projects, migration copied {expected}",
            indexed.len()
        )));
    }
    for entry in &indexed {
        let project = store.open_project(entry.id)?;
        let config: ProjectConfig = read_yaml(&project.root.join(CONFIG_FILE))?;
        if config.directory_id != Some(entry.directory_id) {
            return Err(MigrateError::Validation(format!(
                "project {} config directory_id disagrees with its index row",
                entry.id
            )));
        }
        project.list_agents()?;
        let journal = project.root.join(JOURNAL_FILE);
        if journal.exists() {
            let _: Vec<JournalRecord> = read_jsonl(&journal)?;
        }
    }
    Ok(())
}
