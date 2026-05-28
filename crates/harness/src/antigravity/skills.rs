//! Antigravity skills registry loader.
//!
//! Skills are nested under plugins:
//! `~/.gemini/config/plugins/<plugin>/skills/<skill>/SKILL.md`. Each immediate
//! subdirectory of a plugin's `skills/` that contains a `SKILL.md` is one
//! skill, displayed **qualified** as `<plugin>/<skill>` so two plugins shipping
//! a same-named skill stay distinct.
//!
//! **User scope only.** Workspace-scoped plugins are plausible but unverified
//! (see `docs/research/archive/antigravity-cli-observed.md`), so the `cwd` parameter is
//! reserved for a future workspace scope and not scanned today.
//!
//! Kept separate from `gemini/skills.rs` rather than shared: Gemini's skills
//! are flat (`~/.agents/skills/<name>/`) while Antigravity's are nested under
//! plugins, so the directory walk genuinely differs — they share the `SKILL.md`
//! convention but not the layout. Keep loaders separate until a stable shared
//! contract exists.
//!
//! **Failure policy** (display-only registry, best-effort):
//! - Plugins root missing → empty, no warning.
//! - Plugins root unreadable → empty + one warning.
//! - A single plugin whose `skills/` is unreadable → that plugin contributes
//!   nothing + one warning, but the scan continues — one bad plugin must not
//!   erase the rest.
//! - A subdir without `SKILL.md`, or a non-directory entry → silently skipped.

use std::path::Path;

use super::paths;

/// Resolve the skills registry for an Antigravity agent, as qualified
/// `<plugin>/<skill>` names, sorted.
///
/// `_cwd` is reserved for a future workspace scope (not scanned — user scope
/// only).
#[must_use]
pub fn load_skills(home_dir: &Path, _cwd: &Path) -> Vec<String> {
    let plugins_root = paths::plugins_root(home_dir);
    let plugin_entries = match std::fs::read_dir(&plugins_root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                path = %plugins_root.display(),
                error = %e,
                "Antigravity skills: failed to read plugins root; treating as empty"
            );
            return Vec::new();
        }
    };

    let mut qualified = Vec::new();
    for plugin in plugin_entries.flatten() {
        let plugin_path = plugin.path();
        if !plugin_path.is_dir() {
            continue;
        }
        let Some(plugin_name) = plugin_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        for skill in scan_plugin_skills(&plugin_path.join("skills")) {
            qualified.push(format!("{plugin_name}/{skill}"));
        }
    }
    qualified.sort();
    qualified
}

/// Scan one plugin's `skills/` directory for subdirectories containing a
/// `SKILL.md`. A missing dir is normal (a plugin may ship no skills); an
/// unreadable dir warns but does not abort the caller's scan of other plugins.
fn scan_plugin_skills(skills_dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                path = %skills_dir.display(),
                error = %e,
                "Antigravity skills: failed to read a plugin's skills dir; skipping that plugin"
            );
            return Vec::new();
        }
    };

    let mut names = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join("SKILL.md").is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            names.push(name.to_owned());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_skill(plugins_root: &Path, plugin: &str, skill: &str) {
        let dir = plugins_root.join(plugin).join("skills").join(skill);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "# skill content").unwrap();
    }

    #[test]
    fn missing_plugins_root_yields_empty() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        assert!(load_skills(home.path(), cwd.path()).is_empty());
    }

    #[test]
    fn single_plugin_multiple_skills_qualified_and_sorted() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let root = paths::plugins_root(home.path());
        make_skill(&root, "chrome-devtools-plugin", "troubleshooting");
        make_skill(&root, "chrome-devtools-plugin", "a11y-debugging");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(
            result,
            vec![
                "chrome-devtools-plugin/a11y-debugging",
                "chrome-devtools-plugin/troubleshooting",
            ]
        );
    }

    #[test]
    fn multiple_plugins_each_contribute_qualified_names() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let root = paths::plugins_root(home.path());
        make_skill(&root, "plugin-b", "skill-x");
        make_skill(&root, "plugin-a", "skill-x");

        // Same skill name across plugins stays distinct via qualification.
        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["plugin-a/skill-x", "plugin-b/skill-x"]);
    }

    #[test]
    fn skill_dir_without_skill_md_is_skipped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let root = paths::plugins_root(home.path());
        make_skill(&root, "p", "real");
        std::fs::create_dir_all(root.join("p").join("skills").join("empty")).unwrap();

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["p/real"]);
    }

    #[test]
    fn plugin_without_skills_dir_is_skipped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let root = paths::plugins_root(home.path());
        // A plugin directory that ships no skills/ subdir at all.
        std::fs::create_dir_all(root.join("bare-plugin")).unwrap();
        make_skill(&root, "real-plugin", "s");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["real-plugin/s"]);
    }

    #[test]
    fn one_unreadable_plugin_does_not_abort_scanning_others() {
        // Best-effort: a plugin whose skills/ path is a file (not a dir) must
        // not erase the other plugins' skills.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let root = paths::plugins_root(home.path());
        make_skill(&root, "good-plugin", "good-skill");
        // "broken-plugin/skills" is a regular file, so read_dir on it errors.
        let broken = root.join("broken-plugin");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("skills"), "not a directory").unwrap();

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(
            result,
            vec!["good-plugin/good-skill"],
            "the readable plugin's skill survives the broken one"
        );
    }

    #[test]
    fn non_directory_entry_in_plugins_root_is_skipped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let root = paths::plugins_root(home.path());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "not a plugin").unwrap();
        make_skill(&root, "p", "s");

        let result = load_skills(home.path(), cwd.path());
        assert_eq!(result, vec!["p/s"]);
    }
}
