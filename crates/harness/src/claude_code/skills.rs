//! Claude Code skills registry loader.
//!
//! Skills are directory-based. Claude treats each immediate subdirectory
//! under one of the skills directories that contains a `SKILL.md` as one
//! skill; the directory name is the skill name.
//!
//! Two scopes:
//! - `~/.claude/skills/<name>/SKILL.md` — user-scope.
//! - `<cwd>/.claude/skills/<name>/SKILL.md` — project-scope.
//!
//! Merge by name; collisions deduplicate. Surface is just the name list.
//!
//! **Failure policy** mirrors the Codex skills loader (`codex/skills.rs`):
//! missing directory → empty no warning; unreadable directory → empty +
//! warning; per-entry malformed (subdir without `SKILL.md`, non-directory
//! entry) → silently skipped.

use std::path::Path;

/// Resolve the skills registry for a Claude Code agent.
#[must_use]
pub fn load_skills(home_dir: &Path, cwd: &Path) -> Vec<String> {
    let user_skills = scan_skills_directory(&home_dir.join(".claude").join("skills"));
    let project_skills = scan_skills_directory(&cwd.join(".claude").join("skills"));
    merge_scopes(user_skills, project_skills)
}

fn scan_skills_directory(skills_dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                path = %skills_dir.display(),
                error = %e,
                "Claude skills: failed to read directory; treating as empty"
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
                    "Claude skills: directory iteration error on one entry; skipping"
                );
                continue;
            }
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("SKILL.md").is_file() {
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
        assert!(load_skills(home.path(), cwd.path()).is_empty());
    }

    #[test]
    fn user_scope_subdirs_with_skill_md_are_returned_sorted() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".claude").join("skills");
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
        let user_skills_dir = home.path().join(".claude").join("skills");
        let project_skills_dir = cwd.path().join(".claude").join("skills");
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
        let user_skills_dir = home.path().join(".claude").join("skills");
        let project_skills_dir = cwd.path().join(".claude").join("skills");
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
        let user_skills_dir = home.path().join(".claude").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        std::fs::create_dir(user_skills_dir.join("scratch")).unwrap();
        make_skill(&user_skills_dir, "real_skill");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["real_skill"]);
    }

    #[test]
    fn skills_path_that_is_a_file_returns_empty_no_propagation() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let claude_dir = home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("skills"), "i am a file, not a directory").unwrap();

        let result = load_skills(home.path(), cwd.path());
        assert!(result.is_empty());
    }
}
