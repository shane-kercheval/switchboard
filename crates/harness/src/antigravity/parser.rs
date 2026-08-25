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
//! - **the conversation transcript** carries one record per "step": user input,
//!   model planner responses (with `thinking` + `tool_calls`), and tool
//!   results (`RUN_COMMAND`, `VIEW_FILE`, other `CortexStep*` types). It
//!   has no top-level metadata record and no terminal "turn complete"
//!   record — the conversation UUID lives in the directory name, and the
//!   turn terminates when the `agy` process exits. Current versions write a
//!   lossless `transcript_full.jsonl` plus a compact `transcript.jsonl` fallback.
//!
//! See `docs/research/archive/antigravity-cli-observed.md` for the ground-truth
//! shapes these types mirror.

use std::collections::VecDeque;

use serde::Deserialize;
use serde_json::Value;

use crate::events::{AdapterEvent, ContentKind, TurnId};

/// One record (line) of an Antigravity transcript. The fields below are the subset
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
    pub error: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// A tool invocation inside a `PLANNER_RESPONSE.tool_calls[]`. Compact
/// transcript args are pre-stringified; full-transcript args retain native
/// JSON types. The raw object remains the tool `input` for provenance.
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

    /// A `MODEL` record that is not a planner response is a normal tool result
    /// (`RUN_COMMAND`, `VIEW_FILE`, `CortexStep*`...). Antigravity instead
    /// writes invalid tool invocations as `SYSTEM` / `ERROR_MESSAGE` records;
    /// those are tool results only when their payload explicitly identifies an
    /// invalid tool call. Other system errors, such as quota exhaustion, are
    /// turn-level and must not consume a pending tool id.
    pub(crate) fn is_tool_result(&self) -> bool {
        (self.source == "MODEL" && !self.is_planner_response()) || self.is_invalid_tool_call_error()
    }

    pub(crate) fn tool_result_is_error(&self) -> bool {
        self.is_invalid_tool_call_error()
            || self.status.as_deref() == Some("FAILED")
            || self
                .content
                .as_deref()
                .is_some_and(tool_result_content_is_error)
    }

    pub(crate) fn tool_result_output(&self) -> String {
        self.error
            .as_deref()
            .filter(|error| !error.trim().is_empty())
            .or(self.content.as_deref())
            .unwrap_or_default()
            .to_owned()
    }

    fn is_invalid_tool_call_error(&self) -> bool {
        self.source == "SYSTEM"
            && self.record_type == "ERROR_MESSAGE"
            && [self.error.as_deref(), self.content.as_deref()]
                .into_iter()
                .flatten()
                .any(|text| text.to_ascii_lowercase().contains("invalid tool call"))
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
    /// Tool failures seen on the control stream, held unpaired until the final
    /// drain. Kept apart from `pending_tool_results` on purpose — see
    /// [`AntigravityParserState::record_stream_tool_failure`].
    stream_tool_failures: VecDeque<StreamToolFailure>,
}

#[derive(Debug)]
struct StreamToolFailure {
    step_index: i64,
    message: Option<String>,
}

#[derive(Debug)]
struct PendingToolStart {
    tool_use_id: String,
    /// The transcript `step_index` this call's result will occupy, if it
    /// produces one: `planner_step + 1 + call_index`.
    ///
    /// **Capture-established, not assumed** (agy 1.1.19, 2026-08-24). Ten
    /// planner records across five transcripts agree, covering single-call,
    /// multi-call, success, invisible failure, and MCP wrappers — the two
    /// `tool-failure-*.transcript.jsonl` fixtures are the probes that forced
    /// the failure shapes, and `tool-vocabulary` supplies the two multi-call
    /// records (planner 26 → 27,28 and planner 30 → 31,32).
    ///
    /// Replaces an arrival-order (FIFO) rule that was wrong in two ways. It
    /// mispaired whenever a call produced no record — agy writes nothing for a
    /// rejected tool — handing the *next* tool's result to the silent one. And
    /// it was sensitive to file order, which agy does not guarantee: in
    /// `tool-vocabulary.transcript.jsonl` the result at step 27 is written
    /// *before* its planner at step 26. Matching on this number is immune to
    /// both.
    expected_result_step: i64,
}

#[derive(Debug)]
struct PendingToolResult {
    step_index: i64,
    output: String,
    is_error: bool,
}

/// Shown for a tool call that ended without a recorded result.
///
/// **States only what is known.** An earlier wording said Antigravity
/// "rejected" the call, which claims a cause this constant cannot vouch for:
/// [`AntigravityParserState::close_pending_tools`] closes *every* still-open
/// tool at turn end, including one with no stream failure recorded against it
/// at all — a turn cut short, say. All that is actually established in the
/// general case is the absence of a result.
///
/// Preferred over an empty output, which would read as "succeeded with nothing
/// to show". Where the stream *did* supply a message it is used verbatim
/// instead, being more specific and drift-resilient.
///
/// **Shared with hydration on purpose.** Reopening a project cannot recover the
/// stream's per-tool message — it was never written to disk — so the reopen
/// path shows this same text. Both paths referencing one constant is what keeps
/// the two renderings of the same failure from drifting apart; do not author a
/// second copy of this string.
pub(crate) const MISSING_TOOL_RESULT_OUTPUT: &str =
    "Antigravity did not record a result for this tool call.";

impl AntigravityParserState {
    /// Record a tool failure observed on the **stream** rather than the
    /// transcript. Deliberately does **not** pair it yet.
    ///
    /// **Why nothing is paired here.** Pairing needs to know which pending tool
    /// the failure belongs to, and mid-turn there is no sound way to know.
    /// stdout is read as lines arrive while the transcript is polled every
    /// ~100ms, so a *later* tool's failure routinely reaches this queue before
    /// an *earlier* tool's planner record has been tailed. Every mid-turn rule
    /// therefore measures polling lag rather than identity — including "pair
    /// when exactly one tool is currently pending", which looks safe and is
    /// not: with B's failure queued and only A tailed so far, A is the sole
    /// candidate and would wrongly take B's error. Feeding these into
    /// `pending_tool_results` (as an earlier revision did) is worse still,
    /// because an unrelated planner's `pop_plausible_result` can then claim
    /// one — that rule is only safe for the strictly-ordered transcript source
    /// it was written for.
    ///
    /// Misattribution is the failure to avoid: it shows a tool that succeeded
    /// as failed, hides the real failure's message, and strands the tool that
    /// actually failed. A dangling tool for the rest of the turn is strictly
    /// better, and [`Self::close_pending_tools`] resolves it at the one moment
    /// the picture is provably complete.
    pub(crate) fn record_stream_tool_failure(&mut self, step_index: i64, message: Option<String>) {
        self.stream_tool_failures.push_back(StreamToolFailure {
            step_index,
            message,
        });
    }

    /// Resolve every unfinished tool at the **final post-exit drain**, once the
    /// process has exited and the transcript is fully flushed.
    ///
    /// That boundary is what makes this sound: anything still pending has no
    /// transcript result, and a tool with no result did not succeed. So the
    /// pending set *is* the failed set, and no ordering question remains.
    ///
    /// Each stream failure is attributed to the tool whose
    /// [`PendingToolStart::expected_result_step`] it names — the same
    /// capture-established relationship used for ordinary results, so several
    /// failures in one turn each land on their own call rather than collapsing
    /// to generic copy. A failure naming a step no pending tool expects is
    /// **not** guessed onto anything; it is returned for diagnostics, and any
    /// tool left without a message closes with [`MISSING_TOOL_RESULT_OUTPUT`].
    ///
    /// Returns the events to emit plus any `(step_index, message)` pairs that
    /// could not be attributed. The step index rides along because it is the
    /// raw material for re-deriving the invariant if agy's numbering ever
    /// shifts.
    pub(crate) fn close_pending_tools(
        &mut self,
        turn_id: TurnId,
    ) -> (Vec<AdapterEvent>, Vec<(i64, String)>) {
        let mut events = Vec::new();
        let mut unattributed = Vec::new();

        for failure in self.stream_tool_failures.drain(..) {
            match self
                .pending_tool_ids
                .iter()
                .position(|pending| pending.expected_result_step == failure.step_index)
            {
                Some(index) => {
                    let pending = self
                        .pending_tool_ids
                        .remove(index)
                        .expect("index from position");
                    events.push(AdapterEvent::ToolCompleted {
                        turn_id,
                        tool_use_id: pending.tool_use_id,
                        output: failure
                            .message
                            .unwrap_or_else(|| MISSING_TOOL_RESULT_OUTPUT.to_owned()),
                        is_error: true,
                    });
                }
                None => {
                    if let Some(message) = failure.message {
                        unattributed.push((failure.step_index, message));
                    }
                }
            }
        }

        // Whatever remains had no result and no failure naming it — close it
        // too, stating only what is known.
        while let Some(pending) = self.pending_tool_ids.pop_front() {
            events.push(AdapterEvent::ToolCompleted {
                turn_id,
                tool_use_id: pending.tool_use_id,
                output: MISSING_TOOL_RESULT_OUTPUT.to_owned(),
                is_error: true,
            });
        }
        (events, unattributed)
    }

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
    record_to_live_events_with_encoding(
        rec,
        turn_id,
        state,
        super::facets::ArgumentEncoding::CompactJsonStrings,
    )
}

pub(crate) fn record_to_live_events_with_encoding(
    rec: &TranscriptRecord,
    turn_id: TurnId,
    state: &mut AntigravityParserState,
    encoding: super::facets::ArgumentEncoding,
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
            let (kind, facet) = match encoding {
                super::facets::ArgumentEncoding::CompactJsonStrings => {
                    super::facets::classify_antigravity_tool(&call.name, &call.args)
                }
                super::facets::ArgumentEncoding::Native => {
                    super::facets::classify_antigravity_tool_with_encoding(
                        &call.name, &call.args, encoding,
                    )
                }
            };
            out.push(AdapterEvent::ToolStarted {
                turn_id,
                tool_use_id: tool_use_id.clone(),
                kind,
                facet,
                name: call.name.clone(),
                input: call.args.clone(),
            });
            let expected = expected_result_step(rec.step_index, call_index);
            if let Some(result) = take_result_for_step(&mut state.pending_tool_results, expected) {
                out.push(AdapterEvent::ToolCompleted {
                    turn_id,
                    tool_use_id,
                    output: result.output,
                    is_error: result.is_error,
                });
            } else {
                state.pending_tool_ids.push_back(PendingToolStart {
                    tool_use_id: tool_use_id.clone(),
                    expected_result_step: expected,
                });
            }
        }
    }

    if rec.is_tool_result() {
        let is_error = rec.tool_result_is_error();
        let output = rec.tool_result_output();
        // Attach to the call that *expects* this step, never to whichever call
        // happens to be oldest. The old arrival-order rule ("first pending tool
        // whose planner step is lower") silently swapped identities the moment
        // any call produced no record: with A at planner 2 (rejected, nothing
        // written) and B at planner 4, B's result at step 5 satisfied `> 2` and
        // was emitted under A's id — A rendered as a success carrying B's
        // output while B was later closed as failed carrying A's error. See
        // `expected_result_step` for the evidence behind the replacement.
        if let Some(index) = state
            .pending_tool_ids
            .iter()
            .position(|pending| pending.expected_result_step == rec.step_index)
        {
            let pending = state
                .pending_tool_ids
                .remove(index)
                .expect("index from position");
            out.push(AdapterEvent::ToolCompleted {
                turn_id,
                tool_use_id: pending.tool_use_id,
                output,
                is_error,
            });
        } else {
            // The call this belongs to has not been tailed yet (agy does not
            // guarantee file order — a result can precede its planner record).
            // Buffered for the planner side to claim by the same exact step.
            state.pending_tool_results.push_back(PendingToolResult {
                step_index: rec.step_index,
                output,
                is_error,
            });
        }
    }

    out
}

/// The transcript step a tool call's result occupies, if it produces one.
///
/// See [`PendingToolStart::expected_result_step`] for the captures this rests
/// on and why it replaced an arrival-order rule.
pub(crate) fn expected_result_step(planner_step: i64, call_index: usize) -> i64 {
    planner_step + 1 + i64::try_from(call_index).unwrap_or(i64::MAX)
}

/// Take a buffered result that belongs to exactly this step.
///
/// Deliberately **no fallback to the nearest or oldest candidate**. A result
/// with no matching expectation means the step relationship this adapter
/// relies on did not hold, and guessing is precisely the wrong-provenance bug
/// this replaced: attaching it anyway would show one tool's output under
/// another's name, confidently. It is left unclaimed instead, and the tool
/// that expected a result it never got is closed as failed at turn end.
fn take_result_for_step(
    pending_results: &mut VecDeque<PendingToolResult>,
    expected_step: i64,
) -> Option<PendingToolResult> {
    let idx = pending_results
        .iter()
        .position(|result| result.step_index == expected_step)?;
    pending_results.remove(idx)
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
    use crate::events::{ToolKind, TurnOutcome};
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
    fn invalid_tool_call_errors_complete_the_correct_pending_tools() {
        let records = [
            r#"{"step_index":8,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"view_file","args":{"AbsolutePath":"\"/tmp/missing-read\""}}]}"#,
            r#"{"step_index":9,"source":"SYSTEM","type":"ERROR_MESSAGE","status":"DONE","error":"There was a problem parsing the tool call. Error Message: model output error: invalid tool call error (invalid_args) failed to read file: no such file","content":"Created At: now\nError invalid tool call: timestamped read failure"}"#,
            r#"{"step_index":10,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"replace_file_content","args":{"TargetFile":"\"/tmp/missing-edit\""}}]}"#,
            r#"{"step_index":11,"source":"SYSTEM","type":"ERROR_MESSAGE","status":"DONE","error":"There was a problem parsing the tool call. Error Message: model output error: invalid tool call error (invalid_args) target file does not exist","content":"Created At: now\nError invalid tool call: timestamped edit failure"}"#,
            r#"{"step_index":12,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"missing-command\""}}]}"#,
            r#"{"step_index":13,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"The command failed with exit code: 127\nOutput:\nzsh: command not found: missing-command"}"#,
        ];
        let mut state = AntigravityParserState::default();
        let turn = tid();
        let events: Vec<AdapterEvent> = records
            .iter()
            .flat_map(|line| record_to_live_events(&parse(line), turn, &mut state))
            .collect();

        let completions: Vec<(&str, &str, bool)> = events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::ToolCompleted {
                    tool_use_id,
                    output,
                    is_error,
                    ..
                } => Some((tool_use_id.as_str(), output.as_str(), *is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(completions.len(), 3);
        assert_eq!(completions[0].0, "8:0:view_file");
        assert!(completions[0].1.contains("failed to read file"));
        assert_eq!(completions[1].0, "10:0:replace_file_content");
        assert!(completions[1].1.contains("target file does not exist"));
        assert_eq!(completions[2].0, "12:0:run_command");
        assert!(completions[2].1.contains("command not found"));
        assert!(completions.iter().all(|(_, _, is_error)| *is_error));
        assert!(state.pending_tool_ids.is_empty());
    }

    #[test]
    fn mcp_wrappers_pair_normal_and_invalid_results_to_their_own_calls() {
        // Step numbers follow the real relationship (`planner + 1 + call_index`):
        // planner 8 → result 9, planner 10 → result 11. An earlier revision of
        // this fixture used adjacent planners at 8 and 9, a shape agy cannot
        // actually produce — a tool-calling planner reserves the steps its
        // results occupy, and no captured transcript has two planners closer
        // than that. It only passed because pairing was arrival-ordered.
        let records = [
            r#"{"step_index":8,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"call_mcp_tool","args":{"ServerName":"\"notes_alias\"","ToolName":"\"edit_content\"","Arguments":"{\"id\":\"note-example\",\"type\":\"note\",\"old_str\":\"before\",\"new_str\":\"after\"}"}}]}"#,
            r#"{"step_index":9,"source":"MODEL","type":"CortexStepMcpTool","status":"DONE","content":"edit ok"}"#,
            r#"{"step_index":10,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"call_mcp_tool","args":{"ServerName":"\"prompts_alias\"","ToolName":"\"create_prompt\"","Arguments":"{\"name\":\"sample-prompt\",\"content\":\"Prompt body\"}"}}]}"#,
            r#"{"step_index":11,"source":"SYSTEM","type":"ERROR_MESSAGE","status":"DONE","error":"There was a problem parsing the tool call. Error Message: invalid tool call error (invalid_args) creation rejected"}"#,
        ];
        let mut state = AntigravityParserState::default();
        let turn = tid();
        let events: Vec<_> = records
            .iter()
            .flat_map(|line| record_to_live_events(&parse(line), turn, &mut state))
            .collect();

        let starts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::ToolStarted {
                    tool_use_id,
                    kind,
                    facet,
                    name,
                    ..
                } => Some((tool_use_id, kind, facet, name)),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 2);
        assert!(starts.iter().all(|(_, kind, _, _)| **kind == ToolKind::Mcp));
        assert!(
            starts
                .iter()
                .all(|(_, _, _, name)| *name == "call_mcp_tool")
        );
        assert!(matches!(
            starts[0].2,
            crate::facets::ToolFacet::Mcp {
                mutation: Some(mutation),
                ..
            } if matches!(mutation.as_ref(), crate::facets::McpMutation::TextEdit { .. })
        ));
        assert!(matches!(
            starts[1].2,
            crate::facets::ToolFacet::Mcp {
                mutation: Some(mutation),
                ..
            } if matches!(mutation.as_ref(), crate::facets::McpMutation::TextCreation { .. })
        ));

        let completions: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::ToolCompleted {
                    tool_use_id,
                    output,
                    is_error,
                    ..
                } => Some((tool_use_id, output, is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(completions.len(), 2);
        assert_eq!(completions[0].0, "8:0:call_mcp_tool");
        assert_eq!(completions[0].1, "edit ok");
        assert!(!completions[0].2);
        assert_eq!(completions[1].0, "10:0:call_mcp_tool");
        assert!(completions[1].1.contains("creation rejected"));
        assert!(completions[1].2);
        assert!(state.pending_tool_ids.is_empty());
    }

    #[test]
    fn turn_level_system_error_does_not_complete_a_pending_tool() {
        let started = parse(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{}}]}"#,
        );
        let quota = parse(
            r#"{"step_index":3,"source":"SYSTEM","type":"ERROR_MESSAGE","status":"DONE","error":"RESOURCE_EXHAUSTED (code 429): Individual quota reached."}"#,
        );
        let mut state = AntigravityParserState::default();
        let turn = tid();

        let _ = record_to_live_events(&started, turn, &mut state);
        assert!(record_to_live_events(&quota, turn, &mut state).is_empty());
        assert_eq!(state.pending_tool_ids.len(), 1);
        assert!(state.pending_tool_results.is_empty());
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

    /// The exact ordering that made every mid-turn pairing rule unsound: B's
    /// failure reaches the stream before *any* transcript record is tailed, so
    /// when planner A is finally seen, A is the only pending tool. A rule that
    /// paired "when exactly one tool is pending" would hand A the error that
    /// belongs to B — showing a tool that succeeded as failed and stranding
    /// the one that actually failed.
    #[test]
    fn stream_failure_arriving_before_any_transcript_record_never_lands_on_the_wrong_tool() {
        let turn_id = Uuid::now_v7();
        let mut state = AntigravityParserState::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        // Stream: tool B (step 5) failed. Nothing tailed yet.
        state.record_stream_tool_failure(5, Some("B blew up".to_owned()));

        // Transcript catches up: planner A (step 2), A's result (step 3),
        // planner B (step 4). A must keep its own successful output.
        for line in [
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"run_command","args":{}}]}"#,
            r#"{"step_index":3,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"A ok"}"#,
            r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"view_file","args":{}}]}"#,
        ] {
            let rec: TranscriptRecord = serde_json::from_str(line).unwrap();
            for event in record_to_live_events(&rec, turn_id, &mut state) {
                let _ = tx.send(event);
            }
        }

        let mut completions: Vec<(String, String, bool)> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } = event
            {
                completions.push((tool_use_id, output, is_error));
            }
        }
        assert_eq!(
            completions.len(),
            1,
            "only A resolves mid-turn: {completions:?}"
        );
        assert!(
            completions[0].0.starts_with("2:"),
            "A's own id: {completions:?}"
        );
        assert_eq!(completions[0].1, "A ok", "A keeps its own output");
        assert!(
            !completions[0].2,
            "A succeeded and must not be marked failed"
        );

        // Final drain: B is the only tool left, and the only recorded failure,
        // so its message is attributable.
        let (closed, unattributed) = state.close_pending_tools(turn_id);
        assert!(unattributed.is_empty(), "{unattributed:?}");
        assert_eq!(closed.len(), 1);
        match &closed[0] {
            AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } => {
                assert!(tool_use_id.starts_with("4:"), "B's id: {tool_use_id}");
                assert_eq!(output, "B blew up");
                assert!(*is_error);
            }
            other => panic!("expected B's failure; got {other:?}"),
        }
    }

    /// The identity swap that arrival-order pairing produced, pinned so it
    /// cannot return. Shape taken from a real capture
    /// (`tool-failure-cross-planner.transcript.jsonl`): A is rejected and agy
    /// writes nothing for it, so under the old rule B's result at step 5
    /// satisfied `> 2` and was emitted under **A's** id — A rendered as a
    /// success carrying B's output, and B was then closed as failed carrying
    /// A's error. Both tools wrong, both confidently.
    #[test]
    fn a_rejected_tool_never_absorbs_the_next_tools_result() {
        let turn_id = Uuid::now_v7();
        let mut state = AntigravityParserState::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let feed = |line: &str,
                    state: &mut AntigravityParserState,
                    tx: &tokio::sync::mpsc::UnboundedSender<AdapterEvent>| {
            let rec: TranscriptRecord = serde_json::from_str(line).unwrap();
            for event in record_to_live_events(&rec, turn_id, state) {
                let _ = tx.send(event);
            }
        };

        // planner(2) = A (view_file). Its result step, 3, is never written.
        feed(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"view_file","args":{}}]}"#,
            &mut state,
            &tx,
        );
        state.record_stream_tool_failure(3, Some("A REAL ERROR".to_owned()));
        // planner(4) = B (run_command), result at 5.
        feed(
            r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"run_command","args":{}}]}"#,
            &mut state,
            &tx,
        );
        feed(
            r#"{"step_index":5,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"B REAL OUTPUT"}"#,
            &mut state,
            &tx,
        );

        let mut got: Vec<(String, String, bool)> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } = event
            {
                got.push((tool_use_id, output, is_error));
            }
        }
        // B resolves live, on its own id, with its own output.
        assert_eq!(
            got,
            vec![(
                "4:0:run_command".to_owned(),
                "B REAL OUTPUT".to_owned(),
                false
            )]
        );

        // A closes at turn end with its own error — never B's output.
        let (closed, unattributed) = state.close_pending_tools(turn_id);
        assert!(unattributed.is_empty(), "{unattributed:?}");
        assert_eq!(closed.len(), 1);
        match &closed[0] {
            AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "2:0:view_file");
                assert_eq!(output, "A REAL ERROR");
                assert!(*is_error);
            }
            other => panic!("expected A's failure; got {other:?}"),
        }
    }

    /// The same-planner shape from `tool-failure-same-planner.transcript.jsonl`:
    /// two calls in one record, the first rejected. The surviving result at
    /// step 4 belongs to call index 1, which arrival-order pairing would have
    /// handed to call index 0.
    #[test]
    fn two_calls_in_one_record_attribute_by_call_index() {
        let turn_id = Uuid::now_v7();
        let mut state = AntigravityParserState::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let planner: TranscriptRecord = serde_json::from_str(r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"view_file","args":{}},{"name":"run_command","args":{}}]}"#).unwrap();
        for event in record_to_live_events(&planner, turn_id, &mut state) {
            let _ = tx.send(event);
        }
        state.record_stream_tool_failure(3, Some("view_file rejected".to_owned()));
        let result: TranscriptRecord = serde_json::from_str(r#"{"step_index":4,"source":"MODEL","type":"GENERIC","status":"DONE","content":"echo output"}"#).unwrap();
        for event in record_to_live_events(&result, turn_id, &mut state) {
            let _ = tx.send(event);
        }

        let mut got: Vec<(String, String, bool)> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } = event
            {
                got.push((tool_use_id, output, is_error));
            }
        }
        assert_eq!(
            got,
            vec![(
                "2:1:run_command".to_owned(),
                "echo output".to_owned(),
                false
            )],
            "step 4 belongs to call index 1, not index 0"
        );

        let (closed, _) = state.close_pending_tools(turn_id);
        match &closed[0] {
            AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                ..
            } => {
                assert_eq!(tool_use_id, "2:0:view_file");
                assert_eq!(output, "view_file rejected");
            }
            other => panic!("expected call 0's failure; got {other:?}"),
        }
    }

    #[test]
    fn close_pending_tools_attributes_several_failures_to_their_own_calls() {
        // Two tools, two failures. Each failure names the step its own call
        // expects, so both land exactly — no collapsing to generic copy, and
        // no possibility of showing one tool's error under the other's name.
        let turn_id = Uuid::now_v7();
        let mut state = AntigravityParserState::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        for line in [
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"view_file","args":{}}]}"#,
            r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"run_command","args":{}}]}"#,
        ] {
            let rec: TranscriptRecord = serde_json::from_str(line).unwrap();
            for event in record_to_live_events(&rec, turn_id, &mut state) {
                let _ = tx.send(event);
            }
        }
        while rx.try_recv().is_ok() {}

        state.record_stream_tool_failure(3, Some("view_file blew up".to_owned()));
        state.record_stream_tool_failure(5, Some("run_command blew up".to_owned()));

        let (closed, unattributed) = state.close_pending_tools(turn_id);
        assert!(unattributed.is_empty(), "{unattributed:?}");
        let mut got: Vec<(String, String)> = closed
            .into_iter()
            .filter_map(|event| match event {
                AdapterEvent::ToolCompleted {
                    tool_use_id,
                    output,
                    is_error: true,
                    ..
                } => Some((tool_use_id, output)),
                _ => None,
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("2:0:view_file".to_owned(), "view_file blew up".to_owned()),
                (
                    "4:0:run_command".to_owned(),
                    "run_command blew up".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn close_pending_tools_does_not_guess_a_failure_onto_an_unexpecting_tool() {
        // A failure naming a step no pending call expects means the step
        // relationship did not hold. Attaching it anyway is the wrong-provenance
        // bug; the tool still closes as failed, but with neutral copy, and the
        // orphaned message is handed back for diagnostics.
        let turn_id = Uuid::now_v7();
        let mut state = AntigravityParserState::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let rec: TranscriptRecord = serde_json::from_str(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"view_file","args":{}}]}"#,
        )
        .unwrap();
        for event in record_to_live_events(&rec, turn_id, &mut state) {
            let _ = tx.send(event);
        }
        while rx.try_recv().is_ok() {}

        // Expects step 3; the failure names 99.
        state.record_stream_tool_failure(99, Some("belongs to nothing here".to_owned()));

        let (closed, unattributed) = state.close_pending_tools(turn_id);
        assert_eq!(
            unattributed,
            vec![(99, "belongs to nothing here".to_owned())]
        );
        assert_eq!(closed.len(), 1);
        match &closed[0] {
            AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "2:0:view_file");
                assert!(*is_error);
                assert_eq!(output, MISSING_TOOL_RESULT_OUTPUT);
            }
            other => panic!("expected ToolCompleted; got {other:?}"),
        }
    }

    #[test]
    fn close_pending_tools_uses_authored_copy_when_the_stream_carried_no_message() {
        let turn_id = Uuid::now_v7();
        let mut state = AntigravityParserState::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let rec: TranscriptRecord = serde_json::from_str(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"view_file","args":{}}]}"#,
        )
        .unwrap();
        for event in record_to_live_events(&rec, turn_id, &mut state) {
            let _ = tx.send(event);
        }
        while rx.try_recv().is_ok() {}

        state.record_stream_tool_failure(3, None);
        let (closed, unattributed) = state.close_pending_tools(turn_id);
        assert!(unattributed.is_empty());
        match &closed[0] {
            AdapterEvent::ToolCompleted { output, .. } => {
                assert_eq!(output, MISSING_TOOL_RESULT_OUTPUT);
            }
            other => panic!("expected ToolCompleted; got {other:?}"),
        }
    }

    #[test]
    fn close_pending_tools_closes_a_tool_with_no_recorded_failure_at_all() {
        // A tool left open with zero stream evidence — a turn cut short, say.
        // It must still close (nothing more can arrive), but the copy may not
        // claim a rejection, because none was observed.
        let turn_id = Uuid::now_v7();
        let mut state = AntigravityParserState::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let rec: TranscriptRecord = serde_json::from_str(
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"run_command","args":{}}]}"#,
        )
        .unwrap();
        for event in record_to_live_events(&rec, turn_id, &mut state) {
            let _ = tx.send(event);
        }
        while rx.try_recv().is_ok() {}

        let (closed, unattributed) = state.close_pending_tools(turn_id);
        assert!(unattributed.is_empty(), "nothing was recorded to attribute");
        assert_eq!(closed.len(), 1, "the tool must not be left spinning");
        match &closed[0] {
            AdapterEvent::ToolCompleted {
                output, is_error, ..
            } => {
                assert!(*is_error);
                assert_eq!(output, MISSING_TOOL_RESULT_OUTPUT);
                assert!(
                    !output.to_ascii_lowercase().contains("rejected"),
                    "must not assert a cause that was never observed: {output}"
                );
            }
            other => panic!("expected ToolCompleted; got {other:?}"),
        }
    }

    #[test]
    fn close_pending_tools_is_a_no_op_when_every_tool_resolved() {
        let turn_id = Uuid::now_v7();
        let mut state = AntigravityParserState::default();
        assert_eq!(state.close_pending_tools(turn_id).0.len(), 0);
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
