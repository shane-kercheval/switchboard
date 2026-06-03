//! The prompt service: the single entry point `crates/app` drives through its
//! Tauri command shims. Holds the resolved (injected) config path, default
//! prompts directory, and home directory; builds providers on demand and
//! dispatches `list` / `render` by provider prefix.
//!
//! This milestone scans local directories on each call — cheap and offline. The
//! build-once cache (and the MCP provider) arrive in a later milestone; this
//! service is where that cache will live, so the command surface stays stable.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{PromptConfig, resolve_local_dirs};
use crate::error::PromptError;
use crate::local::LocalProvider;
use crate::model::{LOCAL_PROVIDER, Prompt};
use crate::provider::PromptProvider;

/// Rendered prompt text, as returned to the frontend. A struct (rather than a
/// bare string) keeps the wire shape stable as later milestones add fields.
#[derive(Debug, Clone, Serialize)]
pub struct RenderedPrompt {
    pub text: String,
}

/// Resolves prompts from user-global config. Construct with [`PromptService::new`]
/// in production (paths injected by `crates/app`); [`PromptService::disabled`]
/// yields an inert service (lists nothing, render fails) for contexts with no
/// configured prompt store (e.g. command tests that don't exercise prompts).
pub struct PromptService {
    config_path: Option<PathBuf>,
    default_prompt_dir: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl PromptService {
    #[must_use]
    pub fn new(config_path: PathBuf, default_prompt_dir: PathBuf, home: Option<PathBuf>) -> Self {
        Self {
            config_path: Some(config_path),
            default_prompt_dir: Some(default_prompt_dir),
            home,
        }
    }

    /// An inert service for contexts with no resolved prompt store. Listing is
    /// empty; rendering fails as provider-not-found.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            config_path: None,
            default_prompt_dir: None,
            home: None,
        }
    }

    /// All prompts across all configured providers. Never hard-fails: a missing
    /// config or unreadable directory degrades to fewer (or zero) prompts with a
    /// warning, so the compose box and local prompts keep working.
    #[must_use]
    pub fn list(&self) -> Vec<Prompt> {
        let Some(local) = self.local_provider() else {
            return Vec::new();
        };
        local.list()
    }

    /// Render `name` from `provider` with `args`. Serves both preview and send —
    /// the same args map must be passed to both so they never diverge.
    pub fn render(
        &self,
        provider: &str,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<RenderedPrompt, PromptError> {
        match provider {
            LOCAL_PROVIDER => {
                let local = self
                    .local_provider()
                    .ok_or_else(|| PromptError::ProviderNotFound {
                        provider: provider.to_owned(),
                    })?;
                Ok(RenderedPrompt {
                    text: local.render(name, args)?,
                })
            }
            // Generic/preset MCP providers are configured-but-inert this
            // milestone; their client lands later.
            other => Err(PromptError::ProviderNotFound {
                provider: other.to_owned(),
            }),
        }
    }

    /// Build the local provider from current config, or `None` when this service
    /// has no resolved prompt store (disabled).
    fn local_provider(&self) -> Option<LocalProvider> {
        let default_dir = self.default_prompt_dir.as_deref()?;
        let config = self.load_config();
        let dirs = resolve_local_dirs(&config, default_dir, self.home.as_deref());
        Some(LocalProvider::new(dirs))
    }

    /// Read the user-global config. A missing file is the common case (empty
    /// config → default dir). A corrupt file degrades to defaults with a warning
    /// rather than breaking local prompts; config.yaml holds no secrets, so the
    /// parse error is safe to log.
    fn load_config(&self) -> PromptConfig {
        let Some(path) = self.config_path.as_deref() else {
            return PromptConfig::default();
        };
        if !path.exists() {
            return PromptConfig::default();
        }
        match switchboard_core::read_yaml::<PromptConfig>(path) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not read prompt config; using defaults"
                );
                PromptConfig::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn write(dir: &Path, file: &str, content: &str) {
        std::fs::write(dir.join(file), content).unwrap();
    }

    #[test]
    fn disabled_service_lists_nothing_and_render_fails() {
        let service = PromptService::disabled();
        assert!(service.list().is_empty());
        let err = service.render("local", "x", &BTreeMap::new()).unwrap_err();
        assert!(matches!(err, PromptError::ProviderNotFound { .. }));
    }

    #[test]
    fn lists_and_renders_from_default_dir_when_no_config() {
        let dir = TempDir::new().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir(&prompts_dir).unwrap();
        write(
            &prompts_dir,
            "p.md",
            "---\nname: p\ndescription: d\n---\nHello\n",
        );

        // config.yaml does not exist → default dir is used.
        let service = PromptService::new(dir.path().join("config.yaml"), prompts_dir, None);
        let prompts = service.list();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "p");

        let rendered = service.render("local", "p", &BTreeMap::new()).unwrap();
        assert!(rendered.text.contains("Hello"));
    }

    #[test]
    fn config_local_prompt_dirs_override_default() {
        let root = TempDir::new().unwrap();
        let custom = root.path().join("custom");
        let default = root.path().join("prompts");
        std::fs::create_dir(&custom).unwrap();
        std::fs::create_dir(&default).unwrap();
        write(
            &custom,
            "c.md",
            "---\nname: from-custom\ndescription: d\n---\nB\n",
        );
        write(
            &default,
            "d.md",
            "---\nname: from-default\ndescription: d\n---\nB\n",
        );

        let config_path = root.path().join("config.yaml");
        std::fs::write(
            &config_path,
            format!("local_prompt_dirs:\n  - {}\n", custom.display()),
        )
        .unwrap();

        let service = PromptService::new(config_path, default, None);
        let names: Vec<String> = service.list().into_iter().map(|p| p.name).collect();
        // Only the configured dir is scanned; the default is not implicitly added.
        assert_eq!(names, vec!["from-custom".to_owned()]);
    }

    #[test]
    fn render_unknown_provider_fails() {
        let dir = TempDir::new().unwrap();
        let service = PromptService::new(
            dir.path().join("config.yaml"),
            dir.path().join("prompts"),
            None,
        );
        let err = service.render("tiddly", "x", &BTreeMap::new()).unwrap_err();
        assert!(matches!(err, PromptError::ProviderNotFound { provider } if provider == "tiddly"));
    }

    #[test]
    fn corrupt_config_degrades_to_default_dir() {
        let dir = TempDir::new().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir(&prompts_dir).unwrap();
        write(
            &prompts_dir,
            "p.md",
            "---\nname: p\ndescription: d\n---\nHi\n",
        );
        let config_path = dir.path().join("config.yaml");
        // Not valid YAML for PromptConfig (a scalar where a mapping is expected).
        std::fs::write(&config_path, "just a string, not a mapping\n").unwrap();

        let service = PromptService::new(config_path, prompts_dir, None);
        // Local prompts from the default dir still resolve.
        assert_eq!(service.list().len(), 1);
    }
}
