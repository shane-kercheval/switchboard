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
    assert_eq!(report.migrated[0].projects, vec!["alpha".to_owned()]);
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
fn a_directory_without_legacy_state_reports_as_empty() {
    let plain = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    let report =
        switchboard_migrate::migrate(&[plain.path().to_path_buf()], target.path()).unwrap();
    assert!(report.migrated.is_empty());
    assert_eq!(report.empty.len(), 1);
}
