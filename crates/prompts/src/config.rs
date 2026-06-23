//! The user-global prompt config (`config.yaml`) model and local-directory
//! resolution. Prompt config is user-global — there is no directory- or
//! project-scope (`docs/system-design.md` §6). The config file's path is
//! resolved and injected by `crates/app`; this module only parses and resolves.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::LOCAL_PROVIDER;

/// On-disk shape of the user-global `config.yaml` **local** prompt section.
/// Deliberately models *only* `local_prompt_dirs` and ignores everything else
/// (including `mcp_providers`): MCP providers are read by a **separate** parse
/// (`load_mcp_providers`) so a malformed MCP section can never fail this parse
/// and discard a user's valid local prompt directories. `local_prompt_dirs`
/// defaults to empty so a partial or absent file is valid.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptConfig {
    /// Directories scanned for local prompts, in declared (resolution) order.
    /// Empty/absent means "use the default prompts dir" (see `resolve_local_dirs`).
    #[serde(default)]
    pub local_prompt_dirs: Vec<PathBuf>,
}

/// A configured generic MCP-server prompt provider — non-secret config only. The
/// bearer token lives in the keychain (resolved at use time via `SecretStore`),
/// never here. `name` is the addressing prefix (`<name>:<prompt>`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpProviderConfig {
    pub name: String,
    pub transport: McpTransport,
}

/// Transport for an MCP provider. Only HTTP (Streamable HTTP) is supported in
/// v1; `#[non_exhaustive]` + the `type` tag leave room for `stdio` later without
/// a breaking change. Mirrors the `transport: { type: http, url: … }` config shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpTransport {
    Http { url: String },
}

/// The `mcp_providers:` section, captured as raw values so each entry can be
/// validated independently (a malformed entry degrades that one provider rather
/// than failing the whole section). Read separately from [`PromptConfig`].
#[derive(Debug, Default, Deserialize)]
pub(crate) struct McpSection {
    #[serde(default)]
    pub mcp_providers: Vec<serde_norway::Value>,
}

impl McpSection {
    /// Validate each raw entry into an `McpProviderConfig`, skipping (with a
    /// warning) any that don't parse, carry an invalid name, or duplicate an
    /// earlier name — so one bad entry never discards the rest, and the surviving
    /// set is a clean addressing namespace.
    pub(crate) fn into_configs(self) -> Vec<McpProviderConfig> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out = Vec::new();
        for value in self.mcp_providers {
            let config = match serde_norway::from_value::<McpProviderConfig>(value) {
                Ok(config) => config,
                Err(e) => {
                    tracing::warn!(error = %e, "skipping malformed mcp_providers entry");
                    continue;
                }
            };
            if !is_valid_provider_name(&config.name) {
                tracing::warn!(
                    name = %config.name,
                    "skipping mcp provider with invalid name (empty, reserved `local`, or containing `:`)"
                );
                continue;
            }
            if !seen.insert(config.name.clone()) {
                // First occurrence wins; a later duplicate would otherwise share
                // one secret-store key and shadow the first under one prefix.
                tracing::warn!(name = %config.name, "skipping duplicate mcp provider name");
                continue;
            }
            out.push(config);
        }
        out
    }
}

/// Whether `name` is usable as a provider addressing prefix. A provider name is
/// the prefix in `<name>:<prompt>`, so it must be non-empty, must not be the
/// reserved `local` (which always routes to the file store), and must not contain
/// `:` (which would break address parsing). Duplicate detection is the caller's
/// job — it needs the full set. The "Add MCP server" form reuses this rule.
pub(crate) fn is_valid_provider_name(name: &str) -> bool {
    !name.trim().is_empty() && name != LOCAL_PROVIDER && !name.contains(':')
}

/// Resolve the ordered list of directories to scan for local prompts.
///
/// An empty `local_prompt_dirs` falls back to `[default_dir]` (the
/// OS-conventional prompts path). Otherwise each declared entry is used in
/// order, with a leading `~` expanded against `home` when available. Declared
/// order is resolution order: an earlier directory shadows a later one.
pub fn resolve_local_dirs(
    config: &PromptConfig,
    default_dir: &Path,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    if config.local_prompt_dirs.is_empty() {
        return vec![default_dir.to_path_buf()];
    }
    config
        .local_prompt_dirs
        .iter()
        .map(|dir| expand_tilde(dir, home))
        .collect()
}

/// Expand a leading `~` or `~/...` against `home`. A path with no leading tilde,
/// or any tilde when `home` is unknown, is returned unchanged.
fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    home.join(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_falls_back_to_default_dir() {
        let config = PromptConfig::default();
        let default = PathBuf::from("/data/prompts");
        assert_eq!(
            resolve_local_dirs(&config, &default, None),
            vec![PathBuf::from("/data/prompts")]
        );
    }

    #[test]
    fn declared_dirs_preserve_order_and_skip_default() {
        let config = PromptConfig {
            local_prompt_dirs: vec![PathBuf::from("/a"), PathBuf::from("/b")],
        };
        let default = PathBuf::from("/data/prompts");
        assert_eq!(
            resolve_local_dirs(&config, &default, None),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn tilde_expands_against_home() {
        let config = PromptConfig {
            local_prompt_dirs: vec![PathBuf::from("~/repos/my-prompts"), PathBuf::from("/abs")],
        };
        let home = PathBuf::from("/home/dev");
        assert_eq!(
            resolve_local_dirs(&config, Path::new("/default"), Some(&home)),
            vec![
                PathBuf::from("/home/dev/repos/my-prompts"),
                PathBuf::from("/abs"),
            ]
        );
    }

    #[test]
    fn tilde_without_home_is_left_verbatim() {
        let config = PromptConfig {
            local_prompt_dirs: vec![PathBuf::from("~/x")],
        };
        assert_eq!(
            resolve_local_dirs(&config, Path::new("/default"), None),
            vec![PathBuf::from("~/x")]
        );
    }

    #[test]
    fn mcp_providers_section_is_ignored_and_does_not_affect_local_dirs() {
        // mcp_providers is inert this milestone — modeled as an ignored unknown
        // key. A config that declares it (preset and generic shapes) must still
        // parse and yield the local dirs unchanged.
        let yaml = r"
local_prompt_dirs:
  - ~/repos/my-prompts
mcp_providers:
  - name: tiddly
    preset: tiddly
  - name: my-team-mcp
    transport:
      type: http
      url: https://mcp.example.com
    auth:
      type: bearer
";
        let config: PromptConfig = serde_norway::from_str(yaml).unwrap();
        assert_eq!(
            config.local_prompt_dirs,
            vec![PathBuf::from("~/repos/my-prompts")]
        );
    }

    #[test]
    fn mcp_section_validates_per_entry_and_skips_bad_ones() {
        let yaml = r"
mcp_providers:
  - name: team
    transport:
      type: http
      url: https://mcp.example.com
  - nme: typo-no-name
    transport:
      type: http
      url: https://bad.example.com
  - name: no-transport
";
        let section: McpSection = serde_norway::from_str(yaml).unwrap();
        let configs = section.into_configs();
        // Only the well-formed entry survives; the typo'd-name and the
        // missing-transport entries are skipped, not fatal.
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "team");
        assert_eq!(
            configs[0].transport,
            McpTransport::Http {
                url: "https://mcp.example.com".to_owned()
            }
        );
    }

    #[test]
    fn absent_mcp_section_yields_no_providers() {
        let section: McpSection = serde_norway::from_str("local_prompt_dirs:\n  - /a\n").unwrap();
        assert!(section.into_configs().is_empty());
    }

    #[test]
    fn rejects_invalid_provider_names() {
        // `local` is reserved (routes to the file store); names with `:` break
        // address parsing; empty/whitespace names are unusable prefixes. Each is
        // skipped-with-warning, leaving the valid sibling.
        let yaml = r#"
mcp_providers:
  - name: local
    transport: { type: http, url: https://a.example.com }
  - name: "has:colon"
    transport: { type: http, url: https://b.example.com }
  - name: "   "
    transport: { type: http, url: https://c.example.com }
  - name: ok
    transport: { type: http, url: https://d.example.com }
"#;
        let section: McpSection = serde_norway::from_str(yaml).unwrap();
        let names: Vec<String> = section.into_configs().into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["ok".to_owned()]);
    }

    #[test]
    fn dedupes_provider_names_first_wins() {
        let yaml = r"
mcp_providers:
  - name: team
    transport:
      type: http
      url: https://first.example.com
  - name: team
    transport:
      type: http
      url: https://second.example.com
";
        let section: McpSection = serde_norway::from_str(yaml).unwrap();
        let configs = section.into_configs();
        assert_eq!(configs.len(), 1);
        assert_eq!(
            configs[0].transport,
            McpTransport::Http {
                url: "https://first.example.com".to_owned()
            }
        );
    }

    #[test]
    fn provider_name_validator_rules() {
        assert!(is_valid_provider_name("team"));
        assert!(is_valid_provider_name("my-team_mcp"));
        assert!(!is_valid_provider_name("local"));
        assert!(!is_valid_provider_name(""));
        assert!(!is_valid_provider_name("   "));
        assert!(!is_valid_provider_name("a:b"));
    }

    #[test]
    fn malformed_mcp_section_preserves_local_dirs() {
        // The headline guarantee: a typo in the (unused) MCP section must never
        // discard valid local prompt dirs. Because mcp_providers is an ignored
        // unknown key, even a scalar-instead-of-list — or any garbage — parses
        // fine and local_prompt_dirs survives intact.
        for mcp in [
            "mcp_providers: not-a-list",
            "mcp_providers:\n  - nme: typo\n    bogus: true",
            "mcp_providers: 42",
        ] {
            let yaml = format!("local_prompt_dirs:\n  - /a\n  - /b\n{mcp}\n");
            let config: PromptConfig = serde_norway::from_str(&yaml)
                .unwrap_or_else(|e| panic!("{mcp:?} should still parse: {e}"));
            assert_eq!(
                config.local_prompt_dirs,
                vec![PathBuf::from("/a"), PathBuf::from("/b")],
                "local dirs must survive a malformed MCP section ({mcp:?})"
            );
        }
    }

    #[test]
    fn ignores_unknown_top_level_keys() {
        // The user-global config.yaml also holds non-prompt personal prefs.
        let yaml = r"
theme: dark
local_prompt_dirs:
  - /a
";
        let config: PromptConfig = serde_norway::from_str(yaml).unwrap();
        assert_eq!(config.local_prompt_dirs, vec![PathBuf::from("/a")]);
    }
}
