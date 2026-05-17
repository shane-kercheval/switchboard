//! Claude Code MCP-server registry loader.
//!
//! Claude reads MCP-server config from three scopes, in increasing precedence:
//! - **User**: `~/.claude.json` top-level `"mcpServers"` object.
//! - **Local**: `~/.claude.json` nested under `"projects"."<cwd>"."mcpServers"`
//!   — same file as user scope, keyed by the user's bound working directory.
//! - **Project**: `<cwd>/.mcp.json` top-level `"mcpServers"` object.
//!
//! Merge rule: **project > local > user** on name collision. The merged
//! surface is just the name list (the registry is display-only — see the
//! rationale in `codex/config.rs`), so the "win" is observationally a
//! deduplication. Order: project entries first, then local-only, then
//! user-only — by-scope is a clearer story for the sidebar than a single
//! alphabetic merge.
//!
//! **Partial-parse policy** (mirrors `codex/config.rs`):
//! - File not found → empty contribution, no warning.
//! - File present but unreadable / top-level malformed → empty contribution
//!   + one `tracing::warn!`.
//! - Individual entry malformed (not a JSON object) → drop that entry, keep
//!   the rest, one warn per drop.
//!
//! Failures never propagate as `Result::Err` — these registries are display
//! information, not load-bearing for dispatch.

use std::path::Path;

use serde_json::Value;

use crate::events::McpServerStatus;

/// Status string for entries loaded from config files. Matches the
/// convention in `codex/config.rs`: "configured" reads honestly as "we
/// have config; we have not attempted a connection" — distinct from
/// Claude's runtime statuses (`connected` / `disconnected` / `failed` /
/// `needs-auth`) which only the live `system/init` event surfaces.
const CONFIGURED_STATUS: &str = "configured";

/// Resolve the configured MCP-server registry for a Claude Code agent.
///
/// `home_dir` is injected (not derived from `HOME`) so tests can stage
/// temp directories without mutating process-wide environment. `cwd` is
/// the user's bound working directory and serves as both the lookup key
/// for the local scope and the path for the project-scope file.
#[must_use]
pub fn load_mcp_servers(home_dir: &Path, cwd: &Path) -> Vec<McpServerStatus> {
    let claude_json = home_dir.join(".claude.json");
    let user_names = load_user_scope(&claude_json);
    let local_names = load_local_scope(&claude_json, cwd);
    let project_names = load_project_scope(&cwd.join(".mcp.json"));
    merge_scopes(user_names, local_names, project_names)
}

fn load_user_scope(claude_json: &Path) -> Vec<String> {
    let Some(value) = read_json_file(claude_json) else {
        return Vec::new();
    };
    extract_mcp_server_names(claude_json, value.get("mcpServers"))
}

fn load_local_scope(claude_json: &Path, cwd: &Path) -> Vec<String> {
    let Some(value) = read_json_file(claude_json) else {
        return Vec::new();
    };
    let projects = value.get("projects").and_then(Value::as_object);
    let Some(projects) = projects else {
        return Vec::new();
    };
    let key = cwd.to_string_lossy();
    let Some(entry) = projects.get(key.as_ref()) else {
        return Vec::new();
    };
    extract_mcp_server_names(claude_json, entry.get("mcpServers"))
}

fn load_project_scope(mcp_json: &Path) -> Vec<String> {
    let Some(value) = read_json_file(mcp_json) else {
        return Vec::new();
    };
    extract_mcp_server_names(mcp_json, value.get("mcpServers"))
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
                "Claude MCP config: failed to read file; treating as empty"
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
                "Claude MCP config: top-level JSON parse failed; treating as empty"
            );
            None
        }
    }
}

/// Extract the names from a `"mcpServers"` JSON object. Each entry must be
/// a JSON object; bare strings / arrays / nulls are dropped with a warning.
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
                "Claude MCP config: entry is not a JSON object; dropping"
            );
            continue;
        }
        names.push(name.clone());
    }
    names
}

/// Three-scope merge with project > local > user precedence. Order in the
/// returned list: project entries first (in source order), then local-only
/// entries that don't collide with project, then user-only entries that
/// don't collide with either project or local.
fn merge_scopes(
    user_names: Vec<String>,
    local_names: Vec<String>,
    project_names: Vec<String>,
) -> Vec<McpServerStatus> {
    let mut merged: Vec<McpServerStatus> = project_names
        .into_iter()
        .map(|name| McpServerStatus {
            name,
            status: CONFIGURED_STATUS.to_owned(),
        })
        .collect();
    for name in local_names {
        if !merged.iter().any(|e| e.name == name) {
            merged.push(McpServerStatus {
                name,
                status: CONFIGURED_STATUS.to_owned(),
            });
        }
    }
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

    fn write_claude_json(home: &Path, value: &Value) {
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_string_pretty(value).unwrap(),
        )
        .unwrap();
    }

    fn write_project_mcp_json(cwd: &Path, value: &Value) {
        std::fs::write(
            cwd.join(".mcp.json"),
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
        write_claude_json(
            home.path(),
            &json!({
                "mcpServers": {
                    "alpha": { "command": "alpha-bin" },
                    "beta": { "command": "beta-bin" }
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
    fn local_scope_only_returns_entries_keyed_by_cwd() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let cwd_key = cwd.path().to_string_lossy().to_string();
        write_claude_json(
            home.path(),
            &json!({
                "projects": {
                    cwd_key: {
                        "mcpServers": {
                            "local_only": { "command": "x" }
                        }
                    }
                }
            }),
        );

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["local_only"]);
    }

    #[test]
    fn project_scope_only_returns_project_entries() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_project_mcp_json(
            cwd.path(),
            &json!({
                "mcpServers": {
                    "project_only": { "command": "p" }
                }
            }),
        );

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["project_only"]);
    }

    #[test]
    fn project_wins_when_same_name_in_all_three_scopes() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let cwd_key = cwd.path().to_string_lossy().to_string();
        // Same name in all three; entry contents differ. The merged list
        // contains the name exactly once (we surface name only, so the
        // "win" is observed as deduplication; the order rule places the
        // project entry first).
        write_claude_json(
            home.path(),
            &json!({
                "mcpServers": {
                    "shared": { "command": "user-cmd" }
                },
                "projects": {
                    cwd_key: {
                        "mcpServers": {
                            "shared": { "command": "local-cmd" }
                        }
                    }
                }
            }),
        );
        write_project_mcp_json(
            cwd.path(),
            &json!({
                "mcpServers": {
                    "shared": { "command": "project-cmd" }
                }
            }),
        );

        let result = load_mcp_servers(home.path(), cwd.path());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "shared");
    }

    #[test]
    fn local_wins_over_user_when_only_those_two_collide() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let cwd_key = cwd.path().to_string_lossy().to_string();
        write_claude_json(
            home.path(),
            &json!({
                "mcpServers": {
                    "shared": { "command": "user-cmd" }
                },
                "projects": {
                    cwd_key: {
                        "mcpServers": {
                            "shared": { "command": "local-cmd" }
                        }
                    }
                }
            }),
        );

        let result = load_mcp_servers(home.path(), cwd.path());
        assert_eq!(result.len(), 1, "no duplicate on name collision");
        assert_eq!(result[0].name, "shared");
    }

    #[test]
    fn malformed_one_scope_does_not_block_others() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        // Project scope is malformed JSON; user scope is valid. The user
        // entry must still come through; project scope contributes empty.
        write_claude_json(
            home.path(),
            &json!({
                "mcpServers": {
                    "from_user": { "command": "u" }
                }
            }),
        );
        std::fs::write(cwd.path().join(".mcp.json"), "{ not valid json").unwrap();

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["from_user"]);
    }

    #[test]
    fn entry_that_is_not_an_object_is_dropped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_claude_json(
            home.path(),
            &json!({
                "mcpServers": {
                    "bad": "not an object",
                    "good": { "command": "ok" }
                }
            }),
        );

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["good"], "bad entry dropped, good preserved");
    }

    #[test]
    fn project_scope_entry_orders_before_user_only_entries() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        // User has `alpha`, project has `omega`. The merged order is
        // project-first (omega), then user-only (alpha). Verifies the
        // by-scope ordering documented in the merge rule.
        write_claude_json(
            home.path(),
            &json!({
                "mcpServers": {
                    "alpha": { "command": "a" }
                }
            }),
        );
        write_project_mcp_json(
            cwd.path(),
            &json!({
                "mcpServers": {
                    "omega": { "command": "o" }
                }
            }),
        );

        let names: Vec<String> = load_mcp_servers(home.path(), cwd.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["omega", "alpha"]);
    }
}
