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

/// Bridges one provider's envelope onto `rmcp`'s [`CredentialStore`]. rmcp only
/// ever hands us a [`StoredCredentials`], so `save` is a read-modify-write that
/// keeps the `registration` block intact, and `clear` (sign-out) nulls only the
/// `tokens` block. All operations go through one guarded read
/// ([`read_envelope`](Self::read_envelope)), so an envelope belonging to a
/// different resource behaves as "no envelope" everywhere, not just on `load`.
pub(crate) struct ProviderCredentialStore {
    secrets: Arc<dyn SecretStore>,
    key: String,
    /// The provider's current canonical URL; an envelope recorded for a
    /// different resource is invisible to every operation on this store.
    resource: String,
}

impl ProviderCredentialStore {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "constructed by the not-yet-implemented OAuth transport path"
        )
    )]
    pub(crate) fn new(secrets: Arc<dyn SecretStore>, provider: &str, resource: &str) -> Self {
        Self {
            secrets,
            key: oauth_secret_key(provider),
            resource: resource.to_owned(),
        }
    }

    /// Create (or overwrite) the envelope for a fresh client registration, with
    /// no tokens yet. The sign-in flow calls this immediately after dynamic
    /// registration succeeds — before the browser opens — so an abandoned flow
    /// never orphans a server-side registration. The `resource` is stamped from
    /// this store's own binding rather than accepted from the caller, so the
    /// invariant cannot arrive as two independently-supplied strings.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "called by the not-yet-implemented browser sign-in flow"
        )
    )]
    pub(crate) fn persist_registration(
        &self,
        client_id: String,
        redirect_uri: String,
    ) -> Result<(), AuthError> {
        self.write_envelope(&OAuthEnvelope {
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
        expect(
            dead_code,
            reason = "called by the not-yet-implemented browser sign-in flow"
        )
    )]
    pub(crate) fn registration(&self) -> Result<Option<OAuthRegistration>, AuthError> {
        Ok(self.read_envelope()?.map(|envelope| envelope.registration))
    }

    /// The single guarded read every operation goes through. Returns `None` for
    /// a missing envelope, a corrupt one (warned; the recovery is "sign in
    /// again"), or one bound to a different resource (warned with both values)
    /// — so callers cannot read, and therefore cannot mutate, an envelope that
    /// doesn't belong to this store's resource.
    fn read_envelope(&self) -> Result<Option<OAuthEnvelope>, AuthError> {
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

    fn write_envelope(&self, envelope: &OAuthEnvelope) -> Result<(), AuthError> {
        let json = serde_json::to_string(envelope)
            .map_err(|e| AuthError::InternalError(format!("could not serialize envelope: {e}")))?;
        self.secrets
            .set(&self.key, &json)
            .map_err(|e| AuthError::InternalError(format!("secret store write failed: {e}")))
    }
}

// `SecretStore` is sync and these methods call it directly from async. That is
// an accepted synchronous-I/O constraint, shared with the bearer path
// (`service.rs::resolve_bearer`) — not a claim that the backends are fast:
// file access blocks, keychain access can involve OS IPC or an unlock prompt,
// and rmcp loads credentials through here on every `get_access_token` call. A
// caller's async timeout cannot preempt a blocked call here. Don't "fix" this
// with `spawn_blocking` reflexively; move the calls to the blocking pool only
// deliberately, if a measurably slow backend ever appears.
#[async_trait::async_trait]
impl CredentialStore for ProviderCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(envelope) = self.read_envelope()? else {
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

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        // Read-modify-write: rmcp calls this on every token exchange/refresh
        // with only a `StoredCredentials`; the registration block must survive.
        // A missing (or, via the guarded read, mismatched-resource) envelope is
        // an invariant violation — the sign-in flow persists the registration
        // before rmcp can ever save — not a cue to invent registration fields
        // we don't have.
        let Some(mut envelope) = self.read_envelope()? else {
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
        self.write_envelope(&envelope)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        let Some(mut envelope) = self.read_envelope()? else {
            return Ok(());
        };
        envelope.tokens = None;
        self.write_envelope(&envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::{InMemorySecretStore, SecretStoreError};
    use rmcp::transport::auth::OAuthTokenResponse;

    const RESOURCE: &str = "https://mcp.example.com/mcp";

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
        ProviderCredentialStore::new(secrets.clone() as Arc<dyn SecretStore>, "team", resource)
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
        let store = ProviderCredentialStore::new(Arc::new(FailingStore), "team", RESOURCE);
        assert!(matches!(
            store.load().await.unwrap_err(),
            AuthError::InternalError(_)
        ));
    }
}
