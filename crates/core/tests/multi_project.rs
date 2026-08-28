//! End-to-end regression test for the multi-project model + on-disk layout.
//!
//! Covers: two working directories, each with a project that has its own
//! agents, with agents and projects named the same across them — and the store's
//! on-disk layout asserted against the system-design §3 spec.
//!
//! The layout assertion is the point: it pins that **nothing is written into the
//! working directories**. That is the property the move exists for — deleting a
//! checkout must not destroy the projects that ran in it.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use switchboard_core::{HarnessKind, Project, ProjectId, Store};
use tempfile::TempDir;

// `agent_a` (the created record) vs `agents_a` (the listed slice) read clearly
// in test code — suppress the workspace-wide `similar_names` lint here so we
// don't have to allow it for production code where it might catch real issues.
#[allow(clippy::similar_names)]
#[test]
fn multi_project_store_end_to_end_with_layout_assertion() {
    let root = TempDir::new().unwrap();
    let cwd_one = TempDir::new().unwrap();
    let cwd_two = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    Store::open(root.path()).unwrap(); // Opening twice is idempotent.

    let dir_one = store.add_directory(cwd_one.path()).unwrap().directory_id;
    let dir_two = store.add_directory(cwd_two.path()).unwrap().directory_id;
    assert_eq!(
        store.add_directory(cwd_one.path()).unwrap().directory_id,
        dir_one,
        "re-adding a directory must not mint a second identity"
    );

    // Two projects in one directory, plus one in another that reuses a name —
    // uniqueness is per directory, so two checkouts can each have an `api`.
    let project_a = store.create_project(dir_one, "backend-feature").unwrap();
    let project_b = store.create_project(dir_one, "frontend-feature").unwrap();
    let project_c = store.create_project(dir_two, "backend-feature").unwrap();
    assert_ne!(project_a.id, project_b.id);
    assert_ne!(project_a.id, project_c.id);

    // Same agent name in different projects must succeed — uniqueness is
    // project-scoped.
    let agent_a = project_a
        .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
        .unwrap();
    let agent_b = project_b
        .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
        .unwrap();
    assert_ne!(agent_a.id, agent_b.id);
    // Both must be Some (Claude Code pre-generates) AND distinct. A bare
    // assert_ne! would silently pass if both were None.
    assert!(agent_a.session_locator.is_some());
    assert!(agent_b.session_locator.is_some());
    assert_ne!(agent_a.session_locator, agent_b.session_locator);

    // A second agent in project_a, to confirm registries don't cross-pollinate.
    let reviewer_a = project_a
        .register_agent("reviewer", HarnessKind::ClaudeCode, None, None)
        .unwrap();

    // Reopen the store and re-read everything from disk.
    let reopened = Store::open(root.path()).unwrap();
    let entries = reopened.list_projects().unwrap();
    assert_eq!(entries.len(), 3);
    let names: HashSet<_> = entries
        .iter()
        .filter(|e| e.directory_id == dir_one)
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(
        names,
        HashSet::from(["backend-feature".to_owned(), "frontend-feature".to_owned()])
    );

    let reopened_a: Project = reopened.open_project(project_a.id).unwrap();
    let reopened_b: Project = reopened.open_project(project_b.id).unwrap();
    assert_eq!(
        reopened_a.directory,
        std::fs::canonicalize(cwd_one.path()).unwrap(),
        "a reopened project must resolve its working directory through the catalog"
    );
    let agents_a = reopened_a.list_agents().unwrap();
    let agents_b = reopened_b.list_agents().unwrap();
    assert_eq!(agents_a.len(), 2);
    assert_eq!(agents_b.len(), 1);
    let names_a: HashSet<_> = agents_a.iter().map(|a| a.name.clone()).collect();
    assert_eq!(
        names_a,
        HashSet::from(["assistant".to_owned(), "reviewer".to_owned()])
    );
    assert_eq!(agents_b[0].name, "assistant");

    // Cross-project agent IDs do not leak.
    let ids_a: HashSet<_> = agents_a.iter().map(|a| a.id).collect();
    let ids_b: HashSet<_> = agents_b.iter().map(|a| a.id).collect();
    assert!(ids_a.is_disjoint(&ids_b));
    assert!(ids_a.contains(&agent_a.id));
    assert!(ids_a.contains(&reviewer_a.id));
    assert!(ids_b.contains(&agent_b.id));

    // Deleting the working directory must not touch any of it.
    drop(cwd_one);
    assert_eq!(
        reopened.list_projects().unwrap().len(),
        3,
        "projects outlive the checkout they were created in"
    );
    assert_eq!(
        reopened.list_project_agents(project_a.id).unwrap().len(),
        2,
        "so do their agents"
    );

    assert_layout(
        root.path(),
        cwd_two.path(),
        &[project_a.id, project_b.id, project_c.id],
    );
}

fn assert_layout(store_root: &Path, working_directory: &Path, project_ids: &[ProjectId]) {
    // Nothing in the working directory. This is the whole point of the move: a
    // deleted checkout must cost the user nothing.
    assert!(
        !working_directory.join(".switchboard").exists(),
        "the store must write nothing into a working directory"
    );

    for relative in [
        "store.yaml",
        "projects.jsonl",
        "directories.jsonl",
        "projects",
        "attachments",
    ] {
        assert!(
            store_root.join(relative).exists(),
            "missing {relative} under the store root"
        );
    }
    assert!(store_root.join("projects").is_dir());

    // Prompts and workflows are user-global siblings of the store, not inside
    // it (system-design §3/§6).
    assert!(!store_root.join("prompts").exists());
    assert!(!store_root.join("workflows").exists());

    for project_id in project_ids {
        let project_root = store_root.join("projects").join(project_id.to_string());
        assert!(
            project_root.is_dir(),
            "project root missing for {project_id}"
        );
        assert!(project_root.join("config.yaml").exists());
        assert!(project_root.join("registry.jsonl").exists());
        // sessions/ is created lazily on the first Codex dispatch; runs/ on the
        // first turn that emits a run log. Neither exists yet — only Claude
        // agents have been registered and nothing has dispatched.
        assert!(!project_root.join("sessions").exists());
        assert!(!project_root.join("runs").exists());
    }

    // projects.jsonl is append-only with one ProjectEntry per line.
    let index = fs::read_to_string(store_root.join("projects.jsonl")).unwrap();
    let line_count = index.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(line_count, project_ids.len(), "projects.jsonl line count");
}
