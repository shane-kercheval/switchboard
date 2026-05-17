//! Codex stream-event parser.
//!
//! Codex emits a flat top-level event stream (no envelope wrapper). Each line
//! is a JSON object discriminated by `type`. The parser maps these to
//! `AdapterEvent`s; events the parser cannot handle deterministically
//! (rate limits, session metadata) are deferred to the post-terminal
//! session-file enrichment (see [`super::session_file`]).
//!
//! **State threading.** `CodexParserState` is constructed fresh per dispatch
//! by the producer task (parallels Claude's `ParserState` pattern in
//! `claude_code.rs::run_producer`). Per-turn freshness means cross-turn
//! poisoning is structurally impossible.
//!
//! **Stdout-vs-stderr discipline.** `state.last_error` is **stdout-parser
//! local only** — it holds the payload of `{type: "error"}` JSON events read
//! from stdout, **never** stderr. Stderr is drained by a separate concurrent
//! task in the adapter (see `mod.rs` and the canonical pattern in
//! `claude_code.rs:213-220`). The two buffers do not share memory or locking
//! and must not be merged.

use serde_json::Value;

use crate::events::{
    AdapterEvent, ContentKind, FailureKind, ToolKind, TurnId, TurnOutcome, TurnUsage,
};
use crate::parser::ParseOutcome;

/// Per-dispatch state held by the Codex producer task.
///
/// `pending_thread_id` is set when the parser observes the first
/// `thread.started` event of the dispatch; the adapter consumes it via
/// `.take()` to write the per-agent session-link sidecar. After consumption
/// the field remains `None` for the rest of the dispatch — Codex emits
/// `thread.started` exactly once per `codex exec` invocation.
///
/// `last_error` accumulates the message payload of `{type: "error"}` stdout
/// events using last-wins semantics — Codex emits multiple `Reconnecting... N/5`
/// retry messages before the final `turn.failed`; the most recent message is
/// the most informative. The buffer is consumed in the EOF-without-terminal
/// path (adapter synthesizes `TurnEnd(AdapterFailure)` with the buffered text
/// appended) and as a defensive fallback when `turn.failed.error.message` is
/// missing or empty. The canonical message source on a normal `turn.failed`
/// is `error.message` from that event itself.
#[derive(Debug, Default)]
pub struct CodexParserState {
    pub pending_thread_id: Option<String>,
    pub last_error: Option<String>,
    /// Set when `thread.started` arrives without a valid `thread_id` field
    /// (missing or non-string). The sidecar cannot be written without a
    /// `thread_id`, so the producer must fail-loud — silently letting the
    /// turn complete would create a silently-unresumable agent (the next
    /// dispatch's `read_latest` returns `Ok(None)`, Codex spawns a fresh
    /// session, prior context is lost without a user-visible signal).
    pub corrupt_thread_started: bool,
}

/// Parse one Codex stream-json line. Returns `ParseOutcome::Skip` for events
/// the parser observed but produced no normalized output (e.g.,
/// `thread.started` captures the `thread_id` into state; `token_count` is
/// session-file-only; `turn.started` is dispatcher-owned).
pub fn parse_line(line: &str, turn_id: TurnId, state: &mut CodexParserState) -> ParseOutcome {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return ParseOutcome::Error(e.to_string()),
    };

    // `turn.started`, `event_msg`, and unknown types all skip; collapsed
    // via the wildcard arm. `event_msg` carries Codex's session-file-bound
    // `token_count` payload, which the post-terminal enrichment owns
    // (rate_limits is session-file-only — see
    // `docs/research/codex-cli-observed.md`).
    match value.get("type").and_then(Value::as_str) {
        Some("thread.started") => parse_thread_started(&value, state),
        Some("item.started") => parse_item_started(&value, turn_id),
        Some("item.completed") => parse_item_completed(&value, turn_id),
        Some("turn.completed") => parse_turn_completed(&value, turn_id),
        Some("turn.failed") => parse_turn_failed(&value, turn_id, state),
        Some("error") => parse_error_event(&value, state),
        _ => ParseOutcome::Skip,
    }
}

fn parse_thread_started(obj: &Value, state: &mut CodexParserState) -> ParseOutcome {
    if let Some(id) = obj.get("thread_id").and_then(Value::as_str) {
        state.pending_thread_id = Some(id.to_owned());
    } else {
        tracing::warn!(
            "Codex thread.started event missing or non-string thread_id — sidecar cannot be written; resume will fail"
        );
        state.corrupt_thread_started = true;
    }
    ParseOutcome::Skip
}

fn parse_item_started(obj: &Value, turn_id: TurnId) -> ParseOutcome {
    let Some(item) = obj.get("item") else {
        return ParseOutcome::Skip;
    };
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");

    // Forward-compat defensive policy: if a future Codex version emits
    // `item.started` for `agent_message`, do NOT synthesize a phantom
    // `ToolStarted` — text messages are not tool calls. Warn and skip; the
    // subsequent `item.completed` still produces the `ContentChunk`.
    if item_type == "agent_message" {
        tracing::warn!(
            "Codex emitted item.started for agent_message; only item.completed was expected — CLI version may have changed"
        );
        return ParseOutcome::Skip;
    }

    let Some(id) = item.get("id").and_then(Value::as_str) else {
        return ParseOutcome::Skip;
    };

    match item_type {
        "command_execution" => ParseOutcome::Event(AdapterEvent::ToolStarted {
            turn_id,
            tool_use_id: id.to_owned(),
            kind: ToolKind::Builtin,
            name: "command_execution".to_owned(),
            input: item.get("command").cloned().unwrap_or(Value::Null),
        }),
        "mcp_tool_call" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("");
            ParseOutcome::Event(AdapterEvent::ToolStarted {
                turn_id,
                tool_use_id: id.to_owned(),
                kind: ToolKind::Mcp,
                name: format!("{server}.{tool}"),
                input: item.get("arguments").cloned().unwrap_or(Value::Null),
            })
        }
        _ => ParseOutcome::Skip,
    }
}

fn parse_item_completed(obj: &Value, turn_id: TurnId) -> ParseOutcome {
    let Some(item) = obj.get("item") else {
        return ParseOutcome::Skip;
    };
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");

    match item_type {
        "agent_message" => {
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if text.is_empty() {
                return ParseOutcome::Skip;
            }
            ParseOutcome::Event(AdapterEvent::ContentChunk {
                turn_id,
                kind: ContentKind::Text,
                text,
            })
        }
        "command_execution" => {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                return ParseOutcome::Skip;
            };
            let output = item
                .get("aggregated_output")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let exit_code = item.get("exit_code").and_then(Value::as_i64);
            // Treat missing / null exit_code as a non-error (in_progress
            // shouldn't reach item.completed, but defensive). The status
            // field is also `"completed"` / `"failed"` — but exit_code is the
            // operational truth.
            let is_error = matches!(exit_code, Some(code) if code != 0);
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                turn_id,
                tool_use_id: id.to_owned(),
                output,
                is_error,
            })
        }
        "mcp_tool_call" => {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                return ParseOutcome::Skip;
            };
            let result = item.get("result");
            let error = item.get("error");
            let status = item.get("status").and_then(Value::as_str).unwrap_or("");
            let is_error = status == "failed" || error.is_some_and(|v| !v.is_null());
            let output = extract_mcp_output(result, error);
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                turn_id,
                tool_use_id: id.to_owned(),
                output,
                is_error,
            })
        }
        _ => ParseOutcome::Skip,
    }
}

/// MCP output extraction policy (mirrors Claude's
/// `stringify_tool_result_content`):
/// - Join `text`-typed content blocks via `as_str().unwrap_or("")`; skip
///   non-text blocks silently when any text block is present.
/// - If all content blocks are non-text, OR `content` is empty, OR `result`
///   is `null`/missing → use the `error` field if non-null; otherwise emit
///   the `[non-text tool result omitted]` placeholder.
/// - Non-string `error` (object, array) → stringify via compact
///   `serde_json::to_string`.
/// - Never panic on malformed shapes — missing or wrong-typed fields collapse
///   to the empty/error case.
fn extract_mcp_output(result: Option<&Value>, error: Option<&Value>) -> String {
    // Result extraction first.
    if let Some(result) = result
        && !result.is_null()
        && let Some(content) = result.get("content").and_then(Value::as_array)
    {
        let mut texts: Vec<&str> = Vec::new();
        let mut had_non_text = false;
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    texts.push(t);
                }
            } else {
                had_non_text = true;
            }
        }
        if !texts.is_empty() {
            return texts.join("\n");
        }
        if had_non_text {
            return "[non-text tool result omitted]".to_owned();
        }
        // Empty content array.
        if let Some(err_str) = stringify_error_field(error) {
            return err_str;
        }
        return "[non-text tool result omitted]".to_owned();
    }

    // result is null / missing or has no content array. Fall back to error.
    if let Some(err_str) = stringify_error_field(error) {
        return err_str;
    }
    "[non-text tool result omitted]".to_owned()
}

/// Stringify the `error` field of an `mcp_tool_call.item.completed` event.
/// String → returned verbatim. Non-string non-null (object, array) → compact
/// JSON. `null` / missing → `None`.
fn stringify_error_field(error: Option<&Value>) -> Option<String> {
    let error = error?;
    if error.is_null() {
        return None;
    }
    if let Some(s) = error.as_str() {
        return Some(s.to_owned());
    }
    serde_json::to_string(error).ok()
}

fn parse_turn_completed(obj: &Value, turn_id: TurnId) -> ParseOutcome {
    let usage = extract_usage_from_turn_completed(obj);
    ParseOutcome::Event(AdapterEvent::TurnEnd {
        turn_id,
        outcome: TurnOutcome::Completed,
        ended_at: chrono::Utc::now(),
        usage,
    })
}

/// Codex's `turn.completed.usage` carries token counts but no cost dollars
/// (subscription auth at the harness boundary) and no `context_window`
/// (session-file-only; the post-terminal enrichment fills it in). Required
/// fields: `input_tokens`, `output_tokens`. Missing/non-numeric required
/// fields → `None` (no fabricated zero-Some); zero-valued real telemetry
/// → `Some`.
fn extract_usage_from_turn_completed(obj: &Value) -> Option<TurnUsage> {
    let usage_obj = obj.get("usage")?;
    let input_tokens = usage_obj.get("input_tokens").and_then(Value::as_u64)?;
    let output_tokens = usage_obj.get("output_tokens").and_then(Value::as_u64)?;
    let cached_input_tokens = usage_obj.get("cached_input_tokens").and_then(Value::as_u64);
    let reasoning_output_tokens = usage_obj
        .get("reasoning_output_tokens")
        .and_then(Value::as_u64);
    Some(TurnUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        reasoning_output_tokens,
        context_window: None,
        total_cost_usd: None,
    })
}

fn parse_turn_failed(obj: &Value, turn_id: TurnId, state: &mut CodexParserState) -> ParseOutcome {
    // Canonical source for the failure message is `turn.failed.error.message`
    // from this event itself. If it's missing or empty, fall back to the
    // most recent buffered `error` event payload — Codex emits multiple
    // `Reconnecting... N/5` retry messages before a degraded `turn.failed`
    // can lose the 401 signal, so the fallback preserves AuthFailure
    // classification on sparse terminal events. The buffer is consumed
    // (taken) so EOF synthesis later doesn't double-surface it.
    let raw_message = obj
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let message_source = if raw_message.is_empty() {
        state.last_error.take().unwrap_or_default()
    } else {
        raw_message.to_owned()
    };
    let message = unwrap_error_message(&message_source);
    let kind = if is_codex_auth_failure(&message) {
        FailureKind::AuthFailure
    } else {
        FailureKind::HarnessError
    };
    ParseOutcome::Event(AdapterEvent::TurnEnd {
        turn_id,
        outcome: TurnOutcome::Failed { kind, message },
        ended_at: chrono::Utc::now(),
        usage: None,
    })
}

fn parse_error_event(obj: &Value, state: &mut CodexParserState) -> ParseOutcome {
    if let Some(message) = obj.get("message").and_then(Value::as_str) {
        // Last-error-wins: each retry message overwrites the previous one.
        // Consumed only as an EOF-fallback by the adapter (see mod.rs); a
        // normal `turn.failed` uses its own `error.message` as canonical.
        state.last_error = Some(message.to_owned());
    }
    ParseOutcome::Skip
}

/// One-pass best-effort unwrap for `turn.failed.error.message`. Codex's CLI
/// returns this field in two shapes:
/// 1. Plain human-readable string (e.g., `"unexpected status 401 Unauthorized: …"`).
///    Pass through verbatim.
/// 2. JSON-encoded JSON containing a nested error object (e.g.,
///    `"{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"...\",
///    \"message\":\"The 'invalid-model-name' model is not supported …\"}}"`).
///    Parse and surface the inner `error.message`.
///
/// Never errors on parse failure — the plain-string case must work.
pub fn unwrap_error_message(raw: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(raw)
        && let Some(inner) = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
    {
        return inner.to_owned();
    }
    raw.to_owned()
}

/// Codex auth-failure detection — pattern-matches the unwrapped error
/// message against the documented 401 signature. A successful API call
/// cannot return 401, so the substring is a sufficient discriminant (per
/// `docs/research/codex-cli-observed.md`).
pub fn is_codex_auth_failure(message: &str) -> bool {
    message.contains("401 Unauthorized")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AdapterEvent;
    use uuid::Uuid;

    fn tid() -> TurnId {
        Uuid::now_v7()
    }

    fn parse_fixture(fixture_relative: &str) -> (Vec<AdapterEvent>, CodexParserState) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(fixture_relative);
        let fixture = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let mut state = CodexParserState::default();
        let turn_id = tid();
        let mut events = Vec::new();
        for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
            match parse_line(line, turn_id, &mut state) {
                ParseOutcome::Event(ev) => events.push(ev),
                ParseOutcome::Events(evs) => events.extend(evs),
                ParseOutcome::Skip => {}
                ParseOutcome::Error(e) => panic!("parse error on line {line}: {e}"),
            }
        }
        (events, state)
    }

    // --- Fixture-driven coverage ---

    #[test]
    fn text_only_fixture_yields_chunk_then_completed() {
        let (events, state) = parse_fixture("text-only.jsonl");
        assert!(state.pending_thread_id.is_some(), "thread_id captured");
        assert_eq!(events.len(), 2);
        match &events[0] {
            AdapterEvent::ContentChunk {
                kind: ContentKind::Text,
                text,
                ..
            } => assert_eq!(text, "ack"),
            other => panic!("expected ContentChunk(Text), got {other:?}"),
        }
        match &events[1] {
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                usage: Some(u),
                ..
            } => {
                assert_eq!(u.input_tokens, 15568);
                assert_eq!(u.output_tokens, 5);
                assert_eq!(u.cached_input_tokens, Some(7552));
                assert_eq!(
                    u.context_window, None,
                    "context_window is enriched post-terminal from the session file"
                );
                assert_eq!(
                    u.total_cost_usd, None,
                    "Codex has no $ at the harness boundary"
                );
            }
            other => panic!("expected TurnEnd(Completed) with usage, got {other:?}"),
        }
    }

    #[test]
    fn tool_use_fixture_yields_command_execution_and_chunk_and_completed() {
        let (events, _) = parse_fixture("tool-use.jsonl");
        // Sequence: ToolStarted(command_execution) → ToolCompleted(command_execution)
        // → ContentChunk(agent_message) → TurnEnd(Completed).
        assert_eq!(events.len(), 4, "got {events:#?}");
        match &events[0] {
            AdapterEvent::ToolStarted {
                kind: ToolKind::Builtin,
                name,
                tool_use_id,
                ..
            } => {
                assert_eq!(name, "command_execution");
                assert_eq!(tool_use_id, "item_0");
            }
            other => panic!("expected ToolStarted(Builtin), got {other:?}"),
        }
        match &events[1] {
            AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "item_0");
                assert_eq!(output, "alpha.txt\nbeta.txt\n");
                assert!(!is_error, "exit_code 0");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
        match &events[2] {
            AdapterEvent::ContentChunk { text, .. } => {
                assert!(text.contains("alpha.txt"));
            }
            other => panic!("expected ContentChunk, got {other:?}"),
        }
        assert!(matches!(
            &events[3],
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ));
    }

    #[test]
    fn auth_failure_fixture_yields_auth_failure_with_401_message() {
        let (events, state) = parse_fixture("auth-failure.jsonl");
        // Buffered errors from retry messages — exists but unused on the
        // normal `turn.failed` path (canonical message is turn.failed.error.message).
        assert!(
            state.last_error.is_some(),
            "last_error buffered from retry messages"
        );
        // Exactly one TurnEnd (auth-failure path is single-turn-failed).
        let turn_ends: Vec<&AdapterEvent> = events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
            .collect();
        assert_eq!(turn_ends.len(), 1, "exactly one TurnEnd");
        match turn_ends[0] {
            AdapterEvent::TurnEnd {
                outcome:
                    TurnOutcome::Failed {
                        kind: FailureKind::AuthFailure,
                        message,
                    },
                ..
            } => {
                assert!(
                    message.contains("401 Unauthorized"),
                    "auth-failure message preserves 401 signal: {message}"
                );
            }
            other => panic!("expected TurnEnd(Failed{{AuthFailure}}), got {other:?}"),
        }
    }

    #[test]
    fn errored_fixture_unwraps_json_encoded_error_message() {
        let (events, _) = parse_fixture("errored.jsonl");
        let turn_ends: Vec<&AdapterEvent> = events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
            .collect();
        assert_eq!(turn_ends.len(), 1);
        match turn_ends[0] {
            AdapterEvent::TurnEnd {
                outcome:
                    TurnOutcome::Failed {
                        kind: FailureKind::HarnessError,
                        message,
                    },
                ..
            } => {
                // The captured fixture has a JSON-encoded error wrapping
                // `error.message = "The 'invalid-model-name' model …"`.
                // Unwrapping must surface the inner message, NOT the raw
                // escaped JSON.
                assert!(
                    message.contains("invalid-model-name"),
                    "JSON-encoded inner message must be unwrapped: {message}"
                );
                assert!(
                    !message.starts_with("{\""),
                    "raw JSON-encoded shape must be unwrapped (was: {message})"
                );
            }
            other => panic!(
                "expected TurnEnd(Failed{{HarnessError}}) with unwrapped message, got {other:?}"
            ),
        }
    }

    #[test]
    fn mcp_tool_call_failure_fixture_emits_is_error_with_error_text() {
        let (events, _) = parse_fixture("mcp-tool-call.jsonl");
        let tool_started = events
            .iter()
            .find_map(|e| match e {
                AdapterEvent::ToolStarted { name, kind, .. } => Some((kind, name.clone())),
                _ => None,
            })
            .expect("ToolStarted expected");
        assert_eq!(*tool_started.0, ToolKind::Mcp);
        assert_eq!(tool_started.1, "example_mcp_server.list_tags");

        let tool_completed = events
            .iter()
            .find_map(|e| match e {
                AdapterEvent::ToolCompleted {
                    output, is_error, ..
                } => Some((output.clone(), *is_error)),
                _ => None,
            })
            .expect("ToolCompleted expected");
        assert!(tool_completed.1, "status=failed must propagate to is_error");
        // The captured fixture's result.content[0].text is "Invalid or expired token".
        // Extraction joins text blocks, so output reflects the server's message.
        assert!(
            tool_completed.0.contains("Invalid or expired token"),
            "MCP output extracts result.content[*].text: {}",
            tool_completed.0
        );
    }

    // --- Inline-constructed coverage for paths the captured fixtures don't cover ---

    #[test]
    fn mcp_success_path_joins_text_content() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"mcp_tool_call","server":"srv","tool":"do","arguments":{},"result":{"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}],"structured_content":null},"error":null,"status":"completed"}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                output, is_error, ..
            }) => {
                assert!(!is_error, "status=completed → is_error=false");
                assert_eq!(output, "first\nsecond");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn mcp_result_null_falls_back_to_error_field_string() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"mcp_tool_call","server":"srv","tool":"do","arguments":{},"result":null,"error":"server unreachable","status":"failed"}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                output, is_error, ..
            }) => {
                assert!(is_error);
                assert_eq!(output, "server unreachable");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn mcp_result_null_and_error_null_yields_placeholder() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"mcp_tool_call","server":"srv","tool":"do","arguments":{},"result":null,"error":null,"status":"failed"}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                output, is_error, ..
            }) => {
                assert!(
                    is_error,
                    "status=failed must set is_error even if no message"
                );
                assert_eq!(output, "[non-text tool result omitted]");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn mcp_result_only_non_text_content_yields_placeholder() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"mcp_tool_call","server":"srv","tool":"do","arguments":{},"result":{"content":[{"type":"image","data":"base64..."}],"structured_content":null},"error":null,"status":"completed"}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted { output, .. }) => {
                assert_eq!(output, "[non-text tool result omitted]");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn mcp_mixed_text_and_non_text_joins_only_text() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"mcp_tool_call","server":"srv","tool":"do","arguments":{},"result":{"content":[{"type":"text","text":"hello"},{"type":"image","data":"..."},{"type":"text","text":"world"}],"structured_content":null},"error":null,"status":"completed"}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted { output, .. }) => {
                assert_eq!(output, "hello\nworld");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn mcp_non_string_error_stringifies_via_compact_json() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"mcp_tool_call","server":"srv","tool":"do","arguments":{},"result":null,"error":{"code":42,"reason":"nope"},"status":"failed"}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                output, is_error, ..
            }) => {
                assert!(is_error);
                // Compact JSON is deterministic key-order per serde_json default.
                assert!(output.contains("\"code\":42"));
                assert!(output.contains("\"reason\":\"nope\""));
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn defensive_item_started_agent_message_emits_no_tool_started() {
        // Forward-compat policy: if Codex starts emitting item.started for
        // agent_message in a future version, must NOT synthesize a phantom
        // ToolStarted — text messages are not tool calls.
        let line =
            r#"{"type":"item.started","item":{"id":"item_0","type":"agent_message","text":""}}"#;
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn empty_agent_message_text_is_skipped() {
        let line =
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":""}}"#;
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn thread_started_captures_thread_id_and_skips() {
        let line =
            r#"{"type":"thread.started","thread_id":"019e2c5f-aaaa-7000-8000-000000000001"}"#;
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Skip
        ));
        assert_eq!(
            state.pending_thread_id.as_deref(),
            Some("019e2c5f-aaaa-7000-8000-000000000001")
        );
        assert!(!state.corrupt_thread_started);
    }

    #[test]
    fn thread_started_with_missing_thread_id_sets_corrupt_flag() {
        // Forward-compat / defensive: thread.started without thread_id can't
        // populate the sidecar, so the producer must fail-loud rather than
        // silently produce an unresumable agent.
        let line = r#"{"type":"thread.started"}"#;
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Skip
        ));
        assert!(state.pending_thread_id.is_none());
        assert!(state.corrupt_thread_started);
    }

    #[test]
    fn thread_started_with_non_string_thread_id_sets_corrupt_flag() {
        let line = r#"{"type":"thread.started","thread_id":42}"#;
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Skip
        ));
        assert!(state.pending_thread_id.is_none());
        assert!(state.corrupt_thread_started);
    }

    #[test]
    fn turn_failed_with_empty_message_falls_back_to_buffered_last_error() {
        // Codex emits retry messages via `error` events before a degraded
        // `turn.failed` (e.g., final `turn.failed` carries no message). The
        // buffered last_error must surface so AuthFailure classification on
        // sparse terminals still fires.
        let mut state = CodexParserState::default();
        let _ = parse_line(
            r#"{"type":"error","message":"Reconnecting... 5/5 (unexpected status 401 Unauthorized)"}"#,
            tid(),
            &mut state,
        );
        let line = r#"{"type":"turn.failed","error":{"message":""}}"#;
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome:
                    TurnOutcome::Failed {
                        kind: FailureKind::AuthFailure,
                        message,
                    },
                ..
            }) => {
                assert!(
                    message.contains("401 Unauthorized"),
                    "fallback must surface buffered 401: {message}"
                );
            }
            other => panic!("expected TurnEnd(AuthFailure), got {other:?}"),
        }
        // The fallback consumed the buffer; subsequent reads would be None.
        assert!(state.last_error.is_none());
    }

    #[test]
    fn turn_failed_with_message_ignores_buffered_last_error() {
        // Canonical priority: when turn.failed.error.message is non-empty,
        // it wins over the buffer. Pins precedence against accidental
        // inversion.
        let mut state = CodexParserState::default();
        let _ = parse_line(
            r#"{"type":"error","message":"stale retry chatter"}"#,
            tid(),
            &mut state,
        );
        let line = r#"{"type":"turn.failed","error":{"message":"canonical failure"}}"#;
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome:
                    TurnOutcome::Failed {
                        kind: FailureKind::HarnessError,
                        message,
                    },
                ..
            }) => {
                assert_eq!(message, "canonical failure");
            }
            other => panic!("expected TurnEnd(HarnessError) with canonical message, got {other:?}"),
        }
    }

    #[test]
    fn error_event_buffers_last_wins() {
        let mut state = CodexParserState::default();
        let _ = parse_line(
            r#"{"type":"error","message":"Reconnecting... 1/5"}"#,
            tid(),
            &mut state,
        );
        let _ = parse_line(
            r#"{"type":"error","message":"final 401 Unauthorized"}"#,
            tid(),
            &mut state,
        );
        assert_eq!(state.last_error.as_deref(), Some("final 401 Unauthorized"));
    }

    #[test]
    fn event_msg_token_count_is_skipped_in_m2_3() {
        let line = r#"{"type":"event_msg","msg":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":42.0}}}}"#;
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn turn_started_is_skipped() {
        let line = r#"{"type":"turn.started"}"#;
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn unknown_top_level_type_is_skipped() {
        let line = r#"{"type":"future_event","data":{}}"#;
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn malformed_json_yields_error_outcome() {
        let line = "not valid json";
        let mut state = CodexParserState::default();
        assert!(matches!(
            parse_line(line, tid(), &mut state),
            ParseOutcome::Error(_)
        ));
    }

    #[test]
    fn unwrap_error_message_passes_plain_string_through() {
        let plain = "unexpected status 401 Unauthorized: …";
        assert_eq!(unwrap_error_message(plain), plain);
    }

    #[test]
    fn unwrap_error_message_extracts_nested_error_message() {
        let json_encoded = r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"bad model"}}"#;
        assert_eq!(unwrap_error_message(json_encoded), "bad model");
    }

    #[test]
    fn unwrap_error_message_returns_raw_when_nested_message_missing() {
        let json_no_inner = r#"{"type":"error","status":400}"#;
        assert_eq!(unwrap_error_message(json_no_inner), json_no_inner);
    }

    #[test]
    fn is_codex_auth_failure_matches_401_substring() {
        assert!(is_codex_auth_failure(
            "unexpected status 401 Unauthorized: Missing bearer …"
        ));
        assert!(!is_codex_auth_failure(
            "The 'invalid-model-name' model is not supported …"
        ));
    }

    #[test]
    fn turn_completed_with_missing_usage_yields_none_usage() {
        let line = r#"{"type":"turn.completed"}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { usage: None, .. }) => {}
            other => panic!("expected TurnEnd with None usage, got {other:?}"),
        }
    }

    #[test]
    fn turn_completed_with_partial_usage_yields_none() {
        // Missing input_tokens / output_tokens → strict None (no fabricated zero-Some).
        let line = r#"{"type":"turn.completed","usage":{"reasoning_output_tokens":3}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { usage: None, .. }) => {}
            other => panic!(
                "expected TurnEnd with None usage when required fields are missing, got {other:?}"
            ),
        }
    }

    #[test]
    fn turn_completed_with_zero_usage_yields_some_zero_some() {
        // Zero is legitimate real telemetry — distinct from None which means
        // "unparseable / absent."
        let line = r#"{"type":"turn.completed","usage":{"input_tokens":0,"output_tokens":0}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { usage: Some(u), .. }) => {
                assert_eq!(u.input_tokens, 0);
                assert_eq!(u.output_tokens, 0);
                assert_eq!(u.cached_input_tokens, None);
            }
            other => panic!("expected Some(zero-valued usage), got {other:?}"),
        }
    }

    #[test]
    fn command_execution_with_nonzero_exit_code_is_error() {
        let line = r#"{"type":"item.completed","item":{"id":"x","type":"command_execution","aggregated_output":"err","exit_code":1,"status":"failed"}}"#;
        let mut state = CodexParserState::default();
        match parse_line(line, tid(), &mut state) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                is_error, output, ..
            }) => {
                assert!(is_error);
                assert_eq!(output, "err");
            }
            other => panic!("expected ToolCompleted with is_error=true, got {other:?}"),
        }
    }
}
