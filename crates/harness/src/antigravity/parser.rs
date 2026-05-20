//! Parser for Antigravity's `transcript.jsonl` records and for the
//! `agy -p` stdout error/auth signals.
//!
//! Antigravity has no structured stream protocol (unlike Claude / Codex /
//! Gemini stream-json). Two parseable surfaces exist:
//!
//! - **stdout** carries the model's final answer text (server-side
//!   "drip"), plus `Error:` / `Warning:` / `Authentication required` lines
//!   on failure. `agy` exits 0 on essentially every condition, so stdout
//!   text — not the exit code — is the failure signal.
//! - **`transcript.jsonl`** carries one record per "step": user input,
//!   model planner responses (with `thinking` + `tool_calls`), and tool
//!   results (`RUN_COMMAND`, `VIEW_FILE`, other `CortexStep*` types). It
//!   has no top-level metadata record and no terminal "turn complete"
//!   record — the conversation UUID lives in the directory name, and the
//!   turn terminates when the `agy` process exits.
//!
//! See `docs/research/antigravity-cli-observed.md` for the ground-truth
//! shapes these types mirror.

use std::collections::VecDeque;

use serde::Deserialize;
use serde_json::Value;

use crate::events::{AdapterEvent, ContentKind, ToolKind, TurnId};

/// One record (line) of `transcript.jsonl`. Fields beyond these exist
/// (`created_at`, etc.) but aren't load-bearing for event mapping;
/// `#[serde]` ignores unknown fields by default, so the type tolerates the
/// large, growing `type` vocabulary and any future field additions.
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptRecord {
    #[serde(default)]
    pub step_index: i64,
    #[serde(default)]
    pub source: String,
    #[serde(rename = "type", default)]
    pub record_type: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// A tool invocation inside a `PLANNER_RESPONSE.tool_calls[]`. `args` values
/// are pre-stringified (each is a JSON string containing a JSON literal);
/// we surface the object verbatim as the tool `input` rather than trying to
/// unwrap, since the shape varies per tool and the UI renders it opaquely.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

impl TranscriptRecord {
    /// `MODEL` + `PLANNER_RESPONSE` is the model's turn: it carries
    /// `thinking`, optional `tool_calls`, and (when the model is done)
    /// final answer `content`.
    fn is_planner_response(&self) -> bool {
        self.source == "MODEL" && self.record_type == "PLANNER_RESPONSE"
    }

    /// A `MODEL` record that is not a planner response is a tool result
    /// (`RUN_COMMAND`, `VIEW_FILE`, `CortexStep*`...). Its `content` is a
    /// pre-rendered text blob.
    fn is_tool_result(&self) -> bool {
        self.source == "MODEL" && !self.is_planner_response()
    }

    /// A planner response with non-empty `content` and no tool calls is the
    /// model's final answer — the signal that the turn produced output.
    /// Used for outcome classification (no structured terminal record
    /// exists).
    pub fn is_terminal_answer(&self) -> bool {
        self.is_planner_response()
            && self.tool_calls.as_ref().is_none_or(Vec::is_empty)
            && self.content.as_ref().is_some_and(|c| !c.trim().is_empty())
    }
}

/// Per-turn parser state. Tracks the FIFO of in-flight tool invocations so
/// a tool-result record can be paired with the `ToolStarted` it completes —
/// the transcript carries no tool id on result records, and Antigravity
/// executes tools sequentially (one `PLANNER_RESPONSE` tool call, then its
/// result record), so order-based pairing is correct.
#[derive(Debug, Default)]
pub struct AntigravityParserState {
    pending_tool_ids: VecDeque<String>,
}

/// Map one transcript record to the **live** adapter events it produces.
///
/// Live path: `thinking` → `ContentChunk{Thinking}`, tool calls →
/// `ToolStarted`, tool results → `ToolCompleted`. The model's final answer
/// `content` is intentionally **not** emitted here — stdout already streamed
/// it live, and re-emitting from the transcript would double the text.
/// User-input and conversation-history records are skipped (the UI already
/// shows the user's prompt). Disk hydration on project reopen reuses
/// [`TranscriptRecord`] but emits the answer content too, since there is no
/// stdout to replay then.
pub fn record_to_live_events(
    rec: &TranscriptRecord,
    turn_id: TurnId,
    state: &mut AntigravityParserState,
) -> Vec<AdapterEvent> {
    let mut out = Vec::new();

    if let Some(thinking) = &rec.thinking
        && !thinking.trim().is_empty()
    {
        out.push(AdapterEvent::ContentChunk {
            turn_id,
            kind: ContentKind::Thinking,
            text: thinking.clone(),
        });
    }

    if let Some(calls) = &rec.tool_calls {
        for (call_index, call) in calls.iter().enumerate() {
            // Include the call index so two same-name tool calls in one
            // planner record (the array allows it) get distinct ids — a
            // bare `{step}:{name}` would collide and make UI/tool pairing
            // ambiguous.
            let tool_use_id = format!("{}:{}:{}", rec.step_index, call_index, call.name);
            state.pending_tool_ids.push_back(tool_use_id.clone());
            out.push(AdapterEvent::ToolStarted {
                turn_id,
                tool_use_id,
                kind: classify_tool_kind(&call.name),
                name: call.name.clone(),
                input: call.args.clone(),
            });
        }
    }

    if rec.is_tool_result() {
        // Pair with the oldest unmatched ToolStarted (FIFO). If no pending
        // id (a tool result with no preceding start we saw — e.g., a resume
        // cursor landing mid-pair), synthesize one from the record so the
        // completion still surfaces rather than vanishing.
        let tool_use_id = state
            .pending_tool_ids
            .pop_front()
            .unwrap_or_else(|| format!("{}:{}", rec.step_index, rec.record_type));
        let is_error = rec.status.as_deref() == Some("FAILED");
        out.push(AdapterEvent::ToolCompleted {
            turn_id,
            tool_use_id,
            output: rec.content.clone().unwrap_or_default(),
            is_error,
        });
    }

    out
}

/// Tool-kind classification. The native tools (`run_command`, `view_file`,
/// `edit_file`, ...) are `Builtin`; `CortexStepMcpTool`-dispatched MCP tools
/// would be `Mcp`, but the transcript records the underlying tool name, not
/// the Cortex step type, so we can't reliably distinguish MCP here yet.
/// Defaults to `Builtin`; refine when an MCP probe pins the name shape.
fn classify_tool_kind(_name: &str) -> ToolKind {
    ToolKind::Builtin
}

/// Detect Antigravity's unauthenticated-dispatch signal on a stdout line.
///
/// Verified shapes (captured from a real logged-out `agy -p` run): the
/// interactive-OAuth fallback prints `Authentication required. Please visit
/// the URL to log in:` and, on the 30s timeout, `Error: authentication
/// timed out.`. Both map to an auth failure. `agy` exits 0 in both cases,
/// so this stdout match is the only reliable signal.
#[must_use]
pub fn is_auth_failure_line(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("Authentication required")
        || l.contains("authentication timed out")
        || l.contains("not logged into Antigravity")
}

/// Scan accumulated stdout lines for a fatal `Error:` line. Returns the
/// first one found. `Warning:` lines are deliberately excluded from the
/// error scan. In particular, `Warning: conversation "..." not found.` is
/// **not** a plain degraded success — it signals that a resume's conversation
/// expired and `agy` forked a fresh one; the adapter's producer detects that
/// separately and runs fork-and-heal (recapture the new conversation), or
/// fails the turn if recapture isn't possible. This function only answers
/// "is there a hard `Error:` line," not how warnings are handled.
#[must_use]
pub fn first_error_line(stdout_lines: &[String]) -> Option<String> {
    stdout_lines
        .iter()
        .map(|l| l.trim())
        .find(|l| l.starts_with("Error:"))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::TurnOutcome;
    use uuid::Uuid;

    fn tid() -> TurnId {
        Uuid::now_v7()
    }

    fn parse(line: &str) -> TranscriptRecord {
        serde_json::from_str(line).expect("valid record")
    }

    #[test]
    fn planner_response_with_thinking_emits_thinking_chunk() {
        let rec = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","thinking":"deliberating","content":"ack"}"#,
        );
        let mut state = AntigravityParserState::default();
        let events = record_to_live_events(&rec, tid(), &mut state);
        // Thinking emitted; final answer content NOT emitted (stdout has it).
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AdapterEvent::ContentChunk {
                kind: ContentKind::Thinking,
                text,
                ..
            } if text == "deliberating"
        ));
    }

    #[test]
    fn planner_response_final_answer_emits_nothing_live() {
        // content-only PLANNER_RESPONSE: stdout already streamed the text,
        // so the live path emits no ContentChunk for it.
        let rec = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"ack"}"#,
        );
        let mut state = AntigravityParserState::default();
        assert!(record_to_live_events(&rec, tid(), &mut state).is_empty());
        assert!(rec.is_terminal_answer());
    }

    #[test]
    fn planner_response_with_tool_calls_emits_tool_started() {
        let rec = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"ls\""}}]}"#,
        );
        let mut state = AntigravityParserState::default();
        let events = record_to_live_events(&rec, tid(), &mut state);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AdapterEvent::ToolStarted {
                tool_use_id,
                name,
                kind,
                ..
            } => {
                assert_eq!(tool_use_id, "2:0:run_command");
                assert_eq!(name, "run_command");
                assert_eq!(*kind, ToolKind::Builtin);
            }
            other => panic!("expected ToolStarted, got {other:?}"),
        }
        // The id was queued for the eventual result record.
        assert_eq!(state.pending_tool_ids.len(), 1);
    }

    #[test]
    fn tool_result_record_pairs_with_pending_started_id() {
        let started = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{}}]}"#,
        );
        let result = parse(
            r#"{"step_index":3,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"Output:\nMARKER.txt\n"}"#,
        );
        let mut state = AntigravityParserState::default();
        let turn = tid();
        let _ = record_to_live_events(&started, turn, &mut state);
        let events = record_to_live_events(&result, turn, &mut state);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } => {
                assert_eq!(
                    tool_use_id, "2:0:run_command",
                    "FIFO-paired to the start id"
                );
                assert!(output.contains("MARKER.txt"));
                assert!(!is_error);
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
        assert!(state.pending_tool_ids.is_empty(), "pending id consumed");
    }

    #[test]
    fn two_same_name_tool_calls_in_one_record_get_distinct_ids() {
        // The tool_calls array can carry multiple calls; two `run_command`s
        // in one planner record must not collide on tool_use_id.
        let rec = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{}},{"name":"run_command","args":{}}]}"#,
        );
        let mut state = AntigravityParserState::default();
        let events = record_to_live_events(&rec, tid(), &mut state);
        assert_eq!(events.len(), 2);
        let ids: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AdapterEvent::ToolStarted { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["2:0:run_command", "2:1:run_command"]);
    }

    #[test]
    fn tool_result_failed_status_sets_is_error() {
        let result = parse(
            r#"{"step_index":3,"source":"MODEL","type":"RUN_COMMAND","status":"FAILED","content":"boom"}"#,
        );
        let mut state = AntigravityParserState::default();
        let events = record_to_live_events(&result, tid(), &mut state);
        assert!(matches!(
            &events[0],
            AdapterEvent::ToolCompleted { is_error: true, .. }
        ));
    }

    #[test]
    fn user_input_record_emits_nothing_live() {
        let rec = parse(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
        );
        let mut state = AntigravityParserState::default();
        assert!(record_to_live_events(&rec, tid(), &mut state).is_empty());
    }

    #[test]
    fn conversation_history_record_emits_nothing() {
        let rec = parse(
            r#"{"step_index":1,"source":"SYSTEM","type":"CONVERSATION_HISTORY","status":"DONE"}"#,
        );
        let mut state = AntigravityParserState::default();
        assert!(record_to_live_events(&rec, tid(), &mut state).is_empty());
    }

    #[test]
    fn unknown_cortex_step_type_treated_as_tool_result() {
        // A MODEL record with an unfamiliar tool-result type still surfaces
        // as a ToolCompleted (forward-compat: the type vocabulary grows).
        let started = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"grep_search","args":{}}]}"#,
        );
        let result = parse(
            r#"{"step_index":3,"source":"MODEL","type":"CortexStepGrepSearch","status":"DONE","content":"3 matches"}"#,
        );
        let mut state = AntigravityParserState::default();
        let turn = tid();
        let _ = record_to_live_events(&started, turn, &mut state);
        let events = record_to_live_events(&result, turn, &mut state);
        assert!(matches!(
            &events[0],
            AdapterEvent::ToolCompleted { output, .. } if output == "3 matches"
        ));
    }

    #[test]
    fn is_auth_failure_line_matches_verified_shapes() {
        assert!(is_auth_failure_line(
            "Authentication required. Please visit the URL to log in:"
        ));
        assert!(is_auth_failure_line("Error: authentication timed out."));
        assert!(is_auth_failure_line(
            "  You are not logged into Antigravity."
        ));
        assert!(!is_auth_failure_line("ack"));
        assert!(!is_auth_failure_line("Error: empty prompt."));
    }

    #[test]
    fn first_error_line_finds_error_skips_warning_and_text() {
        let lines = vec![
            "ack".to_owned(),
            "Warning: conversation \"x\" not found.".to_owned(),
            "Error: timed out waiting for response".to_owned(),
        ];
        assert_eq!(
            first_error_line(&lines).as_deref(),
            Some("Error: timed out waiting for response")
        );
    }

    #[test]
    fn first_error_line_none_when_only_warning() {
        // `first_error_line` only flags hard `Error:` lines. A `Warning:`
        // line is not an `Error:` — note that the conversation-not-found
        // warning here is handled separately by the producer's fork-and-heal
        // path, NOT treated as a plain success by this scan.
        let lines = vec![
            "Warning: conversation \"x\" not found.".to_owned(),
            "Hello! I'm Antigravity.".to_owned(),
        ];
        assert!(first_error_line(&lines).is_none());
    }

    // Sanity that the live event types compose with TurnOutcome in the
    // outcome path (the producer builds TurnEnd from these signals).
    #[test]
    fn terminal_answer_detection_drives_completed_classification() {
        let rec = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"ack"}"#,
        );
        let outcome = if rec.is_terminal_answer() {
            TurnOutcome::Completed
        } else {
            TurnOutcome::Failed {
                kind: crate::events::FailureKind::AdapterFailure,
                message: "no answer".to_owned(),
            }
        };
        assert!(matches!(outcome, TurnOutcome::Completed));
    }
}
