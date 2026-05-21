//! Gemini skills registry loader.
//!
//! Skills are directory-based. Gemini treats each immediate subdirectory
//! containing a `SKILL.md` as one skill; the directory name is the skill
//! name.
//!
//! Two scopes (`gemini skills install --scope user|workspace`):
//! - `~/.agents/skills/<name>/SKILL.md` — user scope. Gemini's user-scope
//!   skills directory is the shared `~/.agents/skills/` location (not
//!   `~/.gemini/skills/`); the `gemini skills install` default writes
//!   here.
//! - `<cwd>/.gemini/skills/<name>/SKILL.md` — workspace scope. The
//!   `--scope workspace` flag writes here.
//!
//! Merge rule: **workspace > user** on name collision. The merged surface
//! is just the name list (the registry is display-only). Workspace
//! entries seed the result; user-only entries are appended. Matches the
//! more-specific-scope-wins convention used across every registry-style
//! loader in Switchboard (Claude / Codex / Gemini MCP + skills) and the
//! broader ecosystem (git, npm, etc.).
//!
//! **Failure policy** mirrors the Claude and Codex skills loaders:
//! missing directory → empty no warning; unreadable directory → empty +
//! warning; per-entry malformed (subdir without `SKILL.md`, non-directory
//! entry) → silently skipped.

use std::path::Path;

/// Resolve the skills registry for a Gemini agent.
#[must_use]
pub fn load_skills(home_dir: &Path, cwd: &Path) -> Vec<String> {
    let user_skills = scan_skills_directory(&home_dir.join(".agents").join("skills"));
    let workspace_skills = scan_skills_directory(&cwd.join(".gemini").join("skills"));
    merge_scopes(user_skills, workspace_skills)
}

fn scan_skills_directory(skills_dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                path = %skills_dir.display(),
                error = %e,
                "Gemini skills: failed to read directory; treating as empty"
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
                    "Gemini skills: directory iteration error on one entry; skipping"
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

fn merge_scopes(user_skills: Vec<String>, workspace_skills: Vec<String>) -> Vec<String> {
    let mut merged = workspace_skills;
    for name in user_skills {
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
        let user_skills_dir = home.path().join(".agents").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        make_skill(&user_skills_dir, "zebra");
        make_skill(&user_skills_dir, "alpha");
        make_skill(&user_skills_dir, "mango");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn merge_seeds_with_workspace_scope_then_appends_user_only_entries() {
        // Pins the merge direction (workspace > user) — not just the
        // dedup behavior. A future flip back to user-first seeding
        // changes the result-vec order and fails this assertion.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
        let workspace_skills_dir = cwd.path().join(".gemini").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        std::fs::create_dir_all(&workspace_skills_dir).unwrap();
        make_skill(&user_skills_dir, "user_skill");
        make_skill(&workspace_skills_dir, "workspace_skill");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(
            result,
            vec!["workspace_skill", "user_skill"],
            "workspace entries must seed the merged list; user-only entries append after"
        );
    }

    #[test]
    fn collision_dedup_preserves_workspace_first_order() {
        // Stages both scopes with one colliding name and one unique
        // name each. Pins:
        // 1. The colliding name appears exactly once (dedup).
        // 2. Workspace entries (collider + workspace_only) come first
        //    in source order, then user-only entries append.
        // Together these pin "workspace > user" without needing per-
        // entry content surfaced on the API (today's surface is just
        // names).
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
        let workspace_skills_dir = cwd.path().join(".gemini").join("skills");
        std::fs::create_dir_all(&user_skills_dir).unwrap();
        std::fs::create_dir_all(&workspace_skills_dir).unwrap();
        make_skill(&user_skills_dir, "shared");
        make_skill(&user_skills_dir, "user_only");
        make_skill(&workspace_skills_dir, "shared");
        make_skill(&workspace_skills_dir, "workspace_only");

        let result = load_skills(home.path(), cwd.path());
        // scan_skills_directory sorts within scope; merge appends user-
        // only after workspace, so the expected order is:
        //   workspace's sorted entries: shared, workspace_only
        //   then user-only (non-colliding): user_only
        assert_eq!(result, vec!["shared", "workspace_only", "user_only"]);
    }

    #[test]
    fn subdir_without_skill_md_is_silently_skipped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_skills_dir = home.path().join(".agents").join("skills");
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
        let agents_dir = home.path().join(".agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("skills"), "i am a file, not a directory").unwrap();

        let result = load_skills(home.path(), cwd.path());
        assert!(result.is_empty());
    }
}
