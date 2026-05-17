//! Codex skills registry loader.
//!
//! Skills are directory-based, not config-file-derived. Codex (and other
//! agentic CLIs that share the `~/.agents/skills/` convention) treats each
//! immediate subdirectory containing a `SKILL.md` as one skill; the
//! directory name is the skill name.
//!
//! Two scopes:
//! - `~/.agents/skills/<name>/SKILL.md` — user-scope.
//! - `<cwd>/.agents/skills/<name>/SKILL.md` — project-scope.
//!
//! Merge by name; project-scope wins (the merged surface is just the name
//! list, so the win is observationally idempotent — see `config.rs` for the
//! same pattern).
//!
//! **Failure policy** mirrors the MCP loader: missing directory → empty list
//! no warning; unreadable directory → empty list + warning; per-entry
//! malformed (subdir without `SKILL.md`, non-directory entry) → silently
//! skipped (each is a normal "scratch / WIP directory" state, not an error).

use std::path::Path;

/// Resolve the skills registry for a Codex agent.
///
/// `home_dir` is injected (not derived from `HOME`) for the same testability
/// reason as `config::load_mcp_servers`. `cwd` is the user's bound working
/// directory.
#[must_use]
pub fn load_skills(home_dir: &Path, cwd: &Path) -> Vec<String> {
    let user_skills = scan_skills_directory(&home_dir.join(".agents").join("skills"));
    let project_skills = scan_skills_directory(&cwd.join(".agents").join("skills"));
    merge_scopes(user_skills, project_skills)
}

/// Scan one skills directory for immediate subdirs that contain `SKILL.md`.
/// Returns the subdir names in alphabetical order (`DirEntry` order is
/// unspecified across platforms; sorting makes the surface deterministic for
/// tests and review).
fn scan_skills_directory(skills_dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                path = %skills_dir.display(),
                error = %e,
                "Codex skills: failed to read directory; treating as empty"
            );
            return Vec::new();
        }
    };

    let mut names = Vec::new();
    for entry_result in entries {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    path = %skills_dir.display(),
                    error = %e,
                    "Codex skills: directory iteration error on one entry; skipping"
                );
                continue;
            }
        };
        let path = entry.path();
        if !path.is_dir() {
            // Top-level non-directory (e.g., a README.md alongside skill
            // subdirs). Not an error — silently skip.
            continue;
        }
        if !path.join("SKILL.md").is_file() {
            // Subdir without SKILL.md. Could be a WIP / scratch directory —
            // silently skip (consistent with Codex's own behavior).
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        names.push(name.to_owned());
    }
    names.sort();
    names
}

fn merge_scopes(user_skills: Vec<String>, project_skills: Vec<String>) -> Vec<String> {
    let mut merged = user_skills;
    for name in project_skills {
        if !merged.iter().any(|existing| existing == &name) {
            merged.push(name);
        }
    }
    // Don't re-sort after the project-scope append: the user-then-project
    // ordering is a clearer story for the sidebar ("inherited from user
    // first, project-specific after") than alphabetical across both. Within
    // each scope the list is already sorted by `scan_skills_directory`.
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_skill(parent: &Path, name: &str) {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "# skill content").unwrap();
    }

    #[test]
    fn missing_directories_yield_empty_vec() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let result = load_skills(home.path(), cwd.path());
        assert!(result.is_empty());
    }

    #[test]
    fn user_scope_subdirs_with_skill_md_are_returned_sorted() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        make_skill(&user_skills_dir, "zebra");
        make_skill(&user_skills_dir, "alpha");
        make_skill(&user_skills_dir, "mango");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn project_scope_appends_non_colliding_skills() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
        let project_skills_dir = cwd.path().join(".agents").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        std::fs::create_dir_all(&project_skills_dir).unwrap();
        make_skill(&user_skills_dir, "user_skill");
        make_skill(&project_skills_dir, "project_skill");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["user_skill", "project_skill"]);
    }

    #[test]
    fn project_scope_does_not_duplicate_colliding_names() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
        let project_skills_dir = cwd.path().join(".agents").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        std::fs::create_dir_all(&project_skills_dir).unwrap();
        make_skill(&user_skills_dir, "shared");
        make_skill(&project_skills_dir, "shared");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["shared"]);
    }

    #[test]
    fn subdir_without_skill_md_is_silently_skipped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        // "scratch" exists but has no SKILL.md — skip.
        std::fs::create_dir(user_skills_dir.join("scratch")).unwrap();
        make_skill(&user_skills_dir, "real_skill");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["real_skill"]);
    }

    #[test]
    fn top_level_files_alongside_skill_subdirs_are_skipped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        // A README.md at the top level (not a subdir) — must be skipped.
        std::fs::write(user_skills_dir.join("README.md"), "user-level readme").unwrap();
        make_skill(&user_skills_dir, "real_skill");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["real_skill"]);
    }

    #[test]
    fn skills_path_that_is_a_file_returns_empty_no_propagation() {
        // Pathologic case: user (or some script) placed a regular file at
        // ~/.agents/skills instead of a directory. `read_dir` fails with
        // NotADirectory. Adapter must not propagate — registries are
        // display-only. Mirrors config.rs's `user_only_unreadable_file_...`
        // test for symmetric partial-parse coverage.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agents_dir = home.path().join(".agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("skills"), "i am a file, not a directory").unwrap();

        // Must not panic, must not return an error — just an empty list.
        let result = load_skills(home.path(), cwd.path());
        assert!(result.is_empty());
    }

    #[test]
    fn empty_skills_directory_yields_empty_vec() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();

        let result = load_skills(home.path(), cwd.path());
        assert!(result.is_empty());
    }
}
