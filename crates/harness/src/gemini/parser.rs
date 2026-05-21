//! Gemini stream-event parser.
//!
//! Gemini emits a flat top-level event stream — each line is a JSON object
//! discriminated by `type`. The parser maps these to `AdapterEvent`s.
//!
//! **Cross-line state.** `GeminiParserState` tracks which `tool_use_id`s
//! were filtered by the internal-tool deny-list so the corresponding
//! `tool_result` skip too. The state is constructed fresh per dispatch by
//! the producer task — per-turn freshness means cross-turn poisoning is
//! structurally impossible.
//!
//! **Why a deny-list, not an allow-list.** `update_topic` is Gemini's
//! internal conversation-state metadata; it auto-fires on most non-trivial
//! turns and carries no user-relevant information. Filtering it at the
//! adapter keeps the unified transcript clean. Future internal tools land
//! here; both the live adapter and the hydrator share the same constant so
//! the filter rule stays in lockstep across surfaces.

use std::collections::HashSet;

use serde_json::Value;
use switchboard_core::AgentId;

use crate::events::{
    AdapterEvent, ContentKind, FailureKind, McpServerStatus, ToolKind, TurnId, TurnOutcome,
    TurnUsage,
};
use crate::parser::ParseOutcome;

/// Tool names that the Gemini CLI auto-fires for internal book-keeping.
/// Both the adapter (live stream) and the hydrator (session-file) filter
/// against this exact constant — keep them in lockstep when adding entries.
pub const GEMINI_INTERNAL_TOOL_NAMES: &[&str] = &["update_topic"];

/// Substrings that flag a Gemini error message as an auth failure.
/// Case-insensitive match. Tightening the rule reactively touches only
/// `is_gemini_auth_failure_message`; both the in-stream `result.status:"error"`
/// path and the exit-42 stderr path call it so auth-detection stays
/// symmetric across failure surfaces. Each caller owns its non-auth
/// fallback (stream → `HarnessError`; exit-42 → `AdapterFailure`).
const AUTH_FAILURE_SUBSTRINGS: &[&str] =
    &["401 unauthorized", "permission_denied", "authentication"];

#[derive(Debug, Default)]
pub struct GeminiParserState {
    /// `tool_use_id`s that the adapter filtered from `ToolStarted` emission
    /// because the tool name was in `GEMINI_INTERNAL_TOOL_NAMES`. The
    /// matching `tool_result` skips too — emitting a `ToolCompleted` for a
    /// tool that has no `ToolStarted` would render a phantom completion
    /// badge in the UI.
    filtered_tool_ids: HashSet<String>,
}

/// Return `true` if the message looks like an auth failure. The two
/// callers (in-stream `result.status:"error"` and exit-42 stderr) own
/// their non-auth fallback — `HarnessError` and `AdapterFailure`
/// respectively. Substring match is case-insensitive and intentionally
/// loose: Gemini hasn't been probed for the canonical auth-failure shape
/// (would require breaking the developer's OAuth state), so any of the
/// known substrings flips it.
#[must_use]
pub fn is_gemini_auth_failure_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    AUTH_FAILURE_SUBSTRINGS.iter().any(|s| lower.contains(s))
}

/// Parse one Gemini stream-json line.
pub fn parse_line(
    line: &str,
    turn_id: TurnId,
    agent_id: AgentId,
    state: &mut GeminiParserState,
) -> ParseOutcome {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return ParseOutcome::Error(e.to_string()),
    };

    match value.get("type").and_then(Value::as_str) {
        Some("init") => parse_init(&value, agent_id),
        Some("message") => parse_message(&value, turn_id),
        Some("tool_use") => parse_tool_use(&value, turn_id, state),
        Some("tool_result") => parse_tool_result(&value, turn_id, state),
        Some("result") => parse_result(&value, turn_id),
        Some(other) => {
            tracing::warn!(
                event_type = other,
                "unknown Gemini stream event type — skipping"
            );
            ParseOutcome::Skip
        }
        None => ParseOutcome::Skip,
    }
}

fn parse_init(obj: &Value, agent_id: AgentId) -> ParseOutcome {
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    // `harness_version` is filled in by the adapter (lazy `gemini --version`
    // fetch via `OnceLock`); the parser doesn't know it.
    ParseOutcome::Event(AdapterEvent::SessionMeta {
        agent_id,
        model,
        harness_version: String::new(),
        tools: Vec::new(),
        mcp_servers: Vec::<McpServerStatus>::new(),
        skills: Vec::new(),
        raw: obj.clone(),
    })
}

fn parse_message(obj: &Value, turn_id: TurnId) -> ParseOutcome {
    let role = obj.get("role").and_then(Value::as_str).unwrap_or("");
    if role != "assistant" {
        // The `user` echo of the prompt is intentionally skipped; Switchboard
        // already has the prompt text from the caller.
        return ParseOutcome::Skip;
    }
    // Require `delta: true`. Every observed assistant message in
    // stream-json mode carries `delta:true` (multi-chunk streaming). If a
    // future Gemini release also emits a final consolidated message
    // *without* the delta flag, naively forwarding it would double-emit
    // the same text and the user would see the response duplicated in the
    // transcript. Warn-and-skip on non-delta surfaces the CLI behavior
    // change instead of corrupting the rendered output.
    if obj.get("delta").and_then(Value::as_bool) != Some(true) {
        tracing::warn!(
            "Gemini assistant message without delta:true — CLI version may have changed; skipping to avoid duplicate emission"
        );
        return ParseOutcome::Skip;
    }
    let text = obj
        .get("content")
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

fn parse_tool_use(obj: &Value, turn_id: TurnId, state: &mut GeminiParserState) -> ParseOutcome {
    let Some(name) = obj.get("tool_name").and_then(Value::as_str) else {
        return ParseOutcome::Skip;
    };
    let Some(id) = obj.get("tool_id").and_then(Value::as_str) else {
        return ParseOutcome::Skip;
    };
    if GEMINI_INTERNAL_TOOL_NAMES.contains(&name) {
        state.filtered_tool_ids.insert(id.to_owned());
        return ParseOutcome::Skip;
    }
    let kind = if name.starts_with("mcp__") {
        ToolKind::Mcp
    } else {
        ToolKind::Builtin
    };
    ParseOutcome::Event(AdapterEvent::ToolStarted {
        turn_id,
        tool_use_id: id.to_owned(),
        kind,
        name: name.to_owned(),
        input: obj.get("parameters").cloned().unwrap_or(Value::Null),
    })
}

fn parse_tool_result(obj: &Value, turn_id: TurnId, state: &mut GeminiParserState) -> ParseOutcome {
    let Some(id) = obj.get("tool_id").and_then(Value::as_str) else {
        return ParseOutcome::Skip;
    };
    if state.filtered_tool_ids.contains(id) {
        return ParseOutcome::Skip;
    }
    let status = obj.get("status").and_then(Value::as_str).unwrap_or("");
    let output = obj
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    ParseOutcome::Event(AdapterEvent::ToolCompleted {
        turn_id,
        tool_use_id: id.to_owned(),
        output,
        is_error: status != "success",
    })
}

fn parse_result(obj: &Value, turn_id: TurnId) -> ParseOutcome {
    let status = obj.get("status").and_then(Value::as_str).unwrap_or("");
    let usage = extract_usage(obj.get("stats"));
    let outcome = if status == "success" {
        TurnOutcome::Completed
    } else {
        let message = obj
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let kind = if is_gemini_auth_failure_message(&message) {
            FailureKind::AuthFailure
        } else {
            FailureKind::HarnessError
        };
        TurnOutcome::Failed { kind, message }
    };
    ParseOutcome::Event(AdapterEvent::TurnEnd {
        turn_id,
        outcome,
        ended_at: chrono::Utc::now(),
        usage: Some(usage),
    })
}

/// Map Gemini's `result.stats` shape into Switchboard's `TurnUsage`.
/// Unknown / missing fields collapse to zero or `None`; the parser never
/// panics on malformed shapes.
fn extract_usage(stats: Option<&Value>) -> TurnUsage {
    let stats = stats.unwrap_or(&Value::Null);
    let input_tokens = stats
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = stats
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached_input_tokens = stats.get("cached").and_then(Value::as_u64);
    TurnUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        reasoning_output_tokens: None,
        context_window: None,
        total_cost_usd: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn turn_id() -> TurnId {
        Uuid::now_v7()
    }

    fn agent_id() -> AgentId {
        Uuid::now_v7()
    }

    #[test]
    fn is_auth_failure_recognizes_known_substrings() {
        assert!(is_gemini_auth_failure_message(
            "API returned 401 Unauthorized"
        ));
        assert!(is_gemini_auth_failure_message(
            "PERMISSION_DENIED: token invalid"
        ));
        assert!(is_gemini_auth_failure_message(
            "Authentication failed; please re-login"
        ));
    }

    #[test]
    fn is_auth_failure_returns_false_for_unmatched_messages() {
        assert!(!is_gemini_auth_failure_message(
            "[API Error: Requested entity was not found.]"
        ));
        assert!(!is_gemini_auth_failure_message(""));
        assert!(!is_gemini_auth_failure_message("model not available"));
    }

    #[test]
    fn parse_init_emits_session_meta_with_model() {
        let line = r#"{"type":"init","session_id":"abc","model":"gemini-3-flash-preview"}"#;
        let mut s = GeminiParserState::default();
        let event = match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(e) => e,
            other => panic!("expected Event, got {other:?}"),
        };
        match event {
            AdapterEvent::SessionMeta {
                model,
                mcp_servers,
                skills,
                ..
            } => {
                assert_eq!(model, "gemini-3-flash-preview");
                assert!(mcp_servers.is_empty());
                assert!(skills.is_empty());
            }
            other => panic!("expected SessionMeta, got {other:?}"),
        }
    }

    #[test]
    fn parse_user_message_is_skipped() {
        let line = r#"{"type":"message","role":"user","content":"hi"}"#;
        let mut s = GeminiParserState::default();
        assert!(matches!(
            parse_line(line, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn parse_assistant_message_emits_content_chunk() {
        let line = r#"{"type":"message","role":"assistant","content":"ack","delta":true}"#;
        let mut s = GeminiParserState::default();
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::ContentChunk { text, kind, .. }) => {
                assert_eq!(text, "ack");
                assert_eq!(kind, ContentKind::Text);
            }
            other => panic!("expected ContentChunk, got {other:?}"),
        }
    }

    #[test]
    fn parse_assistant_message_with_empty_content_is_skipped() {
        let line = r#"{"type":"message","role":"assistant","content":"","delta":true}"#;
        let mut s = GeminiParserState::default();
        assert!(matches!(
            parse_line(line, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn parse_assistant_message_without_delta_true_is_skipped() {
        // Defends against a future Gemini CLI that emits a final consolidated
        // assistant message after the streaming deltas. Naively forwarding it
        // would double-emit the response text.
        let no_delta = r#"{"type":"message","role":"assistant","content":"hello"}"#;
        let delta_false =
            r#"{"type":"message","role":"assistant","content":"hello","delta":false}"#;
        let mut s = GeminiParserState::default();
        assert!(matches!(
            parse_line(no_delta, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
        assert!(matches!(
            parse_line(delta_false, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn parse_update_topic_tool_use_is_filtered_and_tracked() {
        let line =
            r#"{"type":"tool_use","tool_name":"update_topic","tool_id":"ut_1","parameters":{}}"#;
        let mut s = GeminiParserState::default();
        assert!(matches!(
            parse_line(line, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
        assert!(s.filtered_tool_ids.contains("ut_1"));
    }

    #[test]
    fn parse_update_topic_tool_result_is_filtered_after_tool_use() {
        let mut s = GeminiParserState::default();
        let use_line =
            r#"{"type":"tool_use","tool_name":"update_topic","tool_id":"ut_1","parameters":{}}"#;
        let res_line =
            r#"{"type":"tool_result","tool_id":"ut_1","status":"success","output":"topic"}"#;
        let _ = parse_line(use_line, turn_id(), agent_id(), &mut s);
        assert!(matches!(
            parse_line(res_line, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn parse_read_file_tool_use_emits_builtin_started() {
        let line = r#"{"type":"tool_use","tool_name":"read_file","tool_id":"rf_1","parameters":{"file_path":"x"}}"#;
        let mut s = GeminiParserState::default();
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::ToolStarted {
                name,
                kind,
                input,
                tool_use_id,
                ..
            }) => {
                assert_eq!(name, "read_file");
                assert_eq!(kind, ToolKind::Builtin);
                assert_eq!(tool_use_id, "rf_1");
                assert_eq!(input, json!({"file_path":"x"}));
            }
            other => panic!("expected ToolStarted, got {other:?}"),
        }
    }

    #[test]
    fn parse_mcp_prefixed_tool_use_emits_mcp_kind() {
        let line = r#"{"type":"tool_use","tool_name":"mcp__server__action","tool_id":"m_1","parameters":{}}"#;
        let mut s = GeminiParserState::default();
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::ToolStarted { kind, .. }) => {
                assert_eq!(kind, ToolKind::Mcp);
            }
            other => panic!("expected ToolStarted, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_result_success_with_empty_output_is_emitted_as_completed() {
        // Gemini's `read_file` legitimately emits `output:""` on success.
        // The adapter still emits a completion event (lifecycle is the live
        // contract; real content arrives via hydration).
        let line = r#"{"type":"tool_result","tool_id":"rf_1","status":"success","output":""}"#;
        let mut s = GeminiParserState::default();
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                output,
                is_error,
                tool_use_id,
                ..
            }) => {
                assert_eq!(output, "");
                assert!(!is_error);
                assert_eq!(tool_use_id, "rf_1");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_result_non_success_status_marks_is_error() {
        let line = r#"{"type":"tool_result","tool_id":"rf_1","status":"failed","output":"oops"}"#;
        let mut s = GeminiParserState::default();
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::ToolCompleted {
                is_error, output, ..
            }) => {
                assert!(is_error);
                assert_eq!(output, "oops");
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    #[test]
    fn parse_result_success_emits_completed_turn_end_with_usage() {
        let line = r#"{"type":"result","status":"success","stats":{"input_tokens":100,"output_tokens":5,"cached":50}}"#;
        let mut s = GeminiParserState::default();
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { outcome, usage, .. }) => {
                assert!(matches!(outcome, TurnOutcome::Completed));
                let usage = usage.expect("usage is always Some on result");
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 5);
                assert_eq!(usage.cached_input_tokens, Some(50));
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn parse_result_error_emits_failed_turn_end_with_classified_kind() {
        let line = r#"{"type":"result","status":"error","error":{"message":"[API Error: 401 Unauthorized]"},"stats":{}}"#;
        let mut s = GeminiParserState::default();
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { outcome, .. }) => {
                let TurnOutcome::Failed { kind, message } = outcome else {
                    panic!("expected Failed");
                };
                assert_eq!(kind, FailureKind::AuthFailure);
                assert!(message.contains("401 Unauthorized"));
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn parse_result_error_non_auth_falls_back_to_harness_error() {
        let line = r#"{"type":"result","status":"error","error":{"message":"[API Error: Requested entity was not found.]"},"stats":{}}"#;
        let mut s = GeminiParserState::default();
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { outcome, .. }) => {
                let TurnOutcome::Failed { kind, .. } = outcome else {
                    panic!("expected Failed");
                };
                assert_eq!(kind, FailureKind::HarnessError);
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_event_type_skips() {
        let line = r#"{"type":"future_unknown_event","payload":{}}"#;
        let mut s = GeminiParserState::default();
        assert!(matches!(
            parse_line(line, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        let line = "{not valid json";
        let mut s = GeminiParserState::default();
        assert!(matches!(
            parse_line(line, turn_id(), agent_id(), &mut s),
            ParseOutcome::Error(_)
        ));
    }
}
