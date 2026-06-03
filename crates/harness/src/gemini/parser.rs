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

/// Authored auth-failure message for Gemini. Both auth surfaces (in-stream
/// `result.status:"error"` with a 401, exit-42 401-on-stderr, and the new
/// exit-41 "Please set an Auth method" shape) emit this so the user sees
/// uniform actionable text. Reactive-auth posture — never advises
/// "reload Switchboard."
pub const GEMINI_AUTH_MESSAGE: &str =
    "Gemini authentication required — run `gemini` interactively to sign in";

/// Tail phrase that identifies Gemini's known-benign "streamed-then-error"
/// quirk. Gemini frequently appends an empty/malformed trailing step *after*
/// it has already streamed a complete answer; that step taints the turn's
/// `result.status` to `"error"` even though the user got a full response. The
/// real reason rides a standalone `type:"error"` event one line before the
/// `result`, carrying this message.
///
/// We match the distinctive tail phrase (not the generic "invalid stream"
/// prefix, which could front more serious failures) so the rescue is narrow.
/// A narrow match fails safe: if Gemini ever changes this wording the match
/// stops firing and such turns go back to being classified FAILED — never the
/// reverse (a genuine failure silently shown as success because the phrase
/// drifted into matching).
const GEMINI_BENIGN_TRAILING_ERROR: &str = "empty response or malformed tool call";

/// Substring that identifies Gemini's clean-logout shape on stderr
/// (captured 2026-05-27: exit 41 with stderr "Please set an Auth method
/// in your settings…", no stream-json). Case-insensitive match on the
/// exact captured shape — a bare "auth method" substring would falsely
/// match unrelated diagnostic text ("unsupported auth method",
/// "invalid auth method specified"); expand only with new observed shapes.
#[must_use]
pub fn is_gemini_logged_out_message(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains("please set an auth method")
}

#[derive(Debug, Default)]
pub struct GeminiParserState {
    /// `tool_use_id`s that the adapter filtered from `ToolStarted` emission
    /// because the tool name was in `GEMINI_INTERNAL_TOOL_NAMES`. The
    /// matching `tool_result` skips too — emitting a `ToolCompleted` for a
    /// tool that has no `ToolStarted` would render a phantom completion
    /// badge in the UI.
    filtered_tool_ids: HashSet<String>,
    /// `true` once a non-empty assistant `message` chunk has been emitted.
    /// Gates the benign streamed-then-error rescue in `parse_result`: the
    /// user only got a complete answer if content actually streamed.
    streamed_content: bool,
    /// Message from the most-recent standalone `type:"error"` event (last-wins
    /// if several precede the result). Gemini's terminal `result.status:"error"`
    /// line often has no `error` field; the real reason rides this separate
    /// event one line earlier. `parse_result` falls back to it so a failed turn
    /// surfaces a real message.
    last_error_message: Option<String>,
}

/// Return `true` if a Gemini error message is the known-benign
/// "streamed-then-error" quirk (Gemini appends an empty/malformed trailing
/// step after a complete answer). Matched case-insensitively on the
/// distinctive tail phrase, not the generic prefix — see
/// `GEMINI_BENIGN_TRAILING_ERROR` for the fail-safe rationale.
#[must_use]
fn is_benign_invalid_stream_error(message: &str) -> bool {
    message
        .to_lowercase()
        .contains(GEMINI_BENIGN_TRAILING_ERROR)
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
        Some("message") => parse_message(&value, turn_id, state),
        Some("tool_use") => parse_tool_use(&value, turn_id, state),
        Some("tool_result") => parse_tool_result(&value, turn_id, state),
        Some("error") => parse_error_event(&value, state),
        Some("result") => parse_result(&value, turn_id, state),
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

fn parse_message(obj: &Value, turn_id: TurnId, state: &mut GeminiParserState) -> ParseOutcome {
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
    // Record that the user actually received answer content this turn. Gates
    // the benign streamed-then-error rescue in `parse_result`.
    state.streamed_content = true;
    ParseOutcome::Event(AdapterEvent::ContentChunk {
        turn_id,
        kind: ContentKind::Text,
        text,
    })
}

/// Capture a standalone `type:"error"` event. It emits nothing on its own —
/// it's terminal-adjacent context that Gemini sometimes emits one line before
/// a `result.status:"error"` whose own `error` field is absent. `parse_result`
/// reads `state.last_error_message` to surface the real reason and to detect
/// the known-benign streamed-then-error quirk.
fn parse_error_event(obj: &Value, state: &mut GeminiParserState) -> ParseOutcome {
    if let Some(message) = obj.get("message").and_then(Value::as_str) {
        state.last_error_message = Some(message.to_owned());
    }
    ParseOutcome::Skip
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

fn parse_result(obj: &Value, turn_id: TurnId, state: &GeminiParserState) -> ParseOutcome {
    let status = obj.get("status").and_then(Value::as_str).unwrap_or("");
    let usage = extract_usage(obj.get("stats"));
    let outcome = if status == "success" {
        TurnOutcome::Completed
    } else {
        classify_result_failure(obj, state)
    };
    ParseOutcome::Event(AdapterEvent::TurnEnd {
        turn_id,
        outcome,
        ended_at: chrono::Utc::now(),
        usage: Some(usage),
        // Gemini exposes no context window (`extract_usage` leaves it `None`),
        // so there's nothing to persist.
        context_window_source: None,
        spend: None,
    })
}

/// Classify a non-success Gemini `result` into a `TurnOutcome`. Resolves the
/// failure message (falling back to the captured standalone `type:"error"`
/// event when the `result` line carries none), applies the benign
/// streamed-then-error rescue, and otherwise classifies auth-vs-`HarnessError`.
fn classify_result_failure(obj: &Value, state: &GeminiParserState) -> TurnOutcome {
    // The `result` line's own `error.message` is often absent on the benign
    // quirk; fall back to the standalone `type:"error"` event captured into
    // `state.last_error_message` so the real reason surfaces instead of an
    // empty FAILED chip.
    let result_message = obj
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let message = if result_message.is_empty() {
        state.last_error_message.clone().unwrap_or_default()
    } else {
        result_message
    };

    // Benign streamed-then-error rescue: Gemini frequently appends an
    // empty/malformed trailing step *after* a complete answer, tainting the
    // turn's status to `"error"`. If answer content actually streamed and the
    // failure carries this specific known-benign signature, the user got a
    // complete response — treat the turn as Completed rather than flashing a
    // misleading FAILED chip.
    //
    // RESIDUAL RISK: a turn that genuinely failed *after* streaming only
    // partial content, emitting this same benign signature, would be shown as
    // success. Accepted trade vs. a misleading FAILED chip on every other
    // complete turn. The match is narrow (distinctive tail phrase, not the
    // generic prefix) and gated on `streamed_content`.
    if state.streamed_content && is_benign_invalid_stream_error(&message) {
        return TurnOutcome::Completed;
    }

    // Deliberate scope: only `is_gemini_auth_failure_message` is checked here,
    // not `is_gemini_logged_out_message`. The exit-41 logged-out shape is an
    // EOF/stderr-only surface (no stream events emitted), so a stream
    // `result:error` never carries the "Please set an Auth method" text in
    // practice. Revisit if a future Gemini adds a `result.status:"error"`
    // stream variant for logout.
    let (kind, message) = if is_gemini_auth_failure_message(&message) {
        // Author across all auth surfaces (stream, exit-41, exit-42) so the
        // user sees one consistent actionable message — not the raw
        // "401 Unauthorized" / etc. that vary by failure shape.
        (FailureKind::AuthFailure, GEMINI_AUTH_MESSAGE.to_owned())
    } else {
        (FailureKind::HarnessError, message)
    };
    TurnOutcome::Failed { kind, message }
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
        cache_creation_input_tokens: None,
        // Gemini exposes no context window, so occupancy is never computed and
        // the bar stays hidden; leave the reconciled value `None` rather than
        // guess at its (unverified) cache accounting.
        context_input_tokens: None,
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
                // Authored message replaces the raw stream text — the user
                // sees one consistent Gemini auth message across surfaces.
                assert_eq!(message, GEMINI_AUTH_MESSAGE);
                assert!(message.contains("Gemini authentication required"));
                assert!(!message.contains("reload Switchboard"));
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn is_logged_out_recognizes_only_the_captured_shape() {
        // Positive: the exact captured exit-41 shape.
        assert!(is_gemini_logged_out_message(
            "Please set an Auth method in your settings."
        ));
        // Negative: unrelated diagnostics that mention "auth method" but are
        // NOT logged-out states. The substring guard was tightened so these
        // don't falsely route to the authored auth-failure message.
        assert!(!is_gemini_logged_out_message("auth method missing"));
        assert!(!is_gemini_logged_out_message("unsupported auth method"));
        assert!(!is_gemini_logged_out_message(
            "invalid auth method specified"
        ));
        assert!(!is_gemini_logged_out_message("401 Unauthorized"));
        assert!(!is_gemini_logged_out_message(""));
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

    #[test]
    fn parse_error_event_captures_message_into_state() {
        let line = r#"{"type":"error","severity":"error","message":"Invalid stream: The model returned an empty response or malformed tool call."}"#;
        let mut s = GeminiParserState::default();
        assert!(matches!(
            parse_line(line, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
        assert_eq!(
            s.last_error_message.as_deref(),
            Some("Invalid stream: The model returned an empty response or malformed tool call.")
        );
    }

    #[test]
    fn message_less_error_event_does_not_clobber_prior_capture() {
        // A message-bearing error captures the reason; a later message-less
        // error event must leave it intact (don't blank the real reason that
        // `parse_result` falls back to).
        let mut s = GeminiParserState::default();
        let _ = parse_line(
            r#"{"type":"error","message":"the real reason"}"#,
            turn_id(),
            agent_id(),
            &mut s,
        );
        let _ = parse_line(
            r#"{"type":"error","severity":"error"}"#,
            turn_id(),
            agent_id(),
            &mut s,
        );
        assert_eq!(s.last_error_message.as_deref(), Some("the real reason"));
    }

    #[test]
    fn message_less_error_event_stays_none_when_nothing_captured() {
        let mut s = GeminiParserState::default();
        assert!(matches!(
            parse_line(r#"{"type":"error"}"#, turn_id(), agent_id(), &mut s),
            ParseOutcome::Skip
        ));
        assert!(s.last_error_message.is_none());
    }

    #[test]
    fn benign_trailing_error_after_streamed_content_completes_the_turn() {
        // The full answer streamed first; the trailing benign error must not
        // taint the turn — the user got a complete response.
        let mut s = GeminiParserState::default();
        let _ = parse_line(
            r#"{"type":"message","role":"assistant","content":"full answer","delta":true}"#,
            turn_id(),
            agent_id(),
            &mut s,
        );
        let _ = parse_line(
            r#"{"type":"error","severity":"error","message":"Invalid stream: The model returned an empty response or malformed tool call."}"#,
            turn_id(),
            agent_id(),
            &mut s,
        );
        match parse_line(
            r#"{"type":"result","status":"error","stats":{}}"#,
            turn_id(),
            agent_id(),
            &mut s,
        ) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { outcome, .. }) => {
                assert!(
                    matches!(outcome, TurnOutcome::Completed),
                    "benign trailing error after streamed content must complete, got {outcome:?}"
                );
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn benign_trailing_error_without_streamed_content_fails_with_captured_message() {
        // No answer streamed → the rescue must NOT fire. The turn fails, but
        // the captured standalone error message surfaces (was empty before).
        let mut s = GeminiParserState::default();
        let _ = parse_line(
            r#"{"type":"error","severity":"error","message":"Invalid stream: The model returned an empty response or malformed tool call."}"#,
            turn_id(),
            agent_id(),
            &mut s,
        );
        match parse_line(
            r#"{"type":"result","status":"error","stats":{}}"#,
            turn_id(),
            agent_id(),
            &mut s,
        ) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed { kind, message },
                ..
            }) => {
                assert_eq!(kind, FailureKind::HarnessError);
                assert!(
                    message.contains("empty response or malformed tool call"),
                    "captured standalone error message must surface, got {message:?}"
                );
            }
            other => panic!("expected Failed TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn genuine_error_after_streamed_content_still_fails() {
        // A non-benign error after streamed content must remain Failed — the
        // rescue is narrow and must not fire on arbitrary trailing errors.
        let mut s = GeminiParserState::default();
        let _ = parse_line(
            r#"{"type":"message","role":"assistant","content":"partial","delta":true}"#,
            turn_id(),
            agent_id(),
            &mut s,
        );
        let line = r#"{"type":"result","status":"error","error":{"message":"[API Error: Requested entity was not found.]"},"stats":{}}"#;
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed { kind, message },
                ..
            }) => {
                assert_eq!(kind, FailureKind::HarnessError);
                assert!(message.contains("Requested entity was not found"));
            }
            other => panic!("expected Failed TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn auth_error_after_streamed_content_still_classifies_as_auth_failure() {
        // Regression guard: the benign rescue must not shadow auth
        // classification. An auth-substring error stays AuthFailure even when
        // content streamed first.
        let mut s = GeminiParserState::default();
        let _ = parse_line(
            r#"{"type":"message","role":"assistant","content":"hi","delta":true}"#,
            turn_id(),
            agent_id(),
            &mut s,
        );
        let line = r#"{"type":"result","status":"error","error":{"message":"[API Error: 401 Unauthorized]"},"stats":{}}"#;
        match parse_line(line, turn_id(), agent_id(), &mut s) {
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed { kind, message },
                ..
            }) => {
                assert_eq!(kind, FailureKind::AuthFailure);
                assert_eq!(message, GEMINI_AUTH_MESSAGE);
            }
            other => panic!("expected Failed(AuthFailure) TurnEnd, got {other:?}"),
        }
    }
}
