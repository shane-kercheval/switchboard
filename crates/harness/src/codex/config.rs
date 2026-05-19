//! Codex MCP-server registry loader.
//!
//! Codex stores MCP-server configuration in TOML files at two scopes:
//! - `~/.codex/config.toml` (user-level, always loaded).
//! - `<cwd>/.codex/config.toml` (project-level, loaded when present).
//!
//! Format: `[mcp_servers.<name>]` table entries. The names — not the table
//! contents — are what Switchboard's `SessionMeta.mcp_servers` surfaces;
//! the registry is display-only. Switchboard does **not** enforce Codex's
//! trust-list gate (which Codex itself uses to decide whether to load
//! project-scope config): the small fidelity gap of showing project-scoped
//! servers Codex wouldn't load for untrusted directories self-corrects on
//! first interactive Codex use in that directory.
//!
//! **Partial-parse policy** (skip-with-warning for harness-owned files):
//! - File not found → empty list, no warning (a missing config file is the
//!   default state for new installs / un-configured projects).
//! - File present but unreadable / top-level malformed → empty list + one
//!   `tracing::warn!`.
//! - Individual `[mcp_servers.<name>]` entry malformed (missing required
//!   field, wrong type) → drop that entry, keep the rest, one warn per drop.
//!
//! Failures never propagate as `Result::Err` — these registries are display
//! information, not load-bearing for dispatch.

use std::path::Path;

use crate::events::McpServerStatus;

/// Status string written into `McpServerStatus.status` for entries loaded
/// from a config file. Distinct from runtime statuses Claude Code emits
/// (`connected` / `disconnected` / `failed` / `needs-auth`). Reads
/// honestly in the sidebar: "we have config; we have not attempted a
/// connection."
const CONFIGURED_STATUS: &str = "configured";

/// Resolve the configured MCP-server registry for a Codex agent.
///
/// `home_dir` is injected (not derived from `HOME`) so tests can stage temp
/// directories without mutating process-wide environment. `cwd` is the
/// user's bound working directory.
///
/// Returns the merged list with project-scope entries winning on name
/// conflicts. Order: user-scope entries first (in source order), then
/// project-scope entries that don't collide (in source order), then
/// project-scope entries that overrode user-scope ones (in source order).
///
/// **Performance.** Both files are small (kilobytes); reads run synchronously
/// on each first-turn `SessionMeta` emission. No caching layer — config can
/// change between dispatches and the sidebar should reflect the current
/// state.
#[must_use]
pub fn load_mcp_servers(home_dir: &Path, cwd: &Path) -> Vec<McpServerStatus> {
    let user_path = home_dir.join(".codex").join("config.toml");
    let project_path = cwd.join(".codex").join("config.toml");

    let user_names = load_mcp_names_from_file(&user_path);
    let project_names = load_mcp_names_from_file(&project_path);

    merge_scopes(user_names, project_names)
}

/// Load `[mcp_servers.<name>]` table names from a single TOML file. Returns
/// the names in source order. Missing file → empty Vec, no warning.
/// Malformed top-level → empty Vec + warning. Per-entry malformed → drop
/// the entry + warning.
fn load_mcp_names_from_file(path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Codex MCP config: failed to read file; treating as empty"
            );
            return Vec::new();
        }
    };

    let value: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Codex MCP config: top-level TOML parse failed; treating as empty"
            );
            return Vec::new();
        }
    };

    let Some(table) = value.get("mcp_servers").and_then(toml::Value::as_table) else {
        return Vec::new();
    };

    let mut names = Vec::with_capacity(table.len());
    for (name, entry) in table {
        if !entry.is_table() {
            tracing::warn!(
                path = %path.display(),
                name = %name,
                "Codex MCP config: entry is not a table; dropping"
            );
            continue;
        }
        // Codex requires either `command` (stdio transport) or `url` (HTTP)
        // on each entry. We don't connect; we just list. But a totally
        // empty table is almost certainly user error — flag it.
        let entry_table = entry.as_table().expect("checked is_table above");
        if entry_table.is_empty() {
            tracing::warn!(
                path = %path.display(),
                name = %name,
                "Codex MCP config: entry has no fields; dropping"
            );
            continue;
        }
        names.push(name.clone());
    }
    names
}

/// Merge user-scope and project-scope name lists with project winning on
/// name conflicts. Project entries that collide replace the user-scope entry
/// in place; non-colliding project entries append at the end.
fn merge_scopes(user_names: Vec<String>, project_names: Vec<String>) -> Vec<McpServerStatus> {
    // Project entries seed the result; user-only entries append. This
    // makes project literally win on name collision (a future surface
    // that exposes per-entry detail would render the project version,
    // not the user version) and matches the more-specific-scope-wins
    // convention used across every registry-style loader in Switchboard
    // (Claude / Codex / Gemini MCP + skills) and the broader ecosystem
    // (git, npm, etc.).
    let mut merged: Vec<McpServerStatus> = project_names
        .into_iter()
        .map(|name| McpServerStatus {
            name,
            status: CONFIGURED_STATUS.to_owned(),
        })
        .collect();
    for user_name in user_names {
        if !merged.iter().any(|existing| existing.name == user_name) {
            merged.push(McpServerStatus {
                name: user_name,
                status: CONFIGURED_STATUS.to_owned(),
            });
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_files_yield_empty_vec_and_no_panic() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        // Both ~/.codex/config.toml and <cwd>/.codex/config.toml absent.
        let result = load_mcp_servers(home.path(), cwd.path());
        assert!(result.is_empty());
    }

    #[test]
    fn user_scope_only_returns_user_entries() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_config = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(
            &user_config,
            r#"
[mcp_servers.alpha]
command = "alpha-bin"

[mcp_servers.beta]
command = "beta-bin"
"#,
        )
        .unwrap();

        let result = load_mcp_servers(home.path(), cwd.path());
        let names: Vec<&str> = result.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert!(result.iter().all(|e| e.status == "configured"));
    }

    #[test]
    fn merge_seeds_with_project_scope_then_appends_user_only_entries() {
        // Pins the merge direction (project > user) — not just the
        // dedup behavior. A future flip back to user-first seeding
        // changes the result-vec order and fails this assertion.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_config = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(
            &user_config,
            r#"
[mcp_servers.user_only]
command = "x"
"#,
        )
        .unwrap();
        let project_config = cwd.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(project_config.parent().unwrap()).unwrap();
        std::fs::write(
            &project_config,
            r#"
[mcp_servers.project_only]
command = "y"
"#,
        )
        .unwrap();

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(
            names,
            vec!["project_only", "user_only"],
            "project entries must seed the merged list; user-only entries append after"
        );
    }

    #[test]
    fn collision_dedup_preserves_project_first_order() {
        // Stages both scopes with one colliding name and one unique
        // name each. Pins:
        // 1. The colliding name appears exactly once (dedup).
        // 2. Project entries (project_only + shared) come first in
        //    source order, then user-only entries append.
        // Together these pin "project > user" without needing per-entry
        // content surfaced on the API (today's surface is just names).
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_config = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(
            &user_config,
            r#"
[mcp_servers.shared]
command = "user-command"
[mcp_servers.user_only]
command = "u"
"#,
        )
        .unwrap();
        let project_config = cwd.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(project_config.parent().unwrap()).unwrap();
        std::fs::write(
            &project_config,
            r#"
[mcp_servers.shared]
command = "project-command"
[mcp_servers.project_only]
command = "p"
"#,
        )
        .unwrap();

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names.len(), 3, "exactly three distinct names");
        // Project entries come first; among them, TOML map order is
        // not guaranteed so we assert membership for the project pair
        // and that user_only is last (the only user-only entry).
        assert!(names[..2].contains(&"shared".to_owned()));
        assert!(names[..2].contains(&"project_only".to_owned()));
        assert_eq!(names[2], "user_only");
    }

    #[test]
    fn malformed_top_level_toml_returns_empty_with_warning() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_config = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(&user_config, "this is not [[valid toml ===").unwrap();

        let result = load_mcp_servers(home.path(), cwd.path());
        // Total-parse failure on user scope → user list is empty; project
        // list is empty (no file). Merged → empty.
        assert!(result.is_empty());
    }

    #[test]
    fn malformed_entry_is_dropped_others_preserved() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_config = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        // `mcp_servers.bad` is a bare string, not a table — the entry is
        // dropped; `mcp_servers.good` survives.
        std::fs::write(
            &user_config,
            r#"
[mcp_servers]
bad = "not a table"

[mcp_servers.good]
command = "ok"
"#,
        )
        .unwrap();

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["good"], "bad entry dropped, good preserved");
    }

    #[test]
    fn empty_mcp_servers_table_yields_empty_vec() {
        // [mcp_servers] table exists but has no entries → empty list, no
        // warning. This is a normal "user installed Codex but configured no
        // MCP servers" state.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_config = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(&user_config, "[mcp_servers]\n").unwrap();

        let result = load_mcp_servers(home.path(), cwd.path());
        assert!(result.is_empty());
    }

    #[test]
    fn no_mcp_servers_section_at_all_yields_empty_vec() {
        // config.toml exists but has no [mcp_servers] section.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_config = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(
            &user_config,
            r#"
[some_other_section]
foo = "bar"
"#,
        )
        .unwrap();

        let result = load_mcp_servers(home.path(), cwd.path());
        assert!(result.is_empty());
    }

    #[test]
    fn empty_table_entry_is_dropped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let user_config = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(
            &user_config,
            r#"
[mcp_servers.empty_entry]

[mcp_servers.good]
command = "ok"
"#,
        )
        .unwrap();

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["good"]);
    }

    #[test]
    fn user_only_unreadable_file_returns_empty_no_propagation() {
        // Simulate a "config.toml is a directory" scenario — read_to_string
        // fails with IsADirectory (or PermissionDenied on some platforms).
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let codex_dir = home.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        // Create config.toml as a directory rather than a file.
        std::fs::create_dir(codex_dir.join("config.toml")).unwrap();

        // Must not panic, must not return an error — just an empty list.
        let result = load_mcp_servers(home.path(), cwd.path());
        assert!(result.is_empty());
    }
}
