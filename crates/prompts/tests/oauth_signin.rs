//! Hermetic end-to-end tests of the browser sign-in and sign-out flows against
//! an in-process server playing every role: rmcp Streamable-HTTP MCP server,
//! RFC 9728/8414 discovery surface, RFC 7591 registration endpoint, and token
//! endpoint. The "browser" is the injected [`BrowserOpener`]: it captures the
//! authorization URL and drives the loopback callback the way a redirect
//! would, so the whole flow — listener, registration, PKCE, state, exchange,
//! persistence — runs for real over loopback HTTP.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
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
    BrowserOpener, InMemorySecretStore, PromptError, PromptService, ProviderStatus, SecretStore,
    SecretStoreError,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

/// Parks token requests so a test can hold one in flight: the handler signals
/// `entered`, then waits for a semaphore permit. With `refresh_only`, only
/// `grant_type=refresh_token` requests are gated (so a sign-in's
/// authorization-code exchange passes while a refresh stays parked).
struct TokenGate {
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<tokio::sync::Semaphore>,
    refresh_only: bool,
}

#[derive(Default)]
struct ServerState {
    /// Base URL, filled in after bind (handlers embed it in metadata bodies).
    base: std::sync::OnceLock<String>,
    /// Every `Authorization` header value seen on `/mcp` requests.
    auth_headers: std::sync::Mutex<Vec<String>>,
    register_hits: AtomicUsize,
    register_bodies: std::sync::Mutex<Vec<serde_json::Value>>,
    /// Discovery fetches of the protected-resource metadata — the proxy for
    /// "a session client was (re)built".
    prm_fetches: AtomicUsize,
    /// When set, `/register` answers 500 — the DCR-rejection fault.
    register_fail: std::sync::atomic::AtomicBool,
    token_hits: AtomicUsize,
    /// Raw `application/x-www-form-urlencoded` bodies of every token request.
    token_bodies: std::sync::Mutex<Vec<String>>,
    /// `scopes_supported` advertised in the protected-resource metadata.
    /// Mutable so a test can change what the server advertises between two
    /// sign-ins (the M5-shaped scenario).
    prm_scopes: std::sync::Mutex<Option<Vec<&'static str>>>,
    /// When set, an unauthenticated `/mcp` request gets a 401 whose
    /// `WWW-Authenticate` carries this `scope` challenge.
    challenge_scope: Option<&'static str>,
    token_gate: Option<TokenGate>,
}

impl ServerState {
    fn base(&self) -> &str {
        self.base.get().expect("base set after bind")
    }
}

async fn mcp_middleware(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    match auth {
        Some(value) => state.auth_headers.lock().unwrap().push(value),
        None => {
            if let Some(scope) = state.challenge_scope {
                return axum::response::Response::builder()
                    .status(401)
                    .header("WWW-Authenticate", format!("Bearer scope=\"{scope}\""))
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
        }
    }
    next.run(request).await
}

async fn prm(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
) -> axum::Json<serde_json::Value> {
    state.prm_fetches.fetch_add(1, Ordering::SeqCst);
    let base = state.base();
    let mut body = serde_json::json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
    });
    if let Some(scopes) = state.prm_scopes.lock().unwrap().as_ref() {
        body["scopes_supported"] = serde_json::json!(scopes);
    }
    axum::Json(body)
}

async fn as_metadata(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
) -> axum::Json<serde_json::Value> {
    let base = state.base();
    axum::Json(serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn register(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> axum::response::Response {
    if state.register_fail.load(Ordering::SeqCst) {
        return axum::response::Response::builder()
            .status(500)
            .body(axum::body::Body::from("registration rejected"))
            .unwrap();
    }
    let hit = state.register_hits.fetch_add(1, Ordering::SeqCst) + 1;
    let redirect_uris = body["redirect_uris"].clone();
    state.register_bodies.lock().unwrap().push(body);
    axum::response::IntoResponse::into_response(axum::Json(serde_json::json!({
        "client_id": format!("dyn-client-{hit}"),
        "redirect_uris": redirect_uris,
    })))
}

async fn token(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
    body: String,
) -> axum::Json<serde_json::Value> {
    state.token_bodies.lock().unwrap().push(body.clone());
    if let Some(gate) = &state.token_gate
        && (!gate.refresh_only || body.contains("grant_type=refresh_token"))
    {
        let _ = gate.entered.send(());
        gate.release
            .acquire()
            .await
            .expect("token gate semaphore closed")
            .forget();
    }
    let hit = state.token_hits.fetch_add(1, Ordering::SeqCst) + 1;
    axum::Json(serde_json::json!({
        "access_token": format!("signin-tok-{hit}"),
        "token_type": "bearer",
        "expires_in": 3600,
        "refresh_token": format!("signin-refresh-{hit}"),
    }))
}

/// Spawn the combined MCP + authorization server; returns `(base, state)`.
async fn spawn_server(state: ServerState) -> (String, Arc<ServerState>) {
    let state = Arc::new(state);
    let mcp_service = StreamableHttpService::new(
        || Ok(TestPromptServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let mcp_router = axum::Router::new().nest_service("/mcp", mcp_service).layer(
        axum::middleware::from_fn_with_state(state.clone(), mcp_middleware),
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
        .route("/register", axum::routing::post(register))
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

/// How the fake browser answers the authorization request.
#[derive(Clone, Copy)]
enum DriveMode {
    /// Redirect back with a code and the correct state.
    Approve,
    /// Redirect back with `error=access_denied` and the correct state (RFC
    /// 6749 requires the server to echo `state` on error responses too).
    Deny,
    /// Redirect back with a code but the wrong state value.
    WrongState,
    /// Never redirect back — the abandoned-tab case.
    Abandon,
    /// First a forged `error` callback with no state (a hostile local
    /// process), then the real approval — the flow must survive the forgery.
    ForgedErrorThenApprove,
    /// Record the authorization URL but drive nothing — the test itself
    /// issues the callback when its interleaving is ready.
    Capture,
    /// First a request line far past the listener's read cap (never
    /// newline-terminated within it), then the real approval — the oversized
    /// request must be dropped, not parsed from its truncated prefix.
    OversizedThenApprove,
}

/// The injected "browser": records every authorization URL it is asked to
/// open, then (per mode) hits the loopback callback like the redirect would.
struct DrivingOpener {
    urls: std::sync::Mutex<Vec<String>>,
    mode: DriveMode,
}

impl DrivingOpener {
    fn new(mode: DriveMode) -> Arc<Self> {
        Arc::new(Self {
            urls: std::sync::Mutex::new(Vec::new()),
            mode,
        })
    }

    fn opened(&self) -> Vec<String> {
        self.urls.lock().unwrap().clone()
    }
}

/// Decoded query parameters of a captured authorization URL.
fn query_params(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[async_trait::async_trait]
impl BrowserOpener for DrivingOpener {
    async fn open(&self, url: &str) -> Result<(), String> {
        self.urls.lock().unwrap().push(url.to_owned());
        let params = query_params(url);
        let redirect_uri = params["redirect_uri"].clone();
        let state = params["state"].clone();
        let target = match self.mode {
            DriveMode::Approve => format!("{redirect_uri}?code=test-code&state={state}"),
            DriveMode::Deny => format!(
                "{redirect_uri}?error=access_denied&error_description=the%20user%20declined&state={state}"
            ),
            DriveMode::WrongState => {
                format!("{redirect_uri}?code=test-code&state=not-the-real-state")
            }
            DriveMode::Abandon | DriveMode::Capture => return Ok(()),
            DriveMode::OversizedThenApprove => {
                let real = format!("{redirect_uri}?code=test-code&state={state}");
                let oversized_target = format!("/callback?junk={}", "a".repeat(16 * 1024));
                let oversized_redirect = redirect_uri.clone();
                tokio::spawn(async move {
                    send_raw_request(&oversized_redirect, &oversized_target).await;
                    hit_callback(real).await;
                });
                return Ok(());
            }
            DriveMode::ForgedErrorThenApprove => {
                let forged = format!("{redirect_uri}?error=access_denied");
                let real = format!("{redirect_uri}?code=test-code&state={state}");
                tokio::spawn(async move {
                    // Sequential: the forgery fully lands (and is answered)
                    // before the legitimate redirect arrives.
                    hit_callback(forged).await;
                    hit_callback(real).await;
                });
                return Ok(());
            }
        };
        tokio::spawn(hit_callback(target));
        Ok(())
    }
}

/// Write one raw GET for `target` to the listener behind `redirect_uri`,
/// ignoring errors (the listener may drop the connection without responding).
async fn send_raw_request(redirect_uri: &str, target: &str) {
    let url = url::Url::parse(redirect_uri).unwrap();
    let addr = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
    let Ok(mut stream) = tokio::net::TcpStream::connect(addr).await else {
        return;
    };
    let _ = stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
}

/// Issue a plain HTTP GET against the flow's loopback callback listener, the
/// way the browser redirect would.
async fn hit_callback(target: String) {
    let url = url::Url::parse(&target).unwrap();
    let addr = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let path_and_query = &target[target.find("/callback").unwrap()..];
    stream
        .write_all(
            format!(
                "GET {path_and_query} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
}

struct Harness {
    _tmp: TempDir,
    config_path: PathBuf,
    prompts_dir: PathBuf,
    secrets: Arc<InMemorySecretStore>,
    service: PromptService,
    opener: Arc<DrivingOpener>,
}

impl Harness {
    /// A second service over the same config + secret store, with its own
    /// opener — "the user relaunched the app / retried with a different
    /// outcome".
    fn sibling_service(&self, mode: DriveMode) -> (PromptService, Arc<DrivingOpener>) {
        let opener = DrivingOpener::new(mode);
        let service = PromptService::new(
            self.config_path.clone(),
            self.prompts_dir.clone(),
            None,
            self.secrets.clone() as Arc<dyn SecretStore>,
        )
        .with_browser_opener(opener.clone());
        (service, opener)
    }
}

fn harness(mcp_yaml: &str, mode: DriveMode) -> Harness {
    let tmp = TempDir::new().unwrap();
    let prompts_dir = tmp.path().join("prompts");
    std::fs::create_dir(&prompts_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, mcp_yaml).unwrap();
    let secrets = Arc::new(InMemorySecretStore::new());
    let opener = DrivingOpener::new(mode);
    let service = PromptService::new(
        config_path.clone(),
        prompts_dir.clone(),
        None,
        secrets.clone() as Arc<dyn SecretStore>,
    )
    .with_browser_opener(opener.clone());
    Harness {
        _tmp: tmp,
        config_path,
        prompts_dir,
        secrets,
        service,
        opener,
    }
}

fn oauth_yaml(name: &str, url: &str) -> String {
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

fn stored_envelope(secrets: &InMemorySecretStore, provider: &str) -> Option<serde_json::Value> {
    secrets
        .get(&format!("oauth:{provider}"))
        .unwrap()
        .map(|raw| serde_json::from_str(&raw).unwrap())
}

/// Decoded pairs of a captured `application/x-www-form-urlencoded` token body.
fn form_params(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

/// The callback port a flow used, from its captured authorization URL —
/// for asserting the listener is closed afterwards.
async fn callback_port_is_closed(authorize_url: &str) {
    let redirect = query_params(authorize_url)["redirect_uri"].clone();
    let url = url::Url::parse(&redirect).unwrap();
    let addr = format!("127.0.0.1:{}", url.port().unwrap());
    assert!(
        tokio::net::TcpStream::connect(&addr).await.is_err(),
        "callback listener at {addr} must be closed once the flow returns"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_completes_and_prompts_become_usable() {
    let (base, state) = spawn_server(ServerState {
        prm_scopes: std::sync::Mutex::new(Some(vec!["prompts.read", "offline_access"])),
        ..ServerState::default()
    })
    .await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Approve);

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();

    // The authorization URL carried PKCE, the resource, the resolved scopes,
    // the registered client id, and a concrete loopback redirect.
    let opened = harness.opener.opened();
    assert_eq!(opened.len(), 1);
    let params = query_params(&opened[0]);
    assert!(!params["code_challenge"].is_empty());
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["resource"], mcp_url);
    assert_eq!(params["scope"], "offline_access prompts.read");
    assert_eq!(params["client_id"], "dyn-client-1");
    assert!(params["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(params["redirect_uri"].ends_with("/callback"));

    // Registration happened once, port-less, as a public client, with the
    // resolved scopes.
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 1);
    let registration = state.register_bodies.lock().unwrap()[0].clone();
    assert_eq!(
        registration["redirect_uris"],
        serde_json::json!(["http://127.0.0.1/callback"])
    );
    assert_eq!(registration["token_endpoint_auth_method"], "none");
    assert_eq!(registration["scope"], "offline_access prompts.read");

    // The exchange carried the code, a PKCE verifier, the resource, and the
    // concrete redirect the authorization used.
    let exchange = form_params(&state.token_bodies.lock().unwrap()[0]);
    assert_eq!(exchange["code"], "test-code");
    assert_eq!(exchange["grant_type"], "authorization_code");
    assert!(!exchange["code_verifier"].is_empty());
    assert_eq!(exchange["resource"], mcp_url);
    assert_eq!(exchange["redirect_uri"], params["redirect_uri"]);

    // Credentials landed in the envelope: registration (port-less form) plus
    // the exchanged tokens.
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert_eq!(envelope["registration"]["client_id"], "dyn-client-1");
    assert_eq!(
        envelope["registration"]["redirect_uri"],
        "http://127.0.0.1/callback"
    );
    assert_eq!(envelope["registration"]["resource"], mcp_url);
    assert_eq!(
        envelope["tokens"]["token_response"]["access_token"],
        "signin-tok-1"
    );

    // The provider is now fully usable: sync lists, render works, and the MCP
    // requests carry the exchanged token.
    harness.service.sync().await;
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::Ok { prompt_count: 1 }
    );
    let rendered = harness
        .service
        .render("tiddly", "greet", &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(rendered.text, "Hello from OAuth!");
    let headers = state.auth_headers.lock().unwrap().clone();
    assert!(!headers.is_empty());
    assert!(
        headers.iter().all(|h| h == "Bearer signin-tok-1"),
        "unexpected headers: {headers:?}"
    );

    callback_port_is_closed(&opened[0]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_out_then_sign_in_reuses_the_registration() {
    let (base, state) = spawn_server(ServerState {
        prm_scopes: std::sync::Mutex::new(Some(vec!["prompts.read"])),
        ..ServerState::default()
    })
    .await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Approve);

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    harness
        .service
        .sign_out_mcp_provider("tiddly")
        .await
        .unwrap();

    // Signed out: no token, needs sign-in — but the registration survives.
    let info = harness
        .service
        .list_mcp_providers()
        .into_iter()
        .find(|p| p.name == "tiddly")
        .unwrap();
    assert!(!info.has_token);
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert_eq!(envelope["registration"]["client_id"], "dyn-client-1");
    assert!(envelope["tokens"].is_null());
    harness.service.sync().await;
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::NeedsAuth
    );

    // Second sign-in: zero further registrations, same client id, and its own
    // fresh ephemeral listener (each exchange names the concrete redirect its
    // own authorization used).
    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 1);
    let opened = harness.opener.opened();
    assert_eq!(opened.len(), 2);
    assert_eq!(query_params(&opened[1])["client_id"], "dyn-client-1");
    let token_bodies = state.token_bodies.lock().unwrap().clone();
    assert_eq!(token_bodies.len(), 2);
    for (authorize_url, token_body) in opened.iter().zip(&token_bodies) {
        assert_eq!(
            form_params(token_body)["redirect_uri"],
            query_params(authorize_url)["redirect_uri"]
        );
    }

    harness.service.sync().await;
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::Ok { prompt_count: 1 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scopes_override_beats_challenge_and_resource_metadata() {
    let (base, _state) = spawn_server(ServerState {
        prm_scopes: std::sync::Mutex::new(Some(vec!["gamma"])),
        challenge_scope: Some("beta"),
        ..ServerState::default()
    })
    .await;
    let config = format!(
        "mcp_providers:\n  - name: tiddly\n    transport:\n      type: http\n      url: {base}/mcp\n    auth:\n      type: oauth\n      scopes: [alpha]\n"
    );
    let harness = harness(&config, DriveMode::Approve);

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    assert_eq!(query_params(&harness.opener.opened()[0])["scope"], "alpha");
}

#[tokio::test(flavor = "multi_thread")]
async fn challenge_scope_beats_resource_metadata_scopes() {
    let (base, _state) = spawn_server(ServerState {
        prm_scopes: std::sync::Mutex::new(Some(vec!["gamma"])),
        challenge_scope: Some("beta1 beta2"),
        ..ServerState::default()
    })
    .await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::Approve,
    );

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    assert_eq!(
        query_params(&harness.opener.opened()[0])["scope"],
        "beta1 beta2"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_advertised_scopes_omits_the_scope_parameter_entirely() {
    // No override, no challenge, no `scopes_supported`: the scope parameter is
    // absent from both registration and authorization — never an empty string,
    // never rmcp's AS-`scopes_supported` over-ask.
    let (base, state) = spawn_server(ServerState::default()).await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::Approve,
    );

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();

    let params = query_params(&harness.opener.opened()[0]);
    assert!(!params.contains_key("scope"), "{params:?}");
    let registration = state.register_bodies.lock().unwrap()[0].clone();
    assert!(registration.get("scope").is_none(), "{registration}");
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_consent_fails_readably_and_keeps_a_reusable_registration() {
    let (base, state) = spawn_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Deny);

    let err = harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap_err();
    match &err {
        PromptError::OAuthFlow { provider, message } => {
            assert_eq!(provider, "tiddly");
            assert!(message.contains("access_denied"), "{message}");
            assert!(message.contains("the user declined"), "{message}");
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }

    // No tokens — but the registration persisted before the browser opened, so
    // the denied attempt is not an orphaned server-side client.
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert_eq!(envelope["registration"]["client_id"], "dyn-client-1");
    assert!(envelope["tokens"].is_null());
    assert_eq!(state.token_hits.load(Ordering::SeqCst), 0);
    callback_port_is_closed(&harness.opener.opened()[0]).await;

    // A retry (fresh service, approving user) reuses that registration.
    let (retry_service, retry_opener) = harness.sibling_service(DriveMode::Approve);
    retry_service.sign_in_mcp_provider("tiddly").await.unwrap();
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        query_params(&retry_opener.opened()[0])["client_id"],
        "dyn-client-1"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn abandoned_sign_in_times_out_with_no_credentials_stored() {
    let (base, state) = spawn_server(ServerState::default()).await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::Abandon,
    );
    let service = harness
        .service
        .clone()
        .with_sign_in_timeout(Duration::from_millis(400));

    let err = service.sign_in_mcp_provider("tiddly").await.unwrap_err();
    match &err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(message.contains("was not completed"), "{message}");
            // This flow registered fresh, so the message carries the
            // registration-updated caveat.
            assert!(message.contains("already cleared"), "{message}");
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }

    // No exchange ran, no tokens stored; the listener is torn down.
    assert_eq!(state.token_hits.load(Ordering::SeqCst), 0);
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert!(envelope["tokens"].is_null());
    callback_port_is_closed(&harness.opener.opened()[0]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_state_callback_is_ignored_and_no_token_request_is_made() {
    let (base, state) = spawn_server(ServerState::default()).await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::WrongState,
    );
    let service = harness
        .service
        .clone()
        .with_sign_in_timeout(Duration::from_millis(500));

    // The listener drops the state-mismatched callback as noise (it does not
    // belong to this flow), so the sign-in ends in the callback timeout.
    let err = service.sign_in_mcp_provider("tiddly").await.unwrap_err();
    match &err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(message.contains("was not completed"), "{message}");
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }

    // The forged callback never reached the exchange; nothing was stored.
    assert_eq!(state.token_hits.load(Ordering::SeqCst), 0);
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert!(envelope["tokens"].is_null());
    callback_port_is_closed(&harness.opener.opened()[0]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_error_callback_does_not_kill_a_live_sign_in() {
    // A hostile local process fires an `error` callback (without the flow's
    // state) before the real redirect arrives: the forgery must be ignored
    // and the real, state-matched approval must still complete the sign-in.
    let (base, _state) = spawn_server(ServerState::default()).await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::ForgedErrorThenApprove,
    );

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert!(!envelope["tokens"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn rmcp_rejects_an_unknown_state_at_exchange() {
    // The defense-in-depth layer behind the listener's state filter: rmcp's
    // own exchange validates the state against its flow-state store. The
    // listener makes this unreachable through a real flow (mismatches are
    // dropped before the exchange), so this pins the behavior directly — an
    // rmcp upgrade that silently stopped checking would fail here instead of
    // silently widening our attack surface.
    let mut manager = rmcp::transport::auth::AuthorizationManager::new("http://127.0.0.1:9/mcp")
        .await
        .unwrap();
    let metadata: rmcp::transport::auth::AuthorizationMetadata =
        serde_json::from_value(serde_json::json!({
            "issuer": "http://127.0.0.1:9",
            "authorization_endpoint": "http://127.0.0.1:9/authorize",
            "token_endpoint": "http://127.0.0.1:9/token",
            "registration_endpoint": "http://127.0.0.1:9/register",
            "code_challenge_methods_supported": ["S256"],
        }))
        .unwrap();
    manager.set_metadata(metadata);
    manager
        .configure_client(rmcp::transport::auth::OAuthClientConfig::new(
            "client-1",
            "http://127.0.0.1:1/callback",
        ))
        .unwrap();

    // No authorization was started, so no state exists: the exchange must
    // fail before any network request (port 9 would refuse anyway).
    let err = manager
        .exchange_code_for_token("some-code", "unknown-state")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("state"),
        "expected a state-lookup failure, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_without_an_injected_opener_fails_fast() {
    // A service that never got `with_browser_opener` (the default stub) must
    // fail with a readable error instead of waiting out the callback timeout
    // for a browser that was never opened.
    let (base, _state) = spawn_server(ServerState::default()).await;
    let tmp = TempDir::new().unwrap();
    let prompts_dir = tmp.path().join("prompts");
    std::fs::create_dir(&prompts_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, oauth_yaml("tiddly", &format!("{base}/mcp"))).unwrap();
    let service = PromptService::new(
        config_path,
        prompts_dir,
        None,
        Arc::new(InMemorySecretStore::new()),
    );

    let err = service.sign_in_mcp_provider("tiddly").await.unwrap_err();
    match &err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(message.contains("no browser opener"), "{message}");
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }
}

/// The raw envelope JSON with long-expired tokens, so the next
/// `get_access_token` must refresh. The registration carries a full
/// compatibility fingerprint (`issuer` = the fake server's base, no scopes),
/// so a sign-in against a no-scopes server reuses it instead of
/// re-registering.
fn expired_envelope(resource: &str, issuer: &str) -> String {
    serde_json::json!({
        "registration": {
            "client_id": "client-1",
            "redirect_uri": "http://127.0.0.1/callback",
            "resource": resource,
            "scopes": [],
            "issuer": issuer,
        },
        "tokens": {
            "client_id": "client-1",
            "token_response": {
                "access_token": "stale-tok",
                "token_type": "bearer",
                "expires_in": 1,
                "refresh_token": "seeded-refresh",
            },
            "granted_scopes": [],
            "token_received_at": 1,
        },
    })
    .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_in_flight_when_sign_out_begins_cannot_resurrect_tokens() {
    // The interleave sign-out must exclude: a refresh reads stale tokens,
    // sign-out clears the envelope, then the refresh persists its rotated
    // tokens — leaving the user "signed out" with live credentials in the
    // keychain. Holding the cached client's auth-manager mutex across
    // clear + invalidate forces the refresh to fully finish first, and its
    // just-persisted tokens are then cleared.
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (base, state) = spawn_server(ServerState {
        token_gate: Some(TokenGate {
            entered: entered_tx,
            release: release.clone(),
            refresh_only: true,
        }),
        ..ServerState::default()
    })
    .await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Approve);
    harness
        .secrets
        .set("oauth:tiddly", &expired_envelope(&mcp_url, &base))
        .unwrap();

    // A render finds the expired token and enters the refresh; the token
    // endpoint parks it mid-flight, holding the auth-manager mutex.
    let render_service = harness.service.clone();
    let render = tokio::spawn(async move {
        render_service
            .render("tiddly", "greet", &BTreeMap::new())
            .await
    });
    entered_rx.recv().await.unwrap();

    // Sign-out begins while the refresh is parked: it must wait, not clear
    // ahead of a save that would then resurrect the tokens.
    let sign_out_service = harness.service.clone();
    let sign_out =
        tokio::spawn(async move { sign_out_service.sign_out_mcp_provider("tiddly").await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !sign_out.is_finished(),
        "sign-out must serialize behind the in-flight refresh"
    );

    // Release the refresh: it completes and persists rotated tokens, then
    // sign-out clears them. The render itself may legitimately fail — the
    // queued sign-out acquires the manager mutex ahead of the render's MCP
    // request, which then finds no credentials; the invariant under test is
    // the store's final state, not the racing render's outcome.
    release.add_permits(1);
    let _ = render.await.unwrap();
    sign_out.await.unwrap().unwrap();

    assert_eq!(state.token_hits.load(Ordering::SeqCst), 1);
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert!(
        envelope["tokens"].is_null(),
        "rotated tokens must not survive sign-out: {envelope}"
    );
    assert_eq!(envelope["registration"]["client_id"], "client-1");
    let info = harness
        .service
        .list_mcp_providers()
        .into_iter()
        .find(|p| p.name == "tiddly")
        .unwrap();
    assert!(!info.has_token);
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_out_when_never_signed_in_is_a_clean_no_op() {
    let (base, _state) = spawn_server(ServerState::default()).await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::Approve,
    );

    // No envelope, no cached client: sign-out succeeds and stores nothing.
    harness
        .service
        .sign_out_mcp_provider("tiddly")
        .await
        .unwrap();
    assert!(stored_envelope(&harness.secrets, "tiddly").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_server_scopes_re_register_instead_of_reusing() {
    // The M5-shaped scenario: the server's advertised `scopes_supported`
    // changes between two sign-ins. Reusing the old registration would
    // authorize with scopes it doesn't carry — a failure that happens in the
    // browser, invisibly and permanently. The fingerprint check must
    // re-register instead.
    let (base, state) = spawn_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Approve);

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 1);

    // The server starts advertising scopes (tiddly's M5 deploy).
    *state.prm_scopes.lock().unwrap() = Some(vec!["prompts.read"]);

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    assert_eq!(
        state.register_hits.load(Ordering::SeqCst),
        2,
        "a scope change must produce exactly one new registration"
    );
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert_eq!(envelope["registration"]["client_id"], "dyn-client-2");
    assert_eq!(
        envelope["registration"]["scopes"],
        serde_json::json!(["prompts.read"])
    );
    assert!(!envelope["tokens"].is_null());
    // The second authorization used the new registration and scopes.
    let second = query_params(&harness.opener.opened()[1]);
    assert_eq!(second["client_id"], "dyn-client-2");
    assert_eq!(second["scope"], "prompts.read");
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_redirect_or_issuer_re_registers() {
    let (base, state) = spawn_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Approve);
    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 1);

    // A registration recorded for a different redirect form (e.g. after a
    // hypothetical constant change) must not be reused — the authorization
    // server never saw the current form.
    let mut envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    envelope["registration"]["redirect_uri"] = "http://localhost/callback".into();
    harness
        .secrets
        .set("oauth:tiddly", &envelope.to_string())
        .unwrap();
    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 2);

    // Likewise a registration created against a different authorization
    // server: its client id means nothing to the current one.
    let mut envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    envelope["registration"]["issuer"] = "https://other.example.com".into();
    harness
        .secrets
        .set("oauth:tiddly", &envelope.to_string())
        .unwrap();
    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn timed_out_exchange_cannot_store_credentials_later() {
    // The staged-commit guarantee: when the exchange's network step times
    // out, releasing the parked server response afterwards must not deposit
    // credentials — the staging store died with the flow.
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (base, _state) = spawn_server(ServerState {
        token_gate: Some(TokenGate {
            entered: entered_tx,
            release: release.clone(),
            refresh_only: false,
        }),
        ..ServerState::default()
    })
    .await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::Approve,
    );
    // Shrinks the exchange budget (and every HTTP round-trip bound).
    let service = harness
        .service
        .clone()
        .with_provider_timeout(Duration::from_millis(500));

    let err = service.sign_in_mcp_provider("tiddly").await.unwrap_err();
    match &err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(message.contains("token exchange timed out"), "{message}");
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }
    entered_rx.recv().await.unwrap();

    // Release the parked exchange response; give any (wrongly) surviving
    // write a moment to land, then prove nothing did.
    release.add_permits(1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert!(
        envelope["tokens"].is_null(),
        "a timed-out exchange must never store credentials: {envelope}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn commit_failure_is_reported_as_storage_not_success() {
    /// Lets the registration write through, then fails the token commit —
    /// the wedged-keychain-at-the-worst-moment case.
    struct FailSecondOAuthWrite {
        inner: InMemorySecretStore,
        oauth_writes: AtomicUsize,
    }
    impl SecretStore for FailSecondOAuthWrite {
        fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
            if key == "oauth:tiddly" && self.oauth_writes.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(SecretStoreError::Backend("keychain wedged".to_owned()));
            }
            self.inner.set(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
            self.inner.delete(key)
        }
    }

    let (base, _state) = spawn_server(ServerState::default()).await;
    let tmp = TempDir::new().unwrap();
    let prompts_dir = tmp.path().join("prompts");
    std::fs::create_dir(&prompts_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, oauth_yaml("tiddly", &format!("{base}/mcp"))).unwrap();
    let store = Arc::new(FailSecondOAuthWrite {
        inner: InMemorySecretStore::new(),
        oauth_writes: AtomicUsize::new(0),
    });
    let opener = DrivingOpener::new(DriveMode::Approve);
    let service = PromptService::new(config_path, prompts_dir, None, store.clone())
        .with_browser_opener(opener.clone());

    // Write #1 (registration) succeeds, write #2 (the commit) fails: the
    // error must name credential storage — the exchange itself succeeded.
    let err = service.sign_in_mcp_provider("tiddly").await.unwrap_err();
    match &err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(
                message.contains("storing its credentials failed"),
                "{message}"
            );
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }
    // Registration persisted; tokens did not.
    let raw = store.inner.get("oauth:tiddly").unwrap().unwrap();
    let envelope: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(envelope["registration"]["client_id"], "dyn-client-1");
    assert!(envelope["tokens"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_flows_are_refused_not_queued() {
    // A second sign-in (double-click) or a sign-out during a pending sign-in
    // must fail immediately with a readable in-progress error — queueing
    // would open a surprise second browser tab, or wait out the flow and then
    // destroy the tokens the user just obtained.
    let (base, _state) = spawn_server(ServerState::default()).await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::Abandon,
    );
    let service = harness
        .service
        .clone()
        .with_sign_in_timeout(Duration::from_secs(5));

    let pending_service = service.clone();
    let pending = tokio::spawn(async move { pending_service.sign_in_mcp_provider("tiddly").await });
    // The flow is provably past the gate once the opener has been driven.
    while harness.opener.opened().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    for result in [
        service.sign_in_mcp_provider("tiddly").await,
        service.sign_out_mcp_provider("tiddly").await,
    ] {
        match result.unwrap_err() {
            PromptError::OAuthFlow { message, .. } => {
                assert!(message.contains("already in progress"), "{message}");
            }
            other => panic!("expected OAuthFlow, got {other:?}"),
        }
    }

    // The pending flow is unaffected by the refusals (it still times out on
    // the abandoned callback rather than being disturbed).
    pending.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_commit_waits_for_an_in_flight_refresh() {
    // Sign-in racing a refresh on the cached client: the commit step takes
    // the cached client's auth-manager mutex, so the refresh fully persists
    // first and the sign-in's exchange tokens land last — the two writes
    // serialize instead of interleaving.
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (base, state) = spawn_server(ServerState {
        token_gate: Some(TokenGate {
            entered: entered_tx,
            release: release.clone(),
            refresh_only: true,
        }),
        ..ServerState::default()
    })
    .await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Approve);
    harness
        .secrets
        .set("oauth:tiddly", &expired_envelope(&mcp_url, &base))
        .unwrap();

    // A render caches the client and parks inside the refresh, holding the
    // auth-manager mutex.
    let render_service = harness.service.clone();
    let render = tokio::spawn(async move {
        render_service
            .render("tiddly", "greet", &BTreeMap::new())
            .await
    });
    entered_rx.recv().await.unwrap();

    // Sign-in reuses the (fingerprint-compatible) registration, exchanges its
    // code (the authorization-code grant passes the refresh-only gate), and
    // must then block at the commit until the refresh completes.
    let sign_in_service = harness.service.clone();
    let sign_in = tokio::spawn(async move { sign_in_service.sign_in_mcp_provider("tiddly").await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !sign_in.is_finished(),
        "the commit must serialize behind the in-flight refresh"
    );

    // Release the refresh: it persists its rotated tokens, then the commit
    // overwrites them with the sign-in's exchange tokens — the last write is
    // the sign-in's, so the user's explicit action wins.
    release.add_permits(1);
    let _ = render.await.unwrap();
    sign_in.await.unwrap().unwrap();

    assert_eq!(state.register_hits.load(Ordering::SeqCst), 0, "reused");
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    // The exchange hit the token endpoint first (the refresh was parked
    // before the counter), so its tokens are `signin-tok-1`.
    assert_eq!(
        envelope["tokens"]["token_response"]["access_token"],
        "signin-tok-1"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_dcr_leaves_cached_client_and_tokens_intact() {
    // A re-registration attempt whose DCR the server rejects changes nothing
    // locally: the envelope (old client id AND its tokens) survives, and the
    // cached session client must NOT be retired — invalidating it would let a
    // later rebuild coexist with its in-flight operations.
    let (base, state) = spawn_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Approve);

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    // Cache the session client (sign-in itself never caches one).
    harness.service.sync().await;
    assert_eq!(
        provider_status(&harness.service, "tiddly"),
        ProviderStatus::Ok { prompt_count: 1 }
    );

    // The server changes its advertised scopes (forcing re-registration) but
    // rejects the new registration.
    *state.prm_scopes.lock().unwrap() = Some(vec!["prompts.read"]);
    state.register_fail.store(true, Ordering::SeqCst);
    let err = harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap_err();
    match &err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(
                message.contains("dynamic client registration failed"),
                "{message}"
            );
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }

    // Envelope untouched: old client id, tokens still present.
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert_eq!(envelope["registration"]["client_id"], "dyn-client-1");
    assert!(!envelope["tokens"].is_null());

    // The cached client survived: a render succeeds with zero additional
    // discovery (a retired cache would rebuild and re-fetch).
    let discovery_before = state.prm_fetches.load(Ordering::SeqCst);
    let rendered = harness
        .service
        .render("tiddly", "greet", &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(rendered.text, "Hello from OAuth!");
    assert_eq!(state.prm_fetches.load(Ordering::SeqCst), discovery_before);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_registration_persistence_leaves_cached_client_and_tokens_intact() {
    /// Fails the third write to the OAuth envelope: the first sign-in's
    /// registration (1) and commit (2) pass; the re-registration's
    /// persistence (3) fails.
    struct FailThirdOAuthWrite {
        inner: InMemorySecretStore,
        oauth_writes: AtomicUsize,
    }
    impl SecretStore for FailThirdOAuthWrite {
        fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
            if key == "oauth:tiddly" && self.oauth_writes.fetch_add(1, Ordering::SeqCst) == 2 {
                return Err(SecretStoreError::Backend("keychain wedged".to_owned()));
            }
            self.inner.set(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
            self.inner.delete(key)
        }
    }

    let (base, state) = spawn_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let tmp = TempDir::new().unwrap();
    let prompts_dir = tmp.path().join("prompts");
    std::fs::create_dir(&prompts_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, oauth_yaml("tiddly", &mcp_url)).unwrap();
    let store = Arc::new(FailThirdOAuthWrite {
        inner: InMemorySecretStore::new(),
        oauth_writes: AtomicUsize::new(0),
    });
    let opener = DrivingOpener::new(DriveMode::Approve);
    let service = PromptService::new(config_path, prompts_dir, None, store.clone())
        .with_browser_opener(opener.clone());

    service.sign_in_mcp_provider("tiddly").await.unwrap();
    service.sync().await;

    // Force re-registration; DCR succeeds (a server-side orphan, accepted)
    // but persisting the replacement fails — no local identity change.
    *state.prm_scopes.lock().unwrap() = Some(vec!["prompts.read"]);
    let err = service.sign_in_mcp_provider("tiddly").await.unwrap_err();
    match &err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(
                message.contains("registration persistence failed"),
                "{message}"
            );
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 2);

    // Envelope untouched; cached client survived (no rebuild on render).
    let raw = store.inner.get("oauth:tiddly").unwrap().unwrap();
    let envelope: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(envelope["registration"]["client_id"], "dyn-client-1");
    assert!(!envelope["tokens"].is_null());
    let discovery_before = state.prm_fetches.load(Ordering::SeqCst);
    let rendered = service
        .render("tiddly", "greet", &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(rendered.text, "Hello from OAuth!");
    assert_eq!(state.prm_fetches.load(Ordering::SeqCst), discovery_before);
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_commit_serializes_with_a_client_built_during_the_browser_wait() {
    // The cold-cache variant of the commit/refresh race: no client exists
    // when sign-in starts; one is built (and starts refreshing) while the
    // user is "in the browser". The commit re-samples under the client gate,
    // so the mid-flow client is found and its in-flight refresh fully
    // persists before the sign-in's tokens land last.
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (base, state) = spawn_server(ServerState {
        token_gate: Some(TokenGate {
            entered: entered_tx,
            release: release.clone(),
            refresh_only: true,
        }),
        ..ServerState::default()
    })
    .await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Capture);
    harness
        .secrets
        .set("oauth:tiddly", &expired_envelope(&mcp_url, &base))
        .unwrap();

    // Sign-in starts against an empty cache and parks at the browser wait.
    let sign_in_service = harness.service.clone();
    let sign_in = tokio::spawn(async move { sign_in_service.sign_in_mcp_provider("tiddly").await });
    while harness.opener.opened().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A render builds and caches the client mid-flow, then parks inside its
    // refresh, holding the auth-manager mutex.
    let render_service = harness.service.clone();
    let render = tokio::spawn(async move {
        render_service
            .render("tiddly", "greet", &BTreeMap::new())
            .await
    });
    entered_rx.recv().await.unwrap();

    // Deliver the real redirect: the exchange passes (authorization-code
    // grants aren't gated), and the commit must block on the mid-flow
    // client's refresh — a client the old flow-start snapshot never saw.
    let params = query_params(&harness.opener.opened()[0]);
    hit_callback(format!(
        "{}?code=test-code&state={}",
        params["redirect_uri"], params["state"]
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !sign_in.is_finished(),
        "the commit must serialize behind the mid-flow client's refresh"
    );

    // Release: refresh persists first, then the commit overwrites with the
    // sign-in's exchange tokens (the exchange hit the endpoint first, so its
    // tokens are signin-tok-1).
    release.add_permits(1);
    let _ = render.await.unwrap();
    sign_in.await.unwrap().unwrap();
    assert_eq!(state.register_hits.load(Ordering::SeqCst), 0, "reused");
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert_eq!(
        envelope["tokens"]["token_response"]["access_token"],
        "signin-tok-1"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn removal_is_refused_while_a_flow_is_in_progress() {
    // A removal mid-sign-in would let the flow's registration persistence
    // recreate the just-deleted envelope (and later store tokens for a
    // removed provider) — so removal must refuse while the flow gate is held.
    let (base, _state) = spawn_server(ServerState::default()).await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::Abandon,
    );
    let service = harness
        .service
        .clone()
        .with_sign_in_timeout(Duration::from_secs(2));

    let pending_service = service.clone();
    let pending = tokio::spawn(async move { pending_service.sign_in_mcp_provider("tiddly").await });
    while harness.opener.opened().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let err = harness.service.remove_mcp_provider("tiddly").unwrap_err();
    match err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(message.contains("already in progress"), "{message}");
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }
    assert_eq!(harness.service.list_mcp_providers().len(), 1);

    // Once the flow ends, removal succeeds and everything is gone.
    assert!(pending.await.unwrap().is_err());
    harness.service.remove_mcp_provider("tiddly").unwrap();
    assert!(harness.service.list_mcp_providers().is_empty());
    assert!(stored_envelope(&harness.secrets, "tiddly").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_request_line_is_dropped_not_parsed() {
    // A request line past the listener's read cap comes back truncated and
    // unterminated; parsing its prefix as a complete target would be wrong.
    // It must be dropped — and the real callback right behind it must still
    // complete the sign-in.
    let (base, _state) = spawn_server(ServerState::default()).await;
    let harness = harness(
        &oauth_yaml("tiddly", &format!("{base}/mcp")),
        DriveMode::OversizedThenApprove,
    );

    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert!(!envelope["tokens"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn abandoned_reuse_sign_in_reports_state_unchanged() {
    // The common closed-the-tab case on the reuse path must NOT read as
    // though something was destroyed — the caveat about a replaced
    // registration attaches only when the flow actually re-registered.
    let (base, _state) = spawn_server(ServerState::default()).await;
    let mcp_url = format!("{base}/mcp");
    let harness = harness(&oauth_yaml("tiddly", &mcp_url), DriveMode::Approve);
    harness
        .service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap();

    let (retry_service, _retry_opener) = harness.sibling_service(DriveMode::Abandon);
    let retry_service = retry_service.with_sign_in_timeout(Duration::from_millis(400));
    let err = retry_service
        .sign_in_mcp_provider("tiddly")
        .await
        .unwrap_err();
    match &err {
        PromptError::OAuthFlow { message, .. } => {
            assert!(message.contains("existing state is unchanged"), "{message}");
            assert!(!message.contains("already cleared"), "{message}");
        }
        other => panic!("expected OAuthFlow, got {other:?}"),
    }
    // And the original tokens really are untouched.
    let envelope = stored_envelope(&harness.secrets, "tiddly").unwrap();
    assert!(!envelope["tokens"].is_null());
}
