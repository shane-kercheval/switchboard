//! The user-global prompt config (`config.yaml`) model and local-directory
//! resolution. Prompt config is user-global — there is no directory- or
//! project-scope (`docs/system-design.md` §6). The config file's path is
//! resolved and injected by `crates/app`; this module only parses and resolves.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// On-disk shape of the user-global `config.yaml` prompt sections. Unknown
/// top-level keys are ignored (the file may hold other personal preferences —
/// and, until the MCP client lands, the entire `mcp_providers:` section), and
/// `local_prompt_dirs` defaults to empty so a partial or absent file is valid.
///
/// `mcp_providers` is **not modeled here**: this milestone never consumes it, so
/// modeling it would be speculative and — worse — would couple its parse to
/// `local_prompt_dirs`, letting a typo in the (inert) MCP section discard a
/// user's valid local prompt directories. Leaving it as an ignored unknown key
/// means a malformed MCP section can never break local prompts. The MCP config
/// model arrives in the milestone that reads and writes it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptConfig {
    /// Directories scanned for local prompts, in declared (resolution) order.
    /// Empty/absent means "use the default prompts dir" (see `resolve_local_dirs`).
    #[serde(default)]
    pub local_prompt_dirs: Vec<PathBuf>,
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
