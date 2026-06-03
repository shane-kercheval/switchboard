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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Serialize;

use crate::config::{
    McpProviderConfig, McpSection, McpTransport, PromptConfig, is_valid_provider_name,
    resolve_local_dirs,
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

/// An MCP provider as shown in Settings: its non-secret config plus whether a
/// bearer is stored and the outcome of the last cache build.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct McpProviderInfo {
    pub name: String,
    pub url: String,
    /// Whether a bearer token is currently stored for this provider.
    pub has_token: bool,
    pub status: ProviderStatus,
}

/// Outcome of the last attempt to list a provider's prompts.
///
/// Deliberately coarse: `rmcp` collapses transport failures (connection refused,
/// HTTP 401/403) into one opaque error that can't be reliably sub-classified
/// without coupling to its internals, so a failure is one `errored` bucket with
/// the underlying message surfaced for detail — rather than a fragile
/// auth-vs-unreachable split. `store_unavailable` (the keychain couldn't be read)
/// is genuinely distinct and knowable locally; the "no credential stored" nudge
/// is carried by [`McpProviderInfo::has_token`].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProviderStatus {
    /// The last sync listed prompts successfully.
    Ok { prompt_count: usize },
    /// The last sync failed; `message` is the redacted error (never a token).
    Errored { message: String },
    /// The secret store couldn't be read (e.g. keychain locked/absent).
    StoreUnavailable,
    /// No sync has recorded a status yet (e.g. just added, not yet built).
    Unknown,
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
    /// Per-MCP-provider outcome of the last cache build, keyed by provider name.
    /// Read by `list_mcp_providers` to drive the Settings status column.
    provider_status: Arc<RwLock<HashMap<String, ProviderStatus>>>,
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
            provider_status: Arc::new(RwLock::new(HashMap::new())),
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
            provider_status: Arc::new(RwLock::new(HashMap::new())),
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
        // One provider being prepared for the concurrent query phase; carries the
        // secret-store read outcome for the StoreUnavailable status.
        struct Pending {
            name: String,
            store_unavailable: bool,
            provider: McpProvider,
        }

        let _guard = self.sync_lock.lock().await;

        let mut prompts = match self.local_provider() {
            Some(local) => local.list().await,
            None => Vec::new(),
        };
        self.publish(prompts.clone());

        let pendings: Vec<Pending> = self
            .mcp_provider_configs()
            .into_iter()
            .map(|config| {
                let McpTransport::Http { url } = &config.transport;
                let (bearer, store_unavailable) = self.resolve_bearer(&config.name);
                Pending {
                    name: config.name.clone(),
                    store_unavailable,
                    provider: McpProvider::new(
                        config.name.clone(),
                        url.clone(),
                        bearer,
                        PROVIDER_TIMEOUT,
                    ),
                }
            })
            .collect();
        let results =
            futures::future::join_all(pendings.iter().map(|p| p.provider.list_result())).await;

        let mut statuses: HashMap<String, ProviderStatus> = HashMap::new();
        for (pending, result) in pendings.iter().zip(results) {
            let status = match result {
                Ok(provider_prompts) => {
                    let status = ProviderStatus::Ok {
                        prompt_count: provider_prompts.len(),
                    };
                    prompts.extend(provider_prompts);
                    status
                }
                // A store-read failure is the more actionable root cause when the
                // list also failed; otherwise surface the provider's own error.
                Err(_) if pending.store_unavailable => ProviderStatus::StoreUnavailable,
                Err(e) => ProviderStatus::Errored {
                    message: e.to_string(),
                },
            };
            statuses.insert(pending.name.clone(), status);
        }
        *self
            .provider_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = statuses;
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
            let (bearer, _) = self.resolve_bearer(&config.name);
            McpProvider::new(config.name.clone(), url.clone(), bearer, PROVIDER_TIMEOUT)
                .render(name, args)
                .await?
        };
        Ok(RenderedPrompt { text })
    }

    /// Resolve a provider's bearer from the secret store, returning the bearer and
    /// whether the store read **failed** (vs. simply having no credential). Both
    /// degrade to unauthenticated, but the failure flag lets the caller record a
    /// distinct `StoreUnavailable` status.
    fn resolve_bearer(&self, provider: &str) -> (Option<String>, bool) {
        match self.secrets.get(provider) {
            Ok(bearer) => (bearer, false),
            Err(e) => {
                tracing::warn!(provider = %provider, error = %e, "could not read secret store; treating provider as unauthenticated");
                (None, true)
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

    /// All configured MCP providers with their last-build status and whether a
    /// bearer is stored. Status comes from the most recent [`sync`](Self::sync);
    /// a just-added provider reads `Unknown` until the next build completes.
    #[must_use]
    pub fn list_mcp_providers(&self) -> Vec<McpProviderInfo> {
        let statuses = self
            .provider_status
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.mcp_provider_configs()
            .into_iter()
            .map(|config| {
                let McpTransport::Http { url } = &config.transport;
                let has_token = matches!(self.secrets.get(&config.name), Ok(Some(_)));
                let status = statuses
                    .get(&config.name)
                    .cloned()
                    .unwrap_or(ProviderStatus::Unknown);
                McpProviderInfo {
                    name: config.name.clone(),
                    url: url.clone(),
                    has_token,
                    status,
                }
            })
            .collect()
    }

    /// Add a generic HTTP MCP provider: validate the name, write its non-secret
    /// config entry (preserving every other config key), and store its bearer in
    /// the secret store. Does **not** rebuild the cache — the caller triggers a
    /// background sync so a slow server can't block the command.
    pub fn add_mcp_provider(
        &self,
        name: &str,
        url: &str,
        bearer: Option<&str>,
    ) -> Result<(), PromptError> {
        if !is_valid_provider_name(name) {
            return Err(PromptError::InvalidProviderName {
                name: name.to_owned(),
            });
        }
        let mut configs = self.mcp_provider_configs();
        if configs.iter().any(|c| c.name == name) {
            return Err(PromptError::DuplicateProvider {
                name: name.to_owned(),
            });
        }
        configs.push(McpProviderConfig {
            name: name.to_owned(),
            transport: McpTransport::Http {
                url: url.to_owned(),
            },
        });
        self.write_mcp_providers(&configs)?;
        if let Some(bearer) = bearer {
            self.secrets.set(name, bearer)?;
        }
        Ok(())
    }

    /// Remove a generic MCP provider: drop its config entry (preserving others),
    /// delete its stored bearer (best-effort, idempotent), and clear its status.
    /// Idempotent — removing an unconfigured name is not an error.
    pub fn remove_mcp_provider(&self, name: &str) -> Result<(), PromptError> {
        let mut configs = self.mcp_provider_configs();
        let before = configs.len();
        configs.retain(|c| c.name != name);
        if configs.len() != before {
            self.write_mcp_providers(&configs)?;
        }
        let _ = self.secrets.delete(name);
        self.provider_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name);
        Ok(())
    }

    /// Probe a candidate provider before saving: connect, list, and return the
    /// prompt count, or an error. Uses the supplied bearer directly (the form's
    /// value, not yet stored).
    pub async fn test_mcp_connection(
        &self,
        url: &str,
        bearer: Option<String>,
    ) -> Result<usize, PromptError> {
        let provider = McpProvider::new(
            "(test)".to_owned(),
            url.to_owned(),
            bearer,
            PROVIDER_TIMEOUT,
        );
        Ok(provider.list_result().await?.len())
    }

    fn config_path(&self) -> Result<&Path, PromptError> {
        self.config_path
            .as_deref()
            .ok_or(PromptError::NotConfigured)
    }

    /// Overwrite only the `mcp_providers:` key in `config.yaml`, preserving every
    /// other top-level key (`local_prompt_dirs` and any personal prefs). Refuses
    /// to write — rather than clobber — if the existing file isn't a YAML mapping.
    fn write_mcp_providers(&self, configs: &[McpProviderConfig]) -> Result<(), PromptError> {
        let path = self.config_path()?;
        let mut root = read_config_mapping(path)?;
        let key = serde_norway::Value::String("mcp_providers".to_owned());
        if configs.is_empty() {
            root.remove(&key);
        } else {
            let value = serde_norway::to_value(configs).map_err(|e| PromptError::ConfigWrite {
                path: path.to_owned(),
                message: e.to_string(),
            })?;
            root.insert(key, value);
        }
        switchboard_core::write_yaml(path, &serde_norway::Value::Mapping(root)).map_err(|e| {
            PromptError::ConfigWrite {
                path: path.to_owned(),
                message: e.to_string(),
            }
        })
    }
}

/// Read `config.yaml` as a generic YAML mapping for an in-place key edit. Absent,
/// empty, or null → a fresh mapping. A non-mapping or unparseable file is an
/// error: we will not clobber a config we can't safely round-trip.
fn read_config_mapping(path: &Path) -> Result<serde_norway::Mapping, PromptError> {
    use serde_norway::Value;
    if !path.exists() {
        return Ok(serde_norway::Mapping::new());
    }
    let bytes = std::fs::read(path).map_err(|e| PromptError::ConfigWrite {
        path: path.to_owned(),
        message: e.to_string(),
    })?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_norway::Mapping::new());
    }
    match serde_norway::from_slice::<Value>(&bytes) {
        Ok(Value::Mapping(mapping)) => Ok(mapping),
        Ok(Value::Null) => Ok(serde_norway::Mapping::new()),
        Ok(_) => Err(PromptError::ConfigWrite {
            path: path.to_owned(),
            message: "config.yaml is not a YAML mapping; refusing to overwrite it".to_owned(),
        }),
        Err(e) => Err(PromptError::ConfigWrite {
            path: path.to_owned(),
            message: format!(
                "config.yaml is not valid YAML ({e}); fix it before editing providers from Settings"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretStoreError;
    use std::path::Path;
    use tempfile::TempDir;

    /// A secret store whose reads always fail — for the `StoreUnavailable` path.
    struct FailingSecretStore;
    impl SecretStore for FailingSecretStore {
        fn get(&self, _: &str) -> Result<Option<String>, SecretStoreError> {
            Err(SecretStoreError::Backend("store offline".to_owned()))
        }
        fn set(&self, _: &str, _: &str) -> Result<(), SecretStoreError> {
            Err(SecretStoreError::Backend("store offline".to_owned()))
        }
        fn delete(&self, _: &str) -> Result<(), SecretStoreError> {
            Ok(())
        }
    }

    fn write(dir: &Path, file: &str, content: &str) {
        std::fs::write(dir.join(file), content).unwrap();
    }

    fn http_provider_yaml(name: &str, url: &str) -> String {
        format!(
            "mcp_providers:\n  - name: {name}\n    transport:\n      type: http\n      url: {url}\n"
        )
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

    #[tokio::test]
    async fn add_provider_preserves_local_dirs_and_unknown_keys() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        // A pre-existing config with a local dir and a non-prompt personal pref.
        std::fs::write(
            &config_path,
            "theme: dark\nlocal_prompt_dirs:\n  - /my/prompts\n",
        )
        .unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let service = PromptService::new(
            config_path.clone(),
            dir.path().join("prompts"),
            None,
            store.clone(),
        );

        service
            .add_mcp_provider("team", "https://mcp.example.com", Some("secret-tok"))
            .unwrap();

        // The MCP entry was added; the local dir and the unknown `theme` key survive.
        let raw = std::fs::read_to_string(&config_path).unwrap();
        let value: serde_norway::Value = serde_norway::from_str(&raw).unwrap();
        let map = value.as_mapping().unwrap();
        assert_eq!(map.get("theme").and_then(|v| v.as_str()), Some("dark"));
        assert!(map.contains_key("local_prompt_dirs"));
        assert!(map.contains_key("mcp_providers"));
        // The bearer went to the store, never the file.
        assert!(!raw.contains("secret-tok"));
        assert_eq!(store.get("team").unwrap().as_deref(), Some("secret-tok"));

        let providers = service.list_mcp_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name, "team");
        assert_eq!(providers[0].url, "https://mcp.example.com");
        assert!(providers[0].has_token);
        // No sync yet → Unknown.
        assert_eq!(providers[0].status, ProviderStatus::Unknown);
    }

    #[tokio::test]
    async fn add_rejects_duplicate_and_invalid_names() {
        let dir = TempDir::new().unwrap();
        let service = PromptService::new(
            dir.path().join("config.yaml"),
            dir.path().join("prompts"),
            None,
            Arc::new(InMemorySecretStore::new()),
        );
        service.add_mcp_provider("team", "https://a", None).unwrap();
        assert!(matches!(
            service.add_mcp_provider("team", "https://b", None),
            Err(PromptError::DuplicateProvider { .. })
        ));
        assert!(matches!(
            service.add_mcp_provider("local", "https://b", None),
            Err(PromptError::InvalidProviderName { .. })
        ));
        assert!(matches!(
            service.add_mcp_provider("a:b", "https://b", None),
            Err(PromptError::InvalidProviderName { .. })
        ));
    }

    #[tokio::test]
    async fn remove_provider_deletes_config_and_token_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, http_provider_yaml("team", "https://a")).unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        store.set("team", "tok").unwrap();
        let service = PromptService::new(
            config_path.clone(),
            dir.path().join("prompts"),
            None,
            store.clone(),
        );

        service.remove_mcp_provider("team").unwrap();
        assert!(service.list_mcp_providers().is_empty());
        assert_eq!(store.get("team").unwrap(), None);
        // The now-empty section drops the key rather than leaving `mcp_providers: []`.
        let raw = std::fs::read_to_string(&config_path).unwrap();
        assert!(!raw.contains("mcp_providers"));
        // Idempotent: removing again is fine.
        service.remove_mcp_provider("team").unwrap();
    }

    #[tokio::test]
    async fn write_refuses_to_clobber_unparseable_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, "just a scalar, not a mapping\n").unwrap();
        let service = PromptService::new(
            config_path,
            dir.path().join("prompts"),
            None,
            Arc::new(InMemorySecretStore::new()),
        );
        assert!(matches!(
            service.add_mcp_provider("team", "https://a", None),
            Err(PromptError::ConfigWrite { .. })
        ));
    }

    #[tokio::test]
    async fn sync_marks_unreachable_provider_errored() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            http_provider_yaml("team", "http://127.0.0.1:1/mcp"),
        )
        .unwrap();
        let service = PromptService::new(
            config_path,
            dir.path().join("prompts"),
            None,
            Arc::new(InMemorySecretStore::new()),
        );
        service.sync().await;
        let providers = service.list_mcp_providers();
        assert_eq!(providers.len(), 1);
        assert!(matches!(
            providers[0].status,
            ProviderStatus::Errored { .. }
        ));
    }

    #[tokio::test]
    async fn sync_marks_store_unavailable_when_secret_read_fails() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            http_provider_yaml("team", "http://127.0.0.1:1/mcp"),
        )
        .unwrap();
        let service = PromptService::new(
            config_path,
            dir.path().join("prompts"),
            None,
            Arc::new(FailingSecretStore),
        );
        service.sync().await;
        let providers = service.list_mcp_providers();
        assert_eq!(providers[0].status, ProviderStatus::StoreUnavailable);
        assert!(!providers[0].has_token);
    }
}
