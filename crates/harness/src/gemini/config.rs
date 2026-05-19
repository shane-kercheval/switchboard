//! Gemini MCP-server registry loader.
//!
//! Gemini reads MCP-server config from two scopes:
//! - **User**: `~/.gemini/settings.json` top-level `"mcpServers"` object.
//! - **Project (workspace)**: `<cwd>/.gemini/settings.json` top-level
//!   `"mcpServers"` object.
//!
//! Both files share the same shape: `{ "mcpServers": { "<name>": {
//! "command": "...", "args": [...] } } }`. The `gemini mcp add` /
//! `gemini mcp remove` CLI subcommands manipulate these files; `--scope
//! user` writes the user file, default writes the workspace file.
//!
//! Merge rule: **project > user** on name collision. The merged surface
//! is just the name list (the registry is display-only — same rationale
//! as `claude_code/config.rs` and `codex/config.rs`). Order: project
//! entries first, then user-only — by-scope is a clearer story for the
//! sidebar than alphabetic merging.
//!
//! **Partial-parse policy** (mirrors the Claude and Codex loaders):
//! - File not found → empty contribution, no warning.
//! - File present but unreadable / top-level malformed → empty
//!   contribution + one `tracing::warn!`.
//! - Individual entry malformed (not a JSON object) → drop that entry,
//!   keep the rest, one warn per drop.
//!
//! Failures never propagate as `Result::Err` — these registries are
//! display information, not load-bearing for dispatch.

use std::path::Path;

use serde_json::Value;

use crate::events::McpServerStatus;

/// Status string for entries loaded from config files. Matches the
/// convention in the Claude and Codex loaders: "configured" reads as
/// "we have config; we have not attempted a connection."
const CONFIGURED_STATUS: &str = "configured";

/// Resolve the configured MCP-server registry for a Gemini agent.
///
/// `home_dir` is injected (not derived from `HOME`) so tests can stage
/// temp directories without mutating process-wide environment. `cwd` is
/// the user's bound working directory and serves as the lookup path
/// for the project-scope file.
#[must_use]
pub fn load_mcp_servers(home_dir: &Path, cwd: &Path) -> Vec<McpServerStatus> {
    let user_names = load_scope(&home_dir.join(".gemini").join("settings.json"));
    let project_names = load_scope(&cwd.join(".gemini").join("settings.json"));
    merge_scopes(user_names, project_names)
}

fn load_scope(settings_path: &Path) -> Vec<String> {
    let Some(value) = read_json_file(settings_path) else {
        return Vec::new();
    };
    extract_mcp_server_names(settings_path, value.get("mcpServers"))
}

/// Read a JSON file as a `serde_json::Value`. Missing → None (no warning).
/// Unreadable / malformed → None + one warning.
fn read_json_file(path: &Path) -> Option<Value> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Gemini MCP config: failed to read file; treating as empty"
            );
            return None;
        }
    };
    match serde_json::from_str(&content) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Gemini MCP config: top-level JSON parse failed; treating as empty"
            );
            None
        }
    }
}

/// Extract the names from a `"mcpServers"` JSON object. Each entry must
/// be a JSON object; bare strings / arrays / nulls are dropped with a
/// warning.
fn extract_mcp_server_names(path: &Path, mcp_servers: Option<&Value>) -> Vec<String> {
    let Some(obj) = mcp_servers.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut names = Vec::with_capacity(obj.len());
    for (name, entry) in obj {
        if !entry.is_object() {
            tracing::warn!(
                path = %path.display(),
                name = %name,
                "Gemini MCP config: entry is not a JSON object; dropping"
            );
            continue;
        }
        names.push(name.clone());
    }
    names
}

/// Two-scope merge with project > user precedence. Order in the
/// returned list: project entries first (in source order), then
/// user-only entries that don't collide with project.
fn merge_scopes(user_names: Vec<String>, project_names: Vec<String>) -> Vec<McpServerStatus> {
    let mut merged: Vec<McpServerStatus> = project_names
        .into_iter()
        .map(|name| McpServerStatus {
            name,
            status: CONFIGURED_STATUS.to_owned(),
        })
        .collect();
    for name in user_names {
        if !merged.iter().any(|e| e.name == name) {
            merged.push(McpServerStatus {
                name,
                status: CONFIGURED_STATUS.to_owned(),
            });
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write_settings(dir: &Path, value: &Value) {
        let gemini = dir.join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        std::fs::write(
            gemini.join("settings.json"),
            serde_json::to_string_pretty(value).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn missing_all_files_yields_empty_vec() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        assert!(load_mcp_servers(home.path(), cwd.path()).is_empty());
    }

    #[test]
    fn user_scope_only_returns_user_entries() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_settings(
            home.path(),
            &json!({
                "mcpServers": {
                    "alpha": { "command": "alpha-bin", "args": [] },
                    "beta": { "command": "beta-bin", "args": [] }
                }
            }),
        );

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.contains(&"alpha".to_owned()));
        assert!(names.contains(&"beta".to_owned()));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn project_scope_only_returns_workspace_entries() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_settings(
            cwd.path(),
            &json!({
                "mcpServers": {
                    "workspace_only": { "command": "x" }
                }
            }),
        );
        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["workspace_only".to_owned()]);
    }

    #[test]
    fn project_scope_takes_precedence_over_user_on_name_collision() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_settings(
            home.path(),
            &json!({
                "mcpServers": {
                    "shared": { "command": "user-version" },
                    "user_only": { "command": "u" }
                }
            }),
        );
        write_settings(
            cwd.path(),
            &json!({
                "mcpServers": {
                    "shared": { "command": "workspace-version" },
                    "workspace_only": { "command": "w" }
                }
            }),
        );

        // Project entries first (in source order), then user-only.
        // The "shared" name collides — only one entry survives.
        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "shared");
        assert_eq!(names[1], "workspace_only");
        assert_eq!(names[2], "user_only");
    }

    #[test]
    fn entries_all_carry_configured_status() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_settings(
            home.path(),
            &json!({
                "mcpServers": {
                    "alpha": { "command": "x" }
                }
            }),
        );
        let entries = load_mcp_servers(home.path(), cwd.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, CONFIGURED_STATUS);
    }

    #[test]
    fn malformed_top_level_json_yields_empty_and_does_not_propagate() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join(".gemini")).unwrap();
        std::fs::write(
            home.path().join(".gemini").join("settings.json"),
            "{ this is not valid json",
        )
        .unwrap();
        assert!(load_mcp_servers(home.path(), cwd.path()).is_empty());
    }

    #[test]
    fn non_object_mcp_servers_value_is_treated_as_no_entries() {
        // If `mcpServers` is somehow a string / number / array instead of
        // an object, the loader returns an empty list rather than
        // panicking or surfacing a typed error.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_settings(
            home.path(),
            &json!({
                "mcpServers": "not an object"
            }),
        );
        assert!(load_mcp_servers(home.path(), cwd.path()).is_empty());
    }

    #[test]
    fn individual_non_object_entry_is_dropped_others_survive() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_settings(
            home.path(),
            &json!({
                "mcpServers": {
                    "good": { "command": "x" },
                    "bad": "this should be an object",
                    "alsogood": { "command": "y" }
                }
            }),
        );
        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"good".to_owned()));
        assert!(names.contains(&"alsogood".to_owned()));
        assert!(!names.contains(&"bad".to_owned()));
    }

    #[test]
    fn settings_without_mcp_servers_key_yields_empty() {
        // The user's settings.json may not carry any MCP config at all
        // (just auth, ui, etc.). Should be a no-op, not an error.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_settings(
            home.path(),
            &json!({
                "security": { "auth": { "selectedType": "oauth-personal" } },
                "ui": { "theme": "Default Light" }
            }),
        );
        assert!(load_mcp_servers(home.path(), cwd.path()).is_empty());
    }
}
