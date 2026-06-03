//! The prompt service: the single entry point `crates/app` drives through its
//! Tauri command shims. Owns the resolved (injected) config path, default
//! prompts directory, home directory, the secret store, and the **build-once
//! prompt cache**.
//!
//! - `list` is **synchronous** — it reads the cache and never touches a provider
//!   or the network.
//! - `sync` is **async** — it (re)builds the cache: scans local dirs and queries
//!   each MCP provider under a per-provider timeout, degrading a down provider to
//!   nothing rather than failing the build.
//! - `render` is **async** — provider-dispatched; local renders via `MiniJinja`,
//!   MCP via `prompts/get`. It does **not** use the cache (it must reflect the
//!   provider's current state at invocation time).
//!
//! `PromptService` is cheaply `Clone` (paths + `Arc`s) so the app can hand a
//! clone to a background task that warms the cache at startup while the original
//! lives in `AppState`; both share the same cache `Arc`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Serialize;

use crate::config::{
    McpProviderConfig, McpSection, McpTransport, PromptConfig, resolve_local_dirs,
};
use crate::error::PromptError;
use crate::local::LocalProvider;
use crate::mcp::McpProvider;
use crate::model::{LOCAL_PROVIDER, Prompt};
use crate::provider::PromptProvider;
use crate::secret::{InMemorySecretStore, SecretStore};

/// Per-provider budget for the whole connect + request round-trip during a cache
/// build, so one slow/cold MCP server can't stall startup.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

/// Rendered prompt text, as returned to the frontend. A struct (rather than a
/// bare string) keeps the wire shape stable as later milestones add fields.
#[derive(Debug, Clone, Serialize)]
pub struct RenderedPrompt {
    pub text: String,
}

/// Resolves prompts from user-global config. Construct with [`PromptService::new`]
/// in production (paths + secret store injected by `crates/app`);
/// [`PromptService::disabled`] yields an inert service (lists nothing, render
/// fails) for contexts with no configured prompt store.
#[derive(Clone)]
pub struct PromptService {
    config_path: Option<PathBuf>,
    default_prompt_dir: Option<PathBuf>,
    home: Option<PathBuf>,
    secrets: Arc<dyn SecretStore>,
    cache: Arc<RwLock<Vec<Prompt>>>,
    /// Serializes cache rebuilds so an older, slower `sync` can't finish after a
    /// newer one and overwrite the cache with stale results.
    sync_lock: Arc<tokio::sync::Mutex<()>>,
}

impl PromptService {
    #[must_use]
    pub fn new(
        config_path: PathBuf,
        default_prompt_dir: PathBuf,
        home: Option<PathBuf>,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            config_path: Some(config_path),
            default_prompt_dir: Some(default_prompt_dir),
            home,
            secrets,
            cache: Arc::new(RwLock::new(Vec::new())),
            sync_lock: Arc::new(tokio::sync::Mutex::new(())),
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
            secrets: Arc::new(InMemorySecretStore::new()),
            cache: Arc::new(RwLock::new(Vec::new())),
            sync_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// All cached prompts. Synchronous, offline, instant — the `list_prompts`
    /// hot path. Empty until the first [`sync`](Self::sync) completes.
    #[must_use]
    pub fn list(&self) -> Vec<Prompt> {
        self.cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Rebuild the cache from all configured providers.
    ///
    /// - **Serialized** (the `sync_lock`): concurrent rebuilds can't interleave
    ///   or publish stale results out of order.
    /// - **Local prompts publish immediately** after the (fast) filesystem scan,
    ///   so they're never held hostage by a slow or down MCP server.
    /// - **MCP providers are queried concurrently**, so the per-provider timeout
    ///   bounds the *whole* MCP phase (~1×`PROVIDER_TIMEOUT`), not the sum, and a
    ///   slow provider can't delay the others. A provider that errors or times
    ///   out contributes nothing (with a warning).
    pub async fn sync(&self) {
        let _guard = self.sync_lock.lock().await;

        let mut prompts = match self.local_provider() {
            Some(local) => local.list().await,
            None => Vec::new(),
        };
        self.publish(prompts.clone());

        let providers: Vec<McpProvider> = self
            .mcp_provider_configs()
            .into_iter()
            .map(|config| {
                let McpTransport::Http { url } = &config.transport;
                let bearer = self.resolve_bearer(&config.name);
                McpProvider::new(config.name.clone(), url.clone(), bearer, PROVIDER_TIMEOUT)
            })
            .collect();
        let mcp_lists = futures::future::join_all(providers.iter().map(McpProvider::list)).await;
        for list in mcp_lists {
            prompts.extend(list);
        }
        self.publish(prompts);
    }

    fn publish(&self, prompts: Vec<Prompt>) {
        *self
            .cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = prompts;
    }

    /// Render `name` from `provider` with `args`. Serves both preview and send —
    /// the same args map must be passed to both so they never diverge. Does not
    /// read the cache: local re-reads the file, MCP calls `prompts/get` live.
    pub async fn render(
        &self,
        provider: &str,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<RenderedPrompt, PromptError> {
        let text = if provider == LOCAL_PROVIDER {
            let local = self
                .local_provider()
                .ok_or_else(|| PromptError::ProviderNotFound {
                    provider: provider.to_owned(),
                })?;
            local.render(name, args).await?
        } else {
            let config = self
                .mcp_provider_configs()
                .into_iter()
                .find(|c| c.name == provider)
                .ok_or_else(|| PromptError::ProviderNotFound {
                    provider: provider.to_owned(),
                })?;
            let McpTransport::Http { url } = &config.transport;
            let bearer = self.resolve_bearer(&config.name);
            McpProvider::new(config.name.clone(), url.clone(), bearer, PROVIDER_TIMEOUT)
                .render(name, args)
                .await?
        };
        Ok(RenderedPrompt { text })
    }

    /// Resolve a provider's bearer from the secret store. A *missing* credential
    /// (`Ok(None)`) and a *store failure* (`Err`) both degrade to unauthenticated
    /// here, but the store failure is logged distinctly — M3's status UI uses the
    /// same distinction to show "store unavailable" vs "token missing".
    fn resolve_bearer(&self, provider: &str) -> Option<String> {
        match self.secrets.get(provider) {
            Ok(bearer) => bearer,
            Err(e) => {
                tracing::warn!(provider = %provider, error = %e, "could not read secret store; treating provider as unauthenticated");
                None
            }
        }
    }

    /// Build the local provider from current config, or `None` when this service
    /// has no resolved prompt store (disabled).
    fn local_provider(&self) -> Option<LocalProvider> {
        let default_dir = self.default_prompt_dir.as_deref()?;
        let config = self.load_local_config();
        let dirs = resolve_local_dirs(&config, default_dir, self.home.as_deref());
        Some(LocalProvider::new(dirs))
    }

    /// Read the user-global config's local section. A missing file is the common
    /// case (empty config → default dir). A corrupt file degrades to defaults
    /// with a warning; config.yaml holds no secrets, so the error is safe to log.
    fn load_local_config(&self) -> PromptConfig {
        let Some(path) = self.config_path.as_deref() else {
            return PromptConfig::default();
        };
        if !path.exists() {
            return PromptConfig::default();
        }
        match switchboard_core::read_yaml::<PromptConfig>(path) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "could not read prompt config; using defaults");
                PromptConfig::default()
            }
        }
    }

    /// Read the MCP-provider entries. Read **independently** of the local config
    /// (see [`PromptConfig`]) so a malformed `mcp_providers:` section can never
    /// break local prompts; individual bad entries are skipped (with a warning).
    fn mcp_provider_configs(&self) -> Vec<McpProviderConfig> {
        let Some(path) = self.config_path.as_deref() else {
            return Vec::new();
        };
        if !path.exists() {
            return Vec::new();
        }
        match switchboard_core::read_yaml::<McpSection>(path) {
            Ok(section) => section.into_configs(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "could not read mcp_providers; ignoring");
                Vec::new()
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

    fn service_with_prompts_dir() -> (TempDir, PromptService) {
        let dir = TempDir::new().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir(&prompts_dir).unwrap();
        let service = PromptService::new(
            dir.path().join("config.yaml"),
            prompts_dir,
            None,
            Arc::new(InMemorySecretStore::new()),
        );
        (dir, service)
    }

    #[tokio::test]
    async fn disabled_service_lists_nothing_and_render_fails() {
        let service = PromptService::disabled();
        service.sync().await;
        assert!(service.list().is_empty());
        let err = service
            .render("local", "x", &BTreeMap::new())
            .await
            .unwrap_err();
        assert!(matches!(err, PromptError::ProviderNotFound { .. }));
    }

    #[tokio::test]
    async fn syncs_and_renders_local_from_default_dir() {
        let (dir, service) = service_with_prompts_dir();
        write(
            &dir.path().join("prompts"),
            "p.md",
            "---\nname: p\ndescription: d\n---\nHello\n",
        );

        // Before sync the cache is empty; after sync the local prompt appears.
        assert!(service.list().is_empty());
        service.sync().await;
        let prompts = service.list();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "p");

        // Render does not depend on the cache.
        let rendered = service
            .render("local", "p", &BTreeMap::new())
            .await
            .unwrap();
        assert!(rendered.text.contains("Hello"));
    }

    #[tokio::test]
    async fn config_local_prompt_dirs_override_default() {
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

        let service = PromptService::new(
            config_path,
            default,
            None,
            Arc::new(InMemorySecretStore::new()),
        );
        service.sync().await;
        let names: Vec<String> = service.list().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["from-custom".to_owned()]);
    }

    #[tokio::test]
    async fn render_unknown_provider_fails() {
        let (_dir, service) = service_with_prompts_dir();
        let err = service
            .render("tiddly", "x", &BTreeMap::new())
            .await
            .unwrap_err();
        assert!(matches!(err, PromptError::ProviderNotFound { provider } if provider == "tiddly"));
    }

    #[tokio::test]
    async fn local_prompts_survive_unreachable_mcp_provider() {
        // Local prompts must be published even when an MCP provider is down
        // (port 1 → connection refused). Verifies the local-first publish + merge.
        let dir = TempDir::new().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir(&prompts_dir).unwrap();
        write(
            &prompts_dir,
            "note.md",
            "---\nname: note\ndescription: d\n---\nB\n",
        );
        let config_path = dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "mcp_providers:\n  - name: team\n    transport:\n      type: http\n      url: http://127.0.0.1:1/mcp\n",
        )
        .unwrap();

        let service = PromptService::new(
            config_path,
            prompts_dir,
            None,
            Arc::new(InMemorySecretStore::new()),
        );
        service.sync().await;
        let names: Vec<String> = service.list().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["note".to_owned()]);
    }

    #[tokio::test]
    async fn concurrent_syncs_produce_a_consistent_cache() {
        // The sync_lock serializes rebuilds; two concurrent syncs must leave the
        // cache in the single-sync state, never a torn/duplicated one.
        let (dir, service) = service_with_prompts_dir();
        write(
            &dir.path().join("prompts"),
            "p.md",
            "---\nname: p\ndescription: d\n---\nB\n",
        );
        let other = service.clone();
        tokio::join!(service.sync(), other.sync());
        assert_eq!(service.list().len(), 1);
    }

    #[tokio::test]
    async fn corrupt_config_degrades_to_default_dir() {
        let dir = TempDir::new().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir(&prompts_dir).unwrap();
        write(
            &prompts_dir,
            "p.md",
            "---\nname: p\ndescription: d\n---\nHi\n",
        );
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, "just a string, not a mapping\n").unwrap();

        let service = PromptService::new(
            config_path,
            prompts_dir,
            None,
            Arc::new(InMemorySecretStore::new()),
        );
        service.sync().await;
        assert_eq!(service.list().len(), 1);
    }
}
