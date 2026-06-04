//! User-global personal preferences (`config.yaml`).
//!
//! The first **backend-persisted** preferences in Switchboard — distinct from
//! the frontend-only theme (which stays in `localStorage`, since it's a
//! device-local presentation concern the backend never reads). The rule: a
//! setting lives where its consumer is. These are backend-consumed (the Git
//! view's open-in-editor / open-in-terminal actions run them) and so live in a
//! backend-owned YAML file, a sibling of `workspace.yaml` / `git-view.yaml`.
//!
//! Shape and persistence mirror [`crate::git_registry`]: a user-global YAML file
//! resolved through the same mechanism, with graceful degradation — a missing or
//! corrupt `config.yaml` degrades to defaults rather than failing app startup.
//!
//! Every field carries `#[serde(default)]` so the file is forward/backward
//! compatible: a `config.yaml` written by an older build (missing a key) loads
//! with that key defaulted, and an unknown future key is ignored. Later
//! milestones add keys here (worktree base path, diff style) the same way.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// The default terminal application opened by the Git view's "open in terminal"
/// action when no override is set. macOS ships Terminal.app; power users can name
/// another (`iTerm`, `Ghostty`, …).
fn default_terminal_app() -> String {
    "Terminal".to_owned()
}

/// How the diff panel lays out a file's changes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiffStyle {
    /// Old and new content in two columns. Default — the diff panel is a wide
    /// bottom split, so side-by-side reads well.
    #[default]
    SideBySide,
    /// One column with interleaved removed/added lines.
    Unified,
}

/// Personal preferences, persisted to `config.yaml`. All fields default, so any
/// subset may be present in the file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Preferences {
    /// Command used to open a worktree folder in an external editor, e.g. `code`,
    /// `cursor`, `zed`. Run as `<command> <path>`. Empty/absent → fall back to the
    /// OS folder-open (`open <path>` on macOS), so it works with zero config.
    pub editor_command: Option<String>,

    /// Name of the terminal application the "open in terminal" action launches
    /// (`open -a <name> <path>` on macOS). Defaults to `Terminal`.
    pub terminal_app: String,

    /// Diff panel layout. Defaults to side-by-side.
    pub diff_style: DiffStyle,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            editor_command: None,
            terminal_app: default_terminal_app(),
            diff_style: DiffStyle::default(),
        }
    }
}

impl Preferences {
    /// Enforce the field invariants the backend owns, regardless of how the value
    /// arrived (deserialized from a hand-edited `config.yaml`, or sent by a
    /// client). Trims surrounding whitespace; a blank editor command becomes
    /// `None` (→ OS folder-open) and a blank terminal app becomes the default.
    /// Applied at every boundary (`load` + `set`) so consumers — the Git-view
    /// open-actions — never see an empty command to spawn.
    #[must_use]
    pub fn normalized(self) -> Self {
        let editor_command = self
            .editor_command
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty());
        let terminal_app = {
            let trimmed = self.terminal_app.trim();
            if trimmed.is_empty() {
                default_terminal_app()
            } else {
                trimmed.to_owned()
            }
        };
        Self {
            editor_command,
            terminal_app,
            diff_style: self.diff_style,
        }
    }
}

/// Read preferences from `path`. Never fails: a missing or corrupt file degrades
/// to [`Preferences::default`] rather than aborting startup. Unlike the
/// registries there is no "persistable" distinction — preferences are only
/// written on an explicit user save (never auto-rewritten on read), so an
/// unreadable file simply yields defaults this session and the next explicit
/// save replaces it. That's acceptable for preferences (losing them resets to
/// defaults) in a way it isn't for the registries (which must never clobber a
/// real-but-unreadable directory set).
pub fn load(path: &Path) -> Preferences {
    if !path.exists() {
        return Preferences::default();
    }
    match switchboard_core::read_yaml::<Preferences>(path) {
        Ok(prefs) => prefs.normalized(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "config.yaml could not be read — using default preferences this session"
            );
            Preferences::default()
        }
    }
}

/// Persist preferences to `path`, creating the parent directory if needed.
/// Atomic temp-write + rename via `switchboard_core::write_yaml`.
///
/// `config.yaml` is **shared**: it also holds the prompt providers
/// (`mcp_providers`) and local prompt dirs. So this merges only the preference
/// keys into the existing mapping rather than serializing the `Preferences`
/// struct over the whole file — otherwise saving a preference would wipe the
/// user's prompt config (and vice-versa; the prompt service round-trips the same
/// way). Refuses to write if the existing file isn't a YAML mapping, rather than
/// clobber it.
pub fn save(path: &Path, prefs: &Preferences) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AppError::PreferencesPersist {
            path: path.to_owned(),
            source,
        })?;
    }
    let mut root = read_config_mapping(path)?;
    let serialized =
        serde_norway::to_value(prefs).map_err(|e| persist_err(path, &e.to_string()))?;
    if let serde_norway::Value::Mapping(fields) = serialized {
        for (key, value) in fields {
            root.insert(key, value);
        }
    }
    switchboard_core::write_yaml(path, &serde_norway::Value::Mapping(root))?;
    Ok(())
}

/// Read `config.yaml` as a generic YAML mapping so a save can merge the
/// preference keys without disturbing the other sections it shares the file with.
/// Missing / empty / `null` → an empty mapping; a file that isn't a mapping is
/// refused (returning an error) so a save never clobbers a malformed-but-present
/// config.
fn read_config_mapping(path: &Path) -> Result<serde_norway::Mapping, AppError> {
    if !path.exists() {
        return Ok(serde_norway::Mapping::new());
    }
    let bytes = std::fs::read(path).map_err(|source| AppError::PreferencesPersist {
        path: path.to_owned(),
        source,
    })?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_norway::Mapping::new());
    }
    match serde_norway::from_slice::<serde_norway::Value>(&bytes) {
        Ok(serde_norway::Value::Mapping(mapping)) => Ok(mapping),
        Ok(serde_norway::Value::Null) => Ok(serde_norway::Mapping::new()),
        Ok(_) => Err(persist_err(
            path,
            "config.yaml is not a YAML mapping; refusing to overwrite it",
        )),
        Err(e) => Err(persist_err(path, &e.to_string())),
    }
}

/// Wrap a non-I/O persist failure (YAML parse / serialize / shape) as the
/// preferences-persist error, reusing its `path`-naming Display.
fn persist_err(path: &Path, message: &str) -> AppError {
    AppError::PreferencesPersist {
        path: path.to_owned(),
        source: std::io::Error::other(message.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn default_has_no_editor_and_terminal_app() {
        let p = Preferences::default();
        assert_eq!(p.editor_command, None);
        assert_eq!(p.terminal_app, "Terminal");
        assert_eq!(p.diff_style, DiffStyle::SideBySide);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("config.yaml");

        let prefs = Preferences {
            editor_command: Some("cursor".to_owned()),
            terminal_app: "iTerm".to_owned(),
            diff_style: DiffStyle::Unified,
        };
        save(&path, &prefs).unwrap();
        assert_eq!(load(&path), prefs);
    }

    #[test]
    fn save_preserves_unknown_keys_in_the_shared_config() {
        // `config.yaml` is shared with the prompt providers; saving preferences
        // must merge its keys, not clobber the rest of the file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "mcp_providers:\n  - name: team\n    url: https://example.test\nlocal_prompt_dirs:\n  - ~/prompts\n",
        )
        .unwrap();

        save(
            &path,
            &Preferences {
                editor_command: Some("zed".to_owned()),
                terminal_app: "iTerm".to_owned(),
                diff_style: DiffStyle::Unified,
            },
        )
        .unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        // The prompt sections survive…
        assert!(
            raw.contains("mcp_providers"),
            "mcp_providers must be preserved: {raw}"
        );
        assert!(
            raw.contains("local_prompt_dirs"),
            "local_prompt_dirs must be preserved: {raw}"
        );
        // …and the preference keys are written.
        assert!(raw.contains("zed") && raw.contains("iTerm"));
    }

    #[test]
    fn save_refuses_to_clobber_a_non_mapping_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "just a scalar, not a mapping\n").unwrap();

        assert!(save(&path, &Preferences::default()).is_err());
        // The original content is left untouched.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "just a scalar, not a mapping\n"
        );
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        assert_eq!(load(&path), Preferences::default());
    }

    #[test]
    fn corrupt_file_loads_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "this: is: not: valid: [").unwrap();
        assert_eq!(load(&path), Preferences::default());
    }

    #[test]
    fn blank_and_whitespace_values_normalize_at_load() {
        // A hand-edited config.yaml with empty/whitespace values must not reach
        // consumers as meaningful — blank editor → None, blank terminal → default.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "editor_command: \"   \"\nterminal_app: \"\"\n").unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.editor_command, None);
        assert_eq!(loaded.terminal_app, "Terminal");
    }

    #[test]
    fn normalized_trims_and_maps_blanks() {
        let p = Preferences {
            editor_command: Some("  cursor  ".to_owned()),
            terminal_app: "  iTerm  ".to_owned(),
            diff_style: DiffStyle::Unified,
        }
        .normalized();
        assert_eq!(p.editor_command.as_deref(), Some("cursor"));
        assert_eq!(p.terminal_app, "iTerm");
        assert_eq!(
            p.diff_style,
            DiffStyle::Unified,
            "diff_style carries through"
        );

        let blank = Preferences {
            editor_command: Some("   ".to_owned()),
            terminal_app: "   ".to_owned(),
            diff_style: DiffStyle::default(),
        }
        .normalized();
        assert_eq!(blank.editor_command, None);
        assert_eq!(blank.terminal_app, "Terminal");
    }

    #[test]
    fn partial_file_defaults_missing_keys() {
        // A file with only `editor_command` set must load with `terminal_app`
        // defaulted — the forward/backward-compat contract for added keys.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "editor_command: zed\n").unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.editor_command.as_deref(), Some("zed"));
        assert_eq!(loaded.terminal_app, "Terminal");
    }
}
