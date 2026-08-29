//! Fixture-driven end-to-end tests for the migration.
//!
//! **All `#[ignore]`d, deliberately — these do not run in `make check`.** The
//! tool is a one-off: it runs by hand, a few times ever, and is deleted once
//! every user has migrated. Its tests are for *developing* it, not for guarding
//! it forever; keeping them out of the default gate keeps a throwaway's
//! maintenance cost at zero. Run them when touching the tool:
//!
//!     cargo test -p switchboard-migrate -- --ignored

use std::path::{Path, PathBuf};

use switchboard_core::{JournalRecord, ProjectConfig, Store, read_jsonl, read_yaml};
use tempfile::TempDir;
use uuid::Uuid;

/// Build a legacy working directory holding one project with an
/// attachment-bearing journal — the shape the tool exists to transform.
fn legacy_directory(project_name: &str) -> (TempDir, Uuid, PathBuf) {
    let dir = TempDir::new().unwrap();
    let project_id = Uuid::now_v7();
    let root = dir
        .path()
        .join(".switchboard")
        .join("projects")
        .join(project_id.to_string());
    std::fs::create_dir_all(root.join("sessions")).unwrap();
    std::fs::create_dir_all(root.join("attachments")).unwrap();

    std::fs::write(
        dir.path().join(".switchboard").join("projects.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "id": project_id,
                "name": project_name,
                "created_at": "2026-08-23T15:39:44.709696Z"
            })
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("config.yaml"),
        format!("version: 1\nname: {project_name}\ncreated_at: 2026-08-23T15:39:44.709696Z\n"),
    )
    .unwrap();
    let agent_id = Uuid::now_v7();
    std::fs::write(
        root.join("registry.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "id": agent_id,
                "project_id": project_id,
                "name": "alice",
                "harness": "claude_code",
                "session_locator": {"uuid": Uuid::now_v7()},
                "created_at": "2026-08-23T15:39:44.709696Z"
            })
        ),
    )
    .unwrap();
    let staged = root.join("attachments").join("shot.png");
    std::fs::write(&staged, b"png").unwrap();
    std::fs::write(
        root.join("journal.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "send",
                "send_id": Uuid::new_v4(),
                "turn_id": Uuid::now_v7(),
                "agent_id": agent_id,
                "prompt": "look at this",
                "attachments": [{
                    "label": "image-1",
                    "kind": "image",
                    "path": staged.to_str().unwrap(),
                    "original_name": "shot.png",
                }],
                "at": "2026-08-23T15:40:00Z"
            })
        ),
    )
    .unwrap();
    // The lock token the tool must drop.
    std::fs::write(root.join("instance.lock"), b"").unwrap();
    (dir, project_id, staged)
}

fn send_attachment_paths(journal: &Path) -> Vec<(String, Option<String>)> {
    read_jsonl::<JournalRecord>(journal)
        .unwrap()
        .into_iter()
        .filter_map(|record| match record {
            JournalRecord::Send { attachments, .. } => Some(attachments),
            _ => None,
        })
        .flatten()
        .map(|a| (a.path, a.dispatched_path))
        .collect()
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn migrates_a_directory_end_to_end_and_the_app_code_reads_it_back() {
    let (legacy, project_id, staged) = legacy_directory("alpha");
    let target = TempDir::new().unwrap();

    let report =
        switchboard_migrate::migrate(&[legacy.path().to_path_buf()], target.path()).unwrap();
    assert_eq!(report.migrated.len(), 1);
    assert_eq!(
        report.migrated[0].projects,
        vec![(project_id, "alpha".to_owned())]
    );
    assert_eq!(
        report.migrated[0].attachments_rewritten, 1,
        "a clean exit is not evidence the rewrite fired; this count is"
    );
    assert!(report.migrated[0].attachments_left.is_empty());
    assert!(report.skipped.is_empty());

    // Everything below reads through the app's own code paths — the point of
    // the tool being Rust at all.
    let store = Store::open(target.path()).unwrap();
    let indexed = store.list_projects().unwrap();
    assert_eq!(indexed.len(), 1);
    assert_eq!(indexed[0].id, project_id, "the project keeps its identity");
    let project = store.open_project(project_id).unwrap();
    assert_eq!(project.list_agents().unwrap().len(), 1);

    // The directory identity was stamped everywhere it belongs.
    let config: ProjectConfig = read_yaml(&project.root.join("config.yaml")).unwrap();
    assert_eq!(config.directory_id, Some(indexed[0].directory_id));
    let catalog = store.list_directories().unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(
        catalog[0].path,
        legacy.path().canonicalize().unwrap(),
        "the catalog maps the identity back to the working directory"
    );

    // Attachment: file copied, path rewritten, original preserved.
    let paths = send_attachment_paths(&project.root.join("journal.jsonl"));
    assert_eq!(paths.len(), 1);
    let (new_path, dispatched) = &paths[0];
    assert!(
        Path::new(new_path).starts_with(&project.root),
        "path must point into the migrated project, got {new_path}"
    );
    assert!(Path::new(new_path).exists(), "the staged file came along");
    assert_eq!(
        dispatched.as_deref(),
        Some(staged.to_str().unwrap()),
        "the originally-dispatched path survives — send<->turn correlation \
         reconstructs the exact dispatched text, footer paths included"
    );

    assert!(
        !project.root.join("instance.lock").exists(),
        "a dead process's lock token must not be copied"
    );
    assert!(
        legacy.path().join(".switchboard").exists(),
        "originals untouched"
    );
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn refuses_a_target_that_already_holds_projects() {
    // No merging, ever: re-run means delete-and-redo. Merging is where the
    // dangerous machinery (idempotency records, write-once guards, app locks)
    // would come back in.
    let (legacy, _, _) = legacy_directory("alpha");
    let target = TempDir::new().unwrap();
    switchboard_migrate::migrate(&[legacy.path().to_path_buf()], target.path()).unwrap();

    let (second, _, _) = legacy_directory("beta");
    let err = switchboard_migrate::migrate(&[second.path().to_path_buf()], target.path())
        .expect_err("a populated target must be refused");
    assert!(err.to_string().contains("never merges"), "got: {err}");
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn an_unavailable_directory_is_skipped_and_the_rest_migrate() {
    let (legacy, _, _) = legacy_directory("alpha");
    let gone = PathBuf::from("/nonexistent/never-here");
    let target = TempDir::new().unwrap();

    let report =
        switchboard_migrate::migrate(&[gone.clone(), legacy.path().to_path_buf()], target.path())
            .unwrap();
    assert_eq!(report.migrated.len(), 1, "the available directory migrated");
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].0, gone);
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn a_journal_path_under_no_known_root_is_reported_not_silent() {
    // The failure that must never be silent: a journal path still pointing into
    // a legacy root after the rewrite means the app's GC deletes the migrated
    // file on the project's first open. Simulated here by a path under a
    // .switchboard root the tool knows about but spelled under neither the
    // as-recorded nor the canonical form — a moved-then-restored checkout shape.
    let (legacy, project_id, _) = legacy_directory("alpha");
    let root = legacy
        .path()
        .join(".switchboard")
        .join("projects")
        .join(project_id.to_string());
    // Overwrite the journal with a path under a *different* spelling of this
    // project's own legacy root: prefix-matches neither source root, but is
    // inside the legacy layout, so leaving it unrewritten is the deletion bug.
    let phantom = format!(
        "/nonexistent-old-mount{}/attachments/shot.png",
        root.display()
    );
    std::fs::write(
        root.join("journal.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "send",
                "send_id": Uuid::new_v4(),
                "turn_id": Uuid::now_v7(),
                "agent_id": Uuid::now_v7(),
                "prompt": "p",
                "attachments": [{
                    "label": "image-1",
                    "kind": "image",
                    "path": phantom,
                    "original_name": "shot.png",
                }],
                "at": "2026-08-23T15:40:00Z"
            })
        ),
    )
    .unwrap();
    let target = TempDir::new().unwrap();
    let report =
        switchboard_migrate::migrate(&[legacy.path().to_path_buf()], target.path()).unwrap();
    // Under neither root ⇒ left alone, but visibly: the report names it. This is
    // the deliberate outcome for a genuinely-external path; the *validation*
    // failure below is reserved for a path that still matches a known root.
    assert_eq!(report.migrated[0].attachments_rewritten, 0);
    assert_eq!(report.migrated[0].attachments_left, vec![phantom]);
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn a_duplicated_project_id_across_directories_is_refused_before_any_write() {
    // The realistic shape: a cp -a'd checkout (or a restore) carries the same
    // .switchboard state, so two legitimate directories list the same ids.
    // Unrefused, both copy into one projects/<id>/ while appending two index
    // rows with different owners.
    let (first, project_id, _) = legacy_directory("alpha");
    let second = TempDir::new().unwrap();
    copy_dir(
        &first.path().join(".switchboard"),
        &second.path().join(".switchboard"),
    );

    let target = TempDir::new().unwrap();
    let err = switchboard_migrate::migrate(
        &[first.path().to_path_buf(), second.path().to_path_buf()],
        target.path(),
    )
    .expect_err("the same project id in two sources must refuse");
    assert!(
        err.to_string().contains(&project_id.to_string()) && err.to_string().contains("copied"),
        "the refusal must name the id and the likely cause: {err}"
    );
    // Before any write: the refusal must not leave half a store behind.
    let store = Store::open(target.path()).unwrap();
    assert!(store.list_projects().unwrap().is_empty());
    assert!(store.list_directories().unwrap().is_empty());
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn a_target_with_residue_but_an_empty_index_is_refused() {
    // A crashed run can leave a catalog row or an orphan project directory with
    // no index row. "Fresh" must mean fresh, not "no indexed projects" — the
    // loose check would run over the residue and validate clean around it.
    let (legacy, _, _) = legacy_directory("alpha");
    let target = TempDir::new().unwrap();
    {
        let store = Store::open(target.path()).unwrap();
        let cwd = TempDir::new().unwrap();
        store.add_directory(cwd.path()).unwrap();
    }
    let err = switchboard_migrate::migrate(&[legacy.path().to_path_buf()], target.path())
        .expect_err("a catalog row is residue; the target is not fresh");
    assert!(err.to_string().contains("not empty"), "got: {err}");
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn a_relative_target_writes_absolute_journal_paths() {
    // A relative target would write relative attachment paths into the
    // journals, which can never match the absolute paths the app's directory
    // scan produces — so the GC would delete every migrated attachment on
    // first open.
    let (legacy, project_id, _) = legacy_directory("alpha");
    let parent = TempDir::new().unwrap();
    // `set_current_dir` is process-global and tests run in parallel; safe here
    // only because every other test in this binary uses absolute paths
    // exclusively. A future test that touches a relative path must not share a
    // binary with this one.
    let prior = std::env::current_dir().unwrap();
    std::env::set_current_dir(parent.path()).unwrap();
    let result = switchboard_migrate::migrate(
        &[legacy.path().to_path_buf()],
        Path::new("./relative-target"),
    );
    std::env::set_current_dir(prior).unwrap();
    result.unwrap();

    let store = Store::open(&parent.path().join("relative-target")).unwrap();
    let project = store.open_project(project_id).unwrap();
    let paths = send_attachment_paths(&project.root.join("journal.jsonl"));
    assert!(
        Path::new(&paths[0].0).is_absolute(),
        "journal paths must be absolute, got {}",
        paths[0].0
    );
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn a_symlinked_target_spelling_is_kept_verbatim() {
    // The app opens the store through its *configured* spelling, symlinks
    // unresolved — so the tool resolving them would write journal paths in one
    // spelling while the app's GC compares in another, and every migrated
    // attachment would be deleted on first open. The as-given spelling must
    // survive into the journals.
    let (legacy, project_id, _) = legacy_directory("alpha");
    let real = TempDir::new().unwrap();
    let link_holder = TempDir::new().unwrap();
    let link = link_holder.path().join("store-link");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();

    switchboard_migrate::migrate(&[legacy.path().to_path_buf()], &link).unwrap();

    let store = Store::open(&link).unwrap();
    let project = store.open_project(project_id).unwrap();
    let paths = send_attachment_paths(&project.root.join("journal.jsonl"));
    assert!(
        paths[0].0.starts_with(link.to_str().unwrap()),
        "the journal must carry the as-given (symlinked) spelling, got {}",
        paths[0].0
    );
}

/// Plain recursive copy for fixtures (mirrors cp -a on the legacy dir).
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
#[ignore = "migration-tool development test — run with: cargo test -p switchboard-migrate -- --ignored"]
fn a_directory_without_legacy_state_reports_as_empty() {
    let plain = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    let report =
        switchboard_migrate::migrate(&[plain.path().to_path_buf()], target.path()).unwrap();
    assert!(report.migrated.is_empty());
    assert_eq!(report.empty.len(), 1);
}
