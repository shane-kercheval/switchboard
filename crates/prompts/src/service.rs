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

use rmcp::transport::auth::{AuthClient, AuthError, AuthorizationManager, CredentialStore as _};
use serde::Serialize;

use crate::builtin::{BuiltinProvider, builtin_prompt_source};
use crate::config::{
    McpAuth, McpProviderConfig, McpSection, McpTransport, PromptConfig, is_valid_provider_name,
    resolve_local_dirs,
};
use crate::error::PromptError;
use crate::local::LocalProvider;
use crate::mcp::McpProvider;
use crate::model::{BUILTIN_PROVIDER, LOCAL_PROVIDER, Prompt};
use crate::oauth::{
    CredentialLifecycle, CredentialState, ProviderCredentialStore, canonicalize_resource_url,
    oauth_secret_key,
};
use crate::preflight::preflight;
use crate::provider::PromptProvider;
use crate::secret::{InMemorySecretStore, SecretStore};
use crate::signin::{BrowserOpener, NoBrowserOpener, SignInRequest, run_sign_in};

/// Per-provider budget for the whole connect + request round-trip during a cache
/// build — and, for OAuth providers, the client-acquisition + token-probe +
/// connect + request sequence — so one slow/cold MCP server can't stall startup.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a sign-in waits for the user to complete the browser flow before
/// tearing down the callback listener. Generous — the user may be signing in
/// to the identity provider and completing MFA.
const SIGN_IN_TIMEOUT: Duration = Duration::from_mins(5);

/// One provider's lazily-built OAuth client. `tokio::sync::OnceCell` gives
/// single-flight construction: concurrent first users (a `sync` racing a
/// `render`) share one initialization instead of building two `AuthClient`s —
/// which matters because refresh serialization lives in the client's internal
/// mutex, and two instances would each refresh against a rotating refresh
/// token and invalidate the other.
type OAuthClientCell = Arc<tokio::sync::OnceCell<AuthClient<reqwest::Client>>>;

/// The session cache, keyed by `(provider name, canonical URL)` — the URL in
/// the key makes invalidation on URL change automatic. Guarded by a *sync*
/// mutex held only for map lookups/inserts, never across I/O: holding it
/// through discovery would serialize unrelated providers and defeat `sync`'s
/// concurrent fan-out.
type OAuthClientMap = std::sync::Mutex<HashMap<(String, String), OAuthClientCell>>;

/// Per-provider credential lifecycle state (transaction lock + I/O gate — see
/// [`CredentialLifecycle`]), keyed by provider name. The transaction lock is
/// the exclusion that stops an in-flight refresh from resurrecting credentials
/// the user just removed.
///
/// Entries are **deliberately never pruned** — not even on provider removal.
/// Pruning would let a later lookup mint a *fresh* lock while an in-flight
/// operation still holds the old one, so the two would no longer exclude each
/// other and the resurrection guard would silently stop guarding. The cost is
/// one tiny entry per provider name ever seen this process (historical names,
/// not current count) — accepted.
type CredentialLockMap = std::sync::Mutex<HashMap<String, Arc<CredentialLifecycle>>>;

/// Rendered prompt text, as returned to the frontend. A struct (rather than a
/// bare string) keeps the wire shape stable as later milestones add fields.
#[derive(Debug, Clone, Serialize)]
pub struct RenderedPrompt {
    pub text: String,
}

/// A prompt's raw, **unrendered** template body, for a read-only UI preview
/// (e.g. a workflow step chip showing what a prompt says). A struct rather than a
/// bare string keeps the wire shape stable, mirroring [`RenderedPrompt`].
#[derive(Debug, Clone, Serialize)]
pub struct PromptSource {
    pub text: String,
}

/// An MCP provider as shown in Settings: its non-secret config plus whether a
/// credential is stored and the outcome of the last cache build.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct McpProviderInfo {
    pub name: String,
    pub url: String,
    /// Whether a *token* is currently stored for this provider — the pasted
    /// bearer, or (OAuth) signed-in tokens. Never registration presence: a
    /// signed-out OAuth provider must not render as credentialed.
    pub has_token: bool,
    /// The provider's auth mode, so the UI can pick the right affordance
    /// ("paste a token" vs. "sign in").
    pub auth: McpAuth,
    pub status: ProviderStatus,
}

/// Outcome of the last attempt to list a provider's prompts.
///
/// Deliberately coarse for **bearer** providers: `rmcp` collapses transport
/// failures (connection refused, HTTP 401/403) into one opaque error that can't
/// be reliably sub-classified without coupling to its internals, so a failure
/// is one `errored` bucket with the underlying message surfaced for detail —
/// rather than a fragile auth-vs-unreachable split. `store_unavailable` (the
/// keychain couldn't be read) is genuinely distinct and knowable locally; the
/// "no credential stored" nudge is carried by [`McpProviderInfo::has_token`].
///
/// **OAuth changes that calculus for `needs_auth` only**: the proactive
/// `AuthClient::get_access_token()` probe returns a *typed*
/// `AuthorizationRequired` **before any request to the MCP server** — a local
/// determination, and the only permitted source of this status. It is still
/// never inferred from a transport error: a token that is inside its expiry
/// window but revoked server-side fails mid-call as generic `errored` (the
/// next sync's probe reports `needs_auth` once its refresh fails) — a recorded
/// limitation, because sub-classifying mid-flight failures would mean reaching
/// into rmcp's type-erased transport error, exactly the coupling argued
/// against above. `store_unavailable` likewise stays a *direct* secret-store
/// probe, never a classification of rmcp's overloaded `InternalError`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProviderStatus {
    /// The last sync listed prompts successfully.
    Ok { prompt_count: usize },
    /// The last sync failed; `message` is the redacted error (never a token).
    Errored { message: String },
    /// The secret store couldn't be read (e.g. keychain locked/absent).
    StoreUnavailable,
    /// OAuth provider with no usable credentials (never signed in, signed out,
    /// or refresh failed) — the fix is signing in, not debugging a server.
    NeedsAuth,
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
    /// Serializes the config-file read-modify-write in `add`/`remove` so two
    /// concurrent edits can't both read the old file and clobber each other's
    /// change. Synchronous (the mutators are sync fns), distinct from `sync_lock`.
    config_write_lock: Arc<std::sync::Mutex<()>>,
    /// Whether the app-owned read-only built-in library participates. True for a
    /// real service, false for [`disabled`](Self::disabled) (which must stay
    /// inert). This is *not* the user's "show built-ins" toggle — that is an
    /// app-layer visibility filter; the built-ins are always listed and
    /// resolvable here so a workflow wired to one never breaks.
    include_builtins: bool,
    /// Per-provider OAuth clients (see [`OAuthClientMap`]). **Every** OAuth
    /// caller — `sync`, `render`, the saved-provider probe — must go through
    /// [`oauth_client`](Self::oauth_client); a second `AuthClient` for one
    /// provider would defeat the refresh serialization its internal mutex
    /// provides. Invalidated on provider removal (and by sign-out/sign-in when
    /// those land); a URL change invalidates automatically via the cache key.
    oauth_clients: Arc<OAuthClientMap>,
    /// See [`CredentialLockMap`].
    credential_locks: Arc<CredentialLockMap>,
    /// Shared bounded HTTP client for preflight fetches and (injected via
    /// `with_client`) rmcp's discovery/token requests — replacing rmcp's own
    /// 30s-per-request default so those round trips respect the provider
    /// budget. Built lazily: constructing it can fail (TLS backend), and the
    /// bearer-only path should never pay for it.
    http: Arc<tokio::sync::OnceCell<reqwest::Client>>,
    /// The per-provider budget. `PROVIDER_TIMEOUT` in production; tests shrink
    /// it via [`with_provider_timeout`](Self::with_provider_timeout) so
    /// timeout-path assertions don't wait out ten real seconds.
    provider_timeout: Duration,
    /// Opens the browser for OAuth sign-in. Defaults to a fail-fast stub;
    /// production injects the OS opener via
    /// [`with_browser_opener`](Self::with_browser_opener).
    browser: Arc<dyn BrowserOpener>,
    /// How long a sign-in waits for the browser callback. `SIGN_IN_TIMEOUT` in
    /// production; tests shrink it via
    /// [`with_sign_in_timeout`](Self::with_sign_in_timeout).
    sign_in_timeout: Duration,
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
            config_write_lock: Arc::new(std::sync::Mutex::new(())),
            include_builtins: true,
            oauth_clients: Arc::new(std::sync::Mutex::new(HashMap::new())),
            credential_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            http: Arc::new(tokio::sync::OnceCell::new()),
            provider_timeout: PROVIDER_TIMEOUT,
            browser: Arc::new(NoBrowserOpener),
            sign_in_timeout: SIGN_IN_TIMEOUT,
        }
    }

    /// Inject the browser opener the OAuth sign-in flow uses. The app supplies
    /// its validated OS opener; without this, sign-in fails fast with a
    /// readable "no browser opener" error (tests, the disabled service).
    #[must_use]
    pub fn with_browser_opener(mut self, opener: Arc<dyn BrowserOpener>) -> Self {
        self.browser = opener;
        self
    }

    /// Test hook: shrink the sign-in callback wait so abandoned-flow tests
    /// don't wait out the production five minutes. Hidden — not product API.
    #[doc(hidden)]
    #[must_use]
    pub fn with_sign_in_timeout(mut self, timeout: Duration) -> Self {
        self.sign_in_timeout = timeout;
        self
    }

    /// Test hook: shrink the per-provider budget so timeout-path tests don't
    /// wait out the production ten seconds. **Construction-time only**: it
    /// resets this clone's shared HTTP client and OAuth-client cells (both
    /// bake in the budget at first build), so it must be called before any
    /// OAuth operation runs — and the re-budgeted clone stops sharing those
    /// caches with its siblings. Hidden — not product API.
    #[doc(hidden)]
    #[must_use]
    pub fn with_provider_timeout(mut self, timeout: Duration) -> Self {
        self.provider_timeout = timeout;
        self.http = Arc::new(tokio::sync::OnceCell::new());
        self.oauth_clients = Arc::new(std::sync::Mutex::new(HashMap::new()));
        self
    }

    /// The per-provider credential lifecycle state (see [`CredentialLockMap`]).
    fn credential_lifecycle(&self, name: &str) -> Arc<CredentialLifecycle> {
        self.credential_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(name.to_owned())
            .or_default()
            .clone()
    }

    /// Build this provider's credential-store view (envelope + lifecycle).
    fn credential_store(&self, name: &str, canonical_url: &str) -> ProviderCredentialStore {
        ProviderCredentialStore::new(
            self.secrets.clone(),
            name,
            canonical_url,
            self.credential_lifecycle(name),
        )
    }

    /// An inert service for contexts with no resolved prompt store. Listing is
    /// empty (no local, MCP, or built-in prompts); rendering fails as
    /// provider-not-found.
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
            config_write_lock: Arc::new(std::sync::Mutex::new(())),
            include_builtins: false,
            oauth_clients: Arc::new(std::sync::Mutex::new(HashMap::new())),
            credential_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            http: Arc::new(tokio::sync::OnceCell::new()),
            provider_timeout: PROVIDER_TIMEOUT,
            browser: Arc::new(NoBrowserOpener),
            sign_in_timeout: SIGN_IN_TIMEOUT,
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

    /// One prompt by `provider:name`, or `None` if absent. Synchronous and
    /// offline. The **freshness contract**: `builtin` and `local` prompts are
    /// always resolvable without a sync (compiled-in / a filesystem scan), so they
    /// are resolved **directly** here — a cold cache never reports them missing.
    /// MCP prompts live only in the cache, so a not-yet-synced one returns `None`;
    /// callers treat that as "unresolved" (distinct from a hard error) and
    /// re-check after a sync.
    #[must_use]
    pub fn get(&self, provider: &str, name: &str) -> Option<Prompt> {
        let direct = match provider {
            BUILTIN_PROVIDER if self.include_builtins => BuiltinProvider::list_sync(),
            LOCAL_PROVIDER => self
                .local_provider()
                .map(|local| local.list_sync())
                .unwrap_or_default(),
            _ => {
                return self
                    .cache
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .find(|p| p.provider == provider && p.name == name)
                    .cloned();
            }
        };
        direct.into_iter().find(|p| p.name == name)
    }

    /// The raw, **unrendered** template body of a `builtin` or `local` prompt
    /// (frontmatter stripped, `MiniJinja` placeholders intact), for a **read-only
    /// preview** in the UI. Synchronous and offline.
    ///
    /// Returns `None` for an MCP provider: the MCP protocol exposes only rendered
    /// output (`prompts/get`), never the un-rendered template, so a server-side
    /// prompt has no source to show — the UI falls back to the cached metadata
    /// (description + arguments). Also `None` for a prompt that doesn't resolve.
    ///
    /// Deliberately exposes the template, unlike [`render`](Self::render): the
    /// body is shown to the *user* as a preview, never sent to an agent (agents
    /// only ever receive rendered text). Built-ins resolve regardless of the
    /// app's "show built-ins" toggle, matching [`get`](Self::get)/`render`.
    #[must_use]
    pub fn source(&self, provider: &str, name: &str) -> Option<PromptSource> {
        let text = match provider {
            BUILTIN_PROVIDER if self.include_builtins => builtin_prompt_source(name),
            LOCAL_PROVIDER => self.local_provider().and_then(|local| local.source(name)),
            _ => None,
        }?;
        Some(PromptSource { text })
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
        // Built-ins are baked-in and instant, so they publish in the same fast
        // first pass as local prompts — never held behind a slow MCP server.
        if self.include_builtins {
            prompts.extend(BuiltinProvider::new().list().await);
        }
        self.publish(prompts.clone());

        // Each provider's whole pipeline — including, for OAuth, the credential
        // probe and (cold) client construction — runs inside its own concurrent
        // branch. Hoisting any of it above the join would let one slow provider
        // delay every sibling's start, breaking the isolation guarantee below.
        let configs = self.mcp_provider_configs();
        let results =
            futures::future::join_all(configs.iter().map(|config| self.query_provider(config)))
                .await;

        let mut statuses: HashMap<String, ProviderStatus> = HashMap::new();
        for (name, status, provider_prompts) in results {
            prompts.extend(provider_prompts);
            statuses.insert(name, status);
        }
        *self
            .provider_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = statuses;
        self.publish(prompts);
    }

    /// List one provider's prompts per its auth mode, mapping the outcome to a
    /// status. Infallible by shape — every failure becomes a status — so a
    /// broken provider degrades to "no prompts + a status" and never affects a
    /// sibling in the same `sync`.
    async fn query_provider(
        &self,
        config: &McpProviderConfig,
    ) -> (String, ProviderStatus, Vec<Prompt>) {
        let McpTransport::Http { url } = &config.transport;
        let name = config.name.clone();
        match &config.auth {
            McpAuth::Bearer => {
                // One budget over credential resolution + list: without it, a
                // wedged keychain read would keep this future pending forever,
                // `join_all` (and so `sync()`, and so the `sync_lock`) with it.
                let listed = tokio::time::timeout(self.provider_timeout, async {
                    let (bearer, store_unavailable) = self.resolve_bearer(&name).await;
                    let provider =
                        McpProvider::new(name.clone(), url.clone(), bearer, self.provider_timeout);
                    (store_unavailable, provider.list_uncapped().await)
                })
                .await;
                match listed {
                    Err(_elapsed) => (
                        name,
                        ProviderStatus::Errored {
                            message: format!(
                                "timed out after {}s",
                                self.provider_timeout.as_secs()
                            ),
                        },
                        vec![],
                    ),
                    Ok((_, Ok(provider_prompts))) => {
                        let status = ProviderStatus::Ok {
                            prompt_count: provider_prompts.len(),
                        };
                        (name, status, provider_prompts)
                    }
                    // A store-read failure is the more actionable root cause
                    // when the list also failed; otherwise the provider's own.
                    Ok((true, Err(_))) => (name, ProviderStatus::StoreUnavailable, vec![]),
                    Ok((false, Err(e))) => (
                        name,
                        ProviderStatus::Errored {
                            message: e.to_string(),
                        },
                        vec![],
                    ),
                }
            }
            McpAuth::Oauth { scopes } => {
                self.query_oauth_provider(name, url, scopes.as_deref())
                    .await
            }
        }
    }

    /// The OAuth listing pipeline, the whole sequence under one provider
    /// budget: local credential check (short-circuit) → (cached or freshly
    /// preflighted) client → proactive token probe → list.
    async fn query_oauth_provider(
        &self,
        name: String,
        url: &str,
        scopes_override: Option<&[String]>,
    ) -> (String, ProviderStatus, Vec<Prompt>) {
        /// The pipeline's outcome before status mapping — statuses that aren't
        /// listing errors need their own arm.
        enum Outcome {
            StoreUnavailable,
            NeedsAuth,
            Listed(Vec<Prompt>),
        }

        let listed = tokio::time::timeout(self.provider_timeout, async {
            let canonical =
                canonicalize_resource_url(url).map_err(|e| PromptError::OAuthValidation {
                    provider: name.clone(),
                    message: format!("invalid provider URL {url:?}: {e}"),
                })?;
            // Local short-circuit: a signed-out provider is knowable from the
            // keychain alone — report `NeedsAuth` without spending the budget
            // on discovery, and *correctly* even when offline (a network-first
            // path would misreport "server error" when the fix is signing in).
            // `StoreUnavailable` comes from this direct check alone — never
            // from classifying rmcp's `AuthError::InternalError`, which it
            // overloads for non-store failures (see the `ProviderStatus` docs).
            match self
                .credential_store(&name, &canonical)
                .credential_state()
                .await
            {
                CredentialState::Unavailable(reason) => {
                    tracing::warn!(provider = %name, %reason, "secret store unavailable for OAuth provider");
                    return Ok(Outcome::StoreUnavailable);
                }
                CredentialState::SignedOut => return Ok(Outcome::NeedsAuth),
                CredentialState::SignedIn => {}
            }
            let client = self
                .oauth_client(&name, &canonical, scopes_override)
                .await?;
            // Proactive probe: refreshes a near-expiry token once, ahead of the
            // listing, and turns "no usable credentials" into a typed outcome
            // before any request reaches the MCP server.
            match client.get_access_token().await {
                Ok(_) => {}
                Err(AuthError::AuthorizationRequired) => return Ok(Outcome::NeedsAuth),
                Err(e) => {
                    return Err(PromptError::McpConnect {
                        provider: name.clone(),
                        message: e.to_string(),
                    });
                }
            }
            let provider =
                McpProvider::new_oauth(name.clone(), canonical, client, self.provider_timeout);
            provider.list_uncapped().await.map(Outcome::Listed)
        })
        .await;

        match listed {
            Err(_elapsed) => (
                name,
                ProviderStatus::Errored {
                    message: format!("timed out after {}s", self.provider_timeout.as_secs()),
                },
                vec![],
            ),
            Ok(Err(e)) => (
                name,
                ProviderStatus::Errored {
                    message: e.to_string(),
                },
                vec![],
            ),
            Ok(Ok(Outcome::StoreUnavailable)) => (name, ProviderStatus::StoreUnavailable, vec![]),
            Ok(Ok(Outcome::NeedsAuth)) => (name, ProviderStatus::NeedsAuth, vec![]),
            Ok(Ok(Outcome::Listed(provider_prompts))) => {
                let status = ProviderStatus::Ok {
                    prompt_count: provider_prompts.len(),
                };
                (name, status, provider_prompts)
            }
        }
    }

    /// The per-provider `AuthClient`, built on first use. Single-flight via the
    /// entry's `OnceCell`; the map mutex is held only for the lookup/insert,
    /// never across the build's I/O. A failed build leaves the cell empty, so
    /// the next caller retries.
    ///
    /// The whole lookup + build runs under the provider's `client_gate`, so a
    /// sign-in's token commit can exclude a build that is in flight when the
    /// commit runs (see [`CredentialLifecycle`]). This adds no contention of
    /// its own: concurrent builders of one provider already serialize on the
    /// cell's single-flight init.
    async fn oauth_client(
        &self,
        name: &str,
        canonical_url: &str,
        scopes_override: Option<&[String]>,
    ) -> Result<AuthClient<reqwest::Client>, PromptError> {
        let lifecycle = self.credential_lifecycle(name);
        let _construction_gate = lifecycle.client_gate.lock().await;
        let cell = {
            let mut map = self
                .oauth_clients
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.entry((name.to_owned(), canonical_url.to_owned()))
                .or_default()
                .clone()
        };
        let client = cell
            .get_or_try_init(|| self.build_oauth_client(name, canonical_url, scopes_override))
            .await?;
        Ok(client.clone())
    }

    /// Preflight (M2's validation boundary), then assemble rmcp's client around
    /// the validated metadata and this provider's credential envelope. The
    /// `set_metadata` install is what stops rmcp re-discovering an unvalidated
    /// copy; `initialize_from_store` then wires the stored client id without a
    /// network round trip.
    async fn build_oauth_client(
        &self,
        name: &str,
        canonical_url: &str,
        scopes_override: Option<&[String]>,
    ) -> Result<AuthClient<reqwest::Client>, PromptError> {
        let http = self.http_client(name).await?;
        let outcome = preflight(http, name, canonical_url, scopes_override).await?;
        // The resolved scopes are consumed by the sign-in flow; this client
        // only presents stored credentials, so they are not needed here.
        let map_err = |e: AuthError| PromptError::McpConnect {
            provider: name.to_owned(),
            message: e.to_string(),
        };
        let mut manager = AuthorizationManager::new(canonical_url)
            .await
            .map_err(map_err)?;
        manager.with_client(http.clone()).map_err(map_err)?;
        manager.set_credential_store(self.credential_store(name, canonical_url));
        manager.set_metadata(outcome.metadata);
        manager.initialize_from_store().await.map_err(map_err)?;
        Ok(AuthClient::new(http.clone(), manager))
    }

    /// The shared bounded HTTP client (see the field docs).
    async fn http_client(&self, provider: &str) -> Result<&reqwest::Client, PromptError> {
        let timeout = self.provider_timeout;
        self.http
            .get_or_try_init(|| async move {
                reqwest::Client::builder()
                    .timeout(timeout)
                    // No redirects, anywhere this client is used: during
                    // discovery a 3xx could launder a validated URL into an
                    // internal or http destination, and on the token/refresh
                    // and OAuth MCP-transport paths, silently following
                    // redirects with credential-bearing requests is its own
                    // hazard (the OAuth BCP position). A legitimately
                    // redirecting server surfaces a diagnosable refusal
                    // instead (see `preflight::fetch_json`).
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| PromptError::McpConnect {
                        provider: provider.to_owned(),
                        message: format!("could not build HTTP client: {e}"),
                    })
            })
            .await
    }

    /// Drop any cached OAuth client for `name` (all URLs). Called on provider
    /// removal, on sign-out, and by sign-in when the registration changed.
    ///
    /// Safe against a build racing this call: `oauth_client` clones the
    /// entry's `Arc<OnceCell>` before releasing the map lock, so the `retain`
    /// here unlinks the cell from the map *before* a racing builder populates
    /// it — the populated client is visible only to callers already holding
    /// the cell and never re-enters the cache. It is also inert: it re-reads
    /// the credential store on every call, and its saves are refused after a
    /// sign-out (the sign-out epoch) or a re-registration (the client-id
    /// check). No generation counter on the cache is needed.
    ///
    /// Keeping a cached client across an unchanged-registration sign-in is
    /// safe only because **no client is ever cached while signed out**: the
    /// `CredentialState::SignedOut` short-circuits in `query_oauth_provider`
    /// and `render_oauth` run before `oauth_client()`, and rmcp's
    /// `initialize_from_store` configures the client only when a token
    /// response exists. A caller that built a client without that
    /// short-circuit would cache an unconfigured manager whose first refresh
    /// fails as a non-recoverable internal error until restart.
    fn invalidate_oauth_clients(&self, name: &str) {
        self.oauth_clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(cached_name, _), _| cached_name != name);
    }

    /// The provider's already-built cached client, if any — without
    /// constructing one (sign-out must not run a preflight just to lock a
    /// client that doesn't exist).
    fn cached_oauth_client(
        &self,
        name: &str,
        canonical_url: &str,
    ) -> Option<AuthClient<reqwest::Client>> {
        self.oauth_clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(name.to_owned(), canonical_url.to_owned()))
            .and_then(|cell| cell.get().cloned())
    }

    /// The URL and scopes override of provider `name`, which must be an OAuth
    /// provider — the shared precondition of sign-in and sign-out.
    fn oauth_provider_config(
        &self,
        name: &str,
    ) -> Result<(String, Option<Vec<String>>), PromptError> {
        let config = self
            .mcp_provider_configs()
            .into_iter()
            .find(|c| c.name == name)
            .ok_or_else(|| PromptError::ProviderNotFound {
                provider: name.to_owned(),
            })?;
        let McpTransport::Http { url } = config.transport;
        match config.auth {
            McpAuth::Oauth { scopes } => Ok((url, scopes)),
            McpAuth::Bearer => Err(PromptError::OAuthFlow {
                provider: name.to_owned(),
                message: "this provider uses a pasted bearer token, not browser sign-in".to_owned(),
            }),
        }
    }

    /// Browser sign-in for an OAuth provider: preflight, register (or reuse)
    /// the client registration, open the browser via the injected opener,
    /// await the loopback callback, exchange the code, and durably commit the
    /// tokens. Serialized per provider against sign-out (and other sign-ins)
    /// by the flow gate — which refuses a busy provider rather than queueing
    /// (a queued second sign-in would open a surprise browser tab minutes
    /// later; a queued sign-out would wait out the flow and then destroy the
    /// tokens the user just obtained). The gate is deliberately **not** the
    /// credential transaction lock — the flow's own registration persistence
    /// and token save take that lock internally (see [`CredentialLifecycle`]).
    pub async fn sign_in_mcp_provider(&self, name: &str) -> Result<(), PromptError> {
        let (url, scopes_override) = self.oauth_provider_config(name)?;
        let canonical =
            canonicalize_resource_url(&url).map_err(|e| PromptError::OAuthValidation {
                provider: name.to_owned(),
                message: format!("invalid provider URL {url:?}: {e}"),
            })?;
        let lifecycle = self.credential_lifecycle(name);
        let Ok(_flow) = lifecycle.flow_gate.try_lock() else {
            return Err(Self::flow_in_progress(name));
        };
        let http = self.http_client(name).await?;
        let store = self.credential_store(name, &canonical);
        let resample_cached_client = || self.cached_oauth_client(name, &canonical);
        let (registration_changed, result) = run_sign_in(SignInRequest {
            http,
            opener: self.browser.as_ref(),
            store: &store,
            lifecycle: &lifecycle,
            resample_cached_client: &resample_cached_client,
            provider: name,
            canonical_url: &canonical,
            scopes_override: scopes_override.as_deref(),
            exchange_timeout: self.provider_timeout,
            callback_timeout: self.sign_in_timeout,
        })
        .await;
        // Invalidate only when the flow re-registered (reported even when a
        // later step failed — the new registration is already persisted): a
        // cached client configured with the previous client id would refresh
        // against the wrong registration from then on. When the registration
        // is unchanged the cached client deliberately stays: it re-reads the
        // store on every call so it picks up the new tokens, while retiring
        // it would let a later rebuild coexist with its still-running
        // operations — two live managers refreshing one rotating
        // refresh-token family.
        if registration_changed {
            self.invalidate_oauth_clients(name);
        }
        result
    }

    /// The refusal both flows return when the provider's flow gate is held.
    fn flow_in_progress(name: &str) -> PromptError {
        PromptError::OAuthFlow {
            provider: name.to_owned(),
            message: "a sign-in or sign-out for this provider is already in progress".to_owned(),
        }
    }

    /// Sign out an OAuth provider: clear its stored tokens (the registration
    /// survives for the next sign-in) and drop its cached client.
    ///
    /// Serializes against in-flight token use by holding the cached client's
    /// auth-manager mutex across the clear *and* the cache invalidation: an
    /// in-flight refresh either completes first (its just-persisted tokens are
    /// then cleared) or starts after and finds nothing to refresh —
    /// `get_access_token` re-loads from the credential store on every call, so
    /// there is no in-memory copy a late refresh could resurrect from. Without
    /// this, a refresh racing sign-out could re-persist tokens *after* the
    /// user was told they were removed.
    pub async fn sign_out_mcp_provider(&self, name: &str) -> Result<(), PromptError> {
        let (url, _scopes) = self.oauth_provider_config(name)?;
        let canonical =
            canonicalize_resource_url(&url).map_err(|e| PromptError::OAuthValidation {
                provider: name.to_owned(),
                message: format!("invalid provider URL {url:?}: {e}"),
            })?;
        let lifecycle = self.credential_lifecycle(name);
        let Ok(_flow) = lifecycle.flow_gate.try_lock() else {
            return Err(Self::flow_in_progress(name));
        };
        let cached = self.cached_oauth_client(name, &canonical);
        let _refresh_exclusion = match &cached {
            Some(client) => Some(client.auth_manager.lock().await),
            None => None,
        };
        // Retire every credential store created before this instant: a client
        // the mutex above cannot see (mid-construction, or orphaned by an
        // earlier invalidation) may already have loaded the tokens and be
        // refreshing them; bumping the epoch before the clear makes its
        // eventual save refuse instead of resurrecting credentials (see
        // `CredentialLifecycle::sign_out_epoch`).
        lifecycle
            .sign_out_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cleared = self.credential_store(name, &canonical).clear().await;
        self.invalidate_oauth_clients(name);
        cleared.map_err(|e| {
            PromptError::Secret(crate::secret::SecretStoreError::Backend(format!(
                "could not clear stored credentials: {e}"
            )))
        })
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
        } else if provider == BUILTIN_PROVIDER {
            // Always resolvable when built-ins are enabled, regardless of the
            // app's "show built-ins" toggle — that toggle hides them from the
            // pickers, it does not unwire a workflow that already points at one.
            if !self.include_builtins {
                return Err(PromptError::ProviderNotFound {
                    provider: provider.to_owned(),
                });
            }
            BuiltinProvider::new().render(name, args).await?
        } else {
            let config = self
                .mcp_provider_configs()
                .into_iter()
                .find(|c| c.name == provider)
                .ok_or_else(|| PromptError::ProviderNotFound {
                    provider: provider.to_owned(),
                })?;
            let McpTransport::Http { url } = &config.transport;
            match &config.auth {
                McpAuth::Bearer => {
                    // One budget over credential resolution + render, mirroring
                    // the sync arm — a wedged keychain read must not hang the
                    // render forever.
                    let provider_name = config.name.clone();
                    tokio::time::timeout(self.provider_timeout, async {
                        let (bearer, _) = self.resolve_bearer(&provider_name).await;
                        McpProvider::new(
                            provider_name.clone(),
                            url.clone(),
                            bearer,
                            self.provider_timeout,
                        )
                        .render_uncapped(name, args)
                        .await
                    })
                    .await
                    .map_err(|_| PromptError::McpRequest {
                        provider: config.name.clone(),
                        name: name.to_owned(),
                        message: format!("timed out after {}s", self.provider_timeout.as_secs()),
                    })??
                }
                McpAuth::Oauth { scopes } => tokio::time::timeout(
                    self.provider_timeout,
                    self.render_oauth(&config.name, url, scopes.as_deref(), name, args),
                )
                .await
                .map_err(|_| PromptError::McpRequest {
                    provider: config.name.clone(),
                    name: name.to_owned(),
                    message: format!("timed out after {}s", self.provider_timeout.as_secs()),
                })??,
            }
        };
        Ok(RenderedPrompt { text })
    }

    /// The OAuth render pipeline, run by `render` under one provider budget.
    /// Render goes through the same cached client as sync — `render`'s "builds
    /// fresh" framing is about the *prompt* cache, not license to construct a
    /// second `AuthClient` (which would defeat refresh serialization). One
    /// budget covers the local credential check + acquisition (a cold cache
    /// runs the full preflight) + token probe + render, mirroring
    /// `query_oauth_provider` — two stacked timers would let a cold render take
    /// double the budget. The local check turns "signed out" into the typed
    /// `McpNeedsAuth` with zero network traffic (correct even offline); the
    /// later probe still catches a failed refresh.
    async fn render_oauth(
        &self,
        provider_name: &str,
        url: &str,
        scopes_override: Option<&[String]>,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<String, PromptError> {
        let canonical =
            canonicalize_resource_url(url).map_err(|e| PromptError::OAuthValidation {
                provider: provider_name.to_owned(),
                message: format!("invalid provider URL {url:?}: {e}"),
            })?;
        match self
            .credential_store(provider_name, &canonical)
            .credential_state()
            .await
        {
            CredentialState::Unavailable(reason) => {
                return Err(PromptError::Secret(
                    crate::secret::SecretStoreError::Backend(reason),
                ));
            }
            CredentialState::SignedOut => {
                return Err(PromptError::McpNeedsAuth {
                    provider: provider_name.to_owned(),
                });
            }
            CredentialState::SignedIn => {}
        }
        let client = self
            .oauth_client(provider_name, &canonical, scopes_override)
            .await?;
        match client.get_access_token().await {
            Ok(_) => {}
            Err(AuthError::AuthorizationRequired) => {
                return Err(PromptError::McpNeedsAuth {
                    provider: provider_name.to_owned(),
                });
            }
            Err(e) => {
                return Err(PromptError::McpConnect {
                    provider: provider_name.to_owned(),
                    message: e.to_string(),
                });
            }
        }
        McpProvider::new_oauth(
            provider_name.to_owned(),
            canonical,
            client,
            self.provider_timeout,
        )
        .render_uncapped(name, args)
        .await
    }

    /// Resolve a provider's bearer from the secret store, returning the bearer and
    /// whether the store read **failed** (vs. simply having no credential). Both
    /// degrade to unauthenticated, but the failure flag lets the caller record a
    /// distinct `StoreUnavailable` status.
    ///
    /// Async: the read runs on the blocking pool. The provider fan-out polls
    /// every provider inside one task, so an inline blocked keychain call here
    /// would freeze every sibling's progress, not just this provider's. The
    /// per-provider I/O gate is owned by the closure (same construction as the
    /// credential bridge) so repeated timed-out reads against a wedged
    /// keychain wait on the gate instead of parking ever more threads.
    async fn resolve_bearer(&self, provider: &str) -> (Option<String>, bool) {
        let permit = self
            .credential_lifecycle(provider)
            .io_gate
            .clone()
            .lock_owned()
            .await;
        let secrets = self.secrets.clone();
        let key = provider.to_owned();
        let read = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            secrets.get(&key)
        })
        .await;
        match read {
            Ok(Ok(bearer)) => (bearer, false),
            Ok(Err(e)) => {
                tracing::warn!(provider = %provider, error = %e, "could not read secret store; treating provider as unauthenticated");
                (None, true)
            }
            Err(e) => {
                tracing::warn!(provider = %provider, error = %e, "secret store read task failed; treating provider as unauthenticated");
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
    /// token is stored. Status comes from the most recent [`sync`](Self::sync);
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
                let has_token = match &config.auth {
                    McpAuth::Bearer => matches!(self.secrets.get(&config.name), Ok(Some(_))),
                    McpAuth::Oauth { .. } => {
                        canonicalize_resource_url(url).is_ok_and(|canonical| {
                            self.credential_store(&config.name, &canonical)
                                .tokens_present()
                        })
                    }
                };
                let status = statuses
                    .get(&config.name)
                    .cloned()
                    .unwrap_or(ProviderStatus::Unknown);
                McpProviderInfo {
                    name: config.name.clone(),
                    url: url.clone(),
                    has_token,
                    auth: config.auth.clone(),
                    status,
                }
            })
            .collect()
    }

    /// Add a generic HTTP MCP provider: validate the name, write its non-secret
    /// config entry (preserving every other config key), and — for a bearer
    /// provider — store its pasted token in the secret store. An OAuth
    /// provider is added credential-less; its honest next step is
    /// [`sign_in_mcp_provider`](Self::sign_in_mcp_provider). Does **not**
    /// rebuild the cache — the caller triggers a background sync so a slow
    /// server can't block the command.
    pub fn add_mcp_provider(
        &self,
        name: &str,
        url: &str,
        auth: McpAuth,
        bearer: Option<&str>,
    ) -> Result<(), PromptError> {
        if !is_valid_provider_name(name) {
            return Err(PromptError::InvalidProviderName {
                name: name.to_owned(),
            });
        }
        if bearer.is_some() && !matches!(auth, McpAuth::Bearer) {
            return Err(PromptError::OAuthFlow {
                provider: name.to_owned(),
                message: "a pasted bearer token does not apply to an OAuth provider".to_owned(),
            });
        }
        let _guard = self
            .config_write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            auth,
        });
        // Store the secret *before* writing the config entry. A stored secret with
        // no config entry is benign (never read, overwritten on retry); a config
        // entry with no secret is a visible, retry-blocking broken provider. If the
        // config write then fails, roll the secret back so a failed add is a no-op.
        if let Some(bearer) = bearer {
            self.secrets.set(name, bearer)?;
        }
        if let Err(e) = self.write_mcp_providers(&configs) {
            if bearer.is_some() {
                let _ = self.secrets.delete(name);
            }
            return Err(e);
        }
        Ok(())
    }

    /// Remove a generic MCP provider: delete its stored credentials (both the
    /// bearer key and the OAuth envelope), drop its config entry (preserving
    /// others), and clear its status. Idempotent — removing an unconfigured name
    /// is not an error. Deletes the secrets *first* and surfaces a deletion
    /// failure rather than swallowing it: removing the config while a credential
    /// lingers would report the server gone while its token (or refresh token)
    /// remains in the keychain. Both deletes are attempted unconditionally — a
    /// failure on one must not strand the other credential — and the first
    /// failure is the one surfaced.
    pub fn remove_mcp_provider(&self, name: &str) -> Result<(), PromptError> {
        let _guard = self
            .config_write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Refuse while a sign-in/sign-out flow is live, and hold the gate
        // through the deletes: the transaction lock alone cannot protect
        // against a flow's `persist_registration`, which *recreates* a
        // missing envelope — a removal landing between a sign-in's dynamic
        // registration and its persistence would otherwise be resurrected
        // (and the flow would later store tokens for a provider the user
        // deleted). Never held for bearer providers, so this is a no-op for
        // them; a `try_lock` on a sync path is fine (no await).
        let lifecycle = self.credential_lifecycle(name);
        let Ok(_flow) = lifecycle.flow_gate.try_lock() else {
            return Err(Self::flow_in_progress(name));
        };
        let mut configs = self.mcp_provider_configs();
        let before = configs.len();
        configs.retain(|c| c.name != name);
        self.invalidate_oauth_clients(name);
        // The deletes run under the provider's credential transaction lock: an
        // in-flight refresh on a still-live AuthClient clone either finishes
        // its whole read-modify-write first (and everything is then deleted),
        // or waits and finds no envelope (its save errors). Without this lock
        // the interleave read → delete → write would resurrect credentials
        // under a provider the user removed — cache invalidation above cannot
        // prevent that, because live clones outlive the cache entry. The
        // lifecycle entry is deliberately NOT removed from the map afterwards —
        // see `CredentialLockMap` for why pruning would reopen this race.
        let (bearer_delete, oauth_delete) = {
            let _guard = lifecycle
                .txn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                self.secrets.delete(name),
                self.secrets.delete(&oauth_secret_key(name)),
            )
        };
        bearer_delete.and(oauth_delete)?;
        if configs.len() != before {
            self.write_mcp_providers(&configs)?;
        }
        self.provider_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name);
        Ok(())
    }

    /// Probe a candidate provider before saving: connect, list, and return the
    /// prompt count, or an error. Uses the supplied bearer directly (the form's
    /// value, not yet stored). **Bearer-only by design**: an OAuth provider
    /// that hasn't been saved has no registration or credentials to probe with
    /// — the honest sequence there is add, sign in, then
    /// [`test_saved_mcp_provider`](Self::test_saved_mcp_provider).
    pub async fn test_mcp_connection(
        &self,
        url: &str,
        bearer: Option<String>,
    ) -> Result<usize, PromptError> {
        let provider = McpProvider::new(
            "(test)".to_owned(),
            url.to_owned(),
            bearer,
            self.provider_timeout,
        );
        Ok(provider.list_result().await?.len())
    }

    /// Probe a **saved** provider by name with its stored credentials — the
    /// row-level Test action. Runs the same per-mode pipeline as `sync`
    /// (OAuth goes through the cached client) and returns the outcome in the
    /// status vocabulary the rows already render — `Ok { prompt_count }`,
    /// `NeedsAuth`, `Errored`, `StoreUnavailable` — without touching the
    /// recorded per-provider status (a probe is a peek, not a build). `Err` is
    /// reserved for "no such provider".
    pub async fn test_saved_mcp_provider(&self, name: &str) -> Result<ProviderStatus, PromptError> {
        let config = self
            .mcp_provider_configs()
            .into_iter()
            .find(|c| c.name == name)
            .ok_or_else(|| PromptError::ProviderNotFound {
                provider: name.to_owned(),
            })?;
        let (_, status, _) = self.query_provider(&config).await;
        Ok(status)
    }

    fn config_path(&self) -> Result<&Path, PromptError> {
        self.config_path
            .as_deref()
            .ok_or(PromptError::NotConfigured)
    }

    /// Overwrite only the `mcp_providers:` key in `config.yaml`, preserving every
    /// other top-level key (`local_prompt_dirs` and any personal prefs). Refuses
    /// to write — rather than clobber — if the existing file isn't a YAML mapping.
    ///
    /// This is the user-global config (the OS config dir, not the git-tracked
    /// per-directory `.switchboard/config.yaml`). Because the round-trip goes
    /// through `serde`, hand-added YAML comments in this file are **not**
    /// preserved when a server is added or removed from Settings.
    fn write_mcp_providers(&self, configs: &[McpProviderConfig]) -> Result<(), PromptError> {
        let path = self.config_path()?;
        let key = serde_norway::Value::String("mcp_providers".to_owned());
        // Serialize before the edit so the closure stays infallible (and the
        // shared config lock inside `edit_yaml_mapping` is held only across the
        // read-modify-write). `edit_yaml_mapping` preserves every other top-level
        // key (`local_prompt_dirs`, personal prefs) and serializes against the
        // preferences writer, so the two subsystems can't clobber the shared file.
        let value = if configs.is_empty() {
            None
        } else {
            Some(
                serde_norway::to_value(configs).map_err(|e| PromptError::ConfigWrite {
                    path: path.to_owned(),
                    message: e.to_string(),
                })?,
            )
        };
        switchboard_core::edit_yaml_mapping(path, move |root| match value {
            Some(value) => {
                root.insert(key, value);
            }
            None => {
                root.remove(&key);
            }
        })
        .map_err(|e| PromptError::ConfigWrite {
            path: path.to_owned(),
            message: e.to_string(),
        })
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

    /// A secret store that holds a value but whose `delete` always fails — for
    /// the remove-path "credential deletion failed" case.
    struct FailingDeleteSecretStore;
    impl SecretStore for FailingDeleteSecretStore {
        fn get(&self, _: &str) -> Result<Option<String>, SecretStoreError> {
            Ok(Some("tok".to_owned()))
        }
        fn set(&self, _: &str, _: &str) -> Result<(), SecretStoreError> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), SecretStoreError> {
            Err(SecretStoreError::Backend("store offline".to_owned()))
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

    /// The cache contents with the always-present built-in library filtered out,
    /// so local/MCP-focused assertions aren't perturbed by the bundled prompts
    /// (which a real service always lists — covered separately below).
    fn non_builtin(service: &PromptService) -> Vec<Prompt> {
        service
            .list()
            .into_iter()
            .filter(|p| p.provider != BUILTIN_PROVIDER)
            .collect()
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
        let prompts = non_builtin(&service);
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
        let names: Vec<String> = non_builtin(&service).into_iter().map(|p| p.name).collect();
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
        let names: Vec<String> = non_builtin(&service).into_iter().map(|p| p.name).collect();
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
        assert_eq!(non_builtin(&service).len(), 1);
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
        assert_eq!(non_builtin(&service).len(), 1);
    }

    #[tokio::test]
    async fn built_ins_list_and_resolve_alongside_local_without_collision() {
        // A real service surfaces the built-in library after sync, and a user's
        // same-named local prompt coexists with the built-in under its own
        // provider identity (no collision). Resolution is per-provider.
        let (dir, service) = service_with_prompts_dir();
        write(
            &dir.path().join("prompts"),
            "code-review.md",
            "---\nname: code-review\ndescription: mine\n---\nMY OWN REVIEW PROMPT\n",
        );
        service.sync().await;

        let by_provider = |p: &str| -> Vec<String> {
            service
                .list()
                .into_iter()
                .filter(|x| x.provider == p)
                .map(|x| x.name)
                .collect()
        };
        assert!(by_provider(LOCAL_PROVIDER).contains(&"code-review".to_owned()));
        assert!(by_provider(BUILTIN_PROVIDER).contains(&"code-review".to_owned()));
        assert!(by_provider(BUILTIN_PROVIDER).contains(&"analyze-ai-reviews".to_owned()));

        // Each resolves to its own content.
        let mine = service
            .render(LOCAL_PROVIDER, "code-review", &BTreeMap::new())
            .await
            .unwrap();
        assert!(mine.text.contains("MY OWN REVIEW PROMPT"));
        let builtin = service
            .render(BUILTIN_PROVIDER, "code-review", &BTreeMap::new())
            .await
            .unwrap();
        assert!(builtin.text.contains("Code Review Guidelines"));
    }

    #[tokio::test]
    async fn source_returns_unrendered_body_for_local_and_builtin_none_for_mcp() {
        let dir = TempDir::new().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir(&prompts_dir).unwrap();
        // A local prompt whose body carries a MiniJinja placeholder — the preview
        // must show it verbatim, NOT rendered/substituted.
        write(
            &prompts_dir,
            "p.md",
            "---\nname: p\ndescription: d\narguments:\n  - name: focus\n---\nReview.\n{% if focus %}Focus: {{ focus }}{% endif %}\n",
        );
        // A configured MCP provider — its source is server-side, so None.
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, http_provider_yaml("team", "https://a")).unwrap();
        let service = PromptService::new(
            config_path,
            prompts_dir,
            None,
            Arc::new(InMemorySecretStore::new()),
        );

        // Local: the raw template body, placeholders intact (not rendered). No
        // sync needed — source resolves builtin/local directly, like `get`.
        let local = service.source(LOCAL_PROVIDER, "p").unwrap();
        assert!(local.text.contains("{% if focus %}"));
        assert!(local.text.contains("{{ focus }}"));
        assert!(!local.text.contains("---")); // frontmatter stripped

        // Built-in: the baked template body.
        let builtin = service.source(BUILTIN_PROVIDER, "code-review").unwrap();
        assert!(builtin.text.contains("Code Review Guidelines"));

        // MCP: no un-rendered source available.
        assert!(service.source("team", "anything").is_none());
        // Unknown local prompt: None (not a panic/error).
        assert!(service.source(LOCAL_PROVIDER, "nope").is_none());
    }

    #[tokio::test]
    async fn disabled_service_has_no_prompt_source() {
        let service = PromptService::disabled();
        // Inert service exposes no built-in source even for a real built-in name.
        assert!(service.source(BUILTIN_PROVIDER, "code-review").is_none());
        assert!(service.source(LOCAL_PROVIDER, "x").is_none());
    }

    #[tokio::test]
    async fn disabled_service_lists_no_built_ins_and_render_fails() {
        // The inert service stays inert — built-ins are a real-service feature.
        let service = PromptService::disabled();
        service.sync().await;
        assert!(service.list().is_empty());
        let err = service
            .render(BUILTIN_PROVIDER, "code-review", &BTreeMap::new())
            .await
            .unwrap_err();
        assert!(matches!(err, PromptError::ProviderNotFound { .. }));
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
            .add_mcp_provider(
                "team",
                "https://mcp.example.com",
                McpAuth::Bearer,
                Some("secret-tok"),
            )
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
        service
            .add_mcp_provider("team", "https://a", McpAuth::Bearer, None)
            .unwrap();
        assert!(matches!(
            service.add_mcp_provider("team", "https://b", McpAuth::Bearer, None),
            Err(PromptError::DuplicateProvider { .. })
        ));
        assert!(matches!(
            service.add_mcp_provider("local", "https://b", McpAuth::Bearer, None),
            Err(PromptError::InvalidProviderName { .. })
        ));
        // The built-in library's namespace is reserved at the boundary a user
        // hits — an MCP provider named `builtin` can't shadow the read-only
        // built-ins. This is the milestone's no-collision keystone.
        assert!(matches!(
            service.add_mcp_provider("builtin", "https://b", McpAuth::Bearer, None),
            Err(PromptError::InvalidProviderName { .. })
        ));
        assert!(matches!(
            service.add_mcp_provider("a:b", "https://b", McpAuth::Bearer, None),
            Err(PromptError::InvalidProviderName { .. })
        ));
    }

    #[tokio::test]
    async fn add_oauth_provider_writes_auth_mode_and_stores_no_secret() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        let store = Arc::new(InMemorySecretStore::new());
        let service = PromptService::new(
            config_path.clone(),
            dir.path().join("prompts"),
            None,
            store.clone(),
        );

        service
            .add_mcp_provider(
                "tiddly",
                "https://prompts-mcp.tiddly.me/mcp",
                McpAuth::Oauth { scopes: None },
                None,
            )
            .unwrap();

        let raw = std::fs::read_to_string(&config_path).unwrap();
        assert!(raw.contains("type: oauth"), "{raw}");
        let providers = service.list_mcp_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].auth, McpAuth::Oauth { scopes: None });
        // Credential-less until sign-in.
        assert!(!providers[0].has_token);
        assert!(store.get("oauth:tiddly").unwrap().is_none());

        // A pasted token combined with OAuth mode is an API misuse, refused
        // before anything is written.
        let err = service
            .add_mcp_provider(
                "other",
                "https://a/mcp",
                McpAuth::Oauth { scopes: None },
                Some("tok"),
            )
            .unwrap_err();
        assert!(matches!(err, PromptError::OAuthFlow { .. }));
        assert_eq!(service.list_mcp_providers().len(), 1);
    }

    #[tokio::test]
    async fn sign_in_and_sign_out_require_an_oauth_provider() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, http_provider_yaml("team", "https://a")).unwrap();
        let service = PromptService::new(
            config_path,
            dir.path().join("prompts"),
            None,
            Arc::new(InMemorySecretStore::new()),
        );

        // A bearer provider has no browser flow in either direction.
        let err = service.sign_in_mcp_provider("team").await.unwrap_err();
        assert!(
            matches!(err, PromptError::OAuthFlow { ref provider, .. } if provider == "team"),
            "{err:?}"
        );
        let err = service.sign_out_mcp_provider("team").await.unwrap_err();
        assert!(matches!(err, PromptError::OAuthFlow { .. }));

        // An unconfigured name is "no such provider", not a flow failure.
        assert!(matches!(
            service.sign_in_mcp_provider("nope").await.unwrap_err(),
            PromptError::ProviderNotFound { .. }
        ));
        assert!(matches!(
            service.sign_out_mcp_provider("nope").await.unwrap_err(),
            PromptError::ProviderNotFound { .. }
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
    async fn config_rewrite_preserves_oauth_auth_mode() {
        // Adding a provider re-writes the whole mcp_providers section; an
        // existing OAuth entry (with a scopes override) must survive verbatim,
        // and the freshly-added bearer entry must not gain an `auth:` key.
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "mcp_providers:\n  - name: tiddly\n    transport:\n      type: http\n      url: https://prompts-mcp.tiddly.me/mcp\n    auth:\n      type: oauth\n      scopes: [openid]\n",
        )
        .unwrap();
        let service = PromptService::new(
            config_path.clone(),
            dir.path().join("prompts"),
            None,
            Arc::new(InMemorySecretStore::new()),
        );

        service
            .add_mcp_provider("team", "https://a", McpAuth::Bearer, None)
            .unwrap();

        let raw = std::fs::read_to_string(&config_path).unwrap();
        let section: McpSection = serde_norway::from_str(&raw).unwrap();
        let configs = section.into_configs();
        assert_eq!(configs.len(), 2);
        assert_eq!(
            configs[0].auth,
            McpAuth::Oauth {
                scopes: Some(vec!["openid".to_owned()]),
            }
        );
        assert_eq!(configs[1].auth, McpAuth::Bearer);
        // The bearer entry stays in the pre-OAuth on-disk shape (no auth key on
        // it); the file's only `auth:` block belongs to the OAuth entry.
        assert_eq!(raw.matches("auth:").count(), 1);
    }

    #[tokio::test]
    async fn remove_deletes_oauth_envelope_alongside_bearer() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, http_provider_yaml("team", "https://a")).unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        store.set("team", "tok").unwrap();
        store.set("oauth:team", "{\"envelope\":true}").unwrap();
        let service =
            PromptService::new(config_path, dir.path().join("prompts"), None, store.clone());

        service.remove_mcp_provider("team").unwrap();
        assert_eq!(store.get("team").unwrap(), None);
        assert_eq!(store.get("oauth:team").unwrap(), None);
    }

    #[tokio::test]
    async fn remove_attempts_second_delete_when_first_fails() {
        /// Fails deleting the bearer key but records every delete attempted, to
        /// prove a failed first delete doesn't short-circuit the second.
        struct FirstDeleteFailsStore {
            deleted: std::sync::Mutex<Vec<String>>,
        }
        impl SecretStore for FirstDeleteFailsStore {
            fn get(&self, _: &str) -> Result<Option<String>, SecretStoreError> {
                Ok(None)
            }
            fn set(&self, _: &str, _: &str) -> Result<(), SecretStoreError> {
                Ok(())
            }
            fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
                self.deleted
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(key.to_owned());
                if key == "team" {
                    Err(SecretStoreError::Backend("store offline".to_owned()))
                } else {
                    Ok(())
                }
            }
        }

        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, http_provider_yaml("team", "https://a")).unwrap();
        let store = Arc::new(FirstDeleteFailsStore {
            deleted: std::sync::Mutex::new(Vec::new()),
        });
        let service =
            PromptService::new(config_path, dir.path().join("prompts"), None, store.clone());

        // The bearer delete fails, but the OAuth-envelope delete must still be
        // attempted (a refresh token must not be stranded), and the failure
        // still surfaces so the provider isn't reported gone.
        let err = service.remove_mcp_provider("team").unwrap_err();
        assert!(matches!(err, PromptError::Secret(_)));
        let deleted = store
            .deleted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(deleted, vec!["team".to_owned(), "oauth:team".to_owned()]);
        assert_eq!(service.list_mcp_providers().len(), 1);
    }

    #[tokio::test]
    async fn add_leaves_no_provider_configured_when_secret_store_fails() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        let service = PromptService::new(
            config_path.clone(),
            dir.path().join("prompts"),
            None,
            Arc::new(FailingSecretStore),
        );

        // Secret is stored before the config entry, so a failed `set` aborts the
        // add before anything is written — no half-added, retry-blocking provider.
        let err = service
            .add_mcp_provider("team", "https://a", McpAuth::Bearer, Some("tok"))
            .unwrap_err();
        assert!(matches!(err, PromptError::Secret(_)));
        assert!(service.list_mcp_providers().is_empty());
        assert!(
            !config_path.exists()
                || !std::fs::read_to_string(&config_path)
                    .unwrap()
                    .contains("mcp_providers")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removal_waits_for_in_flight_credential_write_real_path() {
        // (multi_thread: the test thread blocks on channels while the spawned
        // save and the blocking pool must keep making progress.)
        use rmcp::transport::auth::StoredCredentials;

        /// Envelope writes signal entry and park until released; everything
        /// else passes through — so the test *knows* the save is inside its
        /// locked critical section when removal is invoked, with no timing
        /// assumption on that side.
        struct GatedWriteStore {
            inner: InMemorySecretStore,
            entered: std::sync::mpsc::Sender<()>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl SecretStore for GatedWriteStore {
            fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
                self.inner.get(key)
            }
            fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
                if key == "oauth:team" {
                    let _ = self.entered.send(());
                    let _ = self
                        .release
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .recv();
                }
                self.inner.set(key, value)
            }
            fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
                self.inner.delete(key)
            }
        }

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let store = Arc::new(GatedWriteStore {
            inner: InMemorySecretStore::new(),
            entered: entered_tx,
            release: std::sync::Mutex::new(release_rx),
        });
        let resource = "https://mcp.example.com/mcp";
        store
            .inner
            .set(
                "oauth:team",
                &format!(
                    r#"{{"registration":{{"client_id":"client-1","redirect_uri":"http://127.0.0.1/callback","resource":"{resource}"}},"tokens":null}}"#
                ),
            )
            .unwrap();

        let dir = TempDir::new().unwrap();
        let service = PromptService::new(
            dir.path().join("config.yaml"),
            dir.path().join("prompts"),
            None,
            store.clone(),
        );

        // A save through the service's own credential store — the REAL
        // registry lifecycle, not a hand-shared mutex.
        let cred_store = service.credential_store("team", resource);
        let save = tokio::spawn(async move {
            use rmcp::transport::auth::CredentialStore as _;
            cred_store
                .save(StoredCredentials::new(
                    "client-1".to_owned(),
                    None,
                    vec![],
                    None,
                ))
                .await
        });
        entered_rx.recv().unwrap(); // save is now inside its locked write

        // The real removal path must block on the same transaction lock.
        let service_for_removal = service.clone();
        let removal = std::thread::spawn(move || service_for_removal.remove_mcp_provider("team"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !removal.is_finished(),
            "remove_mcp_provider must wait for the in-flight credential write"
        );

        release_tx.send(()).unwrap();
        save.await.unwrap().unwrap(); // the write completes first…
        removal.join().unwrap().unwrap(); // …then removal deletes it
        assert_eq!(
            store.inner.get("oauth:team").unwrap(),
            None,
            "nothing may survive removal"
        );
    }

    #[tokio::test]
    async fn remove_surfaces_secret_deletion_failure_and_keeps_provider() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, http_provider_yaml("team", "https://a")).unwrap();
        let service = PromptService::new(
            config_path.clone(),
            dir.path().join("prompts"),
            None,
            Arc::new(FailingDeleteSecretStore),
        );

        // Deleting the token fails → the whole remove fails, leaving the provider
        // visible rather than reporting it gone while its credential lingers.
        let err = service.remove_mcp_provider("team").unwrap_err();
        assert!(matches!(err, PromptError::Secret(_)));
        let providers = service.list_mcp_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name, "team");
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("mcp_providers")
        );
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
            service.add_mcp_provider("team", "https://a", McpAuth::Bearer, None),
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
