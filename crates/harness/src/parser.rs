use chrono::Utc;
use serde_json::Value;

use crate::events::{AdapterEvent, FailureKind, TurnId, TurnOutcome};

pub enum ParseOutcome {
    Event(AdapterEvent),
    Skip,
    Error(String),
}

/// Parse one stream-json line into an `AdapterEvent`, a skip, or a parse error.
///
/// Only `stream_event.event.content_block_delta.text_delta` → `ContentChunk`.
/// `result.is_error || result.api_error_status != null` → `TurnEnd(Failed)`.
/// `result` success → `TurnEnd(Completed)`.
/// Everything else (system, assistant, user, `rate_limit_event`, other `stream_event`
/// subtypes) → Skip. Unknown top-level types → Skip (forward compat).
///
/// `AdapterEvent::TurnStart` is never emitted here — it is dispatcher-owned (M1.4).
pub fn parse_line(line: &str, turn_id: TurnId) -> ParseOutcome {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return ParseOutcome::Error(e.to_string()),
    };

    match value.get("type").and_then(Value::as_str) {
        Some("stream_event") => parse_stream_event(&value, turn_id),
        Some("result") => parse_result(&value, turn_id),
        _ => ParseOutcome::Skip,
    }
}

fn parse_stream_event(obj: &Value, turn_id: TurnId) -> ParseOutcome {
    let Some(event) = obj.get("event") else {
        return ParseOutcome::Skip;
    };

    // Only `content_block_delta` with `text_delta` emits a ContentChunk.
    // With `--include-partial-messages` enabled, deltas are the sole source of
    // ContentChunk text. The terminal `assistant` message is explicitly skipped
    // (type="assistant") to prevent double-emit.
    if event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return ParseOutcome::Skip;
    }

    let Some(delta) = event.get("delta") else {
        return ParseOutcome::Skip;
    };

    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        // input_json_delta (tool input), thinking_delta, etc. — all skipped.
        return ParseOutcome::Skip;
    }

    let text = delta
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    if text.is_empty() {
        return ParseOutcome::Skip;
    }

    ParseOutcome::Event(AdapterEvent::ContentChunk { turn_id, text })
}

fn parse_result(obj: &Value, turn_id: TurnId) -> ParseOutcome {
    let is_error = obj
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let has_api_error = obj.get("api_error_status").is_some_and(|v| !v.is_null());

    let outcome = if is_error || has_api_error {
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

    ParseOutcome::Event(AdapterEvent::TurnEnd {
        turn_id,
        outcome,
        ended_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn tid() -> TurnId {
        Uuid::now_v7()
    }

    #[test]
    fn text_delta_yields_content_chunk() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}}"#;
        let turn_id = tid();
        match parse_line(line, turn_id) {
            ParseOutcome::Event(AdapterEvent::ContentChunk { text, turn_id: tid }) => {
                assert_eq!(text, "hello");
                assert_eq!(tid, turn_id);
            }
            other => panic!(
                "expected ContentChunk, got other: {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn result_success_yields_turn_end_completed() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":null,"result":"4"}"#;
        match parse_line(line, tid()) {
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
        match parse_line(line, tid()) {
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
        // api_error_status is a non-null integer — treat as error regardless of is_error.
        let line =
            r#"{"type":"result","is_error":false,"api_error_status":500,"result":"server error"}"#;
        match parse_line(line, tid()) {
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
    fn system_init_is_skipped() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc"}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn assistant_message_is_skipped() {
        // Guards the double-emit invariant: the terminal `assistant` message text
        // must NOT become a ContentChunk when --include-partial-messages is active.
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"4"}]}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn rate_limit_event_is_skipped() {
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn user_tool_result_event_is_skipped() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x"}]}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_message_start_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"message_start","message":{}}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_content_block_start_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_content_block_stop_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_message_delta_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"}}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn stream_event_message_stop_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"message_stop"}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn input_json_delta_tool_input_is_skipped() {
        // Tool input stream deltas must not be emitted as ContentChunks.
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{"}}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn empty_text_delta_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":""}}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn invalid_json_yields_error() {
        let line = "{not valid json";
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Error(_)));
    }

    #[test]
    fn unknown_top_level_type_is_skipped_for_forward_compat() {
        let line = r#"{"type":"unknown_future_event","data":{}}"#;
        assert!(matches!(parse_line(line, tid()), ParseOutcome::Skip));
    }

    #[test]
    fn result_missing_error_fields_defaults_to_completed() {
        // If is_error is absent (not false), treat as non-error.
        let line = r#"{"type":"result","result":"ok"}"#;
        assert!(matches!(
            parse_line(line, tid()),
            ParseOutcome::Event(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            })
        ));
    }
}
