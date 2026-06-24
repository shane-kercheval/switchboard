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
    /// Old and new content in two columns.
    SideBySide,
    /// One column with interleaved removed/added lines. Default — it keeps the
    /// diff readable in the fixed-width Git details pane.
    #[default]
    Unified,
}

/// Personal preferences, persisted to `config.yaml`. All fields default, so any
/// subset may be present in the file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Preferences {
    /// Command used to open a worktree folder in an external editor. Defaults
    /// to `code`; blank → fall back to the OS folder-open (`open <path>` on
    /// macOS).
    pub editor_command: Option<String>,

    /// Name of the terminal application the "open in terminal" action launches
    /// (`open -a <name> <path>` on macOS). Defaults to `Terminal`.
    pub terminal_app: String,

    /// Diff panel layout. Defaults to unified.
    pub diff_style: DiffStyle,

    /// Whether the app-owned read-only built-in prompts and workflows appear in
    /// the pickers. Default `true` (show examples); a user who wants only their
    /// own content turns it off. Visibility only — a workflow wired to a built-in
    /// still resolves when this is off.
    pub show_builtins: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            editor_command: Some("code".to_owned()),
            terminal_app: default_terminal_app(),
            diff_style: DiffStyle::default(),
            show_builtins: true,
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
            show_builtins: self.show_builtins,
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
    // Serialize before the edit so the closure stays infallible. `edit_yaml_mapping`
    // merges only the preference keys into the existing mapping (preserving the
    // prompt sections that share the file) and serializes against the prompt
    // writer, so the two co-owners of `config.yaml` can't clobber each other.
    let serialized = serde_norway::to_value(prefs).map_err(|e| AppError::PreferencesPersist {
        path: path.to_owned(),
        source: std::io::Error::other(e.to_string()),
    })?;
    let fields = match serialized {
        serde_norway::Value::Mapping(fields) => fields,
        _ => serde_norway::Mapping::new(),
    };
    switchboard_core::edit_yaml_mapping(path, move |root| {
        for (key, value) in fields {
            root.insert(key, value);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn default_has_code_editor_and_terminal_app() {
        let p = Preferences::default();
        assert_eq!(p.editor_command.as_deref(), Some("code"));
        assert_eq!(p.terminal_app, "Terminal");
        assert_eq!(p.diff_style, DiffStyle::Unified);
        assert!(p.show_builtins, "built-ins are shown by default");
    }

    #[test]
    fn missing_show_builtins_key_defaults_on() {
        // The forward/backward-compat contract: a config.yaml written before this
        // key existed loads with built-ins shown, not hidden.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "terminal_app: iTerm\n").unwrap();
        assert!(load(&path).show_builtins);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("config.yaml");

        let prefs = Preferences {
            editor_command: Some("cursor".to_owned()),
            terminal_app: "iTerm".to_owned(),
            diff_style: DiffStyle::Unified,
            // Non-default so the round-trip exercises an explicitly-off toggle.
            show_builtins: false,
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
                show_builtins: true,
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
    fn concurrent_save_and_add_mcp_provider_preserve_both_sections() {
        // The contract that matters: the two *real* co-owners of `config.yaml`
        // (preferences here, the prompt service for `mcp_providers`) writing it
        // concurrently must each preserve the other's keys. This exercises the
        // production wiring — both routing through `switchboard_core::edit_yaml_mapping`
        // — not just the generic helper in isolation.
        use std::sync::Arc;
        use switchboard_prompts::{InMemorySecretStore, PromptService};

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        let prompts = PromptService::new(
            config_path.clone(),
            dir.path().join("prompts"),
            None,
            Arc::new(InMemorySecretStore::new()),
        );

        let save_path = config_path.clone();
        let saver = std::thread::spawn(move || {
            save(
                &save_path,
                &Preferences {
                    editor_command: Some("zed".to_owned()),
                    terminal_app: "iTerm".to_owned(),
                    diff_style: DiffStyle::Unified,
                    show_builtins: true,
                },
            )
            .unwrap();
        });
        let adder = std::thread::spawn(move || {
            prompts
                .add_mcp_provider("team", "https://example.test", None)
                .unwrap();
        });
        saver.join().unwrap();
        adder.join().unwrap();

        // Both subsystems' sections survive, whichever order they interleaved in.
        let reread: serde_norway::Value = switchboard_core::read_yaml(&config_path).unwrap();
        let map = reread.as_mapping().unwrap();
        let key = |k: &str| serde_norway::Value::String(k.to_owned());
        assert!(
            map.contains_key(key("mcp_providers")),
            "the prompt provider section must survive: {map:?}"
        );
        assert_eq!(map.get(key("editor_command")), Some(&key("zed")));
        assert_eq!(map.get(key("terminal_app")), Some(&key("iTerm")));
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
            show_builtins: true,
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
            show_builtins: true,
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
