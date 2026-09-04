//! User-global personal preferences (`config.yaml`).
//!
//! **Backend-persisted** preferences — distinct from the frontend-only theme
//! (which stays in `localStorage`, since it's a device-local presentation
//! concern that shouldn't sync across machines). Consumers vary: some fields
//! are backend-consumed (the Git view's open-in-editor / open-in-terminal
//! actions run them, dispatch reads `claude_chrome_enabled`), others are read
//! only by the frontend (`diff_style`, `auto_reading_mode`) and merely persist
//! here. Anything that is a real setting — not device-local presentation —
//! lives in this backend-owned YAML file, a sibling of `workspace.yaml` /
//! `git-view.yaml`.
//!
//! Shape and persistence mirror [`crate::git_registry`]: a user-global YAML file
//! resolved through the same mechanism, with graceful degradation — a missing or
//! corrupt `config.yaml` degrades to defaults rather than failing app startup.
//!
//! Every field carries `#[serde(default)]` so the file is forward/backward
//! compatible: a `config.yaml` written by an older build (missing a key) loads
//! with that key defaulted, and an unknown future key is ignored. Later
//! milestones add keys here (worktree base path, diff style) the same way.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::error::AppError;
use switchboard_core::{HarnessKind, normalize_selection};

/// Independent quick choices and starting defaults for one harness.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct AgentDefaults {
    pub model_choices: Vec<String>,
    pub effort_choices: Vec<String>,
    pub default_model: Option<String>,
    pub default_effort: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AgentDefaultsWire {
    model_choices: Vec<String>,
    effort_choices: Vec<String>,
    default_model: Option<String>,
    default_effort: Option<String>,
    primary: Option<LegacyPreferenceProfile>,
    secondary: Option<LegacyPreferenceProfile>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LegacyPreferenceProfile {
    model: Option<String>,
    effort: Option<String>,
}

impl<'de> Deserialize<'de> for AgentDefaults {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AgentDefaultsWire::deserialize(deserializer)?;
        if wire.primary.is_some() || wire.secondary.is_some() {
            let primary = wire.primary.unwrap_or_default();
            let model_choices = legacy_default_choices(
                primary.model.as_ref(),
                wire.secondary
                    .as_ref()
                    .and_then(|profile| profile.model.as_ref()),
            );
            let effort_choices = legacy_default_choices(
                primary.effort.as_ref(),
                wire.secondary
                    .as_ref()
                    .and_then(|profile| profile.effort.as_ref()),
            );
            return Ok(Self {
                default_model: primary.model.or_else(|| model_choices.first().cloned()),
                default_effort: primary.effort.or_else(|| effort_choices.first().cloned()),
                model_choices,
                effort_choices,
            });
        }
        Ok(Self {
            model_choices: wire.model_choices,
            effort_choices: wire.effort_choices,
            default_model: wire.default_model,
            default_effort: wire.default_effort,
        })
    }
}

fn legacy_default_choices(primary: Option<&String>, secondary: Option<&String>) -> Vec<String> {
    primary.into_iter().chain(secondary).cloned().collect()
}

fn default_agent_defaults() -> BTreeMap<HarnessKind, AgentDefaults> {
    BTreeMap::from([
        (
            HarnessKind::ClaudeCode,
            AgentDefaults {
                model_choices: vec!["fable".to_owned(), "opus".to_owned()],
                effort_choices: vec!["medium".to_owned(), "high".to_owned()],
                default_model: Some("opus".to_owned()),
                default_effort: Some("medium".to_owned()),
            },
        ),
        (
            HarnessKind::Codex,
            AgentDefaults {
                model_choices: vec!["gpt-5.6-sol".to_owned(), "gpt-5.6-terra".to_owned()],
                effort_choices: vec!["medium".to_owned(), "high".to_owned()],
                default_model: Some("gpt-5.6-terra".to_owned()),
                default_effort: Some("medium".to_owned()),
            },
        ),
        (
            HarnessKind::Antigravity,
            AgentDefaults {
                model_choices: vec!["gemini-3.8-flash".to_owned(), "gemini-3.1-pro".to_owned()],
                effort_choices: vec!["medium".to_owned(), "high".to_owned()],
                default_model: Some("gemini-3.8-flash".to_owned()),
                default_effort: Some("medium".to_owned()),
            },
        ),
    ])
}

fn deserialize_agent_defaults<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<HarnessKind, AgentDefaults>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = BTreeMap::<String, serde_norway::Value>::deserialize(deserializer)?;
    let mut known = BTreeMap::new();
    for (key, value) in raw {
        let harness = match key.as_str() {
            "claude_code" => HarnessKind::ClaudeCode,
            "codex" => HarnessKind::Codex,
            "antigravity" => HarnessKind::Antigravity,
            _ => continue,
        };
        let defaults = serde_norway::from_value(value).map_err(D::Error::custom)?;
        known.insert(harness, defaults);
    }
    Ok(known)
}

/// Merge a value owned by this version into an existing YAML value while
/// preserving fields only a newer version understands. A scalar, sequence, or
/// explicit `null` is authoritative and replaces the old value; recursion is
/// only valid when both sides are mappings.
fn merge_yaml_value(existing: &mut serde_norway::Value, new: serde_norway::Value) {
    match (existing, new) {
        (serde_norway::Value::Mapping(existing), serde_norway::Value::Mapping(new_fields)) => {
            for (key, value) in new_fields {
                match existing.get_mut(&key) {
                    Some(old) => merge_yaml_value(old, value),
                    None => {
                        existing.insert(key, value);
                    }
                }
            }
        }
        (existing, new) => *existing = new,
    }
}

fn remove_legacy_agent_default_keys(
    value: &mut serde_norway::Value,
    recognized: &serde_norway::Value,
) {
    let serde_norway::Value::Mapping(harnesses) = value else {
        return;
    };
    let serde_norway::Value::Mapping(recognized_harnesses) = recognized else {
        return;
    };
    for harness_key in recognized_harnesses.keys() {
        let Some(serde_norway::Value::Mapping(defaults)) = harnesses.get_mut(harness_key) else {
            continue;
        };
        defaults.remove(serde_norway::Value::String("primary".to_owned()));
        defaults.remove(serde_norway::Value::String("secondary".to_owned()));
    }
}

fn normalize_default_axis(
    choices: Vec<String>,
    default: Option<String>,
    built_in_choices: &[String],
    built_in_default: Option<&String>,
) -> (Vec<String>, Option<String>) {
    let mut normalized = Vec::new();
    for choice in choices {
        if let Some(choice) = normalize_selection(Some(choice))
            && !normalized.contains(&choice)
        {
            normalized.push(choice);
        }
    }
    if normalized.is_empty() {
        return (built_in_choices.to_vec(), built_in_default.cloned());
    }
    let default = normalize_selection(default)
        .filter(|value| normalized.contains(value))
        .or_else(|| normalized.first().cloned());
    (normalized, default)
}

/// The default terminal application used by project/worktree open actions and
/// interactive agent resume. macOS ships Terminal.app.
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
// A bag of independent user toggles — the one shape where a run of bools is the
// clearest representation, not a state enum waiting to be found.
#[allow(clippy::struct_excessive_bools)]
pub struct Preferences {
    /// Command used to open a worktree folder in an external editor. Defaults
    /// to `code`; blank → fall back to the OS folder-open (`open <path>` on
    /// macOS).
    pub editor_command: Option<String>,

    /// Terminal application used by project/worktree open actions and agent
    /// resume execution. Normalized to `Terminal` or `iTerm`.
    pub terminal_app: String,

    /// Diff panel layout. Defaults to unified.
    pub diff_style: DiffStyle,

    /// Whether an OS notification fires when agents finish — a manual activity
    /// batch reaching queue-drained idle, or a workflow run reaching its terminal.
    ///
    /// One switch, not two, because Switchboard cannot express "sound but no
    /// banner" itself: whether a delivered notification is shown as a banner is
    /// the user's macOS per-app alert style, invisible to us. So this is the
    /// on/off switch and macOS owns the presentation — including the sound, which
    /// rides on the notification rather than being played separately.
    pub notify_on_completion: bool,

    /// Whether a project finishing should still notify while the user is working
    /// in Switchboard. Applies to any project the user is not treated as looking
    /// at — normally that means a project other than the one on screen, but the
    /// frontend also reports the *viewed* project as not-on-screen while its
    /// reading mode is on (the user has explicitly asked to be treated as absent
    /// from it), so this preference governs that case too.
    ///
    /// Defaults off. The projects sidebar already marks a background project as
    /// finished, so for someone working in the app the information is on screen,
    /// just quietly; an OS banner on top of that is a real intrusion for some
    /// people and the missing signal for others. Off is the less startling
    /// default, and the Settings copy tells anyone who wants it where to find it.
    pub notify_while_focused: bool,

    /// Whether the app-owned read-only built-in prompts and workflows appear in
    /// the pickers. Default `true` (show examples); a user who wants only their
    /// own content turns it off. Visibility only — a workflow wired to a built-in
    /// still resolves when this is off.
    pub show_builtins: bool,

    /// Whether Claude agents get browser tools, via the Claude in Chrome
    /// extension. Claude-only: the Codex equivalent is a plugin enabled in the
    /// `ChatGPT` desktop app, which Switchboard cannot drive.
    ///
    /// Read live per dispatch, so toggling it takes effect on an agent's next
    /// turn rather than requiring a new agent or session.
    ///
    /// Defaults off. The flag costs context on every turn and only does anything
    /// if the user has actually installed the extension, and nothing Switchboard
    /// can read tells us whether they have — the harness reports success either
    /// way (see `docs/research/claude-chrome-extension.md`). So this is the
    /// user's assertion that the extension exists, not a guess we make for them.
    pub claude_chrome_enabled: bool,

    /// Whether dispatching work — a compose-bar send or a workflow launch —
    /// automatically puts that project into reading mode, as if the user had
    /// toggled it themselves. Consumed entirely by the frontend; the backend
    /// only persists it. Defaults off: reading mode hides the compose box after
    /// every send, which is the point for users who toggle it manually on each
    /// send today and a surprise for everyone else.
    pub auto_reading_mode: bool,

    /// Per-harness model/effort choices preselected by Add Agent and used when a new
    /// project auto-creates its roster.
    #[serde(
        default = "default_agent_defaults",
        deserialize_with = "deserialize_agent_defaults"
    )]
    pub agent_defaults: BTreeMap<HarnessKind, AgentDefaults>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            editor_command: Some("code".to_owned()),
            terminal_app: default_terminal_app(),
            diff_style: DiffStyle::default(),
            show_builtins: true,
            claude_chrome_enabled: false,
            auto_reading_mode: false,
            notify_on_completion: true,
            notify_while_focused: false,
            agent_defaults: default_agent_defaults(),
        }
    }
}

impl Preferences {
    /// Enforce the field invariants the backend owns, regardless of how the value
    /// arrived (deserialized from a hand-edited `config.yaml`, or sent by a
    /// client). Trims surrounding whitespace; a blank editor command becomes
    /// `None` (→ OS folder-open) and a blank terminal app becomes the default.
    /// Applied at every boundary (`load` + `set`) so external-app consumers
    /// never see an empty command to spawn.
    #[must_use]
    pub fn normalized(self) -> Self {
        let editor_command = self
            .editor_command
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty());
        let terminal_app = {
            let trimmed = self.terminal_app.trim();
            if trimmed.eq_ignore_ascii_case("iterm") || trimmed.eq_ignore_ascii_case("iterm2") {
                "iTerm".to_owned()
            } else {
                default_terminal_app()
            }
        };
        let mut agent_defaults = default_agent_defaults();
        for (harness, defaults) in self.agent_defaults {
            let built_in = &agent_defaults[&harness];
            let (model_choices, default_model) = if harness.supports_model_selection() {
                normalize_default_axis(
                    defaults.model_choices,
                    defaults.default_model,
                    &built_in.model_choices,
                    built_in.default_model.as_ref(),
                )
            } else {
                (Vec::new(), None)
            };
            let (effort_choices, default_effort) = if harness.supports_effort_selection() {
                normalize_default_axis(
                    defaults.effort_choices,
                    defaults.default_effort,
                    &built_in.effort_choices,
                    built_in.default_effort.as_ref(),
                )
            } else {
                (Vec::new(), None)
            };
            agent_defaults.insert(
                harness,
                AgentDefaults {
                    model_choices,
                    effort_choices,
                    default_model,
                    default_effort,
                },
            );
        }
        Self {
            editor_command,
            terminal_app,
            diff_style: self.diff_style,
            show_builtins: self.show_builtins,
            claude_chrome_enabled: self.claude_chrome_enabled,
            auto_reading_mode: self.auto_reading_mode,
            notify_on_completion: self.notify_on_completion,
            notify_while_focused: self.notify_while_focused,
            agent_defaults,
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
    let mut fields = match serialized {
        serde_norway::Value::Mapping(fields) => fields,
        _ => serde_norway::Mapping::new(),
    };
    switchboard_core::edit_yaml_mapping(path, move |root| {
        let defaults_key = serde_norway::Value::String("agent_defaults".to_owned());
        if let Some(new_defaults) = fields.remove(&defaults_key) {
            match root.get_mut(&defaults_key) {
                Some(existing) => {
                    // The recursive merge preserves unknown future data, so
                    // known retired schema must be removed deliberately.
                    remove_legacy_agent_default_keys(existing, &new_defaults);
                    merge_yaml_value(existing, new_defaults);
                }
                None => {
                    root.insert(defaults_key, new_defaults);
                }
            }
        }
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
        assert!(p.notify_on_completion, "notifications are on by default");
    }

    #[test]
    fn missing_notify_key_defaults_on() {
        // Forward/backward compat: a config.yaml written before notifications
        // existed must load with them on, not silently off — an upgrade that
        // turned the feature off by default would look exactly like it being
        // broken.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "terminal_app: iTerm\n").unwrap();
        assert!(load(&path).notify_on_completion);
    }

    #[test]
    fn notify_preference_round_trips_when_off() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let prefs = Preferences {
            notify_on_completion: false,
            notify_while_focused: false,
            ..Preferences::default()
        };
        save(&path, &prefs).unwrap();
        assert!(!load(&path).notify_on_completion);
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
    fn missing_claude_chrome_key_defaults_off() {
        // Browser access must never switch itself on for an existing user: the
        // flag costs context every turn and only works if they installed the
        // extension. An upgrade that silently enabled it would be a surprise.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "terminal_app: iTerm\n").unwrap();
        assert!(!load(&path).claude_chrome_enabled);
    }

    #[test]
    fn claude_chrome_survives_normalization_when_on() {
        // `normalized()` rebuilds the struct field by field, so a new key is
        // easy to drop there while every load/save test still passes.
        let prefs = Preferences {
            claude_chrome_enabled: true,
            ..Preferences::default()
        }
        .normalized();
        assert!(prefs.claude_chrome_enabled);
    }

    #[test]
    fn missing_auto_reading_mode_key_defaults_off() {
        // An upgrade must not start hiding the compose box after every send —
        // to a user who never chose that, it reads as the composer breaking.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "terminal_app: iTerm\n").unwrap();
        assert!(!load(&path).auto_reading_mode);
    }

    #[test]
    fn auto_reading_mode_round_trips_and_survives_normalization() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let prefs = Preferences {
            auto_reading_mode: true,
            ..Preferences::default()
        };
        save(&path, &prefs).unwrap();
        assert!(load(&path).auto_reading_mode, "load applies normalized()");
    }

    #[test]
    fn missing_agent_defaults_gets_all_harness_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "terminal_app: iTerm\n").unwrap();

        let loaded = load(&path);

        assert_eq!(loaded.agent_defaults, default_agent_defaults());
    }

    #[test]
    fn every_harness_has_non_empty_quick_choices_and_defaults() {
        let defaults = Preferences::default();

        assert!(
            defaults
                .agent_defaults
                .values()
                .all(|value| !value.model_choices.is_empty()
                    && !value.effort_choices.is_empty()
                    && value.default_model.is_some()
                    && value.default_effort.is_some())
        );
        assert_eq!(
            defaults.agent_defaults[&HarnessKind::Antigravity],
            AgentDefaults {
                model_choices: vec!["gemini-3.8-flash".to_owned(), "gemini-3.1-pro".to_owned()],
                effort_choices: vec!["medium".to_owned(), "high".to_owned()],
                default_model: Some("gemini-3.8-flash".to_owned()),
                default_effort: Some("medium".to_owned()),
            }
        );
    }

    #[test]
    fn agent_defaults_normalize_choices_and_fill_missing_harnesses() {
        let prefs = Preferences {
            agent_defaults: BTreeMap::from([(
                HarnessKind::ClaudeCode,
                AgentDefaults {
                    model_choices: vec!["  sonnet  ".to_owned(), " haiku ".to_owned()],
                    effort_choices: vec![" medium ".to_owned(), " low ".to_owned()],
                    default_model: Some("  sonnet  ".to_owned()),
                    default_effort: Some(" medium ".to_owned()),
                },
            )]),
            ..Preferences::default()
        }
        .normalized();

        assert_eq!(
            prefs.agent_defaults[&HarnessKind::ClaudeCode],
            AgentDefaults {
                model_choices: vec!["sonnet".to_owned(), "haiku".to_owned()],
                effort_choices: vec!["medium".to_owned(), "low".to_owned()],
                default_model: Some("sonnet".to_owned()),
                default_effort: Some("medium".to_owned()),
            }
        );
        assert_eq!(
            prefs.agent_defaults[&HarnessKind::Codex],
            default_agent_defaults()[&HarnessKind::Codex]
        );
    }

    #[test]
    fn agent_defaults_fall_back_from_an_out_of_set_default() {
        let prefs = Preferences {
            agent_defaults: BTreeMap::from([(
                HarnessKind::ClaudeCode,
                AgentDefaults {
                    model_choices: vec![" opus ".to_owned(), "sonnet".to_owned()],
                    effort_choices: vec!["future-effort".to_owned(), "medium".to_owned()],
                    default_model: Some("typo".to_owned()),
                    default_effort: Some("future-effort".to_owned()),
                },
            )]),
            ..Preferences::default()
        }
        .normalized();

        assert_eq!(
            prefs.agent_defaults[&HarnessKind::ClaudeCode],
            AgentDefaults {
                model_choices: vec!["opus".to_owned(), "sonnet".to_owned()],
                effort_choices: vec!["future-effort".to_owned(), "medium".to_owned()],
                default_model: Some("opus".to_owned()),
                default_effort: Some("future-effort".to_owned()),
            }
        );
    }

    #[test]
    fn future_agent_defaults_are_opaque_on_load_and_preserved_recursively_on_save() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "terminal_app: iTerm\nagent_defaults:\n  claude_code:\n    future_mode: adaptive\n    primary:\n      model: sonnet\n      effort: medium\n      future_axis: preserved\n    secondary:\n      model: haiku\n      effort: low\n      future_axis: removed-with-secondary\n  future_harness:\n    - a-shape\n    - this-version-cannot-parse\n",
        )
        .unwrap();

        let mut prefs = load(&path);
        assert_eq!(prefs.terminal_app, "iTerm");
        assert_eq!(
            prefs.agent_defaults[&HarnessKind::ClaudeCode]
                .default_model
                .as_deref(),
            Some("sonnet")
        );
        prefs
            .agent_defaults
            .get_mut(&HarnessKind::ClaudeCode)
            .unwrap()
            .model_choices = vec!["sonnet".to_owned()];
        prefs.editor_command = Some("zed".to_owned());
        save(&path, &prefs).unwrap();

        let reread: serde_norway::Value = switchboard_core::read_yaml(&path).unwrap();
        let defaults = reread
            .get("agent_defaults")
            .and_then(serde_norway::Value::as_mapping)
            .unwrap();
        let future = defaults
            .get(serde_norway::Value::String("future_harness".to_owned()))
            .and_then(serde_norway::Value::as_sequence)
            .unwrap();
        assert_eq!(future.len(), 2);

        let claude = defaults
            .get(serde_norway::Value::String("claude_code".to_owned()))
            .and_then(serde_norway::Value::as_mapping)
            .unwrap();
        assert_eq!(
            claude.get(serde_norway::Value::String("future_mode".to_owned())),
            Some(&serde_norway::Value::String("adaptive".to_owned()))
        );
        assert!(!claude.contains_key(serde_norway::Value::String("primary".to_owned())));
        assert!(!claude.contains_key(serde_norway::Value::String("secondary".to_owned())));
        assert_eq!(
            claude.get(serde_norway::Value::String("model_choices".to_owned())),
            Some(&serde_norway::Value::Sequence(vec![
                serde_norway::Value::String("sonnet".to_owned())
            ]))
        );
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
            // Non-default in the other direction: an explicitly-on toggle.
            claude_chrome_enabled: true,
            auto_reading_mode: false,
            notify_on_completion: true,
            notify_while_focused: false,
            agent_defaults: default_agent_defaults(),
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
                claude_chrome_enabled: false,
                auto_reading_mode: false,
                notify_on_completion: true,
                notify_while_focused: false,
                agent_defaults: default_agent_defaults(),
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
                    claude_chrome_enabled: false,
                    auto_reading_mode: false,
                    notify_on_completion: true,
                    notify_while_focused: false,
                    agent_defaults: default_agent_defaults(),
                },
            )
            .unwrap();
        });
        let adder = std::thread::spawn(move || {
            prompts
                .add_mcp_provider(
                    "team",
                    "https://example.test",
                    switchboard_prompts::McpAuth::Bearer,
                    None,
                )
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
    fn unsupported_legacy_terminal_migrates_to_terminal_at_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "terminal_app: Ghostty\n").unwrap();

        assert_eq!(load(&path).terminal_app, "Terminal");
    }

    #[test]
    fn normalized_trims_and_maps_blanks() {
        let p = Preferences {
            editor_command: Some("  cursor  ".to_owned()),
            terminal_app: "  iTerm  ".to_owned(),
            diff_style: DiffStyle::Unified,
            show_builtins: true,
            claude_chrome_enabled: false,
            auto_reading_mode: false,
            notify_on_completion: true,
            notify_while_focused: false,
            agent_defaults: default_agent_defaults(),
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
            claude_chrome_enabled: false,
            auto_reading_mode: false,
            notify_on_completion: true,
            notify_while_focused: false,
            agent_defaults: default_agent_defaults(),
        }
        .normalized();
        assert_eq!(blank.editor_command, None);
        assert_eq!(blank.terminal_app, "Terminal");

        let legacy = Preferences {
            terminal_app: "Ghostty".to_owned(),
            ..Preferences::default()
        }
        .normalized();
        assert_eq!(legacy.terminal_app, "Terminal");
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
