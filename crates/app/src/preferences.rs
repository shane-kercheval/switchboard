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
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            editor_command: None,
            terminal_app: default_terminal_app(),
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
pub fn save(path: &Path, prefs: &Preferences) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AppError::PreferencesPersist {
            path: path.to_owned(),
            source,
        })?;
    }
    switchboard_core::write_yaml(path, prefs)?;
    Ok(())
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
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("config.yaml");

        let prefs = Preferences {
            editor_command: Some("cursor".to_owned()),
            terminal_app: "iTerm".to_owned(),
        };
        save(&path, &prefs).unwrap();
        assert_eq!(load(&path), prefs);
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
        }
        .normalized();
        assert_eq!(p.editor_command.as_deref(), Some("cursor"));
        assert_eq!(p.terminal_app, "iTerm");

        let blank = Preferences {
            editor_command: Some("   ".to_owned()),
            terminal_app: "   ".to_owned(),
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
