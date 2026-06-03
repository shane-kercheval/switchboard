//! End-to-end regression test for the multi-project model + on-disk layout.
//!
//! Covers: a tempdir with two projects that each have their own agents, named
//! the same (`assistant`) across projects, with the on-disk file layout
//! asserted against the system-design §3 spec.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use switchboard_core::{Directory, HarnessKind, Project};
use tempfile::TempDir;

// `agent_a` (the created record) vs `agents_a` (the listed slice) read clearly
// in test code — suppress the workspace-wide `similar_names` lint here so we
// don't have to allow it for production code where it might catch real issues.
#[allow(clippy::similar_names)]
#[test]
fn multi_project_directory_end_to_end_with_layout_assertion() {
    let tmp = TempDir::new().unwrap();
    let directory = Directory::at(tmp.path()).unwrap();
    directory.init().unwrap();
    directory.init().unwrap(); // Calling twice is idempotent per the plan.

    // Two projects with distinct names.
    let project_a = directory.create_project("backend-feature").unwrap();
    let project_b = directory.create_project("frontend-feature").unwrap();
    assert_ne!(project_a.id, project_b.id);

    // Same agent name in both projects must succeed — uniqueness is project-scoped.
    let agent_a = project_a
        .register_agent("assistant", HarnessKind::ClaudeCode)
        .unwrap();
    let agent_b = project_b
        .register_agent("assistant", HarnessKind::ClaudeCode)
        .unwrap();
    assert_ne!(agent_a.id, agent_b.id);
    // Both must be Some (Claude Code pre-generates) AND distinct. A bare
    // assert_ne! would silently pass if both were None.
    assert!(agent_a.session_locator.is_some());
    assert!(agent_b.session_locator.is_some());
    assert_ne!(agent_a.session_locator, agent_b.session_locator);

    // Adding a second agent in project_a so we can confirm registries don't cross-pollinate.
    let reviewer_a = project_a
        .register_agent("reviewer", HarnessKind::ClaudeCode)
        .unwrap();

    // Reopen the directory and re-read everything from disk.
    let reopened = Directory::at(tmp.path()).unwrap();
    let summaries = reopened.list_projects().unwrap();
    assert_eq!(summaries.len(), 2);
    let names: HashSet<_> = summaries.iter().map(|s| s.name.clone()).collect();
    assert_eq!(
        names,
        HashSet::from(["backend-feature".to_owned(), "frontend-feature".to_owned()])
    );

    let reopened_a: Project = reopened.open_project(project_a.id).unwrap();
    let reopened_b: Project = reopened.open_project(project_b.id).unwrap();
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

    // Assert the on-disk layout exactly matches the system-design §3 spec.
    assert_layout(tmp.path(), &[project_a.id, project_b.id]);
}

fn assert_layout(directory: &Path, project_ids: &[uuid::Uuid]) {
    let sb = directory.join(".switchboard");
    assert!(sb.is_dir(), ".switchboard/ should exist");

    for relative in [
        "config.yaml",
        "workflows",
        "prompts",
        "projects.jsonl",
        "projects",
    ] {
        let path = sb.join(relative);
        assert!(path.exists(), "missing {relative} under .switchboard/");
    }
    assert!(sb.join("workflows").is_dir());
    assert!(sb.join("prompts").is_dir());
    assert!(sb.join("projects").is_dir());

    // Initial layout must NOT eagerly create artifacts that only get
    // populated by later runtime events (cross-process lock, per-agent
    // session sidecars, per-turn run logs). They appear lazily on first
    // use, not at directory init.
    assert!(!sb.join("instance.lock").exists());

    for project_id in project_ids {
        let project_root = sb.join("projects").join(project_id.to_string());
        assert!(
            project_root.is_dir(),
            "project root missing for {project_id}"
        );
        assert!(project_root.join("config.yaml").exists());
        assert!(project_root.join("registry.jsonl").exists());
        // sessions/ is created lazily on the first Codex dispatch; runs/
        // on the first turn that emits a run log. Neither exists yet
        // here — only Claude agents have been registered and nothing has
        // dispatched.
        assert!(!project_root.join("sessions").exists());
        assert!(!project_root.join("runs").exists());
    }

    // projects.jsonl is append-only with one ProjectSummary per line.
    let index = fs::read_to_string(sb.join("projects.jsonl")).unwrap();
    let line_count = index.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(line_count, project_ids.len(), "projects.jsonl line count");
}
