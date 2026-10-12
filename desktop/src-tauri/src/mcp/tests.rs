//! The server end to end, in process: a real listener on an ephemeral port
//! over a mock-harness `AppState`, driven by `rmcp`'s own client, and by raw
//! HTTP where a test needs a request that client never sends (no progress
//! token, a browser's `Origin`, a wrong `Host`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ProgressNotificationParam};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ClientHandler, ServiceExt};
use serde_json::{Value, json};
use switchboard_core::{AgentId, ProjectId, SendId};
use switchboard_prompts::{InMemorySecretStore, SecretStore};
use tokio::sync::Notify;

use crate::ops::ledger::SendStatus;
use crate::ops::send::{Body, OnBusyPolicy, SendRequest};
use crate::ops::test_support::{Script, TestEnv, await_turn_running};
use crate::ops::wait::WaitBudget;

use super::server::{self, Listener};
use super::tools::ToolName;
use super::{AgentAccess, token};

const TOKEN: &str = "test-token";

async fn serve(env: &TestEnv) -> (Listener, String) {
    let listener = server::start(0, TOKEN.to_owned(), env.handle(), env.home())
        .await
        .unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", listener.port());
    (listener, url)
}

/// A client that records the progress notifications it receives.
#[derive(Clone, Default)]
struct Recorder {
    progress: Arc<Mutex<Vec<f64>>>,
}

impl ClientHandler for Recorder {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.progress.lock().unwrap().push(params.progress);
    }
}

async fn connect(url: &str, token: &str) -> RunningService<RoleClient, Recorder> {
    connect_as(Recorder::default(), url, token).await
}

async fn connect_as(
    recorder: Recorder,
    url: &str,
    token: &str,
) -> RunningService<RoleClient, Recorder> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token),
    );
    recorder.serve(transport).await.unwrap()
}

/// Call a tool and return whether it was an error, and its JSON body.
async fn call(
    client: &RunningService<RoleClient, Recorder>,
    tool: &str,
    args: Value,
) -> (bool, Value) {
    let Value::Object(args) = args else {
        panic!("arguments must be an object");
    };
    let result = client
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    (
        result.is_error.unwrap_or(false),
        serde_json::from_str(&text).unwrap(),
    )
}

/// Call a tool that must succeed and return its `result`.
async fn ok(client: &RunningService<RoleClient, Recorder>, tool: &str, args: Value) -> Value {
    let (is_error, body) = call(client, tool, args).await;
    assert!(!is_error, "{tool} failed: {body}");
    body["result"].clone()
}

fn uuid(value: &Value) -> uuid::Uuid {
    value.as_str().unwrap().parse().unwrap()
}

/// A project on disk that this state has not opened, with one agent.
fn unopened_project(env: TestEnv) -> (TestEnv, ProjectId, AgentId) {
    let mut env = env;
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "worker");
    (env.restart(), project_id, agent_id)
}

/// Send to the agent in process and return the send id.
async fn send_in_process(env: &TestEnv, project_id: ProjectId, agent_id: AgentId) -> SendId {
    crate::ops::send::send(
        env.handle(),
        env.home(),
        SendRequest {
            project_id,
            recipients: vec![agent_id],
            body: Body::Text {
                text: "work".to_owned(),
            },
            on_busy: OnBusyPolicy::Refuse,
            idempotency_key: None,
        },
        WaitBudget::default_wait(),
    )
    .await
    .unwrap()
    .send_id
}

#[tokio::test]
async fn a_client_lists_projects_creates_an_agent_sends_and_waits() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let env = env.restart();
    let (listener, url) = serve(&env).await;
    let client = connect(&url, TOKEN).await;

    let projects = ok(&client, "list_projects", json!({})).await;
    assert_eq!(uuid(&projects[0]["id"]), project_id);
    assert_eq!(projects[0]["status"], "idle");

    let agent = ok(
        &client,
        "create_agent",
        json!({ "project_id": project_id, "name": "coder", "harness": "claude_code" }),
    )
    .await;
    let agent_id = uuid(&agent["id"]);

    let receipt = ok(
        &client,
        "send",
        json!({ "project_id": project_id, "recipients": [agent_id], "text": "hello" }),
    )
    .await;
    let send_id = uuid(&receipt["send_id"]);
    assert_eq!(receipt["recipients"][0]["status"], "accepted");

    let mut reply = Value::Null;
    for _ in 0..20 {
        reply = ok(
            &client,
            "wait_send",
            json!({ "project_id": project_id, "send_id": send_id, "timeout_seconds": 5 }),
        )
        .await;
        if reply["done"] == true {
            break;
        }
    }
    assert_eq!(reply["done"], true, "{reply}");
    assert_eq!(reply["recipients"][0]["status"], "completed");
    assert_eq!(reply["recipients"][0]["text"], "ack: hello");

    client.cancel().await.unwrap();
    listener.stop().await;
}

#[tokio::test]
async fn the_tool_list_is_the_catalog_and_declares_the_result_size() {
    let env = TestEnv::new();
    let (listener, url) = serve(&env).await;
    let client = connect(&url, TOKEN).await;

    let info = client.peer_info().unwrap();
    assert!(
        info.instructions
            .as_deref()
            .is_some_and(|i| i.contains("wait_send")),
        "the instructions describe the loop"
    );
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    let expected: Vec<&str> = ToolName::ALL.iter().map(|t| t.as_str()).collect();
    assert_eq!(names, expected);
    for tool in &tools {
        let meta = tool.meta.as_ref().unwrap();
        assert_eq!(
            meta.0.get("anthropic/maxResultSizeChars"),
            Some(&json!(100_000)),
            "{}",
            tool.name
        );
        assert!(tool.description.as_ref().is_some_and(|d| !d.is_empty()));
    }
    let wait = tools.iter().find(|t| t.name == "wait_send").unwrap();
    let description = wait.description.as_deref().unwrap();
    assert!(description.contains("Claude Code") && description.contains("Codex"));

    client.cancel().await.unwrap();
    listener.stop().await;
}

#[tokio::test]
async fn a_tool_error_is_a_result_with_a_code_and_a_message() {
    let (env, project_id, _agent_id) = unopened_project(TestEnv::new());
    let (listener, url) = serve(&env).await;
    let client = connect(&url, TOKEN).await;

    let stranger = uuid::Uuid::now_v7();
    let (is_error, body) = call(
        &client,
        "send",
        json!({ "project_id": project_id, "recipients": [stranger], "text": "hi" }),
    )
    .await;
    assert!(is_error);
    assert_eq!(body["code"], "unknown_recipient");
    assert!(body["message"].as_str().is_some_and(|m| !m.is_empty()));

    // A malformed or misspelt argument is the caller's error too, as a
    // result rather than a protocol error.
    let (is_error, body) = call(
        &client,
        "send",
        json!({ "project_id": project_id, "recipients": [], "text": "hi", "on_bsy": "queue" }),
    )
    .await;
    assert!(is_error);
    assert_eq!(body["code"], "invalid_argument");
    let (is_error, body) = call(&client, "list_agents", json!({ "project_id": "nope" })).await;
    assert!(is_error);
    assert_eq!(body["code"], "invalid_argument");

    client.cancel().await.unwrap();
    listener.stop().await;
}

/// A raw POST of `body` to the server, as a browser or a misconfigured
/// client would send it.
async fn post(
    url: &str,
    headers: &[(reqwest::header::HeaderName, String)],
    body: &Value,
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .body(body.to_string());
    for (name, value) in headers {
        request = request.header(name.clone(), value.clone());
    }
    request.send().await.unwrap()
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "raw", "version": "0" }
        }
    })
}

fn bearer(token: &str) -> (reqwest::header::HeaderName, String) {
    (reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
}

#[tokio::test]
async fn requests_without_the_token_or_from_a_web_page_are_refused() {
    use reqwest::header::{HOST, ORIGIN};
    let env = TestEnv::new();
    let (listener, url) = serve(&env).await;
    let port = listener.port();

    let status = |headers: Vec<(reqwest::header::HeaderName, String)>| {
        let url = url.clone();
        async move { post(&url, &headers, &initialize()).await.status().as_u16() }
    };
    assert_eq!(status(vec![]).await, 401, "no token");
    assert_eq!(status(vec![bearer("wrong")]).await, 401, "wrong token");
    assert_eq!(
        status(vec![
            bearer(TOKEN),
            (ORIGIN, "https://evil.example".to_owned())
        ])
        .await,
        403,
        "a web page's origin"
    );
    assert_eq!(
        status(vec![bearer(TOKEN), (ORIGIN, "null".to_owned())]).await,
        403,
        "a sandboxed page's origin"
    );
    assert_eq!(
        status(vec![bearer(TOKEN), (HOST, format!("evil.example:{port}"))]).await,
        403,
        "a rebound host name"
    );
    let other_port = if port == 65_535 { port - 1 } else { port + 1 };
    assert_eq!(
        status(vec![
            bearer(TOKEN),
            (HOST, format!("localhost:{other_port}"))
        ])
        .await,
        403,
        "localhost on another port"
    );
    assert_eq!(status(vec![bearer(TOKEN)]).await, 200, "a client");
    assert_eq!(
        status(vec![bearer(TOKEN), (HOST, format!("localhost:{port}"))]).await,
        200,
        "a client naming localhost"
    );
    assert_eq!(
        status(vec![
            bearer(TOKEN),
            (ORIGIN, format!("http://localhost:{port}"))
        ])
        .await,
        200,
        "this server's own origin"
    );

    listener.stop().await;
}

#[tokio::test]
async fn stopping_ends_open_streams_and_waits_the_turn_keeps_running_and_the_port_rebinds() {
    let (env, project_id, agent_id) = unopened_project(TestEnv::new());
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("partial", &gate));
    let (listener, url) = serve(&env).await;
    let port = listener.port();
    let client = Arc::new(connect(&url, TOKEN).await);

    let receipt = ok(
        &client,
        "send",
        json!({ "project_id": project_id, "recipients": [agent_id], "text": "work" }),
    )
    .await;
    let send_id = uuid(&receipt["send_id"]);
    await_turn_running(&env.state, agent_id).await;

    // A wait in flight, holding its stream open.
    let waiting = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            let Value::Object(args) = json!({
                "project_id": project_id, "send_id": send_id, "timeout_seconds": 600
            }) else {
                unreachable!()
            };
            client
                .call_tool(CallToolRequestParams::new("wait_send").with_arguments(args))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished());

    let stopped = tokio::time::timeout(Duration::from_secs(5), listener.stop()).await;
    assert!(stopped.is_ok(), "stopping returned promptly");
    // The client's wait ends with an answer, not a dropped stream.
    let answered = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the client's wait ended")
        .unwrap()
        .expect("the wait answered before its session ended");
    let text = answered.content[0].as_text().unwrap().text.clone();
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reply["result"]["done"], false, "{reply}");

    // The agent's turn is untouched by the server stopping.
    assert_eq!(
        env.state.ledger.send(send_id).unwrap().recipients[0].status,
        SendStatus::Running
    );
    gate.notify_one();
    let reply = crate::ops::send::wait_send(
        &env.state,
        project_id,
        send_id,
        &WaitBudget::new(Some(5), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);

    let again = server::start(port, TOKEN.to_owned(), env.handle(), env.home()).await;
    assert!(again.is_ok(), "the same port binds again at once");
    again.unwrap().stop().await;
}

// --- timing, in process ------------------------------------------------------------
//
// The timing tests run the clock paused and drive the router in process: a
// paused clock jumps to the next timer whenever nothing is ready, and a real
// socket's readiness can arrive a moment late, long enough for the clock to
// jump past a session's timers. In process, nothing waits on the OS.
//
// They also turn off the pings on open event streams. `rmcp` schedules each
// ping from the system clock rather than the runtime's, so under a paused
// clock every ping is already due and the stream pings in a tight loop, which
// keeps the clock from ever jumping. The pings never reach a session, so the
// session keep-alive these tests check is the production one.

/// The server's router for a made-up port, and its shutdown.
fn in_process(env: &TestEnv) -> (axum::Router, server::Shutdown) {
    let shutdown = server::Shutdown::new();
    let router = server::router(
        1,
        TOKEN.to_owned(),
        env.handle(),
        env.home(),
        &shutdown,
        None,
    );
    (router, shutdown)
}

async fn post_in_process(
    router: &axum::Router,
    session: Option<&str>,
    body: &Value,
) -> axum::response::Response {
    use tower::ServiceExt as _;
    let mut request = axum::http::Request::post("/mcp")
        .header(axum::http::header::HOST, "127.0.0.1:1")
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(
            axum::http::header::ACCEPT,
            "application/json, text/event-stream",
        );
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    router
        .clone()
        .oneshot(
            request
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Open a session: `initialize`, then `notifications/initialized`.
async fn open_session(router: &axum::Router) -> String {
    let response = post_in_process(router, None, &initialize()).await;
    assert_eq!(response.status().as_u16(), 200);
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    read_sse(response, 1).await;
    let initialized = post_in_process(
        router,
        Some(&session),
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(initialized.status().as_u16(), 202);
    session
}

/// Read an SSE response until the message answering request `id` arrives;
/// return it and every progress notification seen before it. The stream can
/// stay open after the answer, so reading stops there, not at its end.
async fn read_sse(response: axum::response::Response, id: u64) -> (Value, Vec<f64>) {
    use futures::StreamExt as _;
    let mut stream = response.into_body().into_data_stream();
    let mut pending = String::new();
    let mut progress = Vec::new();
    loop {
        let chunk = stream
            .next()
            .await
            .unwrap_or_else(|| panic!("the stream ended before answering {id}"))
            .unwrap();
        pending.push_str(std::str::from_utf8(&chunk).unwrap());
        while let Some(end) = pending.find('\n') {
            let line: String = pending.drain(..=end).collect();
            let Some(data) = line.trim_end().strip_prefix("data:") else {
                continue;
            };
            let Ok(message) = serde_json::from_str::<Value>(data.trim()) else {
                continue;
            };
            if message["method"] == "notifications/progress" {
                progress.push(message["params"]["progress"].as_f64().unwrap());
            } else if message["id"] == id {
                return (message, progress);
            }
        }
    }
}

fn wait_send_call(project_id: ProjectId, send_id: SendId, meta: Option<Value>) -> Value {
    let mut params = json!({
        "name": "wait_send",
        "arguments": { "project_id": project_id, "send_id": send_id, "timeout_seconds": 900 }
    });
    if let Some(meta) = meta {
        params["_meta"] = meta;
    }
    json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": params })
}

fn tool_reply(message: &Value) -> Value {
    let text = message["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_wait_without_a_progress_token_returns_by_fifty_seconds_whatever_it_asked_for() {
    let (env, project_id, agent_id) = unopened_project(TestEnv::new());
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("partial", &gate));
    let send_id = send_in_process(&env, project_id, agent_id).await;
    let (router, _shutdown) = in_process(&env);
    let session = open_session(&router).await;

    let started = tokio::time::Instant::now();
    let response = post_in_process(
        &router,
        Some(&session),
        &wait_send_call(project_id, send_id, None),
    )
    .await;
    let (message, progress) = read_sse(response, 2).await;
    let elapsed = started.elapsed();

    assert_eq!(tool_reply(&message)["result"]["done"], false);
    assert!(progress.is_empty(), "nothing to keep alive");
    assert!(
        elapsed >= Duration::from_secs(50) && elapsed <= Duration::from_secs(55),
        "returned after {elapsed:?}"
    );
    gate.notify_one();
}

#[tokio::test(start_paused = true)]
async fn a_kept_alive_wait_outlives_the_session_keep_alive_with_progress_every_minute() {
    let (env, project_id, agent_id) = unopened_project(TestEnv::new());
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("done at last", &gate));
    let send_id = send_in_process(&env, project_id, agent_id).await;
    let (router, _shutdown) = in_process(&env);
    let session = open_session(&router).await;

    let release = Arc::clone(&gate);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(450)).await;
        release.notify_one();
    });
    let started = tokio::time::Instant::now();
    let response = post_in_process(
        &router,
        Some(&session),
        &wait_send_call(
            project_id,
            send_id,
            Some(json!({ "progressToken": "wait-1" })),
        ),
    )
    .await;
    let (message, progress) = read_sse(response, 2).await;
    let elapsed = started.elapsed();

    let reply = tool_reply(&message);
    assert_eq!(reply["result"]["done"], true, "{reply}");
    assert_eq!(reply["result"]["recipients"][0]["status"], "completed");
    assert_eq!(reply["result"]["recipients"][0]["text"], "done at last");
    assert!(
        elapsed > Duration::from_mins(5),
        "outlived the session keep-alive: {elapsed:?}"
    );
    assert_eq!(
        progress,
        vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0],
        "one notification per minute of the 450-second wait"
    );

    // The session is still usable afterwards.
    let listed = post_in_process(
        &router,
        Some(&session),
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
    )
    .await;
    assert_eq!(listed.status().as_u16(), 200);
    let (message, _) = read_sse(listed, 3).await;
    assert!(
        message["result"]["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty())
    );
}

// --- the Settings lifecycle --------------------------------------------------------

/// A port nothing listens on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn access(
    env: &TestEnv,
    config_dir: &tempfile::TempDir,
) -> (AgentAccess, Arc<InMemorySecretStore>) {
    let secrets = Arc::new(InMemorySecretStore::new());
    let access = AgentAccess::new(
        Some(config_dir.path().join("config.yaml")),
        Arc::clone(&secrets) as Arc<dyn SecretStore>,
        env.handle(),
        env.home(),
    );
    (access, secrets)
}

#[tokio::test]
async fn turning_access_on_creates_the_token_listens_and_is_remembered_across_launches() {
    let env = TestEnv::new();
    let dir = tempfile::TempDir::new().unwrap();
    let (access, secrets) = access(&env, &dir);
    let port = free_port();

    let status = access.status().await;
    assert!(!status.enabled && !status.listening);
    assert_eq!(status.token, None, "no token until first turned on");

    access.set_port(port).await.unwrap();
    let status = access.set_enabled(true).await.unwrap();
    assert!(status.enabled && status.listening, "{status:?}");
    assert_eq!(status.port, port);
    assert_eq!(status.url, format!("http://127.0.0.1:{port}/mcp"));
    let token = status.token.clone().unwrap();
    assert_eq!(token::load(secrets.as_ref()).unwrap(), Some(token.clone()));
    let response = post(&status.url, &[bearer(&token)], &initialize()).await;
    assert_eq!(response.status().as_u16(), 200);
    access.shut_down().await;

    // The next launch reads the setting and listens again with the same
    // token.
    let relaunched = AgentAccess::new(
        Some(dir.path().join("config.yaml")),
        secrets as Arc<dyn SecretStore>,
        env.handle(),
        env.home(),
    );
    let status = relaunched.resume().await;
    assert!(status.listening, "{status:?}");
    assert_eq!(status.token, Some(token));

    let status = relaunched.set_enabled(false).await.unwrap();
    assert!(!status.enabled && !status.listening);
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "turning it off frees the port"
    );
    assert!(!relaunched.resume().await.listening, "off stays off");
}

#[tokio::test]
async fn a_port_in_use_is_shown_not_fatal() {
    let env = TestEnv::new();
    let dir = tempfile::TempDir::new().unwrap();
    let (access, _secrets) = access(&env, &dir);
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();

    access.set_port(port).await.unwrap();
    let status = access.set_enabled(true).await.unwrap();
    assert!(status.enabled && !status.listening);
    assert!(
        status
            .error
            .as_deref()
            .is_some_and(|e| e.contains("already in use")),
        "{status:?}"
    );

    // Moving to a free port recovers.
    let status = access.set_port(free_port()).await.unwrap();
    assert!(status.listening && status.error.is_none(), "{status:?}");
    access.shut_down().await;
}

#[tokio::test]
async fn regenerating_the_token_refuses_the_old_one_and_ends_open_sessions() {
    let env = TestEnv::new();
    let dir = tempfile::TempDir::new().unwrap();
    let (access, _secrets) = access(&env, &dir);
    access.set_port(free_port()).await.unwrap();
    let before = access.set_enabled(true).await.unwrap();
    let old = before.token.clone().unwrap();
    let client = connect(&before.url, &old).await;

    let after = access.regenerate_token().await.unwrap();
    let new = after.token.clone().unwrap();
    assert_ne!(old, new);
    assert!(after.listening);
    assert_eq!(
        post(&after.url, &[bearer(&old)], &initialize())
            .await
            .status()
            .as_u16(),
        401
    );
    assert_eq!(
        post(&after.url, &[bearer(&new)], &initialize())
            .await
            .status()
            .as_u16(),
        200
    );
    // The session opened with the old token is gone with the old listener.
    let listed = tokio::time::timeout(Duration::from_secs(10), client.list_all_tools()).await;
    assert!(
        !matches!(listed, Ok(Ok(_))),
        "a client holding the old token is cut off"
    );
    access.shut_down().await;
}

#[test]
fn port_zero_is_refused() {
    let env = TestEnv::new();
    let dir = tempfile::TempDir::new().unwrap();
    let (access, _secrets) = access(&env, &dir);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(runtime.block_on(access.set_port(0)).is_err());
}
