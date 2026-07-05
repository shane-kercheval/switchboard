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
//! See `docs/research/archive/antigravity-cli-observed.md` for the ground-truth
//! shapes these types mirror.

use std::collections::VecDeque;

use serde::Deserialize;
use serde_json::Value;

use crate::events::{AdapterEvent, ContentKind, ToolKind, TurnId};

/// One record (line) of `transcript.jsonl`. The fields below are the subset
/// Switchboard consumes; `#[serde]` ignores any additional fields, so the type
/// tolerates the large, growing `type` vocabulary and future field additions.
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
    /// Raw RFC3339 UTC timestamp string the record was written. Kept as a
    /// string (not a typed `DateTime`) so a present-but-unparseable value —
    /// plausible if Antigravity drifts its timestamp format — degrades to a
    /// dropped timestamp rather than failing the whole-record deserialize and
    /// silently losing the user prompt or answer. Hydration parses it
    /// leniently and carries the prior record's timestamp forward on failure
    /// (deterministic, no wall-clock). The live path ignores it (process exit
    /// is the live terminator).
    #[serde(default)]
    pub created_at: Option<String>,
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
    pub(crate) fn is_planner_response(&self) -> bool {
        self.source == "MODEL" && self.record_type == "PLANNER_RESPONSE"
    }

    /// A `MODEL` record that is not a planner response is a tool result
    /// (`RUN_COMMAND`, `VIEW_FILE`, `CortexStep*`...). Its `content` is a
    /// pre-rendered text blob.
    pub(crate) fn is_tool_result(&self) -> bool {
        self.source == "MODEL" && !self.is_planner_response()
    }

    pub(crate) fn tool_result_is_error(&self) -> bool {
        self.status.as_deref() == Some("FAILED")
            || self
                .content
                .as_deref()
                .is_some_and(tool_result_content_is_error)
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

fn tool_result_content_is_error(content: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if matches!(lower.as_str(), "output:" | "stdout:" | "stderr:") {
            break;
        }
        if lower.starts_with("the command failed with exit code:") {
            return true;
        }
    }
    false
}

/// Per-turn parser state. Tracks the FIFO of in-flight tool invocations and
/// early tool results so a result record can be paired with the `ToolStarted`
/// it completes. Antigravity's result records carry no tool id, and observed
/// transcripts can write a result before the planner record that names the
/// tool call, so both sides are buffered by arrival order.
#[derive(Debug, Default)]
pub struct AntigravityParserState {
    pending_tool_ids: VecDeque<PendingToolStart>,
    pending_tool_results: VecDeque<PendingToolResult>,
}

#[derive(Debug)]
struct PendingToolStart {
    tool_use_id: String,
    planner_step: i64,
}

#[derive(Debug)]
struct PendingToolResult {
    step_index: i64,
    output: String,
    is_error: bool,
}

impl AntigravityParserState {
    pub fn unmatched_tool_result_steps(&self) -> Vec<i64> {
        self.pending_tool_results
            .iter()
            .map(|result| result.step_index)
            .collect()
    }
}

/// Map one transcript record to the **live** adapter events it produces.
///
/// Live path: `thinking` → `ContentChunk{Thinking}`, the model's final answer
/// `content` → `ContentChunk{Text}`, tool calls → `ToolStarted`, tool results
/// → `ToolCompleted`. User-input and conversation-history records are skipped
/// (the UI already shows the user's prompt).
///
/// **The transcript — not stdout — is the answer-text source.** `agy`'s stdout
/// drip cannot be trusted for per-turn text: on a resume turn it replays the
/// whole conversation's prior answers (observed in production), so emitting
/// stdout would make each turn's bubble accumulate every earlier answer. The
/// transcript records the completed `PLANNER_RESPONSE` per turn, and the
/// resume cursor isolates only the new turn's records — so it is the clean,
/// per-turn source. This makes the live path emit the same answer text that
/// hydration reconstructs from disk. The cost: the answer lands when its
/// record is written (turn completion) rather than char-streaming; thinking
/// and tool lifecycle still stream live as their records arrive.
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

    // Final answer content (a planner response with text and no tool calls).
    // Assumption (verified against captured transcripts — see the research
    // doc's record-mapping section): a tool-calling `PLANNER_RESPONSE` carries
    // its narration in `thinking` (emitted above), not `content`, and the
    // answer always arrives as a separate no-tool-calls record. So gating on
    // `is_terminal_answer` (no tool calls) drops no visible text. Revisit if a
    // tool-calling record is ever seen with non-empty `content`.
    if rec.is_terminal_answer()
        && let Some(content) = &rec.content
    {
        out.push(AdapterEvent::ContentChunk {
            turn_id,
            kind: ContentKind::Text,
            text: content.clone(),
        });
    }

    if let Some(calls) = &rec.tool_calls {
        for (call_index, call) in calls.iter().enumerate() {
            // Include the call index so two same-name tool calls in one
            // planner record (the array allows it) get distinct ids — a
            // bare `{step}:{name}` would collide and make UI/tool pairing
            // ambiguous.
            let tool_use_id = format!("{}:{}:{}", rec.step_index, call_index, call.name);
            out.push(AdapterEvent::ToolStarted {
                turn_id,
                tool_use_id: tool_use_id.clone(),
                kind: classify_tool_kind(&call.name),
                name: call.name.clone(),
                input: call.args.clone(),
            });
            if let Some(result) =
                pop_plausible_result(&mut state.pending_tool_results, rec.step_index)
            {
                out.push(AdapterEvent::ToolCompleted {
                    turn_id,
                    tool_use_id,
                    output: result.output,
                    is_error: result.is_error,
                });
            } else {
                state.pending_tool_ids.push_back(PendingToolStart {
                    tool_use_id: tool_use_id.clone(),
                    planner_step: rec.step_index,
                });
            }
        }
    }

    if rec.is_tool_result() {
        let is_error = rec.tool_result_is_error();
        let output = rec.content.clone().unwrap_or_default();
        if let Some(pending) = state.pending_tool_ids.front()
            && rec.step_index > pending.planner_step
        {
            let pending = state
                .pending_tool_ids
                .pop_front()
                .expect("front checked above");
            out.push(AdapterEvent::ToolCompleted {
                turn_id,
                tool_use_id: pending.tool_use_id,
                output,
                is_error,
            });
        } else {
            state.pending_tool_results.push_back(PendingToolResult {
                step_index: rec.step_index,
                output,
                is_error,
            });
        }
    }

    out
}

fn pop_plausible_result(
    pending_results: &mut VecDeque<PendingToolResult>,
    planner_step: i64,
) -> Option<PendingToolResult> {
    let idx = pending_results
        .iter()
        .position(|result| result.step_index > planner_step)?;
    pending_results.remove(idx)
}

/// Tool-kind classification. The native tools (`run_command`, `view_file`,
/// `edit_file`, ...) are `Builtin`; `CortexStepMcpTool`-dispatched MCP tools
/// would be `Mcp`, but the transcript records the underlying tool name, not
/// the Cortex step type, so we can't reliably distinguish MCP here yet.
/// Defaults to `Builtin`; refine when an MCP probe pins the name shape.
pub(crate) fn classify_tool_kind(_name: &str) -> ToolKind {
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
        // Thinking first, then the answer text — both from the transcript.
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            AdapterEvent::ContentChunk {
                kind: ContentKind::Thinking,
                text,
                ..
            } if text == "deliberating"
        ));
        assert!(matches!(
            &events[1],
            AdapterEvent::ContentChunk {
                kind: ContentKind::Text,
                text,
                ..
            } if text == "ack"
        ));
    }

    #[test]
    fn planner_response_final_answer_emits_text_chunk() {
        // content-only PLANNER_RESPONSE: the transcript is the answer-text
        // source (stdout replays prior answers on resume and can't be
        // trusted), so the live path emits the answer as a Text chunk.
        let rec = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"ack"}"#,
        );
        let mut state = AntigravityParserState::default();
        let events = record_to_live_events(&rec, tid(), &mut state);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AdapterEvent::ContentChunk { kind: ContentKind::Text, text, .. } if text == "ack"
        ));
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
    fn tool_result_before_planner_response_buffers_and_pairs() {
        let result = parse(
            r#"{"step_index":4,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"The command failed with exit code: 128\nOutput:\nfatal: not a git repository"}"#,
        );
        let started = parse(
            r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"git status\""}}]}"#,
        );
        let mut state = AntigravityParserState::default();
        let turn = tid();

        assert!(
            record_to_live_events(&result, turn, &mut state).is_empty(),
            "early results wait for the planner tool id"
        );
        let events = record_to_live_events(&started, turn, &mut state);

        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            AdapterEvent::ToolStarted { tool_use_id, name, .. }
                if tool_use_id == "3:0:run_command" && name == "run_command"
        ));
        assert!(matches!(
            &events[1],
            AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error: true,
                ..
            } if tool_use_id == "3:0:run_command" && output.contains("fatal")
        ));
    }

    #[test]
    fn implausible_early_tool_result_does_not_attach_to_later_tool() {
        let result = parse(
            r#"{"step_index":2,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"orphan"}"#,
        );
        let started = parse(
            r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{}}]}"#,
        );
        let mut state = AntigravityParserState::default();
        let turn = tid();

        assert!(record_to_live_events(&result, turn, &mut state).is_empty());
        let events = record_to_live_events(&started, turn, &mut state);

        assert_eq!(events.len(), 1, "only the tool start should emit");
        assert!(matches!(&events[0], AdapterEvent::ToolStarted { .. }));
        assert_eq!(state.unmatched_tool_result_steps(), vec![2]);
    }

    #[test]
    fn command_failure_phrase_inside_output_body_is_not_error() {
        let result = parse(
            r#"{"step_index":4,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"Created At: now\nCompleted At: now\nOutput:\nThe command failed with exit code: 128"}"#,
        );
        let started = parse(
            r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{}}]}"#,
        );
        let mut state = AntigravityParserState::default();
        let turn = tid();

        assert!(record_to_live_events(&result, turn, &mut state).is_empty());
        let events = record_to_live_events(&started, turn, &mut state);

        assert!(matches!(
            &events[1],
            AdapterEvent::ToolCompleted {
                is_error: false,
                ..
            }
        ));
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
    fn tool_result_failed_command_text_sets_is_error() {
        let result = parse(
            r#"{"step_index":3,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"The command failed with exit code: 1\nOutput:\nboom"}"#,
        );
        let started = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{}}]}"#,
        );
        let mut state = AntigravityParserState::default();
        let turn = tid();

        assert!(record_to_live_events(&result, turn, &mut state).is_empty());
        let events = record_to_live_events(&started, turn, &mut state);
        assert!(matches!(
            &events[1],
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
