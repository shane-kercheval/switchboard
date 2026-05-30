use chrono::Utc;
use serde_json::Value;
use switchboard_core::AgentId;

use crate::events::{
    AdapterEvent, ContentKind, FailureKind, McpServerStatus, ToolKind, TurnId, TurnOutcome,
    TurnUsage,
};

/// Authored auth-failure message for Claude. Replaces Claude's
/// `"Not logged in · Please run /login"` (which refers to the
/// interactive-session slash command, not the CLI command users would
/// typically run from a terminal). The authored copy names the CLI
/// recovery (`claude auth login`) and matches the cross-harness format.
/// Reactive-auth posture — never advises "reload Switchboard."
pub const CLAUDE_AUTH_MESSAGE: &str = "Claude authentication required — run `claude auth login`";

#[derive(Debug)]
pub enum ParseOutcome {
    /// One adapter event was produced. The common case.
    Event(AdapterEvent),
    /// A single line emitted multiple events (e.g., an `assistant` event with
    /// several `tool_use` content blocks). Order is preserved.
    Events(Vec<AdapterEvent>),
    /// Recognized but produces no event.
    Skip,
    /// Line is not valid JSON.
    Error(String),
}

/// Per-turn parser state. Tracks the text-block boundary signals from the
/// stream-json `content_block_start` / `content_block_stop` events so the
/// parser can insert paragraph separators between distinct text blocks
/// within a single turn (claude legitimately emits multiple text blocks
/// per turn when it interleaves text and tool calls).
///
/// Without this, two text blocks separated by a tool-use block (which the
/// parser skips at the delta layer; tool starts/completions are emitted from
/// the `assistant` / `user` envelopes instead) would concatenate directly
/// with no whitespace, producing run-on output like
/// `"...what can I help with today?Saved your name to memory..."`.
#[derive(Debug, Default)]
pub struct ParserState {
    /// Whether at least one text-kind `ContentChunk` has been emitted in
    /// this turn. (Tool events don't drive separator logic; only text-block
    /// boundaries do.) A leading separator is only sensible *between* text
    /// blocks, never before the first one.
    text_chunk_emitted_in_turn: bool,
    /// Set true when a new text block opens *after* prior text has already
    /// been emitted. Cleared when the next `ContentChunk` is emitted (the
    /// separator is prepended onto that chunk's text).
    pending_separator: bool,
    /// Auth-failure stash: `Some(message)` means an `assistant` envelope with
    /// `"error": "authentication_failed"` was observed earlier in this turn.
    /// The stashed message is the authored Switchboard auth string
    /// (`CLAUDE_AUTH_MESSAGE`), not the harness's raw text — authoring
    /// happens at stash time. `parse_result` consumes via `.take()` and
    /// refines the terminal `TurnEnd` from `HarnessError` to `AuthFailure`.
    /// State-flag pattern: `parse_result` remains the sole `TurnEnd` emitter,
    /// preserving the exactly-one-terminal-event invariant.
    pending_auth_failure: Option<String>,
}

/// Parse one stream-json line. Stateful: `state` accumulates text-block
/// boundary information across lines within a single turn. Construct a
/// fresh `ParserState::default()` per turn.
///
/// `agent_id` is used to anchor agent-scoped events (`SessionMeta`,
/// `RateLimitEvent`) that have no turn anchor.
///
/// `AdapterEvent::TurnStart` is never emitted here — it is dispatcher-owned.
pub fn parse_line(
    line: &str,
    turn_id: TurnId,
    agent_id: AgentId,
    state: &mut ParserState,
) -> ParseOutcome {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return ParseOutcome::Error(e.to_string()),
    };

    // Suppress subagent-internal events at the parent stream level.
    //
    // When Claude's `Agent` tool delegates to a subagent, the parent's stream
    // carries every subagent event tagged with `parent_tool_use_id = <Agent
    // tool_use id>`. The parent's own events (including the `Agent` call and
    // its aggregate `tool_result`) carry `null` or absent. Without this
    // short-circuit, subagent-internal `tool_use` / `tool_result` blocks
    // emit `ToolStarted` / `ToolCompleted` at the parent's `turn_id` and the
    // live transcript mis-attributes the subagent's work to the parent —
    // diverging from the rehydrated view (Claude already collapses on disk:
    // the main session file holds only the parent's `Agent` call + aggregate
    // result; subagent internals live in `<session-id>/subagents/agent-<id>.jsonl`).
    //
    // Conservative rule: skip on *any* non-null `parent_tool_use_id`,
    // regardless of record type. Probed against Claude 2.1.153: the field is
    // absent on `result` / `system` / `rate_limit_event`, always null on
    // `stream_event`, and only ever non-null on `assistant` / `user`
    // envelopes that originate from inside a subagent. The conservative rule
    // is therefore safe for every observed record shape and forward-compatible
    // with any new tagged shape (the third — a `user` envelope with text
    // content relaying the subagent's task instruction — was first observed
    // here, not in the original 2026-05-24 probes).
    //
    // From the parent's view, a delegation is a single tool call. Matches
    // Gemini's `invoke_agent` (already opaque) and Antigravity's
    // `invoke_subagent` (separate brain conversation we don't tail).
    if value
        .get("parent_tool_use_id")
        .and_then(Value::as_str)
        .is_some()
    {
        return ParseOutcome::Skip;
    }

    match value.get("type").and_then(Value::as_str) {
        Some("stream_event") => parse_stream_event(&value, turn_id, state),
        Some("result") => parse_result(&value, turn_id, state),
        Some("system") => parse_system_event(&value, agent_id),
        Some("assistant") => parse_assistant_envelope(&value, turn_id, state),
        Some("user") => parse_user_envelope(&value, turn_id),
        Some("rate_limit_event") => parse_rate_limit_event(&value, agent_id),
        _ => ParseOutcome::Skip,
    }
}

fn parse_stream_event(obj: &Value, turn_id: TurnId, state: &mut ParserState) -> ParseOutcome {
    let Some(event) = obj.get("event") else {
        return ParseOutcome::Skip;
    };

    match event.get("type").and_then(Value::as_str) {
        Some("content_block_start") => {
            let block_type = event
                .get("content_block")
                .and_then(|cb| cb.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if block_type == "text" && state.text_chunk_emitted_in_turn {
                // A new text block is opening after prior text — separator
                // will be prepended onto its first emitted chunk.
                state.pending_separator = true;
            }
            ParseOutcome::Skip
        }
        Some("content_block_delta") => parse_content_block_delta(event, turn_id, state),
        _ => ParseOutcome::Skip,
    }
}

fn parse_content_block_delta(
    event: &Value,
    turn_id: TurnId,
    state: &mut ParserState,
) -> ParseOutcome {
    let Some(delta) = event.get("delta") else {
        return ParseOutcome::Skip;
    };

    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        // input_json_delta (tool input), thinking_delta, etc. — all skipped.
        return ParseOutcome::Skip;
    }

    let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
    if text.is_empty() {
        return ParseOutcome::Skip;
    }

    // Interim: the `\n\n` separator is synthesized inline into the chunk text
    // here, which conflates parsing with presentation. A cleaner shape is a
    // structured `TextBlockBoundary` wire variant that lets the reducer / UI
    // choose how to render block boundaries. Future work if `\n\n` proves a
    // rendering issue.
    let chunk_text = if state.pending_separator {
        state.pending_separator = false;
        format!("\n\n{text}")
    } else {
        text.to_owned()
    };
    state.text_chunk_emitted_in_turn = true;

    ParseOutcome::Event(AdapterEvent::ContentChunk {
        turn_id,
        kind: ContentKind::Text,
        text: chunk_text,
    })
}

fn parse_result(obj: &Value, turn_id: TurnId, state: &mut ParserState) -> ParseOutcome {
    let is_error = obj
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let has_api_error = obj.get("api_error_status").is_some_and(|v| !v.is_null());

    // Consume the auth-failure stash (state-flag pattern). When the prior
    // `assistant` envelope flagged auth failure, refine the terminal
    // outcome from `HarnessError` to `AuthFailure` — `parse_result` remains
    // the sole `TurnEnd` emitter, preserving the single-terminal-event
    // contract. Usage extraction below still runs (auth-failure results
    // carry zero-valued telemetry, which is legitimate, not noise).
    let auth_failure = state.pending_auth_failure.take();
    let outcome = if let Some(auth_message) = auth_failure {
        TurnOutcome::Failed {
            kind: FailureKind::AuthFailure,
            message: auth_message,
        }
    } else if is_error || has_api_error {
        let message = obj
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("harness reported an error")
            .to_owned();
        TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message,
        }
    } else {
        TurnOutcome::Completed
    };

    let usage = extract_usage_from_result(obj);

    ParseOutcome::Event(AdapterEvent::TurnEnd {
        turn_id,
        outcome,
        ended_at: Utc::now(),
        usage,
    })
}

/// Pull `TurnUsage` from a `result` event.
///
/// `input_tokens` and `output_tokens` are **required** numeric fields: if
/// either is missing or non-numeric, returns `None`. Malformed or missing
/// usage → `None`, never a fabricated zero-Some. Zero values from a real
/// harness (auth-failure synthetic responses, certain edge cases) DO
/// produce a valid `Some` — what matters is whether the
/// schema is present, not whether the values are non-zero.
///
/// Populated for both Completed and Failed turns. The harness charges for
/// partial work, so token counts on failure are meaningful telemetry.
fn extract_usage_from_result(obj: &Value) -> Option<TurnUsage> {
    let usage_obj = obj.get("usage")?;

    let input_tokens = usage_obj.get("input_tokens").and_then(Value::as_u64)?;
    let output_tokens = usage_obj.get("output_tokens").and_then(Value::as_u64)?;

    let cached_input_tokens = usage_obj
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .or_else(|| usage_obj.get("cached_input_tokens").and_then(Value::as_u64));
    let reasoning_output_tokens = usage_obj
        .get("reasoning_output_tokens")
        .and_then(Value::as_u64);

    let total_cost_usd = obj.get("total_cost_usd").and_then(Value::as_f64);
    let context_window = select_context_window(obj);

    Some(TurnUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        reasoning_output_tokens,
        context_window,
        total_cost_usd,
    })
}

/// Pick the right `contextWindow` from `result.modelUsage` per the selection
/// rule:
///
/// 1. If `result.model` matches a `modelUsage` key, use that entry.
/// 2. Otherwise, use the entry with the largest `inputTokens` (the primary
///    model did the heavy lifting; routing / subordinate models have minimal
///    tokens).
/// 3. Empty or missing `modelUsage` → `None`.
fn select_context_window(result: &Value) -> Option<u32> {
    let model_usage = result.get("modelUsage").and_then(Value::as_object)?;
    if model_usage.is_empty() {
        return None;
    }

    let primary_model = result.get("model").and_then(Value::as_str);
    if let Some(model) = primary_model
        && let Some(entry) = model_usage.get(model)
        && let Some(cw) = entry.get("contextWindow").and_then(Value::as_u64)
    {
        return u32::try_from(cw).ok();
    }

    let max_entry = model_usage
        .values()
        .max_by_key(|v| v.get("inputTokens").and_then(Value::as_u64).unwrap_or(0))?;
    let cw = max_entry.get("contextWindow").and_then(Value::as_u64)?;
    u32::try_from(cw).ok()
}

fn parse_system_event(obj: &Value, agent_id: AgentId) -> ParseOutcome {
    if obj.get("subtype").and_then(Value::as_str) != Some("init") {
        return ParseOutcome::Skip;
    }

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let harness_version = obj
        .get("claude_code_version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let tools = obj
        .get("tools")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mcp_servers = obj
        .get("mcp_servers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(parse_mcp_server_status)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let skills = obj
        .get("skills")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    ParseOutcome::Event(AdapterEvent::SessionMeta {
        agent_id,
        model,
        harness_version,
        tools,
        mcp_servers,
        skills,
        raw: obj.clone(),
    })
}

fn parse_mcp_server_status(v: &Value) -> Option<McpServerStatus> {
    Some(McpServerStatus {
        name: v.get("name").and_then(Value::as_str)?.to_owned(),
        status: v.get("status").and_then(Value::as_str)?.to_owned(),
    })
}

/// Parse an `assistant` envelope: emit `ToolStarted` for each `tool_use`
/// content block. Text content is handled at the delta layer in
/// `parse_stream_event` (the envelope arrives after all the deltas), so we
/// don't emit `ContentChunk`s from here — that would double-emit.
///
/// **Auth-failure detection (state-flag pattern).** If the envelope carries
/// top-level `"error": "authentication_failed"`, stash the displayable
/// message on `state.pending_auth_failure` for `parse_result` to consume;
/// do **not** emit a terminal event here. The result envelope remains the
/// sole `TurnEnd` emitter; the stash just refines its `FailureKind` from
/// `HarnessError` to `AuthFailure`.
fn parse_assistant_envelope(obj: &Value, turn_id: TurnId, state: &mut ParserState) -> ParseOutcome {
    if obj.get("error").and_then(Value::as_str) == Some("authentication_failed") {
        // Stash the authored Switchboard auth message rather than Claude's
        // own `Please run /login` (which is the interactive-session slash
        // command, not the CLI command). Authoring keeps the user-facing
        // copy consistent across all four harnesses' auth surfaces and
        // names the right recovery (the `claude auth login` CLI command).
        // Reactive-auth posture — never advises "reload Switchboard."
        state.pending_auth_failure = Some(CLAUDE_AUTH_MESSAGE.to_owned());
        // Fall through to tool_use extraction — an auth-failed assistant
        // envelope from claude is unlikely to carry tool_use blocks (the
        // synthesized response is plain text), but bypassing extraction
        // here would silently drop them if a future shape change adds any.
    }

    let Some(content) = obj
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return ParseOutcome::Skip;
    };

    let mut events = Vec::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            let Some(id) = block.get("id").and_then(Value::as_str) else {
                continue;
            };
            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
            let input = block.get("input").cloned().unwrap_or(Value::Null);
            events.push(AdapterEvent::ToolStarted {
                turn_id,
                tool_use_id: id.to_owned(),
                kind: classify_claude_tool_kind(name),
                name: name.to_owned(),
                input,
            });
        }
    }

    match events.len() {
        0 => ParseOutcome::Skip,
        1 => ParseOutcome::Event(events.into_iter().next().expect("len==1")),
        _ => ParseOutcome::Events(events),
    }
}

/// Claude-side tool kind classification. `mcp__` prefix → MCP per the
/// documented Claude Code naming convention; otherwise treated as `Builtin`.
/// `Plugin` / `Other` are reserved variants we don't currently emit (no
/// reliable evidence on which Claude tool names map to those).
///
/// `pub(crate)` so the session-file parser in `claude_code/session_file.rs`
/// can reuse the same prefix discriminator — disk and stream emit the same
/// `mcp__<server>__<tool>` shape, so a single classifier covers both.
pub(crate) fn classify_claude_tool_kind(name: &str) -> ToolKind {
    if name.starts_with("mcp__") {
        ToolKind::Mcp
    } else {
        ToolKind::Builtin
    }
}

/// Parse a `user` envelope: emit `ToolCompleted` for each `tool_result`
/// content block. (User envelopes also carry plain user messages, but
/// those don't drive any adapter event.)
fn parse_user_envelope(obj: &Value, turn_id: TurnId) -> ParseOutcome {
    let Some(content) = obj
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return ParseOutcome::Skip;
    };

    let mut events = Vec::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let output = stringify_tool_result_content(block.get("content"));
            events.push(AdapterEvent::ToolCompleted {
                turn_id,
                tool_use_id: tool_use_id.to_owned(),
                output,
                is_error,
            });
        }
    }

    match events.len() {
        0 => ParseOutcome::Skip,
        1 => ParseOutcome::Event(events.into_iter().next().expect("len==1")),
        _ => ParseOutcome::Events(events),
    }
}

/// Claude's `tool_result.content` is either a scalar string or an array of
/// content blocks (`{type: "text", text: "..."}`, plus future image / other
/// types). We concatenate the text blocks; if every block is non-text we
/// emit a `[non-text tool result omitted]` placeholder so the operator sees
/// that something was there rather than an empty tool result.
///
/// **Mixed-content arrays** (e.g., `[image, text, image]`) emit only the
/// joined text — non-text blocks are dropped silently with no per-block
/// placeholder. The placeholder is only emitted when *every* block is
/// non-text. Per-block placeholders are future work if rich tool output
/// rendering surfaces a need.
fn stringify_tool_result_content(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    if let Some(arr) = content.as_array() {
        let mut texts = Vec::new();
        let mut had_non_text = false;
        for block in arr {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    texts.push(t);
                }
            } else {
                had_non_text = true;
            }
        }
        if texts.is_empty() && had_non_text {
            return "[non-text tool result omitted]".to_owned();
        }
        return texts.join("\n");
    }
    String::new()
}

fn parse_rate_limit_event(obj: &Value, agent_id: AgentId) -> ParseOutcome {
    let info = obj.get("rate_limit_info").cloned().unwrap_or(Value::Null);
    // Claude's rate-limit payload lives only on the live stream — no
    // session-file equivalent (class C). Mark it `StreamOnly` so the
    // dispatcher persists it to the metadata sidecar for restart continuity.
    ParseOutcome::Event(AdapterEvent::RateLimitEvent {
        agent_id,
        info,
        source: crate::events::RateLimitSource::StreamOnly,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn tid() -> TurnId {
        Uuid::now_v7()
    }

    fn aid() -> AgentId {
        Uuid::now_v7()
    }

    fn parse_one(line: &str, turn_id: TurnId) -> ParseOutcome {
        let mut state = ParserState::default();
        parse_line(line, turn_id, aid(), &mut state)
    }

    fn parse_one_with_agent(line: &str, turn_id: TurnId, agent_id: AgentId) -> ParseOutcome {
        let mut state = ParserState::default();
        parse_line(line, turn_id, agent_id, &mut state)
    }

    #[test]
    fn text_delta_yields_content_chunk_with_text_kind() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}}"#;
        let turn_id = tid();
        match parse_one(line, turn_id) {
            ParseOutcome::Event(AdapterEvent::ContentChunk { text, kind, .. }) => {
                assert_eq!(text, "hello");
                assert_eq!(kind, ContentKind::Text);
            }
            _ => panic!("expected ContentChunk"),
        }
    }

    #[test]
    fn result_success_yields_turn_end_completed() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":null,"result":"4"}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }) => {}
            _ => panic!("expected TurnEnd(Completed)"),
        }
    }

    #[test]
    fn result_is_error_true_yields_harness_error() {
        let line =
            r#"{"type":"result","is_error":true,"api_error_status":404,"result":"bad model"}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome:
                    TurnOutcome::Failed {
                        kind: FailureKind::HarnessError,
                        message,
                    },
                ..
            }) => {
                assert_eq!(message, "bad model");
            }
            _ => panic!("expected TurnEnd(Failed(HarnessError))"),
        }
    }

    #[test]
    fn result_api_error_status_non_null_yields_harness_error() {
        let line =
            r#"{"type":"result","is_error":false,"api_error_status":500,"result":"server error"}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome:
                    TurnOutcome::Failed {
                        kind: FailureKind::HarnessError,
                        ..
                    },
                ..
            }) => {}
            _ => panic!("expected TurnEnd(Failed(HarnessError))"),
        }
    }

    #[test]
    fn result_with_usage_populates_turn_usage() {
        let line = r#"{"type":"result","is_error":false,"api_error_status":null,"result":"ok","model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":25,"cache_read_input_tokens":50},"modelUsage":{"claude-sonnet-4-6":{"inputTokens":100,"outputTokens":25,"contextWindow":200000}},"total_cost_usd":0.05}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                usage: Some(usage), ..
            }) => {
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 25);
                assert_eq!(usage.cached_input_tokens, Some(50));
                assert_eq!(usage.context_window, Some(200_000));
                assert!((usage.total_cost_usd.unwrap() - 0.05).abs() < f64::EPSILON);
            }
            _ => panic!("expected TurnEnd with Some(usage)"),
        }
    }

    #[test]
    fn result_with_empty_model_usage_yields_no_context_window() {
        let line = r#"{"type":"result","is_error":false,"api_error_status":null,"result":"ok","usage":{"input_tokens":10,"output_tokens":3},"modelUsage":{},"total_cost_usd":0.01}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                usage: Some(usage), ..
            }) => {
                assert_eq!(usage.context_window, None);
                assert_eq!(usage.input_tokens, 10);
            }
            _ => panic!("expected TurnEnd with Some(usage)"),
        }
    }

    #[test]
    fn result_with_missing_required_usage_fields_yields_none() {
        // Malformed or missing usage → None, never a fabricated zero-Some.
        // The Claude auth-failure synthetic response has `"usage":{}` (no
        // input_tokens / output_tokens fields); that must surface as
        // `usage: None` so consumers can distinguish "telemetry
        // unparseable" from "real zero-usage turn."
        let line = r#"{"type":"result","is_error":true,"api_error_status":null,"result":"err","usage":{}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { usage: None, .. }) => {}
            _ => panic!("expected TurnEnd with None usage when required fields are absent"),
        }
    }

    #[test]
    fn result_with_zero_token_counts_still_yields_some() {
        // Schema present with numeric zeros IS valid telemetry — Claude's
        // synthetic responses do this. We return Some so the absence of
        // the schema is the only thing that produces None.
        let line = r#"{"type":"result","is_error":true,"api_error_status":null,"result":"err","usage":{"input_tokens":0,"output_tokens":0}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                usage: Some(usage), ..
            }) => {
                assert_eq!(usage.input_tokens, 0);
                assert_eq!(usage.output_tokens, 0);
            }
            _ => panic!("expected TurnEnd with Some(usage) when zero-token schema is present"),
        }
    }

    #[test]
    fn result_with_missing_usage_object_yields_none() {
        let line = r#"{"type":"result","is_error":false,"api_error_status":null,"result":"ok"}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { usage: None, .. }) => {}
            _ => panic!("expected TurnEnd with None usage when usage field is absent"),
        }
    }

    #[test]
    fn select_context_window_prefers_top_level_model() {
        // result.model points at the primary; even if the routing model has more
        // input tokens, the primary wins.
        let result = json!({
            "model": "claude-opus-4-7[1m]",
            "modelUsage": {
                "claude-haiku-4-5": {"inputTokens": 10_000, "contextWindow": 200_000},
                "claude-opus-4-7[1m]": {"inputTokens": 50, "contextWindow": 1_000_000}
            }
        });
        assert_eq!(select_context_window(&result), Some(1_000_000));
    }

    #[test]
    fn select_context_window_falls_back_to_largest_input_tokens() {
        // No top-level model field — pick the entry with the largest inputTokens.
        let result = json!({
            "modelUsage": {
                "subordinate": {"inputTokens": 50, "contextWindow": 64000},
                "primary": {"inputTokens": 5000, "contextWindow": 200_000}
            }
        });
        assert_eq!(select_context_window(&result), Some(200_000));
    }

    #[test]
    fn select_context_window_empty_modelusage_returns_none() {
        let result = json!({"modelUsage": {}});
        assert_eq!(select_context_window(&result), None);
    }

    #[test]
    fn select_context_window_missing_modelusage_returns_none() {
        let result = json!({"result": "ok"});
        assert_eq!(select_context_window(&result), None);
    }

    #[test]
    fn system_init_yields_session_meta() {
        let agent_id = aid();
        let line = r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"00000000-0000-7000-8000-000000000001","tools":["Bash","Read","mcp__srv__do"],"mcp_servers":[{"name":"srv","status":"connected"}],"model":"claude-sonnet-4-6","claude_code_version":"2.1.140","skills":["debug"]}"#;
        match parse_one_with_agent(line, tid(), agent_id) {
            ParseOutcome::Event(AdapterEvent::SessionMeta {
                agent_id: aid_out,
                model,
                harness_version,
                tools,
                mcp_servers,
                skills,
                ..
            }) => {
                assert_eq!(aid_out, agent_id);
                assert_eq!(model, "claude-sonnet-4-6");
                assert_eq!(harness_version, "2.1.140");
                assert_eq!(tools, vec!["Bash", "Read", "mcp__srv__do"]);
                assert_eq!(mcp_servers.len(), 1);
                assert_eq!(mcp_servers[0].name, "srv");
                assert_eq!(mcp_servers[0].status, "connected");
                assert_eq!(skills, vec!["debug"]);
            }
            _ => panic!("expected SessionMeta"),
        }
    }

    #[test]
    fn system_non_init_subtype_is_skipped() {
        let line = r#"{"type":"system","subtype":"compact_boundary","data":{}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn assistant_with_tool_use_yields_tool_started() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_x","name":"Bash","input":{"command":"ls"}}]}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::ToolStarted {
                tool_use_id,
                kind,
                name,
                input,
                ..
            }) => {
                assert_eq!(tool_use_id, "toolu_x");
                assert_eq!(kind, ToolKind::Builtin);
                assert_eq!(name, "Bash");
                assert_eq!(input["command"], "ls");
            }
            _ => panic!("expected ToolStarted"),
        }
    }

    #[test]
    fn assistant_with_mcp_tool_use_classifies_as_mcp_kind() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_m","name":"mcp__server__list_tags","input":{}}]}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::ToolStarted { kind, name, .. }) => {
                assert_eq!(kind, ToolKind::Mcp);
                assert_eq!(name, "mcp__server__list_tags");
            }
            _ => panic!("expected ToolStarted with Mcp kind"),
        }
    }

    #[test]
    fn assistant_with_only_text_content_yields_no_tool_event() {
        // Preserves the boundary the old `assistant_message_is_skipped` test was
        // guarding: text-only assistant envelopes produce no ToolStarted.
        // Text comes from deltas, not the envelope.
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn assistant_with_multiple_tool_use_blocks_yields_events_vec() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}},{"type":"tool_use","id":"t2","name":"Read","input":{}}]}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Events(events) => {
                assert_eq!(events.len(), 2);
                assert!(
                    matches!(&events[0], AdapterEvent::ToolStarted { tool_use_id, .. } if tool_use_id == "t1")
                );
                assert!(
                    matches!(&events[1], AdapterEvent::ToolStarted { tool_use_id, .. } if tool_use_id == "t2")
                );
            }
            _ => panic!("expected Events vec for multiple tool_use blocks"),
        }
    }

    #[test]
    fn user_with_tool_result_yields_tool_completed() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_x","content":"hello\n","is_error":false}]}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            }) => {
                assert_eq!(tool_use_id, "toolu_x");
                assert_eq!(output, "hello\n");
                assert!(!is_error);
            }
            _ => panic!("expected ToolCompleted"),
        }
    }

    #[test]
    fn user_tool_result_with_error_flag_preserves_is_error() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"file not found","is_error":true}]}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted { is_error, .. }) => {
                assert!(is_error);
            }
            _ => panic!("expected ToolCompleted with is_error=true"),
        }
    }

    #[test]
    fn user_tool_result_with_content_array_concatenates_text_blocks() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"line1"},{"type":"text","text":"line2"}],"is_error":false}]}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted { output, .. }) => {
                assert_eq!(output, "line1\nline2");
            }
            _ => panic!("expected ToolCompleted"),
        }
    }

    #[test]
    fn user_tool_result_with_only_non_text_blocks_emits_placeholder() {
        // When the tool returns only image / non-text blocks, the operator
        // must see "something was here" rather than an empty output line.
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"image","source":{"type":"base64"}}],"is_error":false}]}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted { output, .. }) => {
                assert_eq!(output, "[non-text tool result omitted]");
            }
            _ => panic!("expected ToolCompleted with non-text placeholder"),
        }
    }

    #[test]
    fn user_envelope_without_tool_result_is_skipped() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi from user"}]}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn rate_limit_event_yields_rate_limit_event_marked_stream_only() {
        let agent_id = aid();
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1778701800}}"#;
        match parse_one_with_agent(line, tid(), agent_id) {
            ParseOutcome::Event(AdapterEvent::RateLimitEvent {
                agent_id: aid_out,
                info,
                source,
            }) => {
                assert_eq!(aid_out, agent_id);
                assert_eq!(info["status"], "allowed");
                // Claude rate-limit is stream-only (class C) → must be persisted.
                assert_eq!(source, crate::events::RateLimitSource::StreamOnly);
            }
            _ => panic!("expected RateLimitEvent"),
        }
    }

    #[test]
    fn thinking_delta_is_skipped() {
        // ContentKind::Thinking is reserved but not currently emitted.
        // Thinking deltas must produce ParseOutcome::Skip.
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"deliberating"}}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_message_start_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"message_start","message":{}}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_content_block_start_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_content_block_stop_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_message_delta_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"}}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_message_stop_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"message_stop"}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn input_json_delta_tool_input_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{"}}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn empty_text_delta_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":""}}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn invalid_json_yields_error() {
        let line = "{not valid json";
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Error(_)));
    }

    #[test]
    fn unknown_top_level_type_is_skipped_for_forward_compat() {
        let line = r#"{"type":"unknown_future_event","data":{}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn result_missing_error_fields_defaults_to_completed() {
        let line = r#"{"type":"result","result":"ok"}"#;
        assert!(matches!(
            parse_one(line, tid()),
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            })
        ));
    }

    // --- Multi-text-block separator behaviour ---

    fn run_turn(lines: &[&str]) -> String {
        let mut state = ParserState::default();
        let turn_id = tid();
        let agent_id = aid();
        let mut out = String::new();
        for line in lines {
            if let ParseOutcome::Event(AdapterEvent::ContentChunk { text, .. }) =
                parse_line(line, turn_id, agent_id, &mut state)
            {
                out.push_str(&text);
            }
        }
        out
    }

    #[test]
    fn single_text_block_emits_no_leading_separator() {
        let out = run_turn(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello "}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
        ]);
        assert_eq!(out, "hello world");
    }

    #[test]
    fn two_text_blocks_separated_by_tool_call_get_paragraph_separator() {
        let out = run_turn(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"What can I help with?"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","name":"Bash"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"Saved to memory."}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":2}}"#,
        ]);
        assert_eq!(out, "What can I help with?\n\nSaved to memory.");
    }

    #[test]
    fn three_text_blocks_get_separators_between_each() {
        let out = run_turn(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"one"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"two"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"three"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":2}}"#,
        ]);
        assert_eq!(out, "one\n\ntwo\n\nthree");
    }

    #[test]
    fn empty_text_block_does_not_consume_pending_separator() {
        let out = run_turn(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"first"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"second"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":2}}"#,
        ]);
        assert_eq!(out, "first\n\nsecond");
    }

    #[test]
    fn separator_not_emitted_before_first_text_block() {
        let out = run_turn(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"..."}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"answer"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
        ]);
        assert_eq!(out, "answer");
    }

    // --- Auth-failure state-flag pattern ---

    /// Replays the captured Claude auth-failure fixture through `parse_line`
    /// with a shared `ParserState` across the three lines. Asserts:
    /// - The `assistant` envelope (line 2) emits no terminal event — just
    ///   stashes `pending_auth_failure` on state. The one-terminal-event
    ///   contract must hold; the assistant envelope cannot double-emit.
    /// - The `result` envelope (line 3) emits exactly one `TurnEnd` with
    ///   `kind: AuthFailure` (refined from `HarnessError`) and the message
    ///   extracted from `message.content[0].text` ("Not logged in · Please
    ///   run /login"). Usage extraction still runs (zero-valued telemetry
    ///   from auth-failure result events is legitimate).
    #[test]
    fn claude_auth_failure_fixture_yields_one_turn_end_with_auth_failure_kind() {
        let fixture = include_str!("../tests/fixtures/claude/auth-failure.jsonl");
        let mut state = ParserState::default();
        let turn_id = tid();
        let agent_id = aid();
        let mut events: Vec<AdapterEvent> = Vec::new();
        for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
            match parse_line(line, turn_id, agent_id, &mut state) {
                ParseOutcome::Event(ev) => events.push(ev),
                ParseOutcome::Events(evs) => events.extend(evs),
                ParseOutcome::Skip => {}
                ParseOutcome::Error(e) => panic!("unexpected parse error: {e}"),
            }
        }
        let turn_ends: Vec<&AdapterEvent> = events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
            .collect();
        assert_eq!(
            turn_ends.len(),
            1,
            "exactly one TurnEnd must be emitted per turn; got {turn_ends:#?}"
        );
        match turn_ends[0] {
            AdapterEvent::TurnEnd {
                outcome:
                    TurnOutcome::Failed {
                        kind: FailureKind::AuthFailure,
                        message,
                    },
                ..
            } => {
                // Authored message replaces Claude's own `Please run /login`
                // (an interactive-session slash command). The user sees the
                // CLI recovery command and the harness name.
                assert_eq!(message, CLAUDE_AUTH_MESSAGE);
                assert!(message.contains("Claude authentication required"));
                assert!(message.contains("claude auth login"));
                assert!(!message.contains("reload Switchboard"));
            }
            other => panic!("expected TurnEnd(Failed{{AuthFailure}}), got {other:?}"),
        }
    }

    // --- Subagent rendering: `parent_tool_use_id` short-circuit ---

    /// Replays a captured delegation fixture through `parse_line`. The fixture
    /// is the synthetic shape of a Claude 2.1.153 stream during a delegating
    /// turn (probed 2026-05-27; redacted for inclusion):
    ///
    /// 1. parent's `Agent` `tool_use`           (`parent_tool_use_id`=null) → `ToolStarted{Agent}`
    /// 2. parent-tagged user envelope w/ text   (`parent_tool_use_id`=non-null) → SUPPRESSED
    /// 3. parent-tagged assistant w/ `Bash`     (`parent_tool_use_id`=non-null) → SUPPRESSED
    /// 4. parent-tagged user w/ `tool_result`   (`parent_tool_use_id`=non-null) → SUPPRESSED
    /// 5. parent's aggregate `tool_result`      (`parent_tool_use_id`=null) → `ToolCompleted{Agent}`
    /// 6. terminal `result`                     (`parent_tool_use_id` absent) → `TurnEnd{Completed}`
    ///
    /// Asserts the parent's view collapses to exactly one tool-call pair plus
    /// a terminal — matching what the rehydrated session-file view shows
    /// (Claude collapses subagent internals into a separate sidecar file on
    /// disk, so the stream parser must do the same in memory).
    #[test]
    fn subagent_delegation_fixture_collapses_to_parent_tool_call_pair() {
        let fixture = include_str!("../tests/fixtures/claude/subagent-delegation.jsonl");
        let mut state = ParserState::default();
        let turn_id = tid();
        let agent_id = aid();
        let mut events: Vec<AdapterEvent> = Vec::new();
        for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
            match parse_line(line, turn_id, agent_id, &mut state) {
                ParseOutcome::Event(ev) => events.push(ev),
                ParseOutcome::Events(evs) => events.extend(evs),
                ParseOutcome::Skip => {}
                ParseOutcome::Error(e) => panic!("unexpected parse error: {e}"),
            }
        }

        // Exactly one ToolStarted, naming the parent's `Agent` call.
        let tool_starteds: Vec<&AdapterEvent> = events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::ToolStarted { .. }))
            .collect();
        assert_eq!(
            tool_starteds.len(),
            1,
            "expected exactly one ToolStarted (parent's Agent call); got {tool_starteds:#?}",
        );
        match tool_starteds[0] {
            AdapterEvent::ToolStarted {
                tool_use_id, name, ..
            } => {
                assert_eq!(tool_use_id, "toolu_PARENT_AGENT_CALL");
                assert_eq!(name, "Agent");
            }
            other => panic!("expected ToolStarted, got {other:?}"),
        }

        // Exactly one ToolCompleted, paired to the parent's Agent call.
        let tool_completeds: Vec<&AdapterEvent> = events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::ToolCompleted { .. }))
            .collect();
        assert_eq!(
            tool_completeds.len(),
            1,
            "expected exactly one ToolCompleted (parent's aggregate result); got {tool_completeds:#?}",
        );
        match tool_completeds[0] {
            AdapterEvent::ToolCompleted { tool_use_id, .. } => {
                assert_eq!(tool_use_id, "toolu_PARENT_AGENT_CALL");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }

        // Zero events from subagent-tagged records. If the subagent's Bash
        // tool_use or tool_result had leaked, this fails — that's the bug.
        let subagent_tool_events: Vec<&AdapterEvent> = events
            .iter()
            .filter(|e| match e {
                AdapterEvent::ToolStarted { tool_use_id, .. }
                | AdapterEvent::ToolCompleted { tool_use_id, .. } => {
                    tool_use_id == "toolu_SUBAGENT_BASH_CALL"
                }
                _ => false,
            })
            .collect();
        assert!(
            subagent_tool_events.is_empty(),
            "subagent-internal tool events must not be attributed to the parent turn; got {subagent_tool_events:#?}",
        );

        // Exactly one terminal TurnEnd.
        let turn_ends: Vec<&AdapterEvent> = events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
            .collect();
        assert_eq!(turn_ends.len(), 1, "expected exactly one TurnEnd");
    }

    /// Direct unit-level coverage of the short-circuit rule. The fixture
    /// asserts the overall collapse; this asserts each path the rule cares
    /// about, including the conservative-by-default behavior for record
    /// types that could theoretically grow a `parent_tool_use_id` field
    /// later but don't carry one today.
    #[test]
    fn parent_tagged_records_skip_regardless_of_inner_type() {
        // assistant + tool_use, parent-tagged → Skip (the live mis-attribution case).
        let line = r#"{"type":"assistant","parent_tool_use_id":"toolu_PARENT","message":{"content":[{"type":"tool_use","id":"toolu_INNER","name":"Bash","input":{}}]}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));

        // user + tool_result, parent-tagged → Skip.
        let line = r#"{"type":"user","parent_tool_use_id":"toolu_PARENT","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_INNER","content":"hi","is_error":false}]}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));

        // user + text, parent-tagged (the third shape the original probe missed) → Skip.
        let line = r#"{"type":"user","parent_tool_use_id":"toolu_PARENT","message":{"role":"user","content":[{"type":"text","text":"task instruction"}]}}"#;
        assert!(matches!(parse_one(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn null_or_absent_parent_tool_use_id_does_not_skip() {
        // Explicit null → process normally (matches the parent's own
        // events, which include parent_tool_use_id: null on every record).
        let line = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_AGENT","name":"Agent","input":{}}]}}"#;
        assert!(matches!(
            parse_one(line, tid()),
            ParseOutcome::Event(AdapterEvent::ToolStarted { .. })
        ));

        // Absent entirely (e.g. `result` events) → process normally.
        let line = r#"{"type":"result","is_error":false,"result":"done"}"#;
        assert!(matches!(
            parse_one(line, tid()),
            ParseOutcome::Event(AdapterEvent::TurnEnd { .. })
        ));
    }

    // --- Auth-failure regressions guarded by the suppression rule ---

    /// Guards the per-dispatch `ParserState`-freshness invariant: a fresh
    /// `ParserState` between dispatches means a prior turn's auth failure
    /// cannot poison the next turn. (Structurally enforced by `run_producer`
    /// constructing a new state per turn, but the test pins the behaviour
    /// against regression.)
    #[test]
    fn fresh_parser_state_after_auth_failure_yields_completed_next_turn() {
        // Dispatch 1: full auth-failure sequence with one ParserState.
        let mut state1 = ParserState::default();
        let turn_id_1 = tid();
        let agent_id = aid();
        let fixture = include_str!("../tests/fixtures/claude/auth-failure.jsonl");
        for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
            let _ = parse_line(line, turn_id_1, agent_id, &mut state1);
        }
        assert!(
            state1.pending_auth_failure.is_none(),
            "parse_result must `.take()` the stash — leaving it set would corrupt later results"
        );

        // Dispatch 2: a fresh ParserState (mirrors `run_producer`'s per-turn
        // reset). The next turn's `result` event must NOT see any auth-failure
        // state, regardless of what happened earlier on a different state.
        let mut state2 = ParserState::default();
        let turn_id_2 = tid();
        let success_line = r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":null,"result":"ack"}"#;
        match parse_line(success_line, turn_id_2, agent_id, &mut state2) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }) => {}
            other => panic!("expected TurnEnd(Completed) on the second dispatch, got {other:?}"),
        }
    }
}
