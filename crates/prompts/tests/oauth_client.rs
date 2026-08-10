//! Hermetic end-to-end tests of the OAuth provider mode against an in-process
//! server that plays both roles: an rmcp Streamable-HTTP MCP server (behind an
//! Authorization-capturing layer) and a minimal RFC 9728/8414 authorization
//! surface (protected-resource metadata, AS metadata, token endpoint). All
//! loopback HTTP — the preflight's documented loopback exception is what makes
//! these tests possible.
//!
//! Exercised through the public `PromptService` API. Credentials are pre-seeded
//! into the secret store as the raw envelope JSON (the M1 shape); the browser
//! sign-in flow is a later milestone and is not simulated here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmcp::ServerHandler;
use rmcp::model::{
    ErrorData, GetPromptRequestParams, GetPromptResult, ListPromptsResult, PaginatedRequestParams,
    Prompt, PromptMessage, PromptMessageRole, ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use switchboard_prompts::{
    InMemorySecretStore, PromptError, PromptService, ProviderStatus, SecretStore, SecretStoreError,
};
use tempfile::TempDir;

#[derive(Clone)]
struct TestPromptServer;

impl ServerHandler for TestPromptServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_prompts().build();
        info
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult {
            meta: None,
            next_cursor: None,
            prompts: vec![Prompt::new("greet", Some("Greet someone"), None)],
        })
    }

    async fn get_prompt(
        &self,
        _request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            PromptMessageRole::User,
            "Hello from OAuth!",
        )]))
    }
}

#[derive(Default)]
struct ServerState {
    /// Every `Authorization` header value seen on `/mcp` requests.
    auth_headers: std::sync::Mutex<Vec<String>>,
    prm_fetches: AtomicUsize,
    as_fetches: AtomicUsize,
    token_hits: AtomicUsize,
    /// When set, the protected-resource-metadata handler stalls this long
    /// before responding — the "slow discovery" fault injection.
    prm_stall: Option<Duration>,
    /// Base URL, filled in after bind (handlers embed it in metadata bodies).
    base: std::sync::OnceLock<String>,
}

impl ServerState {
    fn base(&self) -> &str {
        self.base.get().expect("base set after bind")
    }
}

async fn capture_auth(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let Some(value) = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
    {
        state.auth_headers.lock().unwrap().push(value.to_owned());
    }
    next.run(request).await
}

async fn prm(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
) -> axum::Json<serde_json::Value> {
    state.prm_fetches.fetch_add(1, Ordering::SeqCst);
    if let Some(stall) = state.prm_stall {
        tokio::time::sleep(stall).await;
    }
    let base = state.base();
    axum::Json(serde_json::json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "scopes_supported": ["openid", "offline_access"],
    }))
}

async fn as_metadata(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
) -> axum::Json<serde_json::Value> {
    state.as_fetches.fetch_add(1, Ordering::SeqCst);
    let base = state.base();
    axum::Json(serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn token(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
) -> axum::Json<serde_json::Value> {
    let hit = state.token_hits.fetch_add(1, Ordering::SeqCst) + 1;
    axum::Json(serde_json::json!({
        "access_token": format!("refreshed-tok-{hit}"),
        "token_type": "bearer",
        "expires_in": 3600,
        "refresh_token": format!("rotated-refresh-{hit}"),
    }))
}

/// Spawn the combined MCP + authorization server; returns `(base, state)`.
async fn spawn_oauth_server(state: ServerState) -> (String, Arc<ServerState>) {
    let state = Arc::new(state);
    let mcp_service = StreamableHttpService::new(
        || Ok(TestPromptServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    // The capture layer wraps only routes registered before the merge, so
    // metadata/token fetches don't pollute the captured MCP headers.
    let mcp_router = axum::Router::new().nest_service("/mcp", mcp_service).layer(
        axum::middleware::from_fn_with_state(state.clone(), capture_auth),
    );
    let auth_router = axum::Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            axum::routing::get(prm),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(as_metadata),
        )
        .route("/token", axum::routing::post(token))
        .with_state(state.clone());
    let router = mcp_router.merge(auth_router);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    state.base.set(base.clone()).unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, state)
}

/// The raw M1 envelope JSON: a registration for `resource`, and (optionally)
/// seeded tokens. `expires_in`/`token_received_at` control whether the first
/// `get_access_token` refreshes: `None` means "no expiry info — use as-is".
fn envelope_json(resource: &str, tokens: Option<(&str, Option<(u64, u64)>)>) -> String {
    let tokens_value = match tokens {
        None => serde_json::Value::Null,
        Some((access_token, expiry)) => {
            let mut token_response = serde_json::json!({
                "access_token": access_token,
                "token_type": "bearer",
                "refresh_token": "seeded-refresh",
            });
            let mut received_at = serde_json::Value::Null;
            if let Some((expires_in, at)) = expiry {
                token_response["expires_in"] = expires_in.into();
                received_at = at.into();
            }
            serde_json::json!({
                "client_id": "client-1",
                "token_response": token_response,
                "granted_scopes": [],
                "token_received_at": received_at,
            })
        }
    };
    serde_json::json!({
        "registration": {
            "client_id": "client-1",
            "redirect_uri": "http://127.0.0.1/callback",
            "resource": resource,
        },
        "tokens": tokens_value,
    })
    .to_string()
}

struct Harness {
    _tmp: TempDir,
    service: PromptService,
    secrets: Arc<InMemorySecretStore>,
}

/// A service with one local prompt and the given `mcp_providers:` YAML block.
fn service_with_config(mcp_yaml: &str) -> Harness {
    let tmp = TempDir::new().unwrap();
    let prompts_dir = tmp.path().join("prompts");
    std::fs::create_dir(&prompts_dir).unwrap();
    std::fs::write(
        prompts_dir.join("note.md"),
        "---\nname: note\ndescription: a local note\n---\nLocal body\n",
    )
    .unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, mcp_yaml).unwrap();
    let secrets = Arc::new(InMemorySecretStore::new());
    let service = PromptService::new(
        config_path,
        prompts_dir,
        None,
        secrets.clone() as Arc<dyn SecretStore>,
    );
    Harness {
        _tmp: tmp,
        service,
        secrets,
    }
}

fn oauth_provider_yaml(name: &str, url: &str) -> String {
    format!(
        "mcp_providers:\n  - name: {name}\n    transport:\n      type: http\n      url: {url}\n    auth:\n      type: oauth\n"
    )
}

fn provider_status(service: &PromptService, name: &str) -> ProviderStatus {
    service
        .list_mcp_providers()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap()
        .status
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_provider_lists_prompts_and_sends_bearer_header() {
    let (base, state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &mcp_url));
    harness
        .secrets
        .set(
            "oauth:tiddly",
            &envelope_json(&mcp_url, Some(("seeded-tok", None))),
        )
        .unwrap();

    harness.service.sync().await;

    let ids: Vec<String> = harness
        .service
        .list()
        .iter()
        .map(|p| format!("{}:{}", p.provider, p.name))
        .collect();
    assert!(ids.contains(&"tiddly:greet".to_owned()), "{ids:?}");
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::Ok { prompt_count: 1 }
    );

    // Every MCP request carried the seeded token as a standard Bearer header.
    let headers = state.auth_headers.lock().unwrap().clone();
    assert!(!headers.is_empty(), "no Authorization header captured");
    assert!(
        headers.iter().all(|h| h == "Bearer seeded-tok"),
        "unexpected headers: {headers:?}"
    );

    // Info row: OAuth mode, token present.
    let info = harness
        .service
        .list_mcp_providers()
        .into_iter()
        .find(|p| p.name == "tiddly")
        .unwrap();
    assert!(info.has_token);
    assert!(matches!(
        info.auth,
        switchboard_prompts::McpAuth::Oauth { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn uppercase_scheme_and_host_in_config_still_validate() {
    // The config URL is canonicalized before every identity comparison, so an
    // uppercase scheme/host still matches the server's lowercase advertised
    // resource end to end.
    let (base, _state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let shouty = mcp_url.replacen("http://", "HTTP://", 1);
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &shouty));
    harness
        .secrets
        .set(
            "oauth:tiddly",
            &envelope_json(&mcp_url, Some(("seeded-tok", None))),
        )
        .unwrap();

    harness.service.sync().await;
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::Ok { prompt_count: 1 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trailing_slash_mismatch_fails_with_both_values() {
    // ".../mcp/" is canonicalized with its path untouched, so it must NOT
    // match the server's advertised ".../mcp" — and the error names both.
    // (Credentials are seeded for the slash form so the signed-out
    // short-circuit doesn't stop the pipeline before the identity check.)
    let (base, _state) = spawn_oauth_server(ServerState::default()).await;
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &format!("{base}/mcp/")));
    harness
        .secrets
        .set(
            "oauth:tiddly",
            &envelope_json(&format!("{base}/mcp/"), Some(("seeded-tok", None))),
        )
        .unwrap();

    harness.service.sync().await;
    match provider_status(&harness.service, "tiddly") {
        ProviderStatus::Errored { message } => {
            assert!(message.contains(&format!("{base}/mcp/")), "{message}");
            assert!(message.contains(&format!("{base}/mcp\"")), "{message}");
        }
        other => panic!("expected Errored, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_second_discovery_after_metadata_installed() {
    // One cold build = one PRM fetch + one AS fetch. A follow-up render (and
    // everything rmcp does internally) must not re-discover — this is what
    // proves the validation boundary isn't decorative.
    let (base, state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &mcp_url));
    harness
        .secrets
        .set(
            "oauth:tiddly",
            &envelope_json(&mcp_url, Some(("seeded-tok", None))),
        )
        .unwrap();

    harness.service.sync().await;
    let rendered = harness
        .service
        .render("tiddly", "greet", &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(rendered.text, "Hello from OAuth!");

    assert_eq!(state.prm_fetches.load(Ordering::SeqCst), 1);
    assert_eq!(state.as_fetches.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_first_use_constructs_one_client() {
    let (base, state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &mcp_url));
    harness
        .secrets
        .set(
            "oauth:tiddly",
            &envelope_json(&mcp_url, Some(("seeded-tok", None))),
        )
        .unwrap();

    // Cold cache: a sync and a render race to build the client. Single-flight
    // means exactly one preflight (and one AuthClient) regardless of winner.
    let no_args = BTreeMap::new();
    let ((), rendered) = tokio::join!(
        harness.service.sync(),
        harness.service.render("tiddly", "greet", &no_args),
    );
    rendered.unwrap();
    assert_eq!(state.prm_fetches.load(Ordering::SeqCst), 1);
    assert_eq!(state.as_fetches.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn near_expiry_token_refreshes_exactly_once_under_concurrency() {
    let (base, state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &mcp_url));
    // expires_in=1s received at epoch-second 1: long expired, refresh required.
    harness
        .secrets
        .set(
            "oauth:tiddly",
            &envelope_json(&mcp_url, Some(("stale-tok", Some((1, 1))))),
        )
        .unwrap();

    // A concurrent sync and render both need a fresh token; the shared
    // client's internal mutex must serialize to exactly one refresh.
    let no_args = BTreeMap::new();
    let ((), rendered) = tokio::join!(
        harness.service.sync(),
        harness.service.render("tiddly", "greet", &no_args),
    );
    rendered.unwrap();
    assert_eq!(
        state.token_hits.load(Ordering::SeqCst),
        1,
        "concurrent operations must share one refresh"
    );
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::Ok { prompt_count: 1 }
    );

    // The refreshed token is what reached the MCP server — never the stale one.
    let headers = state.auth_headers.lock().unwrap().clone();
    assert!(
        headers.iter().all(|h| h == "Bearer refreshed-tok-1"),
        "unexpected headers: {headers:?}"
    );

    // And the rotated credentials were persisted through the envelope.
    let raw = harness.secrets.get("oauth:tiddly").unwrap().unwrap();
    assert!(
        raw.contains("rotated-refresh-1"),
        "rotated refresh token not persisted"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_without_credentials_reports_needs_auth_and_spares_siblings() {
    // Provider "tiddly" is registered but signed out (tokens: null); provider
    // "team" is a healthy bearer provider on the same fake server's MCP route.
    let (base, _state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let config = format!(
        "mcp_providers:\n  - name: tiddly\n    transport:\n      type: http\n      url: {mcp_url}\n    auth:\n      type: oauth\n  - name: team\n    transport:\n      type: http\n      url: {mcp_url}\n"
    );
    let harness = service_with_config(&config);
    harness
        .secrets
        .set("oauth:tiddly", &envelope_json(&mcp_url, None))
        .unwrap();
    harness.secrets.set("team", "any-bearer").unwrap();

    harness.service.sync().await;

    // The OAuth provider needs sign-in and contributes nothing…
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::NeedsAuth
    );
    let ids: Vec<String> = harness
        .service
        .list()
        .iter()
        .map(|p| format!("{}:{}", p.provider, p.name))
        .collect();
    assert!(!ids.iter().any(|id| id.starts_with("tiddly:")), "{ids:?}");
    // …while the bearer sibling and local prompts are unaffected.
    assert!(ids.contains(&"team:greet".to_owned()), "{ids:?}");
    assert!(ids.contains(&"local:note".to_owned()), "{ids:?}");

    // Registered-but-signed-out must not render as credentialed.
    let info = harness
        .service
        .list_mcp_providers()
        .into_iter()
        .find(|p| p.name == "tiddly")
        .unwrap();
    assert!(!info.has_token);
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_discovery_on_one_provider_spares_a_healthy_sibling() {
    // "slow"'s protected-resource metadata stalls far past the budget; "team"
    // is a healthy bearer provider. The slow provider must time out within its
    // own budget without delaying the sibling — the whole sync stays bounded.
    let (base, _state) = spawn_oauth_server(ServerState {
        prm_stall: Some(Duration::from_mins(1)),
        ..ServerState::default()
    })
    .await;
    let mcp_url = format!("{base}/mcp");
    let config = format!(
        "mcp_providers:\n  - name: slow\n    transport:\n      type: http\n      url: {mcp_url}\n    auth:\n      type: oauth\n  - name: team\n    transport:\n      type: http\n      url: {mcp_url}\n"
    );
    let harness = service_with_config(&config);
    let service = harness
        .service
        .clone()
        .with_provider_timeout(Duration::from_millis(500));
    harness.secrets.set("team", "any-bearer").unwrap();
    // Signed-in credentials so the slow provider actually reaches discovery
    // (a signed-out one would short-circuit to NeedsAuth without a fetch).
    harness
        .secrets
        .set(
            "oauth:slow",
            &envelope_json(&mcp_url, Some(("seeded-tok", None))),
        )
        .unwrap();

    let started = std::time::Instant::now();
    service.sync().await;
    let elapsed = started.elapsed();

    assert!(matches!(
        provider_status(&service, "slow"),
        ProviderStatus::Errored { .. }
    ));
    assert_eq!(
        provider_status(&service, "team"),
        ProviderStatus::Ok { prompt_count: 1 }
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "sync took {elapsed:?} — the stalled provider leaked past its budget"
    );
}

/// Like `service_with_config`, but with a caller-supplied secret store — for
/// the store-fault tests.
fn service_with_store(mcp_yaml: &str, secrets: Arc<dyn SecretStore>) -> (TempDir, PromptService) {
    let tmp = TempDir::new().unwrap();
    let prompts_dir = tmp.path().join("prompts");
    std::fs::create_dir(&prompts_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, mcp_yaml).unwrap();
    let service = PromptService::new(config_path, prompts_dir, None, secrets);
    (tmp, service)
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_provider_reports_needs_auth_without_any_discovery() {
    // A provider that has never signed in (no envelope at all — what every
    // freshly added provider looks like) short-circuits to NeedsAuth from the
    // keychain alone: zero network fetches, no budget spent.
    let (base, state) = spawn_oauth_server(ServerState::default()).await;
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &format!("{base}/mcp")));

    harness.service.sync().await;

    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::NeedsAuth
    );
    assert_eq!(
        state.prm_fetches.load(Ordering::SeqCst),
        0,
        "signed-out status must not cost a discovery round trip"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_out_provider_reports_needs_auth_even_when_server_is_unreachable() {
    // Offline / server down: the fix is still "sign in", and the status must
    // say so instead of misreporting a server error. Port 1 refuses instantly.
    let harness = service_with_config(&oauth_provider_yaml("tiddly", "http://127.0.0.1:1/mcp"));

    harness.service.sync().await;
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::NeedsAuth
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cold_render_is_bounded_by_a_single_provider_budget() {
    // A cold render runs acquisition (stalled here) + probe + render under ONE
    // outer budget — never the stacked two that would double the wait.
    let (base, _state) = spawn_oauth_server(ServerState {
        prm_stall: Some(Duration::from_mins(1)),
        ..ServerState::default()
    })
    .await;
    let mcp_url = format!("{base}/mcp");
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &mcp_url));
    let service = harness
        .service
        .clone()
        .with_provider_timeout(Duration::from_millis(500));
    harness
        .secrets
        .set(
            "oauth:tiddly",
            &envelope_json(&mcp_url, Some(("seeded-tok", None))),
        )
        .unwrap();

    let started = std::time::Instant::now();
    let result = service.render("tiddly", "greet", &BTreeMap::new()).await;
    let elapsed = started.elapsed();

    assert!(result.is_err());
    assert!(
        elapsed < Duration::from_secs(3),
        "cold render took {elapsed:?} — leaked past its single budget"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn render_against_signed_out_provider_returns_typed_needs_auth() {
    let (base, state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &mcp_url));
    harness
        .secrets
        .set("oauth:tiddly", &envelope_json(&mcp_url, None))
        .unwrap();

    let err = harness
        .service
        .render("tiddly", "greet", &BTreeMap::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, PromptError::McpNeedsAuth { ref provider } if provider == "tiddly"),
        "expected McpNeedsAuth, got {err:?}"
    );
    // The typed determination happened locally — not only did no credential
    // reach the MCP endpoint, no discovery request was issued at all.
    assert!(state.auth_headers.lock().unwrap().is_empty());
    assert_eq!(
        state.prm_fetches.load(Ordering::SeqCst),
        0,
        "signed-out render must not run discovery"
    );
    // And the determination was recorded as the provider's status, so
    // Settings agrees with what the composer just reported (a render failure
    // runs no sync, and the row would otherwise keep its stale status).
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::NeedsAuth
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn render_signed_out_reports_needs_auth_even_offline() {
    // The server is unreachable (port 1 refuses instantly): the fix is still
    // "sign in", and the typed error must say so instead of a connection error.
    let harness = service_with_config(&oauth_provider_yaml("tiddly", "http://127.0.0.1:1/mcp"));
    let err = harness
        .service
        .render("tiddly", "greet", &BTreeMap::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, PromptError::McpNeedsAuth { .. }),
        "expected McpNeedsAuth, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wedged_bearer_read_is_bounded_and_sync_completes() {
    // A bearer credential read that blocks far past the budget: the provider
    // must land Errored within its budget and — critically — sync() itself
    // must return. Without the outer timeout on the bearer arm, this future
    // would never resolve, join_all would never complete, and the held
    // sync_lock would wedge every future sync permanently.
    struct WedgedBearerStore {
        inner: InMemorySecretStore,
    }
    impl SecretStore for WedgedBearerStore {
        fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
            if key == "team" {
                // Long enough to blow the 300ms budget; short enough that the
                // runtime's shutdown (which waits for blocking tasks) stays fast.
                std::thread::sleep(Duration::from_secs(3));
            }
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
            self.inner.set(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
            self.inner.delete(key)
        }
    }

    let (base, _state) = spawn_oauth_server(ServerState::default()).await;
    let (_tmp, service) = service_with_store(
        &format!(
            "mcp_providers:\n  - name: team\n    transport:\n      type: http\n      url: {base}/mcp\n"
        ),
        Arc::new(WedgedBearerStore {
            inner: InMemorySecretStore::new(),
        }),
    );
    let service = service.with_provider_timeout(Duration::from_millis(300));

    let started = std::time::Instant::now();
    service.sync().await;
    let elapsed = started.elapsed();

    assert!(matches!(
        provider_status(&service, "team"),
        ProviderStatus::Errored { .. }
    ));
    assert!(
        elapsed < Duration::from_secs(3),
        "sync took {elapsed:?} — a wedged bearer read escaped the provider budget"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_provider_over_failing_store_reports_store_unavailable() {
    /// Every read fails — the locked-keychain case. The user must be pointed
    /// at the store problem, never sent through a browser sign-in that would
    /// fail for the same reason.
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

    let (base, _state) = spawn_oauth_server(ServerState::default()).await;
    let (_tmp, service) = service_with_store(
        &oauth_provider_yaml("tiddly", &format!("{base}/mcp")),
        Arc::new(FailingStore),
    );
    service.sync().await;
    assert_eq!(
        provider_status(&service, "tiddly"),
        ProviderStatus::StoreUnavailable
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn later_store_failure_surfaces_as_errored_not_store_unavailable() {
    // StoreUnavailable comes ONLY from the direct local check. A read that
    // fails later inside rmcp's path surfaces as that operation's error
    // (Errored) — proving no back-classification of rmcp's InternalError.
    struct FailAfterFirstReadStore {
        inner: InMemorySecretStore,
        reads: AtomicUsize,
    }
    impl SecretStore for FailAfterFirstReadStore {
        fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
            if self.reads.fetch_add(1, Ordering::SeqCst) >= 1 {
                return Err(SecretStoreError::Backend(
                    "store died mid-flight".to_owned(),
                ));
            }
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
            self.inner.set(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
            self.inner.delete(key)
        }
    }

    let (base, _state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let store = Arc::new(FailAfterFirstReadStore {
        inner: InMemorySecretStore::new(),
        reads: AtomicUsize::new(0),
    });
    store
        .inner
        .set(
            "oauth:tiddly",
            &envelope_json(&mcp_url, Some(("seeded-tok", None))),
        )
        .unwrap();
    let (_tmp, service) =
        service_with_store(&oauth_provider_yaml("tiddly", &mcp_url), store.clone());

    service.sync().await;
    // Read #1 (the local check) succeeded → SignedIn; read #2 (rmcp's
    // initialize_from_store) failed → the build errors.
    assert!(matches!(
        provider_status(&service, "tiddly"),
        ProviderStatus::Errored { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_store_read_does_not_freeze_sibling_providers() {
    // A store read that BLOCKS the thread (not an awaitable delay) on one
    // provider's key: the fan-out polls every provider in one task, so with
    // inline sync reads this would freeze the sibling too. On the blocking
    // pool, the sibling proceeds and the blocked provider times out on its own
    // budget.
    struct SelectivelyBlockingStore {
        inner: InMemorySecretStore,
        slow_key: String,
    }
    impl SecretStore for SelectivelyBlockingStore {
        fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
            if key == self.slow_key {
                std::thread::sleep(Duration::from_secs(2));
            }
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
            self.inner.set(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
            self.inner.delete(key)
        }
    }

    let (base, _state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let config = format!(
        "mcp_providers:\n  - name: blocked\n    transport:\n      type: http\n      url: {mcp_url}\n    auth:\n      type: oauth\n  - name: team\n    transport:\n      type: http\n      url: {mcp_url}\n"
    );
    let store = Arc::new(SelectivelyBlockingStore {
        inner: InMemorySecretStore::new(),
        slow_key: "oauth:blocked".to_owned(),
    });
    store.inner.set("team", "any-bearer").unwrap();
    let (_tmp, service) = service_with_store(&config, store.clone());
    let service = service.with_provider_timeout(Duration::from_millis(300));

    let started = std::time::Instant::now();
    service.sync().await;
    let elapsed = started.elapsed();

    assert!(matches!(
        provider_status(&service, "blocked"),
        ProviderStatus::Errored { .. }
    ));
    assert_eq!(
        provider_status(&service, "team"),
        ProviderStatus::Ok { prompt_count: 1 }
    );
    // Inline sync reads would serialize the 2s block ahead of everything
    // (sync ≥ 2s); on the blocking pool the sibling finishes concurrently and
    // the blocked provider is abandoned at its 300ms budget.
    assert!(
        elapsed < Duration::from_millis(1500),
        "sync took {elapsed:?} — a blocked store read froze the fan-out"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_provider_probe_reports_status_by_name() {
    let (base, _state) = spawn_oauth_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = service_with_config(&oauth_provider_yaml("tiddly", &mcp_url));

    // Signed out → the probe reports NeedsAuth (not a generic error).
    harness
        .secrets
        .set("oauth:tiddly", &envelope_json(&mcp_url, None))
        .unwrap();
    assert_eq!(
        harness
            .service
            .test_saved_mcp_provider("tiddly")
            .await
            .unwrap(),
        ProviderStatus::NeedsAuth
    );

    // Signed in → Ok with the live prompt count.
    harness
        .secrets
        .set(
            "oauth:tiddly",
            &envelope_json(&mcp_url, Some(("seeded-tok", None))),
        )
        .unwrap();
    assert_eq!(
        harness
            .service
            .test_saved_mcp_provider("tiddly")
            .await
            .unwrap(),
        ProviderStatus::Ok { prompt_count: 1 }
    );

    // Unknown name is the only Err.
    assert!(
        harness
            .service
            .test_saved_mcp_provider("nope")
            .await
            .is_err()
    );
}
