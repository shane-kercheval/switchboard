//! Antigravity MCP-server registry loader.
//!
//! Antigravity reads MCP-server config from `~/.gemini/config/mcp_config.json`
//! — a top-level `{ "mcpServers": { "<name>": { "command": …, "args": […],
//! "env": {…}, "serverUrl": "…" } } }` object. The schema matches Gemini's
//! `settings.json` `mcpServers` key; only the file path differs.
//!
//! **User scope only.** A per-project override
//! (`<cwd>/.gemini/config/mcp_config.json` or similar) is plausible but
//! unverified, so workspace scope is not loaded — the `cwd` parameter is
//! reserved for it. See the "Known limitations" section of
//! `docs/research/antigravity-cli-observed.md`.
//!
//! Kept separate from `gemini/config.rs` rather than factored into a shared
//! module: Antigravity and Gemini currently share the JSON shape but not the
//! path or scoping rules, so a shared loader would couple two contracts that
//! are free to diverge. Two short loaders are simpler than one parameterized
//! one until a stable shared contract exists.
//!
//! **Partial-parse policy** (mirrors the other harness loaders):
//! - File not found → empty contribution, no warning.
//! - File present but unreadable / top-level malformed → empty + one `warn`.
//! - Individual entry malformed (not a JSON object) → drop that entry, keep
//!   the rest, one warn per drop.
//!
//! Failures never propagate as `Result::Err` — this registry is display-only,
//! not load-bearing for dispatch.

use std::path::Path;

use serde_json::Value;

use super::paths;
use crate::events::McpServerStatus;

/// "configured" reads as "we have config; we have not attempted a connection"
/// — same convention as every other harness's loader.
const CONFIGURED_STATUS: &str = "configured";

/// Resolve the configured MCP-server registry for an Antigravity agent.
///
/// `home_dir` is injected (not derived from `HOME`) so tests can stage temp
/// directories. `_cwd` is reserved for a future project-scope override (not
/// loaded today — user scope only).
#[must_use]
pub fn load_mcp_servers(home_dir: &Path, _cwd: &Path) -> Vec<McpServerStatus> {
    let path = paths::mcp_config_path(home_dir);
    let Some(value) = read_json_file(&path) else {
        return Vec::new();
    };
    extract_mcp_server_names(&path, value.get("mcpServers"))
        .into_iter()
        .map(|name| McpServerStatus {
            name,
            status: CONFIGURED_STATUS.to_owned(),
        })
        .collect()
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
                "Antigravity MCP config: failed to read file; treating as empty"
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
                "Antigravity MCP config: top-level JSON parse failed; treating as empty"
            );
            None
        }
    }
}

/// Extract names from a `"mcpServers"` JSON object. Each entry must be a JSON
/// object; bare strings / arrays / nulls are dropped with a warning.
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
                "Antigravity MCP config: entry is not a JSON object; dropping"
            );
            continue;
        }
        names.push(name.clone());
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write_mcp_config(home: &Path, value: &Value) {
        let dir = paths::config_root(home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mcp_config.json"),
            serde_json::to_string_pretty(value).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn missing_file_yields_empty_vec() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        assert!(load_mcp_servers(home.path(), cwd.path()).is_empty());
    }

    #[test]
    fn returns_all_configured_servers() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_mcp_config(
            home.path(),
            &json!({
                "mcpServers": {
                    "alpha": { "command": "alpha-bin", "args": [] },
                    "beta": { "serverUrl": "https://example.test" }
                }
            }),
        );
        let entries = load_mcp_servers(home.path(), cwd.path());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(entries.len(), 2);
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert!(entries.iter().all(|e| e.status == CONFIGURED_STATUS));
    }

    #[test]
    fn malformed_top_level_json_yields_empty() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        std::fs::create_dir_all(paths::config_root(home.path())).unwrap();
        std::fs::write(paths::mcp_config_path(home.path()), "{ not valid json").unwrap();
        assert!(load_mcp_servers(home.path(), cwd.path()).is_empty());
    }

    #[test]
    fn non_object_mcp_servers_value_yields_empty() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_mcp_config(home.path(), &json!({ "mcpServers": "not an object" }));
        assert!(load_mcp_servers(home.path(), cwd.path()).is_empty());
    }

    #[test]
    fn individual_non_object_entry_dropped_others_survive() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_mcp_config(
            home.path(),
            &json!({
                "mcpServers": {
                    "good": { "command": "x" },
                    "bad": "should be an object",
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
    fn config_without_mcp_servers_key_yields_empty() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        write_mcp_config(home.path(), &json!({ "somethingElse": true }));
        assert!(load_mcp_servers(home.path(), cwd.path()).is_empty());
    }
}
