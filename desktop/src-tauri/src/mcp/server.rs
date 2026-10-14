//! The listener: an `axum` router on `127.0.0.1:<port>` with the bearer
//! check in front of `rmcp`'s Streamable HTTP service at `/mcp`, and the MCP
//! handler that runs the tools.
//!
//! **Who is refused, and by what.** `rmcp` checks `Host` and `Origin`,
//! configured here to this port only: its default host list accepts any port,
//! and its default origin list checks nothing. A request without `Origin`
//! passes (MCP clients send none; browsers always do), which is the
//! DNS-rebinding defence the MCP specification requires. The bearer check is
//! the only check of our own, in front of everything (see [`super::token`]).
//!
//! **How stopping ends everything, in order.** First every tool call is told
//! to finish: a wait returns the state it has, and a send or workflow start
//! still preparing stops before dispatch (a send already dispatching keeps
//! going: each runs as its own task). From that moment every new call but a
//! cancel is refused, because turning access off is the user's brake. The calls get a moment to answer, so a client waiting on
//! one receives a reply rather than a dropped stream, which `rmcp`'s own
//! client would retry forever. Then `rmcp`'s token ends every session and
//! open stream, and only then is `axum` shut down: its graceful shutdown
//! waits for open connections, and an MCP session can hold an event stream
//! open indefinitely (`rmcp` keeps it alive every 15 seconds), so shutting
//! `axum` down first could hang and leave the port bound. A connection still
//! open after a short grace is dropped. The calls need telling separately
//! because `rmcp` does not cancel a tool call when its client goes away, and
//! ending a session does not end the calls running in it.
//!
//! **Session keep-alive.** `rmcp` ends a session after 300 seconds without a
//! message in either direction. Clients whose idle session expired reconnect
//! on their own, and a long wait sends progress every 60 seconds, which
//! counts. `rmcp` 1.7 answers Claude Code's `server/discover` with 422 and
//! Claude Code falls back to `initialize`; recheck when `rmcp` is upgraded.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ProgressNotificationParam, ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, ServerHandler};
use serde_json::Value;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::AppError;
use crate::ops::error_code::{INTERNAL, error_code};
use crate::ops::handle::AppStateHandle;
use crate::ops::wait::ProgressHook;

use super::app_nap::NapExemption;
use super::token;
use super::tools::{self, CallContext, ToolName};

/// How long a stop waits for the tool calls it told to finish.
pub(crate) const CALLS_GRACE: Duration = Duration::from_secs(2);
/// How long a finished call's reply gets to reach its stream before the
/// sessions end. The reply crosses in-process channels, so this is generous.
const REPLY_FLUSH: Duration = Duration::from_millis(200);
/// How long a stop waits for open connections after ending every session.
const STOP_GRACE: Duration = Duration::from_secs(2);
/// How often an open event stream is pinged so the connection stays up:
/// `rmcp`'s default. The pings never reach a session, so they do not count
/// toward its 300-second keep-alive.
pub(super) const SSE_PING: Option<Duration> = Some(Duration::from_secs(15));

/// The MCP handler, one per session; every session shares the app state.
#[derive(Clone)]
pub struct SwitchboardServer {
    handle: Arc<dyn AppStateHandle>,
    home: PathBuf,
    /// Cancelled when the server stops: every call's wait budget ends.
    calls: CancellationToken,
    in_flight: Arc<InFlight>,
}

/// The tool calls running, so a stop can wait for them to answer.
#[derive(Default)]
struct InFlight {
    count: AtomicUsize,
    idle: Notify,
}

impl InFlight {
    fn enter(self: &Arc<Self>) -> CallGuard {
        self.count.fetch_add(1, Ordering::SeqCst);
        CallGuard(Arc::clone(self))
    }

    async fn drained(&self) {
        loop {
            // Registered before the check, so a call ending in between still
            // wakes this.
            let idle = self.idle.notified();
            if self.count.load(Ordering::SeqCst) == 0 {
                return;
            }
            idle.await;
        }
    }
}

struct CallGuard(Arc<InFlight>);

impl Drop for CallGuard {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl ServerHandler for SwitchboardServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "switchboard",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(tools::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools::catalog(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(tool) = ToolName::parse(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("no tool named {}", request.name),
                None,
            ));
        };
        let args = request.arguments.unwrap_or_default();
        let ids = LoggedIds::from_args(&args);
        // Turning agent access off is the user's brake: once a stop begins,
        // nothing new starts. Its sessions stay open a moment so running calls
        // can answer, and a call arriving then would otherwise run.
        if self.calls.is_cancelled() && !tool.allowed_while_stopping() {
            tracing::info!(tool = tool.as_str(), project_id = ?ids.project_id, agent_ids = ?ids.agent_ids, outcome = "refused", "agent access tool call");
            return Ok(CallToolResult::error(vec![Content::text(
                serde_json::json!({ "code": INTERNAL, "message": "agent access is turning off" })
                    .to_string(),
            )]));
        }
        let progress_token = context.meta.get_progress_token();
        let cx = CallContext {
            handle: Arc::clone(&self.handle),
            home: self.home.clone(),
            can_keep_alive: progress_token.is_some(),
            cancel: self.calls.child_token(),
            progress: progress_token.map(|token| progress_hook(&context, token)),
        };
        let _in_flight = self.in_flight.enter();
        let started = Instant::now();
        // A client's own cancellation, or its session ending, drops the call.
        // That is safe for every tool: a wait has nothing to undo, and a send
        // runs as its own task, so dropping its caller never leaves it
        // half-done.
        let outcome = tokio::select! {
            outcome = run(tool, args, &cx) => Some(outcome),
            () = context.ct.cancelled() => None,
        };
        let elapsed_ms = started.elapsed().as_millis();
        let Some(outcome) = outcome else {
            tracing::info!(tool = tool.as_str(), project_id = ?ids.project_id, agent_ids = ?ids.agent_ids, elapsed_ms, outcome = "cancelled", "agent access tool call");
            // Nobody reads the reply to a cancelled request; this one says
            // what happened for anyone who does.
            return Ok(CallToolResult::error(vec![Content::text(
                serde_json::json!({ "code": INTERNAL, "message": "the call was cancelled before it finished" })
                    .to_string(),
            )]));
        };
        let result = match outcome {
            Ok(text) => {
                tracing::info!(tool = tool.as_str(), project_id = ?ids.project_id, agent_ids = ?ids.agent_ids, elapsed_ms, outcome = "ok", "agent access tool call");
                CallToolResult::success(vec![Content::text(text)])
            }
            Err(error) => {
                let code = error_code(&error);
                tracing::info!(tool = tool.as_str(), project_id = ?ids.project_id, agent_ids = ?ids.agent_ids, elapsed_ms, outcome = code, "agent access tool call");
                CallToolResult::error(vec![Content::text(error_body(code, &error))])
            }
        };
        Ok(result)
    }
}

/// Run the tool and serialize its fitted result.
async fn run(
    tool: ToolName,
    args: rmcp::model::JsonObject,
    cx: &CallContext,
) -> Result<String, AppError> {
    let fitted = tools::call(tool, args, cx).await?.fit()?;
    serde_json::to_string(&fitted)
        .map_err(|e| AppError::Internal(format!("could not serialize a result: {e}")))
}

/// A tool error as the model reads it: a code it can act on and the message.
fn error_body(code: &str, error: &AppError) -> String {
    serde_json::json!({ "code": code, "message": error.to_string() }).to_string()
}

/// Sends a progress notification on the call's token each time the wait
/// budget asks: every 60 seconds while a kept-alive call waits.
fn progress_hook(
    context: &RequestContext<RoleServer>,
    token: rmcp::model::ProgressToken,
) -> ProgressHook {
    let peer = context.peer.clone();
    let sent = Arc::new(AtomicU32::new(0));
    Arc::new(move || {
        let progress = sent.fetch_add(1, Ordering::Relaxed) + 1;
        let peer = peer.clone();
        let param = ProgressNotificationParam::new(token.clone(), f64::from(progress))
            .with_message("still waiting");
        tokio::spawn(async move {
            if let Err(error) = peer.notify_progress(param).await {
                tracing::debug!(%error, "could not send a progress notification");
            }
        });
    })
}

/// The ids a call names, for its log line. Never any text.
struct LoggedIds {
    project_id: Option<String>,
    agent_ids: Vec<String>,
}

impl LoggedIds {
    fn from_args(args: &rmcp::model::JsonObject) -> Self {
        let id = |value: &Value| value.as_str().map(str::to_owned);
        let mut agent_ids: Vec<String> = args.get("agent_id").and_then(id).into_iter().collect();
        if let Some(Value::Array(recipients)) = args.get("recipients") {
            agent_ids.extend(recipients.iter().filter_map(id));
        }
        Self {
            project_id: args.get("project_id").and_then(id),
            agent_ids,
        }
    }
}

/// What a stop ends, in order: the tool calls, then the sessions.
pub(super) struct Shutdown {
    /// Every call's wait budget ends when this is cancelled.
    calls: CancellationToken,
    in_flight: Arc<InFlight>,
    /// `rmcp`'s token: every session and open stream ends with it.
    sessions: CancellationToken,
}

impl Shutdown {
    pub(super) fn new() -> Self {
        Self {
            calls: CancellationToken::new(),
            in_flight: Arc::new(InFlight::default()),
            sessions: CancellationToken::new(),
        }
    }

    /// The stop's first step: every in-flight call is told to finish, and
    /// every new call but a cancel is refused.
    pub(super) fn begin(&self) {
        self.calls.cancel();
    }

    /// End every in-flight call, give the replies a moment to reach their
    /// streams, then end every session.
    pub(super) async fn run(&self) {
        self.begin();
        if tokio::time::timeout(CALLS_GRACE, self.in_flight.drained())
            .await
            .is_err()
        {
            tracing::info!("agent access: a tool call outlived the stop");
        }
        tokio::time::sleep(REPLY_FLUSH).await;
        self.sessions.cancel();
    }
}

/// A running listener.
pub struct Listener {
    addr: SocketAddr,
    shutdown: Shutdown,
    task: JoinHandle<()>,
    /// Keeps macOS from napping the app for as long as this listener lives.
    _nap: NapExemption,
}

impl Listener {
    #[must_use]
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// End every in-flight call and session, then stop listening, in the
    /// order the module explains. Returns once the port is free.
    pub async fn stop(mut self) {
        self.shutdown.run().await;
        if tokio::time::timeout(STOP_GRACE, &mut self.task)
            .await
            .is_err()
        {
            tracing::info!("agent access: a connection outlived the stop; dropping it");
            self.task.abort();
            let _ = (&mut self.task).await;
        }
        tracing::info!(port = self.addr.port(), "agent access stopped");
    }
}

/// Bind `127.0.0.1:<port>` and serve. Port `0` takes any free port (tests).
/// A port already in use is an error for the caller to show, never a panic.
pub async fn start(
    port: u16,
    token: String,
    handle: Arc<dyn AppStateHandle>,
    home: PathBuf,
) -> Result<Listener, std::io::Error> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    let port = addr.port();
    let shutdown = Shutdown::new();
    let router = router(port, token, handle, home, &shutdown, SSE_PING);
    let graceful = shutdown.sessions.clone();
    let task = tokio::spawn(async move {
        let serve = axum::serve(listener, router)
            .with_graceful_shutdown(async move { graceful.cancelled().await });
        if let Err(error) = serve.await {
            tracing::warn!(%error, "agent access listener failed");
        }
    });
    tracing::info!(port, "agent access listening");
    Ok(Listener {
        addr,
        shutdown,
        task,
        _nap: NapExemption::begin(),
    })
}

/// The whole server for `port` as one router: the bearer check in front of
/// `rmcp`'s service at `/mcp`, its `Host` and `Origin` checks pinned to the
/// port. Separate from the socket so a test can drive it in process.
pub(super) fn router(
    port: u16,
    token: String,
    handle: Arc<dyn AppStateHandle>,
    home: PathBuf,
    shutdown: &Shutdown,
    sse_ping: Option<Duration>,
) -> axum::Router {
    let config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(sse_ping)
        .with_allowed_hosts([format!("127.0.0.1:{port}"), format!("localhost:{port}")])
        .with_allowed_origins([
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
        ])
        .with_cancellation_token(shutdown.sessions.child_token());
    let server = SwitchboardServer {
        handle,
        home,
        calls: shutdown.calls.clone(),
        in_flight: Arc::clone(&shutdown.in_flight),
    };
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn_with_state(
            Arc::<str>::from(token),
            require_bearer,
        ))
}

/// The bearer check. A request that passes goes on without its
/// `Authorization` header: `rmcp` hands every request's headers to the tool
/// handler, and nothing past this check needs the token, so nothing past it
/// can log or leak it.
async fn require_bearer(
    State(token): State<Arc<str>>,
    mut request: Request,
    next: Next,
) -> Response {
    let header = request
        .headers()
        .get(header::AUTHORIZATION)
        .map(axum::http::HeaderValue::as_bytes);
    if token::authorizes(header, &token) {
        request.headers_mut().remove(header::AUTHORIZATION);
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "missing or wrong bearer token",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::{HeaderMap, Request, StatusCode, header};
    use tower::ServiceExt as _;

    use super::require_bearer;

    #[tokio::test]
    async fn a_request_past_the_bearer_check_no_longer_carries_the_token() {
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::post(|headers: HeaderMap| async move {
                    headers.contains_key(header::AUTHORIZATION).to_string()
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Arc::<str>::from("secret"),
                require_bearer,
            ));
        let response = app
            .oneshot(
                Request::post("/")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap();
        assert_eq!(
            &body[..],
            b"false",
            "the handler saw no Authorization header"
        );
    }
}
