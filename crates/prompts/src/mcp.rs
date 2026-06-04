//! The MCP-server prompt provider: connect to a Streamable-HTTP MCP server with
//! a bearer token, list its prompts (following pagination), and render one via
//! `prompts/get`. The server renders; we receive finished text.
//!
//! The wire round-trip is delegated to the official `rmcp` SDK. The
//! response/error *mapping* lives in free functions (`map_prompt`,
//! `extract_text`, `map_service_error`) so it is unit-testable without a server.

use std::collections::BTreeMap;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{
    ErrorCode, GetPromptRequestParams, GetPromptResult, Prompt as McpPrompt, PromptMessageContent,
};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

use crate::error::PromptError;
use crate::model::{Prompt, PromptArgument};
use crate::provider::PromptProvider;

/// A generic HTTP MCP prompt provider. One per configured `mcp_providers` entry.
/// Connects fresh per operation (no pooling — listing happens once per cache
/// build, render once per invocation; pooling is unwarranted complexity for v1).
pub(crate) struct McpProvider {
    name: String,
    url: String,
    bearer: Option<String>,
    timeout: Duration,
}

impl McpProvider {
    pub(crate) fn new(
        name: String,
        url: String,
        bearer: Option<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            name,
            url,
            bearer,
            timeout,
        }
    }

    /// Open a session. The bearer (if any) is sent as the `Authorization`
    /// header; it never appears in any error this returns.
    async fn connect(&self) -> Result<RunningService<RoleClient, ()>, PromptError> {
        let mut config = StreamableHttpClientTransportConfig::with_uri(self.url.clone());
        if let Some(bearer) = &self.bearer {
            config = config.auth_header(bearer.clone());
        }
        let transport = StreamableHttpClientTransport::from_config(config);
        ().serve(transport)
            .await
            .map_err(|e| PromptError::McpConnect {
                provider: self.name.clone(),
                message: e.to_string(),
            })
    }

    /// Connect + `prompts/list` (paginated to completion), bounded by `timeout`,
    /// **surfacing the error** so the caller can record a per-provider status.
    /// The infallible trait `list` wraps this (degrade-to-empty); the cache build
    /// uses this form to distinguish "ok" from "errored" for the Settings UI.
    pub(crate) async fn list_result(&self) -> Result<Vec<Prompt>, PromptError> {
        tokio::time::timeout(self.timeout, self.list_uncapped())
            .await
            .map_err(|_| self.timed_out())?
    }

    async fn list_uncapped(&self) -> Result<Vec<Prompt>, PromptError> {
        let client = self.connect().await?;
        let result = client.list_all_prompts().await;
        let _ = client.cancel().await;
        let prompts = result.map_err(|e| map_service_error(&self.name, None, &e))?;
        Ok(prompts
            .into_iter()
            .map(|p| map_prompt(&self.name, p))
            .collect())
    }

    /// Connect + `prompts/get`, bounded by `timeout`, returning rendered text.
    async fn render_inner(
        &self,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<String, PromptError> {
        tokio::time::timeout(self.timeout, self.render_uncapped(name, args))
            .await
            .map_err(|_| PromptError::McpRequest {
                provider: self.name.clone(),
                name: name.to_owned(),
                message: format!("timed out after {}s", self.timeout.as_secs()),
            })?
    }

    async fn render_uncapped(
        &self,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<String, PromptError> {
        let client = self.connect().await?;
        let mut request = GetPromptRequestParams::new(name.to_owned());
        if let Some(arguments) = to_json_object(args) {
            request = request.with_arguments(arguments);
        }
        let result = client.get_prompt(request).await;
        let _ = client.cancel().await;
        let result = result.map_err(|e| map_service_error(&self.name, Some(name), &e))?;
        extract_text(&self.name, name, &result)
    }

    fn timed_out(&self) -> PromptError {
        PromptError::McpConnect {
            provider: self.name.clone(),
            message: format!("timed out after {}s", self.timeout.as_secs()),
        }
    }
}

impl PromptProvider for McpProvider {
    async fn list(&self) -> Vec<Prompt> {
        match self.list_result().await {
            Ok(prompts) => prompts,
            Err(e) => {
                // Degrade-to-empty-with-warning: one down provider must not fail
                // the cache build or drop other providers' prompts.
                tracing::warn!(provider = %self.name, error = %e, "MCP provider contributed no prompts");
                Vec::new()
            }
        }
    }

    async fn render(
        &self,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<String, PromptError> {
        self.render_inner(name, args).await
    }
}

/// Map an `rmcp` prompt to ours, tagging the provider prefix. MCP's standard
/// `Prompt` has no tags, so `tags` is always empty for MCP prompts.
fn map_prompt(provider: &str, prompt: McpPrompt) -> Prompt {
    Prompt {
        provider: provider.to_owned(),
        name: prompt.name,
        title: prompt.title,
        description: prompt.description,
        arguments: prompt
            .arguments
            .unwrap_or_default()
            .into_iter()
            .map(|a| PromptArgument {
                name: a.name,
                description: a.description,
                required: a.required.unwrap_or(false),
            })
            .collect(),
        tags: Vec::new(),
    }
}

/// Extract the renderable text from a `prompts/get` result: concatenate the text
/// parts (joined by blank lines), dropping non-text parts with a warning. Errors
/// if there is no text at all (nothing to send).
///
/// Multi-message results are **intentionally collapsed**: roles (system/user)
/// are dropped and the texts joined. Switchboard dispatches a single plain user
/// message to the agent, so there is no role structure to preserve — a server
/// returning a system+user prompt becomes one combined message body.
fn extract_text(
    provider: &str,
    name: &str,
    result: &GetPromptResult,
) -> Result<String, PromptError> {
    let mut texts = Vec::new();
    for message in &result.messages {
        match &message.content {
            PromptMessageContent::Text { text } => texts.push(text.clone()),
            other => {
                tracing::warn!(
                    provider = %provider,
                    prompt = %name,
                    kind = content_kind(other),
                    "dropping non-text MCP prompt content part"
                );
            }
        }
    }
    if texts.is_empty() {
        return Err(PromptError::McpEmptyContent {
            provider: provider.to_owned(),
            name: name.to_owned(),
        });
    }
    Ok(texts.join("\n\n"))
}

fn content_kind(content: &PromptMessageContent) -> &'static str {
    match content {
        PromptMessageContent::Text { .. } => "text",
        PromptMessageContent::Image { .. } => "image",
        PromptMessageContent::Resource { .. } => "resource",
        PromptMessageContent::ResourceLink { .. } => "resource_link",
    }
}

/// Map an `rmcp` service error to a `PromptError`. A `-32602` (invalid params —
/// bad name or missing/invalid required argument) becomes `McpInvalidArguments`
/// so the user gets the server's actionable message; everything else is a
/// generic request/connection failure. `name: None` marks a listing failure.
fn map_service_error(provider: &str, name: Option<&str>, error: &ServiceError) -> PromptError {
    if let ServiceError::McpError(data) = error
        && data.code == ErrorCode::INVALID_PARAMS
        && let Some(name) = name
    {
        return PromptError::McpInvalidArguments {
            provider: provider.to_owned(),
            name: name.to_owned(),
            message: data.message.to_string(),
        };
    }
    match name {
        Some(name) => PromptError::McpRequest {
            provider: provider.to_owned(),
            name: name.to_owned(),
            message: error.to_string(),
        },
        None => PromptError::McpConnect {
            provider: provider.to_owned(),
            message: error.to_string(),
        },
    }
}

/// Build the `prompts/get` arguments object from the supplied map. Returns `None`
/// for an empty map (omit the field entirely — let the server apply its own
/// defaults/conditionals for unfilled optionals). Only supplied args are sent.
fn to_json_object(args: &BTreeMap<String, String>) -> Option<rmcp::model::JsonObject> {
    if args.is_empty() {
        return None;
    }
    Some(
        args.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    )
}

/// HTTPS reachability smoke test — guards against a missing TLS backend (a
/// transitive `rmcp`/reqwest feature regression: the streamable-HTTP-client
/// feature pulls reqwest *without* TLS, so HTTPS silently fails with "error
/// sending request"). Hits a stable public HTTPS endpoint that is **not** an MCP
/// server, so it needs no token: if TLS works the connection establishes and the
/// failure is a downstream protocol error, never reqwest's send failure.
///
/// Developer-local (`#[ignore]` — needs network). Run after any `rmcp`/reqwest
/// dependency or feature change: `cargo test -p switchboard-prompts -- --ignored`.
#[cfg(test)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires network — verifies HTTPS/TLS works end to end"]
async fn live_https_connection_establishes() {
    use std::time::Duration;
    let provider = McpProvider::new(
        "(tls-smoke)".to_owned(),
        "https://example.com/".to_owned(),
        None,
        Duration::from_secs(10),
    );
    let err = provider
        .list_result()
        .await
        .expect_err("example.com is not an MCP server, so listing must fail");
    let message = err.to_string();
    assert!(
        !message.contains("error sending request"),
        "HTTPS connection failed — is a TLS backend enabled on rmcp/reqwest? ({message})"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{
        AnnotateAble, ErrorData, PromptArgument as McpArg, PromptMessage, PromptMessageRole,
        RawResource,
    };

    #[test]
    fn maps_prompt_fields_and_defaults_required_to_false() {
        let mut mcp = McpPrompt::new(
            "p",
            Some("d"),
            Some(vec![
                McpArg::new("req")
                    .with_description("desc")
                    .with_required(true),
                McpArg::new("opt"),
            ]),
        );
        // The server's human-friendly title (distinct from the `name` slug).
        mcp.title = Some("Pretty P".to_owned());
        let mapped = map_prompt("team", mcp);
        assert_eq!(mapped.provider, "team");
        assert_eq!(mapped.name, "p");
        assert_eq!(mapped.title.as_deref(), Some("Pretty P"));
        assert_eq!(mapped.description.as_deref(), Some("d"));
        assert!(mapped.tags.is_empty());
        assert_eq!(mapped.arguments.len(), 2);
        assert!(mapped.arguments[0].required);
        assert_eq!(mapped.arguments[0].description.as_deref(), Some("desc"));
        assert!(
            !mapped.arguments[1].required,
            "missing required defaults to false"
        );
    }

    #[test]
    fn extracts_text_and_drops_non_text_parts() {
        let result = GetPromptResult::new(vec![
            PromptMessage::new_text(PromptMessageRole::User, "hello"),
            PromptMessage::new_resource_link(
                PromptMessageRole::User,
                RawResource::new("file:///x", "x").no_annotation(),
            ),
            PromptMessage::new_text(PromptMessageRole::User, "world"),
        ]);
        let text = extract_text("team", "p", &result).unwrap();
        assert_eq!(text, "hello\n\nworld");
    }

    #[test]
    fn no_text_content_is_an_error() {
        let result = GetPromptResult::new(vec![PromptMessage::new_resource_link(
            PromptMessageRole::User,
            RawResource::new("file:///x", "x").no_annotation(),
        )]);
        let err = extract_text("team", "p", &result).unwrap_err();
        assert!(matches!(err, PromptError::McpEmptyContent { .. }));
    }

    #[test]
    fn invalid_params_maps_to_actionable_argument_error() {
        let error = ServiceError::McpError(ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            "missing required argument: focus",
            None,
        ));
        let mapped = map_service_error("team", Some("review"), &error);
        match mapped {
            PromptError::McpInvalidArguments { name, message, .. } => {
                assert_eq!(name, "review");
                assert!(message.contains("focus"));
            }
            other => panic!("expected McpInvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn non_param_error_on_render_is_generic_request_failure() {
        let error = ServiceError::McpError(ErrorData::new(ErrorCode::INTERNAL_ERROR, "boom", None));
        let mapped = map_service_error("team", Some("review"), &error);
        assert!(matches!(mapped, PromptError::McpRequest { .. }));
    }

    #[test]
    fn listing_error_maps_to_connect_failure() {
        let error = ServiceError::McpError(ErrorData::new(ErrorCode::INTERNAL_ERROR, "boom", None));
        let mapped = map_service_error("team", None, &error);
        assert!(matches!(mapped, PromptError::McpConnect { .. }));
    }

    #[test]
    fn empty_args_omits_the_arguments_object() {
        assert!(to_json_object(&BTreeMap::new()).is_none());
        let args = BTreeMap::from([("k".to_owned(), "v".to_owned())]);
        let obj = to_json_object(&args).unwrap();
        assert_eq!(obj.get("k").and_then(|v| v.as_str()), Some("v"));
    }

    // The per-provider timeout is M2's headline resilience guarantee; these tests
    // drive a real in-process server that stalls past a short timeout, so the
    // `tokio::time::timeout` actually fires (the dead-port integration test fails
    // fast and never exercises it).
    mod timeout {
        use super::super::*;
        use rmcp::ServerHandler;
        use rmcp::model::{
            ListPromptsResult, PaginatedRequestParams, Prompt as McpPrompt, ServerCapabilities,
            ServerInfo,
        };
        use rmcp::service::{RequestContext, RoleServer};
        use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService,
        };
        use std::sync::Arc;
        use std::time::Duration;

        #[derive(Clone)]
        struct StallingServer;

        impl ServerHandler for StallingServer {
            fn get_info(&self) -> ServerInfo {
                let mut info = ServerInfo::default();
                info.capabilities = ServerCapabilities::builder().enable_prompts().build();
                info
            }

            async fn list_prompts(
                &self,
                _request: Option<PaginatedRequestParams>,
                _context: RequestContext<RoleServer>,
            ) -> Result<ListPromptsResult, rmcp::model::ErrorData> {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(ListPromptsResult {
                    meta: None,
                    next_cursor: None,
                    prompts: vec![McpPrompt::new("slow", None::<String>, None)],
                })
            }
        }

        async fn spawn_stalling_server() -> String {
            let service = StreamableHttpService::new(
                || Ok(StallingServer),
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

        #[tokio::test(flavor = "multi_thread")]
        async fn list_times_out_and_degrades_to_empty() {
            let url = spawn_stalling_server().await;
            // 50ms budget < the server's 500ms stall → timeout fires → empty.
            let provider =
                McpProvider::new("team".to_owned(), url, None, Duration::from_millis(50));
            assert!(provider.list().await.is_empty());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn list_succeeds_within_a_generous_timeout() {
            let url = spawn_stalling_server().await;
            // Control: a generous budget lets the same stalling server respond,
            // proving the empty result above is the timeout — not a broken server.
            let provider = McpProvider::new("team".to_owned(), url, None, Duration::from_secs(5));
            let prompts = provider.list().await;
            assert_eq!(prompts.len(), 1);
            assert_eq!(prompts[0].name, "slow");
        }
    }
}
