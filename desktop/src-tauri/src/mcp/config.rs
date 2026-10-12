//! Whether agent access is on, and its port, under the `agent_access` key of
//! the user-global `config.yaml`.
//!
//! **Its own key, not a field of `Preferences`.** The window saves
//! preferences by sending the whole `Preferences` value back, so a field
//! there that the window does not edit would be rewritten from whatever the
//! window last read, and a preference save made while agent access was being
//! turned on could turn it back off. Only the Settings commands in
//! [`super`] write this key, and they write nothing else.
//!
//! Named `agent_access` rather than `mcp` because the same file already holds
//! `mcp_providers`, the MCP servers Switchboard *reads prompts from*; this is
//! the opposite direction, Switchboard *serving* MCP.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

const KEY: &str = "agent_access";

/// The installed app's default port.
#[cfg(not(debug_assertions))]
pub const DEFAULT_PORT: u16 = 47_821;
/// A debug build's default port: different from the installed app's, so a
/// development copy and the installed app can both listen.
#[cfg(debug_assertions)]
pub const DEFAULT_PORT: u16 = 47_822;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentAccessConfig {
    /// Off unless the user turned it on: nothing listens until then.
    pub enabled: bool,
    pub port: u16,
}

impl Default for AgentAccessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ConfigFile {
    agent_access: AgentAccessConfig,
}

/// Read the setting. A missing file, a missing key, or an unreadable file
/// reads as off: agent access must never turn itself on, and a corrupt file
/// is reported by the preference loader, which reads the same file.
#[must_use]
pub fn load(path: &Path) -> AgentAccessConfig {
    if !path.exists() {
        return AgentAccessConfig::default();
    }
    match switchboard_core::read_yaml::<ConfigFile>(path) {
        Ok(file) => file.agent_access,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "agent access setting unreadable; treating it as off");
            AgentAccessConfig::default()
        }
    }
}

/// Write the setting into `config.yaml`, leaving every other key as it was.
pub fn save(path: &Path, config: AgentAccessConfig) -> Result<(), AppError> {
    let persist = |source: std::io::Error| AppError::PreferencesPersist {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(persist)?;
    }
    let value = serde_norway::to_value(config)
        .map_err(|e| persist(std::io::Error::other(e.to_string())))?;
    switchboard_core::edit_yaml_mapping(path, move |root| {
        root.insert(serde_norway::Value::String(KEY.to_owned()), value);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn a_file_written_before_agent_access_existed_loads_with_it_off() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "terminal_app: iTerm\nauto_reading_mode: true\n").unwrap();
        assert_eq!(load(&path), AgentAccessConfig::default());
        assert!(!load(&path).enabled);
    }

    #[test]
    fn a_missing_file_loads_with_it_off() {
        let dir = tempdir().unwrap();
        assert!(!load(&dir.path().join("config.yaml")).enabled);
    }

    #[test]
    fn saving_keeps_the_other_keys_and_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "terminal_app: iTerm\nmcp_providers:\n  - name: team\n    transport:\n      type: http\n      url: http://example.com\n",
        )
        .unwrap();
        let config = AgentAccessConfig {
            enabled: true,
            port: 50_000,
        };
        save(&path, config).unwrap();
        assert_eq!(load(&path), config);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("terminal_app: iTerm"), "{text}");
        assert!(text.contains("mcp_providers"), "{text}");
    }

    #[test]
    fn a_preference_save_leaves_agent_access_alone() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let config = AgentAccessConfig {
            enabled: true,
            port: 50_001,
        };
        save(&path, config).unwrap();
        crate::preferences::save(&path, &crate::preferences::Preferences::default()).unwrap();
        assert_eq!(load(&path), config);
    }

    #[test]
    fn a_partial_key_fills_in_the_rest() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "agent_access:\n  enabled: true\n").unwrap();
        assert_eq!(
            load(&path),
            AgentAccessConfig {
                enabled: true,
                port: DEFAULT_PORT,
            }
        );
    }
}
