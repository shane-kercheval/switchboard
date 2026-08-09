//! OAuth credential storage for MCP providers: the keychain envelope and the
//! bridge onto `rmcp`'s [`CredentialStore`] trait.
//!
//! **Why registration and tokens are separate lifetimes.** The envelope holds a
//! non-secret `registration` block (the dynamically-registered client id, the
//! redirect form it was registered with, and the resource URL it belongs to)
//! and a `tokens` block (rmcp's credentials). Sign-out clears only `tokens`;
//! removing the provider deletes the whole key. Keeping the registration across
//! sign-out is what lets a later sign-in reuse the existing client registration
//! instead of accumulating one server-side OAuth application per attempt.
//!
//! **Why credentials are bound to a resource — on reads *and* writes.** Every
//! envelope records the canonical resource URL its credentials were issued for,
//! and the identity check lives in the single shared read path, so every
//! operation inherits it. Reads never present tokens minted for one server to
//! another; mutations never write into an envelope that belongs to a different
//! resource (a stale in-flight refresh after a URL change errors instead of
//! depositing the old server's tokens under the new server's registration), and
//! `save` additionally refuses credentials whose client id doesn't match the
//! stored registration.
//!
//! **Why corrupt data loads as `None`.** A hard error on a corrupt envelope
//! would leave a provider the user cannot repair from the UI; degrading to
//! "signed out" (with a warning) makes the recovery "sign in again".
//!
//! The store key is `oauth:<provider-name>`, disjoint from the bearer-token key
//! (`<provider-name>`) by construction: provider names cannot contain `:`
//! (`config::is_valid_provider_name`).

use std::sync::Arc;

use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use serde::{Deserialize, Serialize};

use crate::secret::SecretStore;

/// The secret-store key holding a provider's OAuth envelope.
pub(crate) fn oauth_secret_key(provider: &str) -> String {
    format!("oauth:{provider}")
}

/// Canonicalize a provider URL via the URL parser's RFC 3986 syntax-based
/// normalization. This single canonical form feeds `AuthorizationManager::new`,
/// the RFC 8707 `resource` parameter, the envelope's `resource`, the
/// session-cache key, and every identity comparison, so they cannot disagree.
///
/// What it normalizes — this is the exact identity contract, so keep it
/// accurate: lowercase scheme and host, default port dropped, dot-segments
/// (`/../`, `/./`) resolved, an empty path becoming `/` (RFC 3986 §6.2.3 for
/// http/https), and the parser's percent-encoding handling. Path **case and
/// trailing slashes are preserved** and stay significant — tiddly requires
/// exactly `/mcp`, and silently rewriting a trailing slash could mask a real
/// mismatch.
///
/// Two reasons this exists: the MCP spec says implementations *"SHOULD accept
/// uppercase scheme and host components for robustness"*, and tiddly's server
/// does the mirror-image normalization in `_canonical_scheme_netloc`, so
/// matching it is symmetric by construction.
pub(crate) fn canonicalize_resource_url(url: &str) -> Result<String, url::ParseError> {
    Ok(url::Url::parse(url)?.to_string())
}

/// The persisted JSON shape under `oauth:<provider>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OAuthEnvelope {
    pub registration: OAuthRegistration,
    /// `None` between registration and the first token exchange, and after
    /// sign-out. This persists rmcp's [`StoredCredentials`] serde shape
    /// verbatim, so the stored format is **owned by that dependency**: a
    /// non-additive shape change in an rmcp upgrade makes every envelope fail
    /// to parse and degrade to "signed out" (a forced browser re-auth for every
    /// OAuth provider). The `persisted_envelope_format_stays_readable` test
    /// pins the current shape so an upgrade breaks a test instead of users;
    /// the implementation plan's rmcp-upgrade risk names this too. rmcp's
    /// `StoredCredentials` redacts token material in `Debug`.
    pub tokens: Option<StoredCredentials>,
}

/// The non-secret client registration a provider holds with its authorization
/// server. Survives sign-out; deleted only with the provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OAuthRegistration {
    pub client_id: String,
    /// The redirect URI *as registered* (the port-less loopback form), not the
    /// concrete per-sign-in `127.0.0.1:<ephemeral port>` one.
    pub redirect_uri: String,
    /// Canonical URL of the MCP endpoint these credentials belong to.
    pub resource: String,
}

/// A provider's local credential situation, determined without any network
/// traffic. Drives the `sync` short-circuit: a signed-out provider must report
/// `NeedsAuth` (fast, and correct even offline) instead of spending its budget
/// discovering a server only to learn what the keychain already knew.
#[derive(Debug)]
pub(crate) enum CredentialState {
    /// The secret store could not be read; the message is backend-diagnostic
    /// (never credential material).
    Unavailable(String),
    /// No envelope, no `tokens` block, or a `tokens` block with no token
    /// response — nothing usable without a browser sign-in.
    SignedOut,
    /// A token response is stored (it may still be expired; refresh handles
    /// that later, on the network path).
    SignedIn,
}

/// One provider's credential concurrency state — two locks with distinct jobs
/// and a strict acquisition order (`io_gate` before `txn`; removal takes `txn`
/// only, so no cycle):
///
/// - **`txn`** — the transaction lock: held across a whole envelope
///   read → check → write (and removal's deletes). Synchronous, held only
///   inside blocking-pool closures or brief sync sections — **never across an
///   `.await`**, and never across an operation that itself persists
///   credentials (that self-deadlocks; the sign-in flow's gate is a *separate*
///   async mutex — see the plan's M3 notes).
/// - **`io_gate`** — bounds abandoned blocking work. Each blocking task takes
///   an **owned** guard *into its closure*, so the gate is released only when
///   the store call actually finishes — not when a timed-out caller gives up.
///   Against a wedged keychain, one thread parks per provider and every later
///   attempt waits on the (cancellable) gate without spawning anything.
///   Caller-held guards would drop on timeout and re-open the pile-up.
#[derive(Default)]
pub(crate) struct CredentialLifecycle {
    pub(crate) txn: std::sync::Mutex<()>,
    pub(crate) io_gate: Arc<tokio::sync::Mutex<()>>,
}

/// Bridges one provider's envelope onto `rmcp`'s [`CredentialStore`]. rmcp only
/// ever hands us a [`StoredCredentials`], so `save` is a read-modify-write that
/// keeps the `registration` block intact, and `clear` (sign-out) nulls only the
/// `tokens` block. All operations go through one guarded read
/// ([`read_envelope_locked`](Self::read_envelope_locked)), so an envelope
/// belonging to a different resource behaves as "no envelope" everywhere, not
/// just on `load`.
///
/// **Mutation atomicity.** The envelope read-modify-write is not atomic on its
/// own, so every mutation holds the provider's transaction lock across the
/// *whole* read → check → write. Provider removal takes the same lock around
/// its deletes: without that, a refresh could read the envelope, removal could
/// delete it, and the refresh's write would then resurrect credentials under a
/// provider the user removed. Cache invalidation alone cannot prevent this —
/// live `AuthClient` clones outlive the cache entry.
pub(crate) struct ProviderCredentialStore {
    secrets: Arc<dyn SecretStore>,
    key: String,
    /// The provider's current canonical URL; an envelope recorded for a
    /// different resource is invisible to every operation on this store.
    resource: String,
    /// Shared with `PromptService`'s removal path via `credential_lifecycle`.
    lifecycle: Arc<CredentialLifecycle>,
}

impl Clone for ProviderCredentialStore {
    fn clone(&self) -> Self {
        Self {
            secrets: self.secrets.clone(),
            key: self.key.clone(),
            resource: self.resource.clone(),
            lifecycle: self.lifecycle.clone(),
        }
    }
}

impl ProviderCredentialStore {
    pub(crate) fn new(
        secrets: Arc<dyn SecretStore>,
        provider: &str,
        resource: &str,
        lifecycle: Arc<CredentialLifecycle>,
    ) -> Self {
        Self {
            secrets,
            key: oauth_secret_key(provider),
            resource: resource.to_owned(),
            lifecycle,
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lifecycle
            .txn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Run a store operation on the blocking pool, with the closure **owning**
    /// the provider's I/O gate for its full duration (see
    /// [`CredentialLifecycle`] for why ownership must live in the closure).
    async fn with_io_gate<T, F>(&self, op: F) -> Result<T, AuthError>
    where
        F: FnOnce(&ProviderCredentialStore) -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = self.lifecycle.io_gate.clone().lock_owned().await;
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            op(&store)
        })
        .await
        .map_err(join_err)
    }

    /// Create (or overwrite) the envelope for a fresh client registration, with
    /// no tokens yet. The sign-in flow calls this immediately after dynamic
    /// registration succeeds — before the browser opens — so an abandoned flow
    /// never orphans a server-side registration. The `resource` is stamped from
    /// this store's own binding rather than accepted from the caller, so the
    /// invariant cannot arrive as two independently-supplied strings.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "consumed by the sign-in flow, its only caller")
    )]
    pub(crate) fn persist_registration(
        &self,
        client_id: String,
        redirect_uri: String,
    ) -> Result<(), AuthError> {
        let _guard = self.locked();
        self.write_envelope_locked(&OAuthEnvelope {
            registration: OAuthRegistration {
                client_id,
                redirect_uri,
                resource: self.resource.clone(),
            },
            tokens: None,
        })
    }

    /// The stored client registration for this provider's current resource, if
    /// any. The sign-in flow reads this (never the raw envelope) to decide
    /// between reusing an existing registration and registering anew — going
    /// through the guarded read means a registration bound to a since-changed
    /// URL reads as absent, so the flow re-registers instead of reusing a
    /// client id the resource check would reject forever after.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "consumed by the sign-in flow, its only caller")
    )]
    pub(crate) fn registration(&self) -> Result<Option<OAuthRegistration>, AuthError> {
        let _guard = self.locked();
        self.read_envelope_locked()
            .map(|envelope| envelope.map(|e| e.registration))
    }

    /// Whether signed-in tokens are stored for this provider's current resource.
    /// Drives the Settings `has_token` column — *token* presence, never
    /// registration presence, so a signed-out provider never renders as
    /// credentialed. A store-read failure reads as `false` (the availability
    /// error surfaces through the status path, not this display nudge).
    ///
    /// Deliberately **lock-free** (the documented exception to the `_locked`
    /// helper contract): this is a read-only display nudge on the synchronous
    /// Settings-listing path, and taking the transaction lock here would queue
    /// the UI behind an in-flight token refresh's keychain write. A single-key
    /// read is atomic in every backend (file store renames atomically, keyring
    /// is per-item, in-memory is mutexed), so a race sees a complete old or new
    /// value, and a momentarily stale `has_token` is harmless. The inline sync
    /// read itself mirrors the bearer arm's pre-existing display read; making
    /// the whole listing async is noted in the plan as an M4 candidate.
    pub(crate) fn tokens_present(&self) -> bool {
        matches!(self.classify_envelope(), CredentialState::SignedIn)
    }

    /// The provider's local credential situation (see [`CredentialState`]).
    /// Async: the store read runs on the blocking pool under the I/O gate so a
    /// slow keychain can't stall the caller's task or pile up threads.
    pub(crate) async fn credential_state(&self) -> CredentialState {
        match self
            .with_io_gate(|store| {
                let _guard = store.locked();
                store.classify_envelope()
            })
            .await
        {
            Ok(state) => state,
            Err(e) => CredentialState::Unavailable(e.to_string()),
        }
    }

    fn classify_envelope(&self) -> CredentialState {
        match self.read_envelope_locked() {
            Err(e) => CredentialState::Unavailable(e.to_string()),
            Ok(None) => CredentialState::SignedOut,
            Ok(Some(envelope)) => {
                // "Usable" means a token *response* is present, not merely a
                // tokens block — a defensively-empty block is still signed out.
                if envelope
                    .tokens
                    .as_ref()
                    .is_some_and(|t| t.token_response.is_some())
                {
                    CredentialState::SignedIn
                } else {
                    CredentialState::SignedOut
                }
            }
        }
    }

    /// The single guarded read every operation goes through. Returns `None` for
    /// a missing envelope, a corrupt one (warned; the recovery is "sign in
    /// again"), or one bound to a different resource (warned with both values)
    /// — so callers cannot read, and therefore cannot mutate, an envelope that
    /// doesn't belong to this store's resource.
    ///
    /// **Caller must hold the transaction lock** (`_locked` suffix); this does
    /// not take it, and the lock is not reentrant — taking it here would hang
    /// every mutating caller. Sole documented exception: `tokens_present`'s
    /// lock-free display read (rationale at that method).
    fn read_envelope_locked(&self) -> Result<Option<OAuthEnvelope>, AuthError> {
        let raw = self
            .secrets
            .get(&self.key)
            .map_err(|e| AuthError::InternalError(format!("secret store read failed: {e}")))?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let envelope: OAuthEnvelope = match serde_json::from_str(&raw) {
            Ok(envelope) => envelope,
            Err(e) => {
                // Never log `raw` — it can contain tokens. The recovery for a
                // corrupt envelope is "sign in again", so degrade, don't fail.
                tracing::warn!(error = %e, "corrupt OAuth credential envelope; treating provider as signed out");
                return Ok(None);
            }
        };
        if envelope.registration.resource != self.resource {
            tracing::warn!(
                stored_resource = %envelope.registration.resource,
                current_resource = %self.resource,
                "stored OAuth envelope belongs to a different resource URL; treating as absent"
            );
            return Ok(None);
        }
        Ok(Some(envelope))
    }

    /// **Caller must hold the transaction lock** (`_locked` suffix); this does
    /// not take it, and the lock is not reentrant.
    fn write_envelope_locked(&self, envelope: &OAuthEnvelope) -> Result<(), AuthError> {
        let json = serde_json::to_string(envelope)
            .map_err(|e| AuthError::InternalError(format!("could not serialize envelope: {e}")))?;
        self.secrets
            .set(&self.key, &json)
            .map_err(|e| AuthError::InternalError(format!("secret store write failed: {e}")))
    }
}

impl ProviderCredentialStore {
    fn load_sync(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let _guard = self.locked();
        let Some(envelope) = self.read_envelope_locked()? else {
            return Ok(None);
        };
        // With `tokens: None` this synthesizes `token_response: None`, which is
        // exactly what makes rmcp report "not authorized" and return
        // `AuthorizationRequired` — a registered-but-signed-out provider.
        let (token_response, granted_scopes, token_received_at) = match envelope.tokens {
            Some(tokens) => (
                tokens.token_response,
                tokens.granted_scopes,
                tokens.token_received_at,
            ),
            None => (None, Vec::new(), None),
        };
        Ok(Some(StoredCredentials::new(
            envelope.registration.client_id,
            token_response,
            granted_scopes,
            token_received_at,
        )))
    }

    fn save_sync(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        // The lifecycle lock spans the whole read-modify-write (see the struct
        // docs): a concurrent removal either waits for this write or wins, in
        // which case the read below sees no envelope and the save errors —
        // never a resurrected credential.
        let _guard = self.locked();
        // rmcp calls this on every token exchange/refresh with only a
        // `StoredCredentials`; the registration block must survive. A missing
        // (or, via the guarded read, mismatched-resource) envelope is an
        // invariant violation — the sign-in flow persists the registration
        // before rmcp can ever save — not a cue to invent registration fields
        // we don't have.
        let Some(mut envelope) = self.read_envelope_locked()? else {
            return Err(AuthError::InternalError(
                "no OAuth registration stored for this provider's resource; cannot persist tokens"
                    .to_owned(),
            ));
        };
        // A stale client from a superseded registration must not deposit its
        // tokens under the current one. Client ids are public identifiers, so
        // naming both is safe and makes the honest failure diagnosable.
        if credentials.client_id != envelope.registration.client_id {
            return Err(AuthError::InternalError(format!(
                "refusing to persist tokens for client {:?} into a registration for client {:?}",
                credentials.client_id, envelope.registration.client_id
            )));
        }
        envelope.tokens = Some(credentials);
        self.write_envelope_locked(&envelope)
    }

    fn clear_sync(&self) -> Result<(), AuthError> {
        let _guard = self.locked();
        let Some(mut envelope) = self.read_envelope_locked()? else {
            return Ok(());
        };
        envelope.tokens = None;
        self.write_envelope_locked(&envelope)
    }
}

/// Takes the error by value so it can be used directly as a `map_err` callback.
#[allow(clippy::needless_pass_by_value)]
fn join_err(e: tokio::task::JoinError) -> AuthError {
    AuthError::InternalError(format!("credential I/O task failed: {e}"))
}

// `CredentialStore`-driven I/O runs on the blocking pool under the owned I/O
// gate, never inline in async. Two reasons this is deliberate (it upgrades an
// earlier accepted-sync-I/O position): the provider fan-out polls every
// provider inside one task, so a single blocked inline call would freeze *all*
// providers' progress and their timeouts; and rmcp loads credentials through
// here on every `get_access_token` call, so an outer budget must be able to
// abandon a blocked read. A fired timeout abandons the await while the
// blocking thread drains in the background; because the gate is owned by the
// closure (see `CredentialLifecycle`), abandoned work is bounded at one parked
// thread per provider no matter how many attempts time out. (The sync display
// read `tokens_present` is the documented exception to "on the blocking
// pool".)
#[async_trait::async_trait]
impl CredentialStore for ProviderCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.with_io_gate(ProviderCredentialStore::load_sync)
            .await?
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.with_io_gate(move |store| store.save_sync(credentials))
            .await?
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.with_io_gate(ProviderCredentialStore::clear_sync)
            .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::{InMemorySecretStore, SecretStoreError};
    use rmcp::transport::auth::OAuthTokenResponse;

    const RESOURCE: &str = "https://mcp.example.com/mcp";

    #[test]
    fn canonicalization_normalizes_syntax_and_preserves_the_path() {
        // Uppercase scheme/host lowercased; explicit default port dropped;
        // path case and trailing slash preserved (they stay significant).
        assert_eq!(
            canonicalize_resource_url("HTTPS://EXAMPLE.com:443/MCP").unwrap(),
            "https://example.com/MCP"
        );
        assert_eq!(
            canonicalize_resource_url("http://Example.com:80/mcp").unwrap(),
            "http://example.com/mcp"
        );
        // Non-default port kept.
        assert_eq!(
            canonicalize_resource_url("https://example.com:8443/mcp").unwrap(),
            "https://example.com:8443/mcp"
        );
        // Trailing slash survives — two distinct canonical identities.
        assert_ne!(
            canonicalize_resource_url("https://example.com/mcp/").unwrap(),
            canonicalize_resource_url("https://example.com/mcp").unwrap()
        );
        // Empty path normalizes to "/" (RFC 3986 §6.2.3 for http/https).
        assert_eq!(
            canonicalize_resource_url("https://example.com").unwrap(),
            "https://example.com/"
        );
        assert!(canonicalize_resource_url("not a url").is_err());
    }

    fn token_response(access_token: &str) -> OAuthTokenResponse {
        serde_json::from_value(serde_json::json!({
            "access_token": access_token,
            "token_type": "bearer",
            "refresh_token": "refresh-1",
        }))
        .unwrap()
    }

    fn credentials(client_id: &str, access_token: &str) -> StoredCredentials {
        StoredCredentials::new(
            client_id.to_owned(),
            Some(token_response(access_token)),
            vec![],
            None,
        )
    }

    fn store_for(secrets: &Arc<InMemorySecretStore>, resource: &str) -> ProviderCredentialStore {
        ProviderCredentialStore::new(
            secrets.clone() as Arc<dyn SecretStore>,
            "team",
            resource,
            Arc::new(CredentialLifecycle::default()),
        )
    }

    /// A store with a persisted registration for `RESOURCE` under client-1.
    fn registered_store(secrets: &Arc<InMemorySecretStore>) -> ProviderCredentialStore {
        let store = store_for(secrets, RESOURCE);
        store
            .persist_registration(
                "client-1".to_owned(),
                "http://127.0.0.1/callback".to_owned(),
            )
            .unwrap();
        store
    }

    #[tokio::test]
    async fn round_trips_registration_and_tokens() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = registered_store(&secrets);

        // Registered but not signed in: credentials exist, token_response absent.
        let creds = store.load().await.unwrap().unwrap();
        assert_eq!(creds.client_id, "client-1");
        assert!(creds.token_response.is_none());

        store
            .save(StoredCredentials::new(
                "client-1".to_owned(),
                Some(token_response("tok-1")),
                vec!["openid".to_owned()],
                Some(1000),
            ))
            .await
            .unwrap();
        let creds = store.load().await.unwrap().unwrap();
        assert_eq!(creds.client_id, "client-1");
        assert!(creds.token_response.is_some());
        assert_eq!(creds.granted_scopes, vec!["openid".to_owned()]);
        assert_eq!(creds.token_received_at, Some(1000));
    }

    #[tokio::test]
    async fn registration_accessor_round_trips_and_respects_resource_binding() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = registered_store(&secrets);

        let registration = store.registration().unwrap().unwrap();
        assert_eq!(registration.client_id, "client-1");
        assert_eq!(registration.redirect_uri, "http://127.0.0.1/callback");
        assert_eq!(registration.resource, RESOURCE);

        // Through a store bound to a different URL the registration is absent —
        // the sign-in flow re-registers rather than reusing a client id that
        // the resource check would reject forever after.
        let moved = store_for(&secrets, "https://other.example.com/mcp");
        assert!(moved.registration().unwrap().is_none());
    }

    #[tokio::test]
    async fn credentials_only_save_preserves_registration() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = registered_store(&secrets);
        store.save(credentials("client-1", "tok-1")).await.unwrap();

        // The registration block survives verbatim in the persisted envelope.
        let raw = secrets.get("oauth:team").unwrap().unwrap();
        let envelope: OAuthEnvelope = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            envelope.registration,
            OAuthRegistration {
                client_id: "client-1".to_owned(),
                redirect_uri: "http://127.0.0.1/callback".to_owned(),
                resource: RESOURCE.to_owned(),
            }
        );
        assert!(envelope.tokens.is_some());
    }

    #[tokio::test]
    async fn clear_drops_tokens_and_keeps_registration() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = registered_store(&secrets);
        store.save(credentials("client-1", "tok-1")).await.unwrap();

        store.clear().await.unwrap();

        // Signed out, still registered: load yields client_id with no tokens —
        // the state that makes rmcp report AuthorizationRequired.
        let creds = store.load().await.unwrap().unwrap();
        assert_eq!(creds.client_id, "client-1");
        assert!(creds.token_response.is_none());
    }

    #[tokio::test]
    async fn missing_envelope_loads_none_and_clear_is_a_noop() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = store_for(&secrets, RESOURCE);
        assert!(store.load().await.unwrap().is_none());
        store.clear().await.unwrap();
        assert!(secrets.get("oauth:team").unwrap().is_none());
    }

    #[tokio::test]
    async fn corrupt_envelope_loads_none() {
        let secrets = Arc::new(InMemorySecretStore::new());
        secrets.set("oauth:team", "{not json").unwrap();
        let store = store_for(&secrets, RESOURCE);
        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn resource_mismatch_loads_none() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = registered_store(&secrets);
        store.save(credentials("client-1", "tok-1")).await.unwrap();

        // Same provider name, different (e.g. user-edited) URL: the stored
        // credentials must never be presented to the new server.
        let moved = store_for(&secrets, "https://other.example.com/mcp");
        assert!(moved.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn save_through_mismatched_resource_is_rejected_and_writes_nothing() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = registered_store(&secrets);
        store.save(credentials("client-1", "tok-1")).await.unwrap();
        let before = secrets.get("oauth:team").unwrap().unwrap();

        // A stale in-flight refresh bound to a different URL must error, not
        // deposit its tokens into this resource's envelope.
        let stale = store_for(&secrets, "https://old.example.com/mcp");
        let err = stale
            .save(credentials("client-1", "tok-stale"))
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::InternalError(_)));
        assert_eq!(secrets.get("oauth:team").unwrap().unwrap(), before);
    }

    #[tokio::test]
    async fn clear_through_mismatched_resource_leaves_tokens_untouched() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = registered_store(&secrets);
        store.save(credentials("client-1", "tok-1")).await.unwrap();

        let stale = store_for(&secrets, "https://old.example.com/mcp");
        stale.clear().await.unwrap();

        let creds = store.load().await.unwrap().unwrap();
        assert!(creds.token_response.is_some());
    }

    #[tokio::test]
    async fn save_with_mismatched_client_id_is_rejected_and_writes_nothing() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = registered_store(&secrets);
        let before = secrets.get("oauth:team").unwrap().unwrap();

        // Tokens from a superseded client registration must not land under the
        // current one.
        let err = store
            .save(credentials("client-2", "tok-stale"))
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::InternalError(_)));
        assert_eq!(secrets.get("oauth:team").unwrap().unwrap(), before);
    }

    #[tokio::test]
    async fn save_without_registration_fails() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let store = store_for(&secrets, RESOURCE);
        let err = store
            .save(credentials("client-1", "tok-1"))
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::InternalError(_)));
        assert!(secrets.get("oauth:team").unwrap().is_none());
    }

    #[tokio::test]
    async fn persisted_envelope_format_stays_readable() {
        // A hard-coded envelope exactly as this version persists it, with a
        // populated token response. This pins the rmcp-owned serde shape
        // (`StoredCredentials` / `OAuthTokenResponse`): if an rmcp upgrade
        // changes it non-additively, this test fails — instead of every stored
        // envelope silently degrading to "signed out" in the release after.
        let legacy = r#"{
            "registration": {
                "client_id": "client-1",
                "redirect_uri": "http://127.0.0.1/callback",
                "resource": "https://mcp.example.com/mcp"
            },
            "tokens": {
                "client_id": "client-1",
                "token_response": {
                    "access_token": "tok-legacy",
                    "token_type": "bearer",
                    "expires_in": 3600,
                    "refresh_token": "refresh-legacy",
                    "scope": "openid offline_access"
                },
                "granted_scopes": ["openid", "offline_access"],
                "token_received_at": 1000
            }
        }"#;
        let secrets = Arc::new(InMemorySecretStore::new());
        secrets.set("oauth:team", legacy).unwrap();

        let store = store_for(&secrets, RESOURCE);
        let creds = store.load().await.unwrap().unwrap();
        assert_eq!(creds.client_id, "client-1");
        assert_eq!(creds.token_received_at, Some(1000));
        // Assert through re-serialization: proves the token material survived
        // the full deserialize→reserialize round-trip, not just that parsing
        // didn't error.
        let tokens = serde_json::to_value(creds.token_response.unwrap()).unwrap();
        assert_eq!(tokens["access_token"], "tok-legacy");
        assert_eq!(tokens["refresh_token"], "refresh-legacy");
        assert_eq!(tokens["expires_in"], 3600);
    }

    #[tokio::test]
    async fn envelope_without_tokens_key_parses_as_signed_out() {
        // serde derives `Option` fields as implicitly optional (missing key →
        // `None`); this pins that behavior, and catches a future change of
        // `tokens` to a non-`Option` type that would break older envelopes.
        let secrets = Arc::new(InMemorySecretStore::new());
        secrets
            .set(
                "oauth:team",
                r#"{"registration": {"client_id": "client-1", "redirect_uri": "http://127.0.0.1/callback", "resource": "https://mcp.example.com/mcp"}}"#,
            )
            .unwrap();

        let store = store_for(&secrets, RESOURCE);
        let creds = store.load().await.unwrap().unwrap();
        assert_eq!(creds.client_id, "client-1");
        assert!(creds.token_response.is_none());
    }

    #[tokio::test]
    async fn store_read_failure_is_an_error_not_signed_out() {
        // Unlike a corrupt/missing envelope (degrade to "sign in again"), a
        // store that cannot be read is surfaced — the credentials may exist.
        struct FailingStore;
        impl SecretStore for FailingStore {
            fn get(&self, _: &str) -> Result<Option<String>, SecretStoreError> {
                Err(SecretStoreError::Backend("keychain locked".to_owned()))
            }
            fn set(&self, _: &str, _: &str) -> Result<(), SecretStoreError> {
                Ok(())
            }
            fn delete(&self, _: &str) -> Result<(), SecretStoreError> {
                Ok(())
            }
        }
        let store = ProviderCredentialStore::new(
            Arc::new(FailingStore),
            "team",
            RESOURCE,
            Arc::new(CredentialLifecycle::default()),
        );
        assert!(matches!(
            store.load().await.unwrap_err(),
            AuthError::InternalError(_)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn io_gate_caps_abandoned_blocking_work_at_one_task() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// A wedged store: `get` counts entries, then parks until the test's
        /// sender drops. Entry count is the deterministic proxy for "blocking
        /// tasks actually dispatched" — thread counts would flake.
        struct WedgedStore {
            entered: Arc<AtomicUsize>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl SecretStore for WedgedStore {
            fn get(&self, _: &str) -> Result<Option<String>, SecretStoreError> {
                self.entered.fetch_add(1, Ordering::SeqCst);
                let _ = self
                    .release
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .recv();
                Ok(None)
            }
            fn set(&self, _: &str, _: &str) -> Result<(), SecretStoreError> {
                Ok(())
            }
            fn delete(&self, _: &str) -> Result<(), SecretStoreError> {
                Ok(())
            }
        }

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let entered = Arc::new(AtomicUsize::new(0));
        let store = ProviderCredentialStore::new(
            Arc::new(WedgedStore {
                entered: entered.clone(),
                release: std::sync::Mutex::new(release_rx),
            }),
            "team",
            RESOURCE,
            Arc::new(CredentialLifecycle::default()),
        );

        // Three attempts, each abandoned by its own timeout. The first parks a
        // blocking thread holding the owned gate; the others must wait on the
        // gate (cancellable) and never dispatch — a caller-held guard would
        // free the gate on timeout and dispatch one task per retry.
        for _ in 0..3 {
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(100),
                store.credential_state(),
            )
            .await;
        }
        assert_eq!(
            entered.load(Ordering::SeqCst),
            1,
            "abandoned retries must wait on the gate, not spawn new blocking work"
        );
        drop(release_tx); // unpark the wedged thread
    }

    // Holding the guard across awaits is the point of this test — it plays the
    // removal side, which owns the lock while the racing save must wait.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn removal_holding_the_lifecycle_lock_prevents_credential_resurrection() {
        // The interleave this guards against: a refresh reads the envelope,
        // removal deletes it, and the refresh's write recreates it — orphaned
        // credentials under a provider the user removed. With the lifecycle
        // lock, a mutation either completes its whole read-modify-write before
        // removal's deletes, or starts after them and observes no envelope.
        let secrets = Arc::new(InMemorySecretStore::new());
        let lifecycle = Arc::new(CredentialLifecycle::default());
        let store = ProviderCredentialStore::new(
            secrets.clone() as Arc<dyn SecretStore>,
            "team",
            RESOURCE,
            lifecycle.clone(),
        );
        store
            .persist_registration(
                "client-1".to_owned(),
                "http://127.0.0.1/callback".to_owned(),
            )
            .unwrap();
        store.save(credentials("client-1", "tok-1")).await.unwrap();

        // "Removal" takes the lifecycle lock first...
        let removal_guard = lifecycle.txn.lock().unwrap();
        // ...while a refresh save races in: it must wait, not interleave.
        let racing_save = {
            let store = store.clone();
            tokio::spawn(async move { store.save(credentials("client-1", "tok-late")).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !racing_save.is_finished(),
            "a mutation must wait for the lifecycle lock"
        );

        // Removal deletes under the lock, then releases it.
        secrets.delete("team").unwrap();
        secrets.delete("oauth:team").unwrap();
        drop(removal_guard);

        // The late save observes the deletion and fails; nothing resurrects.
        let result = racing_save.await.unwrap();
        assert!(matches!(result, Err(AuthError::InternalError(_))));
        assert!(secrets.get("oauth:team").unwrap().is_none());
    }
}
