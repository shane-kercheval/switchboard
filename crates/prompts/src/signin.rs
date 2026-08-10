//! The browser sign-in flow for OAuth MCP providers: loopback callback
//! listener, dynamic client registration (with fingerprint-checked reuse),
//! authorization, and the token exchange with a staged durable commit.
//!
//! **Why the flow drives `AuthorizationManager` directly** rather than rmcp's
//! `OAuthState`: `start_authorization` rediscovers metadata unconditionally
//! (defeating the preflight's validation boundary) and always registers a new
//! client (defeating registration reuse). The PKCE verifier lives in an
//! in-memory state store *inside* the manager, so everything from
//! `get_authorization_url` through `exchange_code_for_token` happens inside
//! this one owned async operation — never split across two IPC commands.
//!
//! **Why the redirect is registered port-less but authorized with a concrete
//! port.** RFC 8252 §7.3 requires an authorization server to allow any port
//! for a loopback redirect. Registering `http://127.0.0.1/callback` once and
//! authorizing with `http://127.0.0.1:<ephemeral>/callback` per sign-in is
//! what lets a fresh ephemeral listener coexist with a reusable registration.
//! A fixed reserved port was considered and rejected: it buys nothing here and
//! makes sign-in fail whenever another instance holds the port.
//!
//! **Why the exchange saves into a staging store.** The token exchange is
//! bounded by a timeout (rmcp builds its own unbounded HTTP client for it),
//! and a timed-out future cannot cancel a keychain write already running on
//! the blocking pool — so if the exchange persisted directly, "sign-in timed
//! out" could be reported while credentials quietly land afterwards. Staging
//! keeps the bounded step free of durable writes; the real commit runs
//! afterwards, **unbounded**, and what it returns is what the user is told.
//!
//! **Never log the callback query string** — it carries the authorization
//! code. Errors surface the server's `error`/`error_description` text, never
//! `code` or `state` values. (rmcp itself debug-logs the raw code during the
//! exchange; the app's logging layer denies that module's debug output — see
//! `crates/app`.)

use std::sync::Arc;
use std::time::Duration;

use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, CredentialStore, OAuthClientConfig,
    StoredCredentials,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::error::PromptError;
use crate::oauth::{CredentialLifecycle, OAuthRegistration, ProviderCredentialStore};
use crate::preflight::{as_identifier, preflight};

/// Opens a URL in the user's browser. Injected into [`crate::PromptService`]
/// (`crates/prompts` stays Tauri-free; the app implements this over its
/// existing validated OS opener) so tests can capture the authorization URL
/// and drive the callback instead of launching anything.
#[async_trait::async_trait]
pub trait BrowserOpener: Send + Sync {
    /// Open `url`; the error string is shown to the user.
    async fn open(&self, url: &str) -> Result<(), String>;
}

/// The default opener for services constructed without one (tests, the
/// disabled service): sign-in fails fast instead of silently doing nothing.
pub(crate) struct NoBrowserOpener;

#[async_trait::async_trait]
impl BrowserOpener for NoBrowserOpener {
    async fn open(&self, _url: &str) -> Result<(), String> {
        Err("no browser opener is configured in this context".to_owned())
    }
}

/// The client name sent at dynamic registration — what the authorization
/// server shows on its consent page and in its application list.
const CLIENT_NAME: &str = "Switchboard";

/// The redirect URI *as registered*: port-less loopback (see module docs).
const REGISTERED_REDIRECT_URI: &str = "http://127.0.0.1/callback";

/// Per-connection budget for reading the callback request. Browsers open
/// speculative connections that never send a request; without this bound one
/// would park its handler task until the whole flow times out.
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on what one callback connection may stream at the listener — a local
/// process could otherwise feed an endless header line for the whole read
/// window, growing a string without bound.
const MAX_REQUEST_BYTES: u64 = 8 * 1024;

/// How long to back off after a failed `accept` before retrying, and how many
/// consecutive failures to tolerate before declaring the listener unusable.
/// Without both, a persistently failing `accept` (file-descriptor exhaustion
/// is the realistic case) would spin this loop at full speed for the whole
/// callback wait; with them, a transient error recovers and a persistent one
/// becomes a diagnosable failure — an unusable listener means the callback
/// can never arrive, so failing beats waiting.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_CONSECUTIVE_ACCEPT_FAILURES: u32 = 10;

/// Everything the flow needs, injected by `PromptService` (which owns the
/// gate/lock discipline around this call — see `CredentialLifecycle`).
pub(crate) struct SignInRequest<'a> {
    /// The shared bounded client (no redirects); also injected into the
    /// manager so registration and discovery respect the provider budget.
    pub http: &'a reqwest::Client,
    pub opener: &'a dyn BrowserOpener,
    pub store: &'a ProviderCredentialStore,
    /// The provider's lifecycle state — the commit takes its `client_gate` so
    /// no session client can finish construction while the tokens land.
    pub lifecycle: &'a CredentialLifecycle,
    /// Looks up the provider's *currently* cached session client. Called at
    /// commit time (under the `client_gate`), never sampled ahead of the
    /// browser wait: a client built during the flow must still be seen, so
    /// its in-flight token refresh fully finishes (and persists) before the
    /// sign-in's tokens land — the two writes serialize instead of
    /// interleaving.
    pub resample_cached_client: &'a (dyn Fn() -> Option<AuthClient<reqwest::Client>> + Sync),
    pub provider: &'a str,
    pub canonical_url: &'a str,
    pub scopes_override: Option<&'a [String]>,
    /// Bound on the token exchange's network step: rmcp builds its own HTTP
    /// client for that request, so the injected client's timeout does not
    /// apply there. The durable commit is deliberately not bounded (see the
    /// module docs).
    pub exchange_timeout: Duration,
    /// Bound on waiting for the browser callback — generous, the user may be
    /// completing MFA.
    pub callback_timeout: Duration,
}

/// Run the whole sign-in: bind the loopback listener, preflight, register (or
/// reuse) the client, open the browser, await the callback, exchange the
/// code, and durably commit the tokens. The listener is owned by this future,
/// so it is closed on **every** exit path — success, denial, timeout, or
/// error.
///
/// Returns whether the flow **re-registered** (persisting a new client id)
/// alongside the outcome — reported even when a later step failed, because
/// the registration change is already durable and the caller must retire any
/// session client built for the previous client id.
pub(crate) async fn run_sign_in(request: SignInRequest<'_>) -> (bool, Result<(), PromptError>) {
    let mut registration_changed = false;
    let result = drive_sign_in(&request, &mut registration_changed).await;
    (registration_changed, result)
}

async fn drive_sign_in(
    request: &SignInRequest<'_>,
    registration_changed: &mut bool,
) -> Result<(), PromptError> {
    // Bound first: the concrete redirect URI must be known before the client
    // is configured, and binding is the one step with no external dependency.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| {
        flow_err(
            request.provider,
            format!("could not bind the local callback listener: {e}"),
        )
    })?;
    let port = listener
        .local_addr()
        .map_err(|e| {
            flow_err(
                request.provider,
                format!("could not read the callback listener address: {e}"),
            )
        })?
        .port();

    let outcome = preflight(
        request.http,
        request.provider,
        request.canonical_url,
        request.scopes_override,
    )
    .await?;
    // `None` means "omit the scope parameter entirely" (see
    // `PreflightOutcome::scopes`); an empty Vec produces exactly that at both
    // registration and authorization. Normalized (sorted, deduplicated) once,
    // so the set sent to the server and the set recorded in the registration
    // fingerprint are the same value. Because this flow bypasses
    // `start_authorization`, rmcp's `add_offline_access_if_supported` never
    // runs — nothing silently appends `offline_access`, so a server that
    // wants refresh tokens must advertise the scope itself.
    let mut scopes = outcome.scopes.unwrap_or_default();
    scopes.sort();
    scopes.dedup();
    // Unreachable past the preflight's issuer gate — but if that invariant
    // ever weakened, a defaulted empty issuer would make every stored
    // registration look incompatible, silently re-registering (and wiping
    // tokens) on every sign-in. Fail loudly instead.
    let issuer = outcome.metadata.issuer.clone().ok_or_else(|| {
        flow_err(
            request.provider,
            "authorization-server metadata unexpectedly lacks an issuer".to_owned(),
        )
    })?;

    let mut manager = AuthorizationManager::new(request.canonical_url)
        .await
        .map_err(|e| auth_err(request.provider, "sign-in setup", &e))?;
    manager
        .with_client(request.http.clone())
        .map_err(|e| auth_err(request.provider, "sign-in setup", &e))?;
    // The exchange persists through this staging store, never the real one —
    // see the module docs for why the bounded step must stay write-free.
    let staging = StagingCredentialStore::default();
    manager.set_credential_store(staging.clone());
    // The preflighted metadata is installed so the manager never re-discovers
    // an unvalidated copy.
    manager.set_metadata(outcome.metadata);

    let client_id = resolve_client_id(
        &mut manager,
        request,
        &scopes,
        &issuer,
        registration_changed,
    )
    .await?;
    manager
        .configure_client(
            OAuthClientConfig::new(client_id, format!("http://127.0.0.1:{port}/callback"))
                .with_scopes(scopes.clone()),
        )
        .map_err(|e| auth_err(request.provider, "client configuration", &e))?;

    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    // Adds PKCE (S256), `state`, and the RFC 8707 `resource` parameter, and
    // parks the verifier in the manager's in-memory state store.
    let authorization_url = manager
        .get_authorization_url(&scope_refs)
        .await
        .map_err(|e| auth_err(request.provider, "authorization URL", &e))?;
    // The `state` this flow minted, recovered from the URL it lives in: the
    // listener drops any callback that doesn't echo it (success *and* error
    // forms), so a stale tab or a forging local process cannot terminate the
    // flow — rmcp's exchange-time state lookup stays as defense in depth.
    let expected_state = query_param(&authorization_url, "state").ok_or_else(|| {
        flow_err(
            request.provider,
            "the authorization URL carries no state parameter".to_owned(),
        )
    })?;
    request
        .opener
        .open(&authorization_url)
        .await
        .map_err(|e| flow_err(request.provider, format!("could not open the browser: {e}")))?;

    exchange_and_commit(
        request,
        &manager,
        &staging,
        &listener,
        &expected_state,
        *registration_changed,
    )
    .await
}

/// The suffix for abandoned/timed-out flows, precise about what did and did
/// not change: no tokens were stored either way, but a flow that re-registered
/// has already (deliberately) cleared any previously stored authorization —
/// without the condition, the common closed-the-tab case would read as though
/// something was destroyed.
fn no_tokens_note(registration_changed: bool) -> &'static str {
    if registration_changed {
        " No sign-in tokens were stored; this flow had updated the provider's client \
         registration, so any previously stored authorization was already cleared"
    } else {
        " No sign-in tokens were stored and the provider's existing state is unchanged"
    }
}

/// The post-browser half of the flow: await the state-matched callback,
/// exchange the code (bounded, into the staging store), and durably commit
/// (unbounded, into the real store).
async fn exchange_and_commit(
    request: &SignInRequest<'_>,
    manager: &AuthorizationManager,
    staging: &StagingCredentialStore,
    listener: &TcpListener,
    expected_state: &str,
    registration_changed: bool,
) -> Result<(), PromptError> {
    let callback = tokio::time::timeout(
        request.callback_timeout,
        await_callback(listener, expected_state),
    )
    .await
    .map_err(|_| {
        flow_err(
            request.provider,
            format!(
                "the browser sign-in was not completed within {}s. If the browser showed an \
                 error page, the server may have reported a failure this app could not \
                 attribute to the pending sign-in.{}",
                request.callback_timeout.as_secs(),
                no_tokens_note(registration_changed)
            ),
        )
    })?
    .map_err(|reason| flow_err(request.provider, reason))?;

    // Validates `state` against the stored flow state, exchanges the code
    // (with the PKCE verifier and `resource`), and saves into the staging
    // store. Bounded: a timeout here provably stores nothing, ever.
    tokio::time::timeout(
        request.exchange_timeout,
        manager.exchange_code_for_token(&callback.code, &callback.state),
    )
    .await
    .map_err(|_| {
        flow_err(
            request.provider,
            format!(
                "the token exchange timed out after {}s.{}",
                request.exchange_timeout.as_secs(),
                no_tokens_note(registration_changed)
            ),
        )
    })?
    .map_err(|e| auth_err(request.provider, "token exchange", &e))?;

    // The durable commit, deliberately unbounded: the exchange demonstrably
    // succeeded, so the honest report is whatever this write actually does —
    // a timeout here would reintroduce "reported failure, stored credentials
    // anyway". The store's registration, client-id, resource, and
    // sign-out-epoch checks all apply.
    let staged = staging.take().ok_or_else(|| {
        flow_err(
            request.provider,
            "the token exchange completed but produced no credentials to store".to_owned(),
        )
    })?;
    // Serialize against every session client, including one whose build was
    // in flight moments ago: the `client_gate` excludes construction, and the
    // *re-sampled* client's manager mutex excludes an in-flight refresh — so
    // a refresh either fully persists first (its tokens are overwritten
    // below) or starts after and loads the tokens this commit writes.
    let _construction_exclusion = request.lifecycle.client_gate.lock().await;
    let _refresh_exclusion = match (request.resample_cached_client)() {
        Some(client) => Some(client.auth_manager.clone().lock_owned().await),
        None => None,
    };
    request.store.save(staged).await.map_err(|e| {
        flow_err(
            request.provider,
            format!("the sign-in completed but storing its credentials failed: {e}"),
        )
    })
}

/// The client id to authorize with: the stored registration when its
/// **compatibility fingerprint** (scopes, redirect form, issuer — see
/// [`OAuthRegistration`]) still matches what this sign-in would register,
/// otherwise a fresh dynamic registration whose client id is persisted
/// **before the browser opens** — rmcp persists nothing itself, and a user
/// who abandons the flow would otherwise orphan a server-side registration
/// and re-register on every attempt.
async fn resolve_client_id(
    manager: &mut AuthorizationManager,
    request: &SignInRequest<'_>,
    scopes: &[String],
    issuer: &str,
    registration_changed: &mut bool,
) -> Result<String, PromptError> {
    let existing = request
        .store
        .with_io_gate(ProviderCredentialStore::registration)
        .await
        .and_then(|r| r)
        .map_err(|e| auth_err(request.provider, "credential store read", &e))?;
    if let Some(registration) = existing {
        if registration_is_compatible(&registration, scopes, issuer) {
            return Ok(registration.client_id);
        }
        // Fall through to re-register. `persist_registration` below wipes any
        // stored tokens along with the superseded registration — deliberate:
        // they were issued to the old client id and old scope set, and
        // keeping them under the new registration would hand rmcp
        // credentials stamped with the wrong client. Consequence: a
        // signed-in user who starts a re-sign-in (because the server's
        // scopes changed) and abandons it ends up signed out — the plan's M4
        // notes ask the UI to warn that signing in again replaces the
        // provider's current authorization.
        tracing::info!(
            provider = %request.provider,
            "stored client registration no longer matches the server's advertised requirements; registering a new client"
        );
    }
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let config = manager
        .register_client(CLIENT_NAME, REGISTERED_REDIRECT_URI, &scope_refs)
        .await
        .map_err(|e| auth_err(request.provider, "dynamic client registration", &e))?;
    let client_id = config.client_id;
    let persisted = client_id.clone();
    let (persisted_scopes, persisted_issuer) = (scopes.to_vec(), issuer.to_owned());
    request
        .store
        .with_io_gate(move |store| {
            store.persist_registration(
                persisted,
                REGISTERED_REDIRECT_URI.to_owned(),
                persisted_scopes,
                persisted_issuer,
            )
        })
        .await
        .and_then(|r| r)
        .map_err(|e| auth_err(request.provider, "registration persistence", &e))?;
    // Only now: the flag means the *persisted* registration identity changed.
    // A failed DCR — or DCR success followed by a persistence failure (a
    // server-side orphan, but no local identity change) — leaves the envelope
    // untouched, so the caller's cached client is still valid and must not be
    // retired for it.
    *registration_changed = true;
    Ok(client_id)
}

/// Whether a stored registration can be reused for a sign-in that would
/// register `scopes` against `issuer`. Scopes compare as normalized values
/// (both sides are sorted/deduplicated at their source); the issuer compares
/// through the preflight's `as_identifier` derivation so spelling-level
/// differences don't churn re-registrations; the redirect form must be the
/// one this build registers.
fn registration_is_compatible(
    registration: &OAuthRegistration,
    scopes: &[String],
    issuer: &str,
) -> bool {
    let issuer_matches = match (
        url::Url::parse(&registration.issuer),
        url::Url::parse(issuer),
    ) {
        (Ok(stored), Ok(current)) => as_identifier(&stored) == as_identifier(&current),
        _ => false,
    };
    registration.scopes == scopes
        && registration.redirect_uri == REGISTERED_REDIRECT_URI
        && issuer_matches
}

/// The credential store installed on the flow's manager. rmcp's exchange
/// persists through `save`; capturing that in memory is what makes the
/// bounded exchange free of durable writes. `load` is never called on the
/// exchange path (verified against rmcp 1.7.0) and defensively reports no
/// credentials.
#[derive(Clone, Default)]
struct StagingCredentialStore {
    staged: Arc<std::sync::Mutex<Option<StoredCredentials>>>,
}

impl StagingCredentialStore {
    fn take(&self) -> Option<StoredCredentials> {
        self.staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

#[async_trait::async_trait]
impl CredentialStore for StagingCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        Ok(None)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        *self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(credentials);
        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.take();
        Ok(())
    }
}

fn flow_err(provider: &str, message: String) -> PromptError {
    PromptError::OAuthFlow {
        provider: provider.to_owned(),
        message,
    }
}

/// rmcp `AuthError`s carry no secrets in their display text; the step label
/// tells the user *where* the flow failed.
fn auth_err(provider: &str, step: &str, error: &AuthError) -> PromptError {
    flow_err(provider, format!("{step} failed: {error}"))
}

/// A decoded query parameter of `url`, if present.
fn query_param(url: &str, name: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|parsed| {
        parsed
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    })
}

/// A successful browser callback. Field values are never logged.
struct Callback {
    code: String,
    state: String,
}

/// What one accepted connection turned out to be.
enum CallbackParse {
    Success(Callback),
    /// A state-matched `error` redirect (e.g. consent denied); the string is
    /// a readable reason built from `error`/`error_description`.
    Denied(String),
    /// A callback-shaped request whose `state` is missing or doesn't match
    /// the pending flow — answered 404 and ignored (the flow keeps waiting),
    /// so a stale tab or forged local request can't terminate a live
    /// sign-in. Carries the `error` code (never the query string) for the
    /// debug log.
    StateMismatch {
        error_code: Option<String>,
    },
    /// Not the callback (favicon probe, wrong path, malformed request) —
    /// answered 404, keep listening.
    NotCallback,
}

/// Accept connections until one delivers a state-matched callback (or
/// denial). Handlers run concurrently: a browser's speculative connection
/// that never sends a request must not block the real callback arriving on a
/// sibling connection. The caller bounds this with the flow's callback
/// timeout.
async fn await_callback(listener: &TcpListener, expected_state: &str) -> Result<Callback, String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Callback, String>>(1);
    let mut accept_failures: u32 = 0;
    loop {
        tokio::select! {
            outcome = rx.recv() => {
                // `None` is unreachable while this loop holds `tx`, but degrade
                // readably rather than panic if that invariant ever breaks.
                return outcome.unwrap_or_else(|| Err("callback listener channel closed unexpectedly".to_owned()));
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    accept_failures = 0;
                    tokio::spawn(answer_connection(stream, expected_state.to_owned(), tx.clone()));
                }
                Err(e) => {
                    // See ACCEPT_RETRY_DELAY for why errors back off and
                    // eventually abort instead of silently re-polling.
                    accept_failures += 1;
                    if accept_failures >= MAX_CONSECUTIVE_ACCEPT_FAILURES {
                        return Err(format!("the local callback listener failed: {e}"));
                    }
                    tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                }
            }
        }
    }
}

/// Read one HTTP request, answer it with a small human-readable page, and
/// forward a parsed, state-matched callback (or denial) to the flow.
/// Non-callback, state-mismatched, and silent connections are answered (or
/// dropped on read timeout) without signalling.
async fn answer_connection(
    mut stream: TcpStream,
    expected_state: String,
    tx: tokio::sync::mpsc::Sender<Result<Callback, String>>,
) {
    // Read timeout (speculative connection) or malformed request: drop it.
    let Ok(Some(target)) =
        tokio::time::timeout(CONNECTION_READ_TIMEOUT, read_request_target(&mut stream)).await
    else {
        return;
    };
    let parsed = parse_callback_target(&target, &expected_state);
    let response = match &parsed {
        CallbackParse::Success(_) => html_response("200 OK", SUCCESS_PAGE),
        CallbackParse::Denied(reason) => html_response("200 OK", &failure_page(reason)),
        CallbackParse::StateMismatch { error_code } => {
            // The one diagnostic for a dropped callback (the flow otherwise
            // ends in a generic timeout): the error code alone — a registered
            // ASCII token — never the query string, which may carry a code.
            tracing::debug!(
                error_code = ?error_code,
                "dropped a loopback callback whose state did not match the pending sign-in"
            );
            html_response("404 Not Found", NOT_FOUND_PAGE)
        }
        CallbackParse::NotCallback => html_response("404 Not Found", NOT_FOUND_PAGE),
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    match parsed {
        CallbackParse::Success(callback) => drop(tx.send(Ok(callback)).await),
        CallbackParse::Denied(reason) => drop(tx.send(Err(reason)).await),
        CallbackParse::StateMismatch { .. } | CallbackParse::NotCallback => {}
    }
}

/// Read the request line and drain the headers, returning the request target
/// (`/callback?...`). `None` for anything that isn't a well-formed `GET`,
/// including a request line truncated by the size cap — its prefix must not
/// be parsed as if it were complete.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES));
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await.ok()?;
    if !request_line.ends_with('\n') {
        return None;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?.to_owned();
    if method != "GET" {
        return None;
    }
    // Drain headers so the browser finishes sending before we respond. A
    // drain cut short by the size cap is fine — the target is already read.
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.ok()?;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    Some(target)
}

/// Classify a request target against the pending flow's `state`. Never log
/// `target` — the query carries the authorization code.
fn parse_callback_target(target: &str, expected_state: &str) -> CallbackParse {
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return CallbackParse::NotCallback;
    };
    if url.path() != "/callback" {
        return CallbackParse::NotCallback;
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "error_description" => error_description = Some(value.into_owned()),
            _ => {}
        }
    }
    // RFC 6749 §4.1.2.1: the redirect must echo the request's `state` on both
    // success and error responses. A missing or mismatched state means this
    // callback does not belong to the pending flow. (Trade-off, made
    // deliberately: a non-conformant server that omits `state` on an early
    // error is also dropped here, turning its error page into our generic
    // timeout — the timeout message and the debug log above cover diagnosis.)
    if state.as_deref() != Some(expected_state) {
        return CallbackParse::StateMismatch { error_code: error };
    }
    if let Some(error) = error {
        let reason = match error_description {
            Some(description) => {
                format!("the authorization server reported {error:?}: {description}")
            }
            None => format!("the authorization server reported {error:?}"),
        };
        return CallbackParse::Denied(reason);
    }
    match code {
        Some(code) => CallbackParse::Success(Callback {
            code,
            state: expected_state.to_owned(),
        }),
        None => CallbackParse::Denied(
            "the browser callback carried neither an authorization code nor an error".to_owned(),
        ),
    }
}

/// Deliberately neutral: at this point the code is received but not yet
/// validated or exchanged, so the page must not claim the sign-in succeeded.
const SUCCESS_PAGE: &str = "<h2>Authorization received</h2><p>You can close this tab and return to Switchboard to finish signing in.</p>";
const NOT_FOUND_PAGE: &str = "<p>Not found.</p>";

/// The denial reason embeds server-controlled text, so it is HTML-escaped.
fn failure_page(reason: &str) -> String {
    format!(
        "<h2>Sign-in failed</h2><p>{}</p><p>You can close this tab and try again from Switchboard.</p>",
        escape_html(reason)
    )
}

fn html_response(status: &str, body: &str) -> String {
    let document = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Switchboard</title></head><body style=\"font-family: -apple-system, sans-serif; margin: 4rem auto; max-width: 30rem;\">{body}</body></html>"
    );
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{document}",
        document.len()
    )
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = "expected-state";

    #[test]
    fn callback_target_parsing_covers_success_denial_and_noise() {
        assert!(matches!(
            parse_callback_target("/callback?code=abc&state=expected-state", STATE),
            CallbackParse::Success(Callback { code, state }) if code == "abc" && state == STATE
        ));
        // Denial with a description folds both into the readable reason.
        match parse_callback_target(
            "/callback?error=access_denied&error_description=User%20declined&state=expected-state",
            STATE,
        ) {
            CallbackParse::Denied(reason) => {
                assert!(reason.contains("access_denied"), "{reason}");
                assert!(reason.contains("User declined"), "{reason}");
            }
            _ => panic!("expected Denied"),
        }
        // A state-matched callback with neither code nor error is a readable
        // failure, not a hang.
        assert!(matches!(
            parse_callback_target("/callback?state=expected-state&foo=bar", STATE),
            CallbackParse::Denied(_)
        ));
        // Non-callback paths (favicon probes) keep the listener waiting.
        assert!(matches!(
            parse_callback_target("/favicon.ico", STATE),
            CallbackParse::NotCallback
        ));
        assert!(matches!(
            parse_callback_target("/", STATE),
            CallbackParse::NotCallback
        ));
    }

    #[test]
    fn state_mismatched_callbacks_are_dropped_not_terminal() {
        // A success form with the wrong state must not reach the exchange…
        assert!(matches!(
            parse_callback_target("/callback?code=abc&state=forged", STATE),
            CallbackParse::StateMismatch { .. }
        ));
        // …a missing state is equally not ours…
        assert!(matches!(
            parse_callback_target("/callback?code=abc", STATE),
            CallbackParse::StateMismatch { .. }
        ));
        // …and an error form without the matching state must not terminate
        // the pending flow (a forged denial would otherwise be a local DoS).
        match parse_callback_target("/callback?error=access_denied", STATE) {
            CallbackParse::StateMismatch { error_code } => {
                assert_eq!(error_code.as_deref(), Some("access_denied"));
            }
            _ => panic!("expected StateMismatch"),
        }
    }

    #[test]
    fn failure_page_escapes_server_controlled_text() {
        let page = failure_page("<script>alert(1)</script>");
        assert!(!page.contains("<script>"));
        assert!(page.contains("&lt;script&gt;"));
    }

    #[test]
    fn registration_compatibility_checks_scopes_redirect_and_issuer() {
        let registration = OAuthRegistration {
            client_id: "client-1".to_owned(),
            redirect_uri: REGISTERED_REDIRECT_URI.to_owned(),
            resource: "https://mcp.example.com/mcp".to_owned(),
            scopes: vec!["a".to_owned(), "b".to_owned()],
            issuer: "https://auth.example.com".to_owned(),
        };
        let scopes = vec!["a".to_owned(), "b".to_owned()];
        assert!(registration_is_compatible(
            &registration,
            &scopes,
            "https://auth.example.com"
        ));
        // Issuer spelling differences (trailing slash) don't churn.
        assert!(registration_is_compatible(
            &registration,
            &scopes,
            "https://auth.example.com/"
        ));
        // Scope drift re-registers.
        assert!(!registration_is_compatible(
            &registration,
            &["a".to_owned()],
            "https://auth.example.com"
        ));
        // Issuer change re-registers.
        assert!(!registration_is_compatible(
            &registration,
            &scopes,
            "https://other.example.com"
        ));
        // A registration for a different redirect form re-registers.
        let moved = OAuthRegistration {
            redirect_uri: "http://localhost/callback".to_owned(),
            ..registration
        };
        assert!(!registration_is_compatible(
            &moved,
            &scopes,
            "https://auth.example.com"
        ));
        // Pre-fingerprint envelopes (serde-defaulted empty issuer) re-register.
        let legacy = OAuthRegistration {
            issuer: String::new(),
            ..moved
        };
        assert!(!registration_is_compatible(
            &legacy,
            &scopes,
            "https://auth.example.com"
        ));
    }
}
