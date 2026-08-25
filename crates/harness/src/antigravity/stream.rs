//! Parser for `agy --output-format stream-json`, the adapter's **control**
//! channel.
//!
//! `agy` 1.1.8 added a structured NDJSON stream to print mode. Switchboard
//! consumes it for control signals only — the conversation id, tool failures,
//! and the terminal outcome — while displayed content keeps coming from the
//! conversation transcript. See the module doc on [`super`] for why that split
//! exists rather than a wholesale cutover.
//!
//! Observed vocabulary (agy 1.1.19, 2026-08-23):
//!
//! ```text
//! {"event":"init","conversation_id":"<uuid>","init":{"cwd":…,"permission_mode":…,"tools":[…]}}
//! {"event":"step_update","step_update":{"conversation_id":…,"step_index":N,
//!    "state":"ACTIVE|DONE|ERROR","step_type":"user_input|checkpoint|agent_response|tool|system_message",
//!    "tool_name":…,"tool_info":{"name","parameters","output"},"text_delta":…,"usage":{…}}}
//! {"event":"result","result":{"conversation_id":…,"status":"SUCCESS|ERROR","response":…,
//!    "error":…,"num_turns":N,"usage":{…}}}
//! ```
//!
//! **The vocabulary is open and treated as such.** `system_message` was not in
//! the first capture and appeared on a later probe; unknown `event` values and
//! unknown `step_type`s are skipped rather than treated as errors, matching
//! every other adapter parser in this crate.
//!
//! **No resume gating.** Probed @ 1.1.19: a `--conversation` resume emits only
//! the new turn's steps (turn 1 → steps 0-2, turn 2 → steps 3-5), unlike text
//! mode, which replays the whole conversation's prior answers on stdout. So
//! there is nothing to filter. This is worth stating because the obvious guard
//! would be *wrong*: the resume cursor this adapter already keeps is a
//! transcript **line** index, and lines diverge from step indices exactly when
//! a tool fails (agy omits that step from the transcript — the whole reason
//! this parser exists), so gating stream events on it would silently drop live
//! events.

use serde::Deserialize;
use uuid::Uuid;

/// One line of the stream, after discriminating on `event`.
///
/// Deliberately not an exhaustive model of the wire format: only the fields
/// the adapter acts on are typed, and `#[serde(other)]` absorbs the rest.
#[derive(Debug, PartialEq)]
pub(crate) enum StreamEvent {
    /// Turn start. Carries the conversation id on **every** turn, including
    /// resumes (probed). On a first turn it is the capture; on a resume the id
    /// is already known, and it is retained only as a **fork candidate** — see
    /// the handler in [`super`]. It is not compared against the resume locator
    /// for its own sake, so this is not a corroboration step.
    Init { conversation_id: Uuid },
    /// A tool step that ended in `ERROR`. This is the event that has no
    /// transcript equivalent: agy 1.1.19 writes **no record at all** for a
    /// tool call rejected during argument validation, so without the stream
    /// the tool would stay pending forever and a later tool's result would
    /// FIFO-pair onto it.
    ///
    /// `message` is the harness's own `tool_info.error.message`, surfaced
    /// verbatim (the project prefers harness text over authored text wherever
    /// one exists — it is more specific and drift-resilient). `None` when the
    /// event carries no message, which the caller replaces with authored copy.
    ToolFailed {
        step_index: i64,
        message: Option<String>,
    },
    /// The terminal event.
    Result(StreamResult),
    /// A parseable stream line the adapter does not act on (`ACTIVE`/`DONE`
    /// tool steps, `agent_response`, unknown `step_type`s, unknown `event`s).
    /// Distinct from "not a stream line at all" — it still proves the process
    /// produced structured output, which the liveness signal relies on.
    Ignored,
}

/// The terminal `result` payload, reduced to what classification needs.
#[derive(Debug, PartialEq)]
pub(crate) struct StreamResult {
    /// `false` when `status` is anything other than `SUCCESS`.
    pub succeeded: bool,
    /// `result.error`, when non-empty.
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct RawLine {
    event: String,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    step_update: Option<RawStepUpdate>,
    #[serde(default)]
    result: Option<RawResult>,
}

#[derive(Deserialize)]
struct RawStepUpdate {
    #[serde(default)]
    step_index: i64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    step_type: String,
    #[serde(default)]
    tool_info: Option<RawToolInfo>,
}

#[derive(Deserialize)]
struct RawToolInfo {
    #[serde(default)]
    error: Option<RawToolError>,
}

#[derive(Deserialize)]
struct RawToolError {
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct RawResult {
    #[serde(default)]
    status: String,
    #[serde(default)]
    error: Option<String>,
}

/// Parse one stdout line.
///
/// `None` means "not a stream event" — either not JSON at all, or JSON without
/// the shape we expect. The caller falls back to the plain-text handlers for
/// those, which is what keeps the pre-stream control signals (the auth line,
/// bare `Error:` lines) working if `agy` ever emits them alongside the stream.
pub(crate) fn parse_line(line: &str) -> Option<StreamEvent> {
    let raw: RawLine = serde_json::from_str(line.trim()).ok()?;
    match raw.event.as_str() {
        "init" => {
            let id = raw.conversation_id.as_deref()?;
            Uuid::parse_str(id)
                .ok()
                .map(|conversation_id| StreamEvent::Init { conversation_id })
        }
        "step_update" => {
            let step = raw.step_update?;
            // Only the ERROR terminal state is consumed. `DONE` tool results
            // keep arriving from the transcript, which is the content source;
            // taking them from both places would complete each tool twice.
            if step.step_type == "tool" && step.state == "ERROR" {
                let message = step
                    .tool_info
                    .and_then(|info| info.error)
                    .and_then(|error| error.message)
                    .filter(|m| !m.trim().is_empty());
                Some(StreamEvent::ToolFailed {
                    step_index: step.step_index,
                    message,
                })
            } else {
                Some(StreamEvent::Ignored)
            }
        }
        "result" => {
            let result = raw.result?;
            let error = result.error.filter(|e| !e.trim().is_empty());
            Some(StreamEvent::Result(StreamResult {
                succeeded: result.status == "SUCCESS",
                error,
            }))
        }
        _ => Some(StreamEvent::Ignored),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_yields_the_conversation_id() {
        let line = r#"{"event":"init","conversation_id":"5a8dd0c7-3450-4048-a5fb-27ae8f663dee","init":{"cwd":"/tmp","permission_mode":"bypass","tools":[]}}"#;
        assert_eq!(
            parse_line(line),
            Some(StreamEvent::Init {
                conversation_id: Uuid::parse_str("5a8dd0c7-3450-4048-a5fb-27ae8f663dee").unwrap()
            })
        );
    }

    #[test]
    fn init_without_a_parseable_id_is_not_an_init() {
        // Degrading to `None` (rather than a malformed capture) keeps the
        // log-line and filesystem fallbacks reachable.
        for line in [
            r#"{"event":"init","conversation_id":"not-a-uuid"}"#,
            r#"{"event":"init"}"#,
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }

    #[test]
    fn tool_error_step_carries_the_harness_message() {
        // Captured verbatim from a real rejected `view_file` @ 1.1.19.
        let err = r#"{"event":"step_update","step_update":{"step_index":3,"state":"ERROR","step_type":"tool","tool_name":"view_file","tool_info":{"name":"view_file","parameters":{},"error":{"type":"TOOL_ERROR","message":"cortex tool view_file: model output error: invalid tool call error (invalid_args) failed to read file"}}}}"#;
        assert_eq!(
            parse_line(err),
            Some(StreamEvent::ToolFailed {
                step_index: 3,
                message: Some(
                    "cortex tool view_file: model output error: invalid tool call error \
                     (invalid_args) failed to read file"
                        .to_owned()
                )
            })
        );
    }

    #[test]
    fn tool_error_step_is_the_only_consumed_step_update() {
        // No `tool_info` at all, and a blank message, both degrade to `None`
        // so the caller can substitute authored copy rather than showing an
        // empty tool output.
        for line in [
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"ERROR","step_type":"tool","tool_name":"view_file"}}"#,
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"ERROR","step_type":"tool","tool_info":{"error":{"message":"  "}}}}"#,
        ] {
            assert_eq!(
                parse_line(line),
                Some(StreamEvent::ToolFailed {
                    step_index: 3,
                    message: None
                }),
                "{line}"
            );
        }

        // Everything else parses but is inert — including the `DONE` tool
        // result, which the transcript already supplies.
        for line in [
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"ACTIVE","step_type":"tool","tool_name":"view_file"}}"#,
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"DONE","step_type":"tool","tool_name":"view_file"}}"#,
            r#"{"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"agent_response"}}"#,
            r#"{"event":"step_update","step_update":{"step_index":4,"state":"DONE","step_type":"system_message"}}"#,
            r#"{"event":"step_update","step_update":{"step_index":9,"state":"ERROR","step_type":"agent_response"}}"#,
        ] {
            assert_eq!(parse_line(line), Some(StreamEvent::Ignored), "{line}");
        }
    }

    #[test]
    fn result_carries_status_and_error() {
        let ok = r#"{"event":"result","result":{"status":"SUCCESS","response":"ack\n","usage":{"output_tokens":5}}}"#;
        assert_eq!(
            parse_line(ok),
            Some(StreamEvent::Result(StreamResult {
                succeeded: true,
                error: None
            }))
        );

        let bad = r#"{"event":"result","result":{"status":"ERROR","response":"","error":"timeout waiting for response"}}"#;
        assert_eq!(
            parse_line(bad),
            Some(StreamEvent::Result(StreamResult {
                succeeded: false,
                error: Some("timeout waiting for response".to_owned())
            }))
        );
    }

    #[test]
    fn result_error_that_is_blank_is_treated_as_absent() {
        // A failure with no message must not produce an empty-string
        // `HarnessError`; classification falls through to a real signal.
        let line = r#"{"event":"result","result":{"status":"ERROR","error":"   "}}"#;
        assert_eq!(
            parse_line(line),
            Some(StreamEvent::Result(StreamResult {
                succeeded: false,
                error: None
            }))
        );
    }

    #[test]
    fn unknown_events_and_non_json_are_distinguished() {
        // An unknown *event* is still structured output (liveness), whereas a
        // non-stream line must fall through to the plain-text handlers.
        assert_eq!(
            parse_line(r#"{"event":"telemetry","payload":{}}"#),
            Some(StreamEvent::Ignored)
        );
        for line in [
            "Authentication required. Please visit the URL to log in:",
            "Error: timeout waiting for response",
            "",
            "{not json",
            // Valid JSON, but not a stream line.
            r#"{"unrelated":true}"#,
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }
}
