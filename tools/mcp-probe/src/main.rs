//! M0 probe for the agent-access plan (`docs/implementation_plans/2026-10-09-mcp.md`).
//!
//! An MCP server whose only job is to measure how real clients behave: how long
//! they let a tool call run, whether progress notifications keep it alive,
//! whether a long call moves to the background, how a large result reaches the
//! model, whether a model keeps calling a tool that says "not done, call again",
//! and what each client declares at `initialize` (MCP tasks support shows there).
//!
//! Every HTTP request, with its JSON-RPC body, and every tool start, progress,
//! and end is appended to a JSON-lines log. The procedure that drives it is in
//! `docs/research/mcp-client-behavior.md`.
//!
//! Configuration, all optional, through the environment:
//! - `MCP_PROBE_PORT` (default 8765)
//! - `MCP_PROBE_TOKEN` (default: a fresh random token, printed at startup)
//! - `MCP_PROBE_LOG` (default `mcp-probe-log.jsonl` in the current directory)

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Meta, ProgressNotificationParam, ServerCapabilities, ServerInfo};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{Peer, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

/// Largest request body the probe reads. MCP requests are small; this only
/// bounds a misbehaving client.
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Largest result `big` will build.
const MAX_BIG_CHARS: usize = 2_000_000;
const SECRET_WORDS: [&str; 8] = [
    "marmalade",
    "lighthouse",
    "quartz",
    "tangerine",
    "harbor",
    "velvet",
    "glacier",
    "pepper",
];

/// Append-only JSON-lines log, also echoed as one short line to stdout.
#[derive(Clone)]
struct Log {
    file: Arc<Mutex<std::fs::File>>,
    started: Instant,
}

impl Log {
    fn open(path: &str) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            started: Instant::now(),
        })
    }

    fn write(&self, event: &str, fields: Value) {
        let elapsed = self.started.elapsed().as_secs_f64();
        let mut line = json!({
            "unix_ms": unix_ms(),
            "t_plus_s": (elapsed * 10.0).round() / 10.0,
            "event": event,
        });
        if let (Some(line), Value::Object(fields)) = (line.as_object_mut(), fields.clone()) {
            line.extend(fields);
        }
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{line}");
        }
        println!("[+{elapsed:>8.1}s] {event} {}", summarize(&fields));
    }
}

/// A compact one-line view of a log entry for the terminal.
fn summarize(fields: &Value) -> String {
    let mut text = fields.to_string();
    if text.len() > 200 {
        text.truncate(200);
        text.push('…');
    }
    text
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

/// A job for the `start_job` / `check_job` re-call loop.
struct Job {
    started: Instant,
    seconds: u64,
    word: &'static str,
    checks: u32,
    last_check: Option<Instant>,
}

/// Logs a tool call's end. If the call's future is dropped before it finishes
/// (the client hung up or timed out), the drop records that, which is how the
/// probe learns a client's limit.
struct CallGuard {
    log: Log,
    tool: &'static str,
    call_id: u64,
    started: Instant,
    finished: bool,
}

impl CallGuard {
    fn finish(mut self, outcome: &str) {
        self.finished = true;
        self.log.write(
            "tool_end",
            json!({
                "tool": self.tool,
                "call_id": self.call_id,
                "outcome": outcome,
                "elapsed_s": self.started.elapsed().as_secs_f64(),
            }),
        );
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.log.write(
                "tool_end",
                json!({
                    "tool": self.tool,
                    "call_id": self.call_id,
                    "outcome": "dropped (client went away)",
                    "elapsed_s": self.started.elapsed().as_secs_f64(),
                }),
            );
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SlowArgs {
    /// How many seconds to wait before returning.
    seconds: u64,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SlowWithProgressArgs {
    /// How many seconds to wait before returning.
    seconds: u64,
    /// Seconds between progress notifications. Defaults to 10.
    interval_seconds: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct BigArgs {
    /// Exact number of characters to return.
    chars: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct StartJobArgs {
    /// How many seconds the job takes to finish.
    seconds: u64,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct CheckJobArgs {
    /// The id `start_job` returned.
    job_id: u64,
    /// Longest this call waits for the job before answering. Defaults to 20.
    wait_seconds: Option<u64>,
}

#[derive(Clone)]
struct Probe {
    log: Log,
    calls: Arc<AtomicU64>,
    jobs: Arc<Mutex<HashMap<u64, Job>>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl Probe {
    fn new(log: Log) -> Self {
        Self {
            log,
            calls: Arc::new(AtomicU64::new(1)),
            jobs: Arc::new(Mutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Test tool. Waits `seconds` seconds and then returns, sending no progress \
                       notifications. Call it only when the user asks you to."
    )]
    async fn slow(
        &self,
        Parameters(args): Parameters<SlowArgs>,
        context: RequestContext<RoleServer>,
    ) -> String {
        self.wait("slow", args.seconds, None, &context).await
    }

    #[tool(
        description = "Test tool. Waits `seconds` seconds and then returns, sending a progress \
                       notification every `interval_seconds` (default 10) when the client asked \
                       for progress. Call it only when the user asks you to."
    )]
    async fn slow_with_progress(
        &self,
        Parameters(args): Parameters<SlowWithProgressArgs>,
        context: RequestContext<RoleServer>,
        meta: Meta,
        peer: Peer<RoleServer>,
    ) -> String {
        let interval = args.interval_seconds.unwrap_or(10).max(1);
        let progress = meta
            .get_progress_token()
            .map(|token| (peer, token, interval));
        self.wait("slow_with_progress", args.seconds, progress, &context)
            .await
    }

    #[tool(
        description = "Test tool. Returns exactly `chars` characters of numbered lines. The first \
                       line states the total and the last line reads END, so you can tell \
                       whether you received all of it. Call it only when the user asks you to."
    )]
    async fn big(&self, Parameters(args): Parameters<BigArgs>) -> String {
        let chars = args.chars.min(MAX_BIG_CHARS);
        self.log.write("big", json!({ "chars": chars }));
        build_big(chars)
    }

    #[tool(
        description = "Test tool. Starts a job that finishes after `seconds` seconds and returns \
                       its job_id. Then call check_job with that job_id, again and again, until it \
                       returns done: true. Call it only when the user asks you to."
    )]
    async fn start_job(&self, Parameters(args): Parameters<StartJobArgs>) -> String {
        let job_id = self.calls.fetch_add(1, Ordering::Relaxed);
        let word = SECRET_WORDS[(job_id as usize) % SECRET_WORDS.len()];
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(
                job_id,
                Job {
                    started: Instant::now(),
                    seconds: args.seconds,
                    word,
                    checks: 0,
                    last_check: None,
                },
            );
        }
        self.log.write(
            "start_job",
            json!({ "job_id": job_id, "seconds": args.seconds }),
        );
        json!({
            "job_id": job_id,
            "done": false,
            "message": "Job started. Call check_job with this job_id until it returns done: true.",
        })
        .to_string()
    }

    #[tool(
        description = "Test tool. Waits up to `wait_seconds` (default 20) for the job to finish. \
                       If it has not finished, returns done: false: call check_job again with the \
                       same job_id. When it has finished, returns done: true and the job's result."
    )]
    async fn check_job(
        &self,
        Parameters(args): Parameters<CheckJobArgs>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let wait = args.wait_seconds.unwrap_or(20);
        let now = Instant::now();
        let snapshot = self.jobs.lock().ok().and_then(|mut jobs| {
            jobs.get_mut(&args.job_id).map(|job| {
                job.checks += 1;
                let gap = job.last_check.map(|t| now.duration_since(t).as_secs_f64());
                job.last_check = Some(now);
                let deadline = job.started + Duration::from_secs(job.seconds);
                (job.checks, gap, deadline, job.word)
            })
        });
        let Some((checks, gap_s, deadline, word)) = snapshot else {
            self.log.write(
                "check_job",
                json!({ "job_id": args.job_id, "unknown": true }),
            );
            return json!({ "error": "unknown job_id" }).to_string();
        };
        self.log.write(
            "check_job",
            json!({
                "job_id": args.job_id,
                "check": checks,
                "seconds_since_previous_check": gap_s,
                "wait_seconds": wait,
            }),
        );
        let until = deadline.min(now + Duration::from_secs(wait));
        tokio::select! {
            () = tokio::time::sleep_until(until.into()) => {}
            () = context.ct.cancelled() => {
                return json!({ "error": "cancelled" }).to_string();
            }
        }
        if Instant::now() >= deadline {
            json!({
                "done": true,
                "checks": checks,
                "result": format!("The job's secret word is {word}."),
            })
            .to_string()
        } else {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
            json!({
                "done": false,
                "remaining_seconds": remaining,
                "message": "Not done yet. Call check_job again with the same job_id.",
            })
            .to_string()
        }
    }
}

impl Probe {
    /// Waits `seconds`, sending progress when asked, and records how the call ended.
    async fn wait(
        &self,
        tool: &'static str,
        seconds: u64,
        progress: Option<(Peer<RoleServer>, rmcp::model::ProgressToken, u64)>,
        context: &RequestContext<RoleServer>,
    ) -> String {
        let call_id = self.calls.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        self.log.write(
            "tool_start",
            json!({
                "tool": tool,
                "call_id": call_id,
                "seconds": seconds,
                "progress_token_received": progress.is_some(),
            }),
        );
        let guard = CallGuard {
            log: self.log.clone(),
            tool,
            call_id,
            started,
            finished: false,
        };
        let deadline = started + Duration::from_secs(seconds);
        let mut sent = 0u64;
        loop {
            let next = match &progress {
                Some((_, _, interval)) => {
                    deadline.min(started + Duration::from_secs(interval * (sent + 1)))
                }
                None => deadline,
            };
            tokio::select! {
                () = tokio::time::sleep_until(next.into()) => {}
                () = context.ct.cancelled() => {
                    guard.finish("cancelled by client");
                    return json!({ "error": "cancelled" }).to_string();
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            if let Some((peer, token, _)) = &progress {
                sent += 1;
                let result = peer
                    .notify_progress(ProgressNotificationParam {
                        progress_token: token.clone(),
                        progress: started.elapsed().as_secs_f64(),
                        total: Some(seconds as f64),
                        message: Some(format!("{sent} progress notifications so far")),
                    })
                    .await;
                self.log.write(
                    "progress_sent",
                    json!({ "call_id": call_id, "n": sent, "ok": result.is_ok() }),
                );
            }
        }
        guard.finish("completed");
        json!({
            "done": true,
            "tool": tool,
            "waited_seconds": seconds,
            "progress_token_received": progress.is_some(),
            "progress_sent": sent,
            "finished_at_unix_ms": unix_ms(),
        })
        .to_string()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Probe {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Switchboard MCP probe: test tools for measuring client behaviour. Call a tool only \
             when the user asks you to, with the arguments the user gives."
                .into(),
        );
        info
    }
}

/// Exactly `chars` characters: a header line, numbered filler lines, then `END\n`.
fn build_big(chars: usize) -> String {
    const END: &str = "END\n";
    let mut out = format!("BIG RESULT: {chars} characters in total; the last line reads END.\n");
    let mut n = 1u64;
    while out.len() + END.len() < chars {
        out.push_str(&format!(
            "line {n:06}: the quick brown fox jumps over the lazy dog 0123456789\n"
        ));
        n += 1;
    }
    out.truncate(chars.saturating_sub(END.len()));
    out.push_str(END);
    out
}

#[derive(Clone)]
struct Gate {
    log: Log,
    expected_authorization: String,
}

/// Logs every HTTP request with its JSON-RPC body (the token itself is never
/// logged), then refuses any request without the bearer token.
async fn gate(State(gate): State<Gate>, request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_REQUEST_BYTES).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request too large").into_response();
    };
    let header_text = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let authorized = header_text(header::AUTHORIZATION.as_str()).as_deref()
        == Some(gate.expected_authorization.as_str());
    let body_value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    gate.log.write(
        "http_request",
        json!({
            "method": parts.method.as_str(),
            "path": parts.uri.path(),
            "authorized": authorized,
            "headers": {
                "user-agent": header_text("user-agent"),
                "host": header_text("host"),
                "origin": header_text("origin"),
                "accept": header_text("accept"),
                "mcp-protocol-version": header_text("mcp-protocol-version"),
                "mcp-session-id": header_text("mcp-session-id"),
                "last-event-id": header_text("last-event-id"),
            },
            "body": body_value,
        }),
    );
    if !authorized {
        return (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response();
    }
    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    if !response.status().is_success() {
        gate.log.write(
            "http_response",
            json!({ "status": response.status().as_u16() }),
        );
    }
    response
}

fn random_token() -> String {
    let mut bytes = [0u8; 24];
    if let Ok(mut urandom) = std::fs::File::open("/dev/urandom") {
        let _ = urandom.read_exact(&mut bytes);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn print_setup(port: u16, token: &str, log_path: &str) {
    let url = format!("http://127.0.0.1:{port}/mcp");
    println!(
        "\nSwitchboard MCP probe listening on {url}\nLog: {log_path}\n\n\
         Claude Code (run in a scratch folder, not a project folder):\n  \
         claude mcp add --transport http mcp-probe {url} --header \"Authorization: Bearer {token}\"\n\n\
         Codex (~/.codex/config.toml; check the keys with `codex mcp add --help`):\n  \
         [mcp_servers.mcp-probe]\n  url = \"{url}\"\n  bearer_token_env_var = \"MCP_PROBE_TOKEN\"\n  \
         # then start codex with MCP_PROBE_TOKEN={token}\n\n\
         Claude Desktop (claude_desktop_config.json, under \"mcpServers\"):\n  \
         \"mcp-probe\": {{\n    \"command\": \"npx\",\n    \
         \"args\": [\"mcp-remote\", \"{url}\", \"--allow-http\", \"--transport\", \"http-only\", \
         \"--header\", \"Authorization:${{AUTH_HEADER}}\"],\n    \
         \"env\": {{ \"AUTH_HEADER\": \"Bearer {token}\" }}\n  }}\n"
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("MCP_PROBE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8765);
    let token = std::env::var("MCP_PROBE_TOKEN").unwrap_or_else(|_| random_token());
    let log_path =
        std::env::var("MCP_PROBE_LOG").unwrap_or_else(|_| "mcp-probe-log.jsonl".to_owned());
    let log = Log::open(&log_path)?;
    log.write("probe_started", json!({ "port": port }));

    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts([format!("127.0.0.1:{port}"), format!("localhost:{port}")])
        .with_allowed_origins([
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
        ]);
    let probe = Probe::new(log.clone());
    let service = StreamableHttpService::new(
        move || Ok(probe.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let gate_state = Gate {
        log: log.clone(),
        expected_authorization: format!("Bearer {token}"),
    };
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(gate_state, gate));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    print_setup(port, &token, &log_path);
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    log.write("probe_stopped", json!({}));
    Ok(())
}
