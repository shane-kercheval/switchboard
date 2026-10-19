//! Parser for Antigravity's `transcript.jsonl` records and for the
//! `agy -p` stdout error/auth signals.
//!
//! Antigravity's structured stream (`--output-format stream-json`, agy 1.1.8)
//! is a **control channel only** — it carries no thinking text, so it never
//! supplies displayed content; see [`super::stream`]. Three parseable surfaces
//! exist:
//!
//! - **stdout** carries the model's final answer text (server-side "drip")
//!   under the pre-1.1.8 text mode, and the NDJSON control stream otherwise.
//!   Its answer text is unusable as the content source regardless, because it
//!   replays prior answers on resume.
//! - **stderr** carries the `Error:` / `Warning:` / `Authentication required`
//!   lines. agy 1.1.x moved these off stdout, and they must be captured **as
//!   drained** rather than read back from the bounded tail, which front-evicts.
//!   Outcome classification is signal-based and never gates on the exit status:
//!   exit behavior varies by failure shape (timeout and empty prompt exit 1, an
//!   exactly-a-flag-token prompt exits 2) but the full map is unprobed.
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

use std::collections::{BTreeMap, VecDeque};

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
    /// (`RUN_COMMAND`, `VIEW_FILE`, `GENERIC`, `CortexStep*`...). agy 1.1.2
    /// instead wrote invalid tool invocations as `SYSTEM` / `ERROR_MESSAGE`
    /// records; those are tool results only when their payload explicitly
    /// identifies an invalid tool call. Other system errors, such as quota
    /// exhaustion, are turn-level and must not consume a pending tool id.
    pub(crate) fn is_tool_result(&self) -> bool {
        (self.source == "MODEL" && !self.is_planner_response()) || self.is_invalid_tool_call_error()
    }

    /// What a tool result says about failure, read before it is paired with
    /// its call; [`ResultFailure::is_error_for`] turns it into the verdict.
    /// Each `agy` generation has written a failed tool differently, and all of
    /// them are still read:
    ///
    /// - 1.1.2: a rejected call as `SYSTEM` / `ERROR_MESSAGE` naming an
    ///   invalid tool call, with `status:"DONE"`.
    /// - 1.1.19: a rejected call wrote no record at all; only the stream
    ///   reported it (see [`AntigravityParserState::close_pending_tools`]).
    /// - 1.4.2 (probed 2026-10-19; the 1.2.4 changelog's "terminal `ERROR`
    ///   steps" fix is the likely origin): a rejected call as an ordinary
    ///   `MODEL` result with `status:"ERROR"` and the reason in `error`, e.g.
    ///   a missing-file `view_file` or a missing-target `replace_file_content`.
    ///
    /// `status:"FAILED"` predates these captures and is kept. A command that
    /// ran and exited non-zero carries no structured signal in any version, so
    /// it is read from the record's prose by [`tool_result_content_is_error`].
    pub(crate) fn tool_result_failure(&self) -> ResultFailure {
        ResultFailure {
            structured: self.is_invalid_tool_call_error()
                || matches!(self.status.as_deref(), Some("FAILED" | "ERROR")),
            nonzero_exit_header: self
                .content
                .as_deref()
                .is_some_and(tool_result_content_is_error),
        }
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
    /// Used for outcome classification. It is not the only terminal signal —
    /// the control stream's `result` event carries agy's own verdict — but it
    /// is the one that proves the turn produced *content*, which is why a
    /// stream success without it still fails the turn loud.
    pub fn is_terminal_answer(&self) -> bool {
        self.is_planner_response()
            && self.tool_calls.as_ref().is_none_or(Vec::is_empty)
            && self.content.as_ref().is_some_and(|c| !c.trim().is_empty())
    }
}

/// The one tool whose results report an exit code. **Failed-command detection
/// is tied to this name:** at agy 1.4.2 every result record has
/// `type:"GENERIC"`, so only the paired call says a result came from a
/// command. If `agy` renamed it, failed commands would read as successes again
/// (`live_antigravity_failing_command_completes_as_tool_error` fails on that).
/// A command that ran in the background and whose exit is reported through a
/// separate status tool is not covered; none has been observed.
const COMMAND_TOOL: &str = "run_command";

/// What a tool-result record says about failure, before it is paired with its
/// call. A result can be read before the planner record that names its call,
/// and part of the verdict depends on that call, so the record's two signals
/// are kept apart until [`Self::is_error_for`] combines them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResultFailure {
    /// A failure the record states whatever the tool: `status:"FAILED"` or
    /// `"ERROR"`, or the 1.1.2 invalid-tool-call shape.
    structured: bool,
    /// The header says a command exited non-zero. A failure only for a
    /// command call.
    nonzero_exit_header: bool,
}

impl ResultFailure {
    /// The verdict for this result once paired with a call named `tool_name`.
    /// **The one place a tool result's failure is decided:** the live parser's
    /// two pairing points and hydration's two all call it, so the live and
    /// reopened views of a tool cannot disagree.
    pub(crate) fn is_error_for(self, tool_name: &str) -> bool {
        self.structured || (self.nonzero_exit_header && tool_name == COMMAND_TOOL)
    }
}

/// Whether a command result's header says the command exited non-zero.
///
/// **A substring match on `agy`'s prose**, because the record has no exit-code
/// field and the stream reports a failed command as `DONE`. Two wordings are
/// known, and both are read so older builds keep working:
///
/// - `The command failed with exit code: 127` (captured @ 1.1.2).
/// - `The command exited with code 1.` (captured @ 1.4.2; every non-zero
///   result on the developer's machine since 2026-08-24, agy 1.1.19, uses it).
///   The same sentence with code 0 is a success.
///
/// Only the header is scanned: the loop stops at the `Output:` marker, so a
/// command that prints one of these sentences is not misread. And the result
/// counts as failed only when its call is [`COMMAND_TOOL`] (see
/// [`ResultFailure::is_error_for`]), so a web page or MCP reply that quotes the
/// sentence is not misread either. A third rewording would read as success
/// again; `live_antigravity_failing_command_completes_as_tool_error` is the
/// guard.
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
        if let Some(rest) = lower.strip_prefix("the command exited with code ") {
            return exit_code_is_nonzero(rest);
        }
    }
    false
}

/// Reads the exit code at the start of `rest` (`"1."` → 1). A code that does
/// not parse is not called a failure: the sentence was recognized but its
/// number was not, and guessing would mark successes as failed.
fn exit_code_is_nonzero(rest: &str) -> bool {
    let code: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    code.parse::<i64>().is_ok_and(|code| code != 0)
}

/// Per-turn parser state. Tracks in-flight tool invocations and early tool
/// results so a result record can be paired with the `ToolStarted` it
/// completes — by **exact expected step**, never arrival order (see
/// `expected_result_step`). Antigravity's result records carry no tool id, and
/// observed transcripts can write a result before the planner record that names
/// the tool call, so both sides are buffered until their expected step is
/// claimed. Both queues still *store* entries in the order they are seen —
/// only the **match** is exact, never the buffering.
#[derive(Debug, Default)]
pub struct AntigravityParserState {
    pending_tool_ids: VecDeque<PendingToolStart>,
    pending_tool_results: VecDeque<PendingToolResult>,
    /// Tool failures seen on the control stream, held unpaired until the final
    /// drain. Kept apart from `pending_tool_results` on purpose — see
    /// [`AntigravityParserState::record_stream_tool_failure`].
    stream_tool_failures: VecDeque<StreamToolFailure>,
    /// Result steps a transcript record completed, with the verdict shown for
    /// that tool (after [`ResultFailure::is_error_for`]). The final drain uses
    /// it to recognize a stream failure the transcript already settled.
    transcript_result_steps: BTreeMap<i64, bool>,
}

/// What [`AntigravityParserState::close_pending_tools`] resolved at the final
/// drain: the events to emit, and the two kinds of stream failure that close
/// no tool, returned together so a caller cannot log one and miss the other.
/// Each carries its `step_index`, the raw material for re-deriving the
/// step→call invariant if agy's numbering ever shifts.
#[derive(Debug)]
pub(crate) struct ClosedTools {
    pub(crate) events: Vec<AdapterEvent>,
    /// Failures naming a step no pending tool expects and no transcript result
    /// settled, with their message.
    pub(crate) unattributed: Vec<(i64, String)>,
    /// Failures on a step whose transcript result reported success. Never
    /// observed; the transcript's result stands.
    pub(crate) contradicted: Vec<(i64, Option<String>)>,
}

#[derive(Debug)]
struct StreamToolFailure {
    step_index: i64,
    message: Option<String>,
}

#[derive(Debug)]
struct PendingToolStart {
    tool_use_id: String,
    /// The call's tool name, which the result's verdict depends on.
    tool_name: String,
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
    failure: ResultFailure,
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
    /// **The transcript settles a step it has a result for.** Since agy 1.4.2
    /// a rejected call is reported twice: as a `status:"ERROR"` transcript
    /// result and as a stream `state:"ERROR"` step. The transcript record
    /// completes the call as soon as it is tailed, whichever channel arrived
    /// first, so by this drain the stream's report for that step has no call
    /// left to close and is dropped here rather than logged as unattributed. The
    /// transcript wins even if the two ever disagree, because it owns the
    /// agents' side of the conversation (system-design §3) and is all a
    /// reopened conversation can read: letting the stream override it would
    /// make the live row and the reopened row differ. A stream failure on a
    /// step the transcript recorded as a *success* is that disagreement, never
    /// observed; it is returned in [`ClosedTools::contradicted`] so the
    /// producer can log it.
    pub(crate) fn close_pending_tools(&mut self, turn_id: TurnId) -> ClosedTools {
        let mut events = Vec::new();
        let mut unattributed = Vec::new();
        let mut contradicted = Vec::new();

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
                None => match self.transcript_result_steps.get(&failure.step_index) {
                    Some(true) => {}
                    Some(false) => contradicted.push((failure.step_index, failure.message)),
                    None => {
                        if let Some(message) = failure.message {
                            unattributed.push((failure.step_index, message));
                        }
                    }
                },
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
        ClosedTools {
            events,
            unattributed,
            contradicted,
        }
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
                let is_error = result.failure.is_error_for(&call.name);
                state
                    .transcript_result_steps
                    .insert(result.step_index, is_error);
                out.push(AdapterEvent::ToolCompleted {
                    turn_id,
                    tool_use_id,
                    output: result.output,
                    is_error,
                });
            } else {
                state.pending_tool_ids.push_back(PendingToolStart {
                    tool_use_id: tool_use_id.clone(),
                    tool_name: call.name.clone(),
                    expected_result_step: expected,
                });
            }
        }
    }

    if rec.is_tool_result() {
        let failure = rec.tool_result_failure();
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
            let is_error = failure.is_error_for(&pending.tool_name);
            state
                .transcript_result_steps
                .insert(rec.step_index, is_error);
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
                failure,
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

/// Detect Antigravity's unauthenticated-dispatch signal on an output line.
///
/// Applied to **both** streams: agy 1.1.x moved diagnostics to stderr (where
/// `StderrSignals` observes each line as it drains, so a chatty burst can't
/// evict the match from the bounded tail), while the stdout scan in
/// `classify_outcome` is retained as a backstop. The 1.1.x auth shape is
/// unverified — probing requires logging the developer out — so neither
/// stream is dropped.
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
                    "result must pair to the start id at its expected step"
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

    /// Captured @ agy 1.4.2 (2026-10-19, Claude Sonnet 5.5, the adapter's
    /// dispatch flags): a rejected edit, then a command that exits 1. The edit
    /// is written as an ordinary result with `status:"ERROR"`; the command as
    /// `status:"DONE"` with the 1.4.2 wording. Both read as successes before.
    const REJECTED_EDIT_THEN_FAILED_COMMAND: [&str; 4] = [
        r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-10-19T17:59:39Z","input_tokens":6629,"cache_read_tokens":14046,"output_tokens":337,"tool_calls":[{"name":"replace_file_content","args":{"AllowMultiple":"false","Description":"\"Replace alpha with beta\"","EndLine":"1","Instruction":"\"Replace alpha with beta\"","ReplacementContent":"\"beta\"","StartLine":"1","TargetContent":"\"alpha\"","TargetFile":"\"/tmp/agyprobe/ABSENT_EDIT.txt\"","toolAction":"\"Editing file\"","toolSummary":"\"Editing absent file\""}}]}"#,
        r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"ERROR","error":"declaring permissions: cortex tool replace_file_content: convert tool call for permissions: model output error: invalid tool call error (invalid_args) /tmp/agyprobe/ABSENT_EDIT.txt does not exist in the current location. Make sure the file path correct. In addition, the user may have moved, renamed, or deleted file.","created_at":"2026-10-19T17:59:42Z","content":"Created At: 2026-10-19T12:59:42-05:00\nCompleted At: 2026-10-19T12:59:42-05:00\nEncountered error in step execution: declaring permissions: cortex tool replace_file_content: convert tool call for permissions: model output error: invalid tool call error (invalid_args) /tmp/agyprobe/ABSENT_EDIT.txt does not exist in the current location. Make sure the file path correct. In addition, the user may have moved, renamed, or deleted file."}"#,
        r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-10-19T17:59:42Z","input_tokens":937,"cache_read_tokens":20267,"output_tokens":210,"tool_calls":[{"name":"run_command","args":{"CommandLine":"\"ls /definitely/not/here\"","Cwd":"\"/tmp/agyprobe/cwd\"","WaitMsBeforeAsync":"3000","toolAction":"\"Running command\"","toolSummary":"\"Listing missing dir\""}}]}"#,
        r#"{"step_index":4,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-10-19T17:59:44Z","content":"Created At: 2026-10-19T12:59:44-05:00\nCompleted At: 2026-10-19T12:59:44-05:00\n\nThe command exited with code 1.\nOutput:\nls: /definitely/not/here: No such file or directory\r\n\n"}"#,
    ];

    /// Captured @ agy 1.4.2 (2026-10-19, Gemini 3.8 Flash): one planner step
    /// with a rejected `view_file` and a successful `run_command`.
    const REJECTED_READ_BESIDE_A_SUCCESSFUL_COMMAND: [&str; 3] = [
        r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-10-19T17:59:39Z","input_tokens":13294,"cache_read_tokens":0,"output_tokens":131,"tool_calls":[{"name":"view_file","args":{"AbsolutePath":"\"/tmp/agyprobe/ABSENT.txt\"","toolAction":"\"Viewing absent file\"","toolSummary":"\"File view probe\""}},{"name":"run_command","args":{"CommandLine":"\"echo PROBE_B\"","Cwd":"\"/tmp/agyprobe/cwd\"","WaitMsBeforeAsync":"5000","toolAction":"\"Echoing probe message\"","toolSummary":"\"Echo probe\""}}]}"#,
        r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"ERROR","error":"declaring permissions: cortex tool view_file: convert tool call for permissions: model output error: invalid tool call error (invalid_args) failed to read file: stat /tmp/agyprobe/ABSENT.txt: no such file or directory","created_at":"2026-10-19T17:59:41Z","content":"Created At: 2026-10-19T12:59:41-05:00\nCompleted At: 2026-10-19T12:59:41-05:00\nEncountered error in step execution: declaring permissions: cortex tool view_file: convert tool call for permissions: model output error: invalid tool call error (invalid_args) failed to read file: stat /tmp/agyprobe/ABSENT.txt: no such file or directory"}"#,
        r#"{"step_index":3,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-10-19T17:59:41Z","content":"Created At: 2026-10-19T12:59:41-05:00\nCompleted At: 2026-10-19T12:59:41-05:00\n\nThe command exited with code 0.\nOutput:\nPROBE_B\r\n\n"}"#,
    ];

    fn completions_of(
        records: &[&str],
        state: &mut AntigravityParserState,
    ) -> Vec<(String, String, bool)> {
        let turn = tid();
        records
            .iter()
            .flat_map(|line| record_to_live_events(&parse(line), turn, state))
            .filter_map(|event| match event {
                AdapterEvent::ToolCompleted {
                    tool_use_id,
                    output,
                    is_error,
                    ..
                } => Some((tool_use_id, output, is_error)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_rejected_call_written_with_status_error_completes_as_failed() {
        let mut state = AntigravityParserState::default();
        let completions = completions_of(&REJECTED_EDIT_THEN_FAILED_COMMAND, &mut state);

        let (id, output, is_error) = &completions[0];
        assert_eq!(id, "1:0:replace_file_content");
        assert!(*is_error, "a status:ERROR result is a failure");
        assert!(
            output.starts_with("declaring permissions:") && output.contains("does not exist"),
            "the top-level error is shown, not the timestamped content: {output}"
        );
    }

    #[test]
    fn a_command_that_exited_non_zero_completes_as_failed() {
        let mut state = AntigravityParserState::default();
        let completions = completions_of(&REJECTED_EDIT_THEN_FAILED_COMMAND, &mut state);

        assert_eq!(completions.len(), 2);
        let (id, output, is_error) = &completions[1];
        assert_eq!(id, "3:0:run_command");
        assert!(*is_error, "`The command exited with code 1.` is a failure");
        assert!(output.contains("No such file or directory"));
        assert!(state.pending_tool_ids.is_empty());
    }

    #[test]
    fn a_rejected_read_fails_without_failing_the_command_beside_it() {
        let mut state = AntigravityParserState::default();
        let completions = completions_of(&REJECTED_READ_BESIDE_A_SUCCESSFUL_COMMAND, &mut state);

        assert_eq!(
            completions
                .iter()
                .map(|(id, _, is_error)| (id.as_str(), *is_error))
                .collect::<Vec<_>>(),
            vec![("1:0:view_file", true), ("1:1:run_command", false)],
            "exit code 0 in the same sentence is a success: {completions:?}"
        );
    }

    #[test]
    fn the_exit_code_sentence_inside_output_is_not_read_as_the_header() {
        let mut state = AntigravityParserState::default();
        let completions = completions_of(
            &[
                r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"cat log.txt\""}}]}"#,
                r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","content":"Created At: now\nCompleted At: now\n\nThe command exited with code 0.\nOutput:\nThe command exited with code 1.\n"}"#,
            ],
            &mut state,
        );
        assert!(!completions[0].2, "{completions:?}");
    }

    /// An MCP reply (1.4.2 writes it as a `GENERIC` result, like a command's)
    /// that quotes the exit sentence on its own line, with no `Output:` marker
    /// to stop the scan. Only a command's exit sentence means failure.
    const MCP_REPLY_QUOTING_THE_EXIT_SENTENCE: [&str; 2] = [
        r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","tool_calls":[{"name":"call_mcp_tool","args":{"ServerName":"\"switchboard\"","ToolName":"\"read_reply\"","Arguments":"{\"agent\":\"builder\"}"}}]}"#,
        r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","content":"Created At: 2026-10-19T13:05:13-05:00\nCompleted At: 2026-10-19T13:05:13-05:00\nThe build agent replied:\nThe command exited with code 1.\nI'll fix the test next."}"#,
    ];

    #[test]
    fn a_non_command_result_quoting_the_exit_sentence_is_not_a_failure() {
        let mut state = AntigravityParserState::default();
        let completions = completions_of(&MCP_REPLY_QUOTING_THE_EXIT_SENTENCE, &mut state);

        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].0, "1:0:call_mcp_tool");
        assert!(!completions[0].2, "{completions:?}");
    }

    /// The settled-steps map holds the verdict shown, not the record's raw
    /// signals: a stream failure on that MCP step is a contradiction to log,
    /// not a duplicate to drop.
    #[test]
    fn a_stream_failure_on_a_non_command_step_quoting_the_sentence_is_a_contradiction() {
        let mut state = AntigravityParserState::default();
        state.record_stream_tool_failure(2, Some("stream says the MCP call failed".to_owned()));
        let _ = completions_of(&MCP_REPLY_QUOTING_THE_EXIT_SENTENCE, &mut state);
        let closed = state.close_pending_tools(tid());

        assert_eq!(
            closed.contradicted,
            vec![(2, Some("stream says the MCP call failed".to_owned()))]
        );
    }

    #[test]
    fn a_failed_command_result_read_before_its_call_still_fails() {
        let mut state = AntigravityParserState::default();
        let records = [
            REJECTED_EDIT_THEN_FAILED_COMMAND[3],
            REJECTED_EDIT_THEN_FAILED_COMMAND[2],
        ];
        let completions = completions_of(&records, &mut state);

        assert_eq!(completions.len(), 1, "{completions:?}");
        assert_eq!(completions[0].0, "3:0:run_command");
        assert!(
            completions[0].2,
            "the buffered result is judged once its call is known"
        );
    }

    #[test]
    fn an_unparseable_exit_code_is_not_called_a_failure() {
        assert!(!tool_result_content_is_error(
            "The command exited with code unknown.\nOutput:\n"
        ));
        assert!(tool_result_content_is_error(
            "The command exited with code -1.\nOutput:\n"
        ));
        assert!(tool_result_content_is_error(
            "The command exited with code 137.\nOutput:\n"
        ));
    }

    /// The double report the fix removes: the stream's `state:"ERROR"` for a
    /// step whose transcript result already said it failed. Either arrival
    /// order must leave exactly one failed completion and nothing to warn about.
    #[test]
    fn a_stream_failure_the_transcript_already_settled_is_dropped() {
        let message = "declaring permissions: cortex tool view_file: … no such file or directory";
        for stream_first in [true, false] {
            let mut state = AntigravityParserState::default();
            if stream_first {
                state.record_stream_tool_failure(2, Some(message.to_owned()));
            }
            let completions =
                completions_of(&REJECTED_READ_BESIDE_A_SUCCESSFUL_COMMAND, &mut state);
            if !stream_first {
                state.record_stream_tool_failure(2, Some(message.to_owned()));
            }
            let closed = state.close_pending_tools(tid());

            assert_eq!(completions.len(), 2, "stream_first={stream_first}");
            assert!(
                closed.events.is_empty()
                    && closed.unattributed.is_empty()
                    && closed.contradicted.is_empty(),
                "stream_first={stream_first}: {closed:?}"
            );
        }
    }

    /// Never observed: the stream calls a step failed that the transcript
    /// recorded as a success. The transcript owns the displayed result, so it
    /// stands, and the disagreement is kept for the producer to log.
    #[test]
    fn a_stream_failure_on_a_step_the_transcript_called_a_success_is_kept_for_diagnostics() {
        let mut state = AntigravityParserState::default();
        state.record_stream_tool_failure(3, Some("stream says the echo failed".to_owned()));
        let completions = completions_of(&REJECTED_READ_BESIDE_A_SUCCESSFUL_COMMAND, &mut state);
        let closed = state.close_pending_tools(tid());

        assert!(!completions[1].2, "the transcript's success stands");
        assert!(closed.events.is_empty(), "{closed:?}");
        assert!(closed.unattributed.is_empty(), "{closed:?}");
        assert_eq!(
            closed.contradicted,
            vec![(3, Some("stream says the echo failed".to_owned()))]
        );
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
        let ClosedTools {
            events: closed,
            unattributed,
            ..
        } = state.close_pending_tools(turn_id);
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
        let ClosedTools {
            events: closed,
            unattributed,
            ..
        } = state.close_pending_tools(turn_id);
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

        let closed = state.close_pending_tools(turn_id).events;
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

        let ClosedTools {
            events: closed,
            unattributed,
            ..
        } = state.close_pending_tools(turn_id);
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

        let ClosedTools {
            events: closed,
            unattributed,
            ..
        } = state.close_pending_tools(turn_id);
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
        let ClosedTools {
            events: closed,
            unattributed,
            ..
        } = state.close_pending_tools(turn_id);
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

        let ClosedTools {
            events: closed,
            unattributed,
            ..
        } = state.close_pending_tools(turn_id);
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
        assert_eq!(state.close_pending_tools(turn_id).events.len(), 0);
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
