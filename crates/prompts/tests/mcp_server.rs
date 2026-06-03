//! Hermetic end-to-end test of the MCP provider against an **in-process** rmcp
//! Streamable-HTTP server on an ephemeral port. A real HTTP round-trip through
//! the official SDK on both ends — no external server, runs in `make test`/CI.
//!
//! Exercised through the public `PromptService` API (the same surface the app
//! drives), so this covers config parse → keychain bearer resolve → connect →
//! `prompts/list` (paginated) → cache, and `prompts/get` render + error mapping.

use std::collections::BTreeMap;
use std::sync::Arc;

use rmcp::ServerHandler;
use rmcp::model::{
    ErrorCode, ErrorData, GetPromptRequestParams, GetPromptResult, ListPromptsResult,
    PaginatedRequestParams, Prompt, PromptArgument, PromptMessage, PromptMessageRole,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use switchboard_prompts::{InMemorySecretStore, PromptError, PromptService, SecretStore};
use tempfile::TempDir;

/// A tiny prompts-only MCP server. `list_prompts` returns two prompts across two
/// pages (to exercise `nextCursor`); `get_prompt` echoes the `who` argument into
/// text and rejects a missing required argument with `-32602`.
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
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let cursor = request.and_then(|r| r.cursor);
        match cursor.as_deref() {
            None => Ok(ListPromptsResult {
                meta: None,
                next_cursor: Some("page-2".into()),
                prompts: vec![Prompt::new(
                    "greet",
                    Some("Greet someone"),
                    Some(vec![
                        PromptArgument::new("who")
                            .with_description("who to greet")
                            .with_required(true),
                    ]),
                )],
            }),
            Some("page-2") => Ok(ListPromptsResult {
                meta: None,
                next_cursor: None,
                prompts: vec![Prompt::new("farewell", Some("Say goodbye"), None)],
            }),
            Some(_) => Ok(ListPromptsResult::default()),
        }
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        match request.name.as_str() {
            "greet" => {
                let who = request
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("who"))
                    .and_then(|v| v.as_str());
                match who {
                    Some(who) => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                        PromptMessageRole::User,
                        format!("Hello, {who}!"),
                    )])),
                    None => Err(ErrorData::new(
                        ErrorCode::INVALID_PARAMS,
                        "missing required argument: who",
                        None,
                    )),
                }
            }
            other => Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                format!("unknown prompt: {other}"),
                None,
            )),
        }
    }
}

/// Bind the in-process server on an ephemeral port; return its `/mcp` URL.
async fn spawn_server() -> String {
    let service = StreamableHttpService::new(
        || Ok(TestPromptServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}/mcp")
}

/// A `PromptService` with one local prompt and one MCP provider (`team` → `url`),
/// bearer stored in the secret store.
fn service_with(url: &str, bearer: &str) -> (TempDir, PromptService) {
    let tmp = TempDir::new().unwrap();
    let prompts_dir = tmp.path().join("prompts");
    std::fs::create_dir(&prompts_dir).unwrap();
    std::fs::write(
        prompts_dir.join("note.md"),
        "---\nname: note\ndescription: a local note\n---\nLocal body\n",
    )
    .unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        format!(
            "mcp_providers:\n  - name: team\n    transport:\n      type: http\n      url: {url}\n"
        ),
    )
    .unwrap();

    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
    secrets.set("team", bearer).unwrap();
    let service = PromptService::new(config_path, prompts_dir, None, secrets);
    (tmp, service)
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_local_and_paginated_mcp_prompts() {
    let url = spawn_server().await;
    let (_tmp, service) = service_with(&url, "any-bearer");

    service.sync().await;
    let prompts = service.list();

    // Local prompt + both MCP prompts (across two pages) are all present.
    let by_id: Vec<String> = prompts
        .iter()
        .map(|p| format!("{}:{}", p.provider, p.name))
        .collect();
    assert!(
        by_id.contains(&"local:note".to_owned()),
        "missing local: {by_id:?}"
    );
    assert!(
        by_id.contains(&"team:greet".to_owned()),
        "missing page 1: {by_id:?}"
    );
    assert!(
        by_id.contains(&"team:farewell".to_owned()),
        "missing page 2 (pagination not followed): {by_id:?}"
    );

    // MCP prompt metadata is mapped (required flag preserved).
    let greet = prompts.iter().find(|p| p.name == "greet").unwrap();
    assert_eq!(greet.arguments.len(), 1);
    assert!(greet.arguments[0].required);
}

#[tokio::test(flavor = "multi_thread")]
async fn renders_mcp_prompt_server_side() {
    let url = spawn_server().await;
    let (_tmp, service) = service_with(&url, "any-bearer");

    let args = BTreeMap::from([("who".to_owned(), "Ada".to_owned())]);
    let rendered = service.render("team", "greet", &args).await.unwrap();
    assert_eq!(rendered.text, "Hello, Ada!");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_required_arg_maps_to_actionable_error() {
    let url = spawn_server().await;
    let (_tmp, service) = service_with(&url, "any-bearer");

    // No `who` supplied → server returns -32602 → mapped to McpInvalidArguments,
    // surfacing the server's message (which names the argument).
    let err = service
        .render("team", "greet", &BTreeMap::new())
        .await
        .unwrap_err();
    match err {
        PromptError::McpInvalidArguments { name, message, .. } => {
            assert_eq!(name, "greet");
            assert!(message.contains("who"), "message: {message}");
        }
        other => panic!("expected McpInvalidArguments, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_provider_contributes_nothing_without_breaking_local() {
    // Port 1 is reserved/closed → connection refused.
    let (_tmp, service) = service_with("http://127.0.0.1:1/mcp", "any-bearer");

    service.sync().await;
    let names: Vec<String> = service.list().into_iter().map(|p| p.name).collect();

    // The down provider drops out silently; the local prompt is unaffected.
    assert_eq!(names, vec!["note".to_owned()]);
}
