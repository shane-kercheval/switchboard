use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use switchboard_core::AgentId;

use crate::events::{
    AdapterEvent, ContentKind, FailureKind, McpServerStatus, PluginEntry, SessionInventory,
    SessionMetaSource, SettingPair, SkillEntry, ToolKind, TurnId, TurnOutcome, TurnSpend,
    TurnUsage,
};

/// Authored auth-failure message for Claude. Replaces Claude's
/// `"Not logged in · Please run /login"` (which refers to the
/// interactive-session slash command, not the CLI command users would
/// typically run from a terminal). The authored copy names the CLI
/// recovery (`claude auth login`) and matches the cross-harness format.
/// Reactive-auth posture — never advises "reload Switchboard."
pub const CLAUDE_AUTH_MESSAGE: &str = "Claude authentication required — run `claude auth login`";

// `Event(AdapterEvent)` dwarfs `Skip`/`Error(String)`, but `AdapterEvent` is the
// whole point of the parser's hot path — boxing it would add an allocation per
// parsed line for no real benefit (the value is consumed immediately, never
// stored in bulk).
#[allow(clippy::large_enum_variant)]
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
    /// refines the fail-fast terminal `TurnEnd` from `HarnessError` to
    /// `AuthFailure`. State-flag pattern: an auth failure always surfaces
    /// through `parse_result`'s failure path (never a second terminal from
    /// the assistant envelope), preserving the one-terminal-per-turn
    /// invariant that the adapter's EOF emission completes.
    pending_auth_failure: Option<String>,
    /// Context-window occupancy of the **most recent** assistant message in
    /// this turn: `input_tokens + cache_read + cache_creation` for that one
    /// model call. Overwritten on every assistant envelope, so at `TurnEnd`
    /// it holds the *final* call's prompt size — which is exactly what the
    /// context window currently holds.
    ///
    /// This is deliberately **not** taken from the terminal `result.usage`:
    /// Claude's `result` event reports usage *summed across every model call*
    /// in the turn (verified against claude 2.1.161 — a two-call turn reports
    /// `input`/`cache_read`/`cache_creation` as the per-call sums). Summed
    /// usage double-counts the shared cached prefix and over-reports occupancy
    /// ~N× for an N-call (tool-use) turn. Mirrors the session-file path, which
    /// keeps the last assistant record's usage.
    last_assistant_context_input_tokens: Option<u64>,
    /// Complete context occupancy after the most recent parent assistant call:
    /// that call's reconciled input side plus its own output. Kept separately
    /// from the terminal result's whole-dispatch output aggregate, which can
    /// include auxiliary/subagent output that never entered the parent context.
    last_assistant_context_tokens_after_turn: Option<u64>,
    /// Overage state from the most recent `rate_limit_event` this turn, stashed
    /// so the terminal `result` can stamp the completing turn's `TurnSpend`.
    /// Claude streams the `rate_limit_event` *before* the terminal `result`
    /// (verified against claude 2.1.161 across normal + tool-use turns), so by
    /// `TurnEnd` this reflects the turn's overage. Defaults to "not overage"
    /// until a rate-limit is seen — so a turn without one shows no cost/marker.
    pending_is_overage: bool,
    pending_overage_resets_at: Option<DateTime<Utc>>,
    /// The most recent assistant message's Anthropic `message.id`, overwritten
    /// on each assistant envelope so at `TurnEnd` it holds the **final**
    /// non-subagent assistant message's id (subagent envelopes are skipped
    /// before this runs). Emitted as the turn's `stable_message_id` — the
    /// durable join key that re-attaches cost/overage to the right message on
    /// reopen (the same id appears in the on-disk session file; verified).
    last_assistant_message_id: Option<String>,
    /// The turn's **first** assistant `message.id`, kept-first. It is emitted as
    /// `first_message_id` → the frontend `hydration_key`.
    /// See `AdapterEvent::TurnEnd::first_message_id`.
    first_assistant_message_id: Option<String>,
    /// The most recent assistant message's `message.model`, kept-last the same
    /// way as the id above — so at `TurnEnd` it is the **final non-subagent**
    /// assistant model (a subagent on a different model never reaches here).
    /// Stamped on the live per-turn `TurnEnd.model`; the reopen counterpart
    /// reads the same `message.model` from the session file.
    last_assistant_model: Option<String>,
    /// The effort this dispatch was launched with — the value the adapter put
    /// behind `--effort`, injected at construction rather than parsed, because
    /// **Claude's live stream carries no effort** (only the session file does,
    /// since 2.1.212 — see `harness-behavior.md` §3.4). Stamping what we sent
    /// is what lets the live footer agree with the reopened one instead of the
    /// value appearing only after a restart. `None` when the agent's effort is
    /// unset ("Default" in the Model settings dialog): we pass no flag, Claude
    /// picks its own level, and we deliberately render nothing live rather than
    /// guess — the session file still records the real level for the reopen.
    dispatched_effort: Option<String>,
    /// Telemetry from the most recent **successful** `result`, kept-last,
    /// awaiting emission as the turn's single Completed `TurnEnd` at stream
    /// EOF ([`ParserState::take_final_turn_end`]). A background-agent dispatch
    /// emits one `result` per internal init→result cycle, with **irregular
    /// delivery timing** (mid-stream and exit-batched both observed) and no
    /// in-stream marker for "this is the last one" — the only reliable
    /// terminal boundary is the stream ending (probed against claude 2.1.198;
    /// captures in `docs/research/archive/claude-background-agent-*.jsonl`).
    /// Kept-last is whole-dispatch-correct: the final result's
    /// `total_cost_usd` is the dispatch total (= Σ `modelUsage[*].costUSD`,
    /// subagent work included) and its `modelUsage` holds whole-dispatch
    /// per-model aggregates. Failure results bypass this stash and fail fast.
    pending_completed_terminal: Option<PendingCompletedTerminal>,
    /// Which operation's stream this state is reading. Compaction-only rules are
    /// gated on this; an ordinary send's handling of every affected record is
    /// unchanged, pinned by the `outside_compaction_mode_*` tests.
    mode: StreamMode,
    /// The compaction verdict from `system/status`, recorded only in
    /// [`StreamMode::Compaction`]. `None` means no verdict was seen — which at
    /// `result` time is itself a failure signal, not a success (see
    /// [`parse_result`]).
    compaction_verdict: Option<CompactionVerdict>,
    /// The model named by the `system/init` Claude re-emits as the session
    /// re-initializes during a compaction. Kept because it is the **only exact
    /// model identifier a compaction's stream carries**: there is no assistant
    /// envelope to name a model, and `result.model` was absent in both captured
    /// compactions, so without this the window lookup would fall through to the
    /// sole-entry guess that [`select_context_window`] exists to avoid. Set in
    /// compaction mode only, so an ordinary send's window resolution is
    /// untouched.
    ///
    /// The ordering this depends on is verified, not assumed: on both the
    /// success and the failure path `init` arrives *before* `result`
    /// (harness-behavior.md §3.9).
    init_model: Option<String>,
    /// Before/after context occupancy from `compact_boundary.compact_metadata`,
    /// recorded in compaction mode whenever the boundary arrives — **not** keyed
    /// to the verdict having been seen first.
    ///
    /// Deliberately independent: nesting it inside the `Succeeded` verdict made
    /// the observed record order (verdict, then boundary) load-bearing for the
    /// feature's central visible output. Claude emits both from one routine and
    /// that order is an observed fact, not a documented contract — so a
    /// reordering upstream would have left the compaction completing with no
    /// numbers and the context bar clean-hiding, silently. [`parse_result`]
    /// joins the two instead: occupancy is read only when the verdict says
    /// success, which keeps "a boundary alone never invents a success" intact
    /// without the ordering dependency.
    compaction_occupancy: Option<CompactionOccupancy>,
}

/// Which operation a [`ParserState`] is reading the stream of. Claude emits the
/// same record vocabulary for all three, but the maintenance operations assign
/// different meaning to some of them — for a compaction, `system/status` carries
/// the verdict, the `<synthetic>` assistant envelope is a diagnostic rather than
/// an answer, and `result` is not the verdict at all; for a context report, the
/// `<synthetic>` envelope *is* the whole payload and is swallowed whole.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamMode {
    #[default]
    Send,
    Compaction,
    ContextReport,
}

/// What Claude's `system/status` record said about a requested compaction.
/// **The verdict, and the only one** — `result` reports `subtype:"success"`,
/// `is_error:false` and exit 0 for a compaction that did nothing at all, so
/// classifying off `result` would report every refusal as a completed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CompactionVerdict {
    Succeeded,
    Failed {
        /// Claude's `compact_error`, when it carried one. `None` — including for
        /// a present-but-blank field — so [`parse_result`] can fall back to the
        /// `result` record's own text rather than to a generic string. Choosing
        /// the wording here instead would destroy the better diagnostic before
        /// anything could reach for it.
        message: Option<String>,
    },
}

/// The before/after context occupancy a successful compaction reports. These are
/// the turn's *only* occupancy source: a compaction makes no assistant call, so
/// the usual final-parent-call derivation has nothing to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactionOccupancy {
    pre_tokens: u64,
    post_tokens: u64,
}

/// The stashed payload of a successful `result`, pending emission at EOF.
/// Only the result-derived fields live here; model and message ids are read
/// from the [`ParserState`] kept-first/kept-last fields at emission time.
#[derive(Debug)]
struct PendingCompletedTerminal {
    usage: Option<TurnUsage>,
    context_window_source: Option<crate::events::ContextWindowSource>,
    spend: Option<TurnSpend>,
}

impl ParserState {
    /// The turn's dedup identity so far: the first non-subagent assistant
    /// `message.id` seen this turn, or `None` if no assistant message has been
    /// parsed yet. Read by the adapter to stamp the *same* `hydration_key` onto
    /// a synthesized **failure** `TurnEnd` (crash/truncation) that the happy
    /// path emits — so a crashed multi-message turn's live `Failed` row dedups
    /// against its on-disk copy instead of rendering a duplicate. Read-only:
    /// the field is private to this module, and a failure path must not mutate
    /// parser state.
    pub(crate) fn first_assistant_message_id(&self) -> Option<&str> {
        self.first_assistant_message_id.as_deref()
    }

    /// The turn's single terminal `TurnEnd`, built from the last successful
    /// `result` folded by [`parse_result`] — or `None` if no successful result
    /// arrived (the caller falls back to truncation synthesis). Called by the
    /// adapter at stream EOF; the caller supplies the `outcome` because it
    /// owns the exit-status gate: a folded intermediate result is not proof
    /// the dispatch finished (a kill between background-agent cycles leaves a
    /// stash behind), so only a clean process exit may pass `Completed` —
    /// a dirty exit passes `Failed`, keeping the folded telemetry (partial
    /// work is still billed) on the failure terminal.
    pub(crate) fn take_final_turn_end(
        &mut self,
        turn_id: TurnId,
        outcome: TurnOutcome,
    ) -> Option<AdapterEvent> {
        let pending = self.pending_completed_terminal.take()?;
        Some(AdapterEvent::TurnEnd {
            turn_id,
            outcome,
            ended_at: Utc::now(),
            usage: pending.usage,
            context_window_source: pending.context_window_source,
            spend: pending.spend,
            // Kept-last across the whole dispatch — the final non-subagent
            // assistant model / message id (the cost-join key), and the
            // kept-first id (the live↔disk dedup key). Read, not taken: the
            // ids must survive every intermediate result so the single
            // terminal carries whole-dispatch identity.
            model: self.last_assistant_model.clone(),
            effort: self.terminal_effort(),
            stable_message_id: self.last_assistant_message_id.clone(),
            first_message_id: self.first_assistant_message_id.clone(),
        })
    }

    /// Construct a state for a stream of `mode`, launched with `effort` (the
    /// value passed to `--effort`, or `None` when the agent leaves it unset).
    pub(crate) fn for_stream(mode: StreamMode, effort: Option<String>) -> Self {
        Self {
            mode,
            dispatched_effort: effort,
            ..Self::default()
        }
    }

    fn compacting(&self) -> bool {
        self.mode == StreamMode::Compaction
    }

    fn reporting_context(&self) -> bool {
        self.mode == StreamMode::ContextReport
    }

    /// The effort to stamp on this turn's terminal, or `None` to render nothing.
    ///
    /// **Gated on the model that actually ran**, because the dispatched value is
    /// an assertion rather than an observation — Claude's live stream reports no
    /// effort for any model, so the only honest thing to echo is a value we know
    /// the harness will corroborate on disk. It does not always: a model with no
    /// reasoning-effort axis accepts `--effort` silently and records **nothing**
    /// (probed @ 2.1.241 — `--model haiku --effort max|low` → no `effort` key,
    /// while Sonnet 5 records `max`/`low` verbatim). Stamping there would show a
    /// level live that vanishes when the same turn is re-read from disk.
    ///
    /// **Default-closed**, and that is the load-bearing property: only families
    /// verified to record effort are echoed, so an unrecognized or future model
    /// degrades to "blank live, correct on reopen" rather than to a wrong value.
    /// A no-axis model added to the picker is therefore safe by default, and the
    /// per-model live assertions in the live suite are what promote a new family
    /// into this list (see the "Model catalog" step in `harness-update-review.md`).
    ///
    /// Keys off the resolved id Claude reports (`message.model`), never the alias
    /// the user picked — aliases move between model generations, and an alias is
    /// not what the session file records. A turn with no assistant record at all
    /// (an auth or argument failure that died before the model ran) has no
    /// resolved id, so it stamps nothing, which is also the truthful answer.
    fn terminal_effort(&self) -> Option<String> {
        let model = self.last_assistant_model.as_deref()?;
        model_records_effort(model).then(|| self.dispatched_effort.clone())?
    }
}

/// Whether a resolved Claude model id is **verified** to record `effort` in its
/// session file.
///
/// Exact ids, not a family prefix or substring, and that is deliberate. The
/// effort axis is a per-*model* property, not a per-family one: within the same
/// family, Haiku 4.5 has no axis at all, and `harness-behavior.md` §3.4 records
/// that Sonnet 4.6 / Opus 4.6 *execute* at a capped level. A family match would
/// therefore assert this property for ids it was never checked against —
/// including older generations and any third-party id that merely contains the
/// family word (`some-vendor-opus-proxy`).
///
/// **Probed @ 2.1.241 with Switchboard's exact `-p` flags** (`claude-fable-5-1`
/// added @ 2.1.257, when the `fable` alias moved to it) — the requested level is
/// written back verbatim for exactly these four:
///
/// | id | `--effort` sent | recorded |
/// |---|---|---|
/// | `claude-opus-5` | `high` | `"high"` |
/// | `claude-sonnet-5` | `max` / `low` | `"max"` / `"low"` |
/// | `claude-fable-5` | `low` | `"low"` (upper bound on disk unprobed; full-id pinning only) |
/// | `claude-fable-5-1` | `low` / `max` | `"low"` / `"max"` (`max` is the live loop's standing check) |
/// | `claude-haiku-4-5-20251001` | `max` / `low` | *no key written* |
///
/// Every other id — older, newer, or third-party — is **withheld because it is
/// unverified, not because it is known to diverge.** Whether a capped model
/// records the requested or the effective level is an open question nobody has
/// probed; withholding sidesteps it rather than betting on an answer.
///
/// **Default-closed, and the staleness path is a failing test rather than a
/// wrong value.** When an alias moves to a new generation, the new id is absent
/// here, so the live echo stops (blank live, still correct on reopen) and
/// `live_claude_session_file_effort_matches_the_dispatched_level` fails on the
/// live-vs-disk mismatch — which is the signal to probe the new id and add it.
/// See the "Model catalog" step in `harness-update-review.md`.
fn model_records_effort(model: &str) -> bool {
    const EFFORT_RECORDING_MODELS: [&str; 4] = [
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-fable-5-1",
    ];
    EFFORT_RECORDING_MODELS.contains(&model)
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
    // Antigravity's
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
        Some("system") => parse_system_event(&value, turn_id, agent_id, state),
        // In context-report mode the assistant envelope carries the report and
        // nothing else, so it is intercepted **before** ordinary assistant
        // handling rather than filtered inside it. The difference is not
        // stylistic: `parse_assistant_envelope` would otherwise announce a
        // `TurnIdentity` from the synthetic message's id and emit the printed
        // table as content — giving the turn a durable dedup key pointing at a
        // message no session file contains, and feeding the report's markdown to
        // the dispatcher's `captured_text` and thence to the forward path. This
        // way nothing downstream has to know to ignore report text.
        Some("assistant") if state.reporting_context() => {
            parse_context_report_envelope(&value, agent_id)
        }
        Some("assistant") => parse_assistant_envelope(&value, turn_id, state),
        Some("user") => parse_user_envelope(&value, turn_id),
        Some("rate_limit_event") => parse_rate_limit_event(&value, agent_id, state),
        _ => ParseOutcome::Skip,
    }
}

fn parse_stream_event(obj: &Value, turn_id: TurnId, state: &mut ParserState) -> ParseOutcome {
    let Some(event) = obj.get("event") else {
        return ParseOutcome::Skip;
    };

    // Drift protection, cheap: neither captured compaction emits a single delta,
    // and a compaction has no answer to stream. Should a future CLI start
    // streaming the summarizer's output, it degrades to a heartbeat rather than
    // printing the summary into the transcript as if the agent had said it.
    if state.compacting() {
        return ParseOutcome::Event(AdapterEvent::Liveness { turn_id });
    }

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

    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => {} // fall through to text handling below
        Some("thinking_delta") => {
            // Claude's reasoning redaction is per-model (see
            // `harness-behavior.md` §3.2): Sonnet 4.6 streams non-empty
            // reasoning text, which flows through as `Thinking` content;
            // Opus 4.8 redacts it to empty, so an empty delta carries no
            // content and surfaces as a non-rendering liveness signal
            // (keeping the heartbeat alive through a long redacted block).
            // Branching on emptiness — not on the model — keeps this correct
            // across both models and any future shift in the server flag.
            let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                return ParseOutcome::Event(AdapterEvent::Liveness { turn_id });
            }
            return ParseOutcome::Event(AdapterEvent::ContentChunk {
                turn_id,
                kind: ContentKind::Thinking,
                text: text.to_owned(),
            });
        }
        // `signature_delta` carries an opaque signature blob (no readable
        // content), and `input_json_delta` streams a tool call's arguments —
        // which we render via the `tool_started`/`tool_completed` pair, NOT
        // from these deltas. But both are signs the harness is actively
        // producing, so they re-arm the heartbeat as liveness rather than
        // counting as silence (a large tool input can stream for many seconds
        // before `tool_started` — emitted from the completed assistant
        // envelope — arrives). `input_json_delta` is per-fragment, so a large
        // tool input emits proportionally many liveness events; that volume is
        // accepted (same order as text streaming, which the event pipeline
        // already absorbs) in exchange for an honest heartbeat — dropping it
        // would falsely show "no response" while the agent is actively
        // generating the tool input.
        Some("signature_delta" | "input_json_delta") => {
            return ParseOutcome::Event(AdapterEvent::Liveness { turn_id });
        }
        _ => return ParseOutcome::Skip,
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
    // outcome from `HarnessError` to `AuthFailure`. Usage extraction below
    // still runs (auth-failure results carry zero-valued telemetry, which is
    // legitimate, not noise).
    let auth_failure = state.pending_auth_failure.take();

    // A compaction's occupancy comes from `compact_boundary`, not from a final
    // assistant call (it makes none). `None` outside compaction mode and on a
    // failed compaction, where the whole usage record is withheld below.
    // The join: occupancy is recorded independently of the verdict, and is used
    // only when the verdict says the compaction actually happened.
    let compaction_occupancy = match &state.compaction_verdict {
        Some(CompactionVerdict::Succeeded) => state.compaction_occupancy,
        _ => None,
    };
    let (context_input_tokens, context_tokens_after_turn) = if state.compacting() {
        (
            compaction_occupancy.map(|o| o.pre_tokens),
            compaction_occupancy.map(|o| o.post_tokens),
        )
    } else {
        (
            state.last_assistant_context_input_tokens,
            state.last_assistant_context_tokens_after_turn,
        )
    };
    let context_window = select_context_window(
        obj,
        state.last_assistant_model.as_deref(),
        // In compaction mode this is the only exact key available; it is `None`
        // for a send, leaving that path's resolution chain untouched.
        state.init_model.as_deref(),
        // Feeds the impossible-window rejection. Passing the compaction's own
        // post-compaction occupancy keeps that check live on this path rather
        // than letting it silently go dead for want of an assistant call.
        context_tokens_after_turn,
    );
    let usage = extract_usage_from_result(
        obj,
        context_input_tokens,
        context_tokens_after_turn,
        context_window
            .as_ref()
            .map(|selected| selected.context_window),
    );

    // Claude's context window is stream-only (`result.modelUsage`), absent from
    // the session file — so when this turn carries one, tag it `StreamOnly` for
    // the dispatcher to persist to the metadata sidecar. `None` when there's no
    // window (nothing to persist).
    let context_window_source =
        context_window
            .as_ref()
            .map(|selected| crate::events::ContextWindowSource::StreamOnly {
                model: selected.model.clone(),
            });

    // Stamp the turn's real-spend attribution from the overage state seen on
    // this turn's `rate_limit_event` (which precedes the `result` — verified).
    // For Claude, real-spend == overage: subscription `total_cost_usd` is only
    // money actually charged when spending overage credits. The frontend gates
    // the inline cost + marker on `real_spend` with no `match harness`.
    let spend = Some(TurnSpend {
        real_spend: state.pending_is_overage,
        is_overage: state.pending_is_overage,
        overage_resets_at: state.pending_overage_resets_at,
    });

    // Failure results fail fast: the stream ends right after a failed result,
    // and stopping the read early is correct for a failed dispatch. Message
    // ids are read (not taken) so a failure terminal still carries the turn's
    // identity keys.
    //
    // **The auth check runs first, deliberately.** An auth-failed compaction
    // never reaches Claude's command handler, so it has no verdict at all — and
    // the no-verdict arm below would otherwise swallow it, replacing an
    // actionable `AuthFailure` (which drives the frontend's "run `claude auth
    // login`" copy) with a generic harness error.
    let failure = if let Some(auth_message) = auth_failure {
        Some(TurnOutcome::Failed {
            kind: FailureKind::AuthFailure,
            message: auth_message,
        })
    } else if let Some(compaction_failure) = compaction_failure(state, result_diagnostic(obj)) {
        Some(compaction_failure)
    } else if is_error || has_api_error {
        let message = obj
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("harness reported an error")
            .to_owned();
        Some(TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message,
        })
    } else {
        None
    };
    if let Some(outcome) = failure {
        return ParseOutcome::Event(AdapterEvent::TurnEnd {
            turn_id,
            outcome,
            ended_at: Utc::now(),
            usage: withhold_usage_on_a_turn_that_measured_nothing(state, usage),
            context_window_source,
            spend,
            model: state.last_assistant_model.clone(),
            effort: state.terminal_effort(),
            stable_message_id: state.last_assistant_message_id.clone(),
            first_message_id: state.first_assistant_message_id.clone(),
        });
    }

    // A successful `result` is NOT the turn's terminal: a background-agent
    // dispatch emits one per internal cycle and may keep producing content
    // afterwards (see `ParserState::pending_completed_terminal`). Fold it —
    // kept-last, whole-dispatch-correct — and surface it as turn progress so
    // the heartbeat stays armed between cycles. The adapter emits the folded
    // terminal at stream EOF via `take_final_turn_end`, gated on exit status.
    state.pending_completed_terminal = Some(PendingCompletedTerminal {
        // Gated on both paths, not just the failure one above: a context report
        // succeeds, so the folded terminal is the only place its zero-valued
        // usage could reach the wire. (A compaction only ever folds here when
        // its verdict was `Succeeded`, where the gate is a no-op.)
        usage: withhold_usage_on_a_turn_that_measured_nothing(state, usage),
        context_window_source,
        spend,
    });
    ParseOutcome::Event(AdapterEvent::Liveness { turn_id })
}

/// The `result` record's own human-readable text, when it carries one. The
/// second-choice diagnostic for a failed compaction, behind `compact_error`.
fn result_diagnostic(result: &Value) -> Option<&str> {
    result
        .get("result")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// The terminal a compaction's verdict demands at `result`, or `None` when this
/// stream is not a compaction or the compaction succeeded (in which case the
/// caller folds the result like any other successful turn, so the adapter's
/// exit-status gate still applies).
///
/// **Fail-closed on a missing verdict, by construction.** `result` reports
/// success for a compaction that did nothing, so "no verdict seen" cannot be
/// read as "it worked." The observed cause is `DISABLE_COMPACT` in the user's
/// environment, but the arm is deliberately written against the *absence* of
/// evidence rather than that one cause, so any future shape in which the command
/// does not run surfaces as a failure rather than a silent no-op.
///
/// It is a named `HarnessError` rather than a truncation `AdapterFailure`
/// because `FailureKind` drives the frontend's recovery copy: "adapter failure"
/// tells the user to file a bug, which is the wrong instruction for something
/// Claude declined to do.
///
/// **Classification is fail-closed; the *message* is best-available.** Those are
/// separate concerns and the precedence exists so the second never weakens the
/// first: Claude's own `compact_error`, then `diagnostic` (the `result` text),
/// then authored wording only when the harness said nothing at all. Without the
/// middle step every cause outside the two captured shapes — an API error, a
/// quota exhaustion, an environment that disabled the command — collapsed to a
/// bare "compaction did not run", which tells the user nothing about whether a
/// retry could work.
///
/// Note this runs **ahead of** the generic `is_error` / `api_error_status`
/// handling, so an API error that kills a compaction before its verdict reads
/// "compaction did not run: API Error …" — the same `HarnessError` kind the
/// generic path would give, plus the fact that nothing was compacted.
fn compaction_failure(state: &ParserState, diagnostic: Option<&str>) -> Option<TurnOutcome> {
    if !state.compacting() {
        return None;
    }
    let message = match &state.compaction_verdict {
        Some(CompactionVerdict::Succeeded) => return None,
        Some(CompactionVerdict::Failed { message }) => message
            .clone()
            .or_else(|| diagnostic.map(str::to_owned))
            .unwrap_or_else(|| "the harness declined to compact this conversation".to_owned()),
        None => match diagnostic {
            Some(diagnostic) => format!("compaction did not run: {diagnostic}"),
            None => "compaction did not run".to_owned(),
        },
    };
    Some(TurnOutcome::Failed {
        kind: FailureKind::HarnessError,
        message,
    })
}

/// Drop the usage record entirely when the turn measured nothing — a failed
/// compaction, or any context report.
///
/// A refused compaction's `result` carries an **empty** `modelUsage`, so the
/// extractor falls back to `result.usage` — whose zero-valued token fields are
/// schema-present and therefore yield a legitimate `Some` with no
/// `context_window`. The sidebar treats the newest usage-bearing turn as
/// authoritative and clean-hides on a missing window, so emitting that record
/// would **blank the context bar after a compaction that changed nothing** —
/// precisely when the previous turn's number is still exactly right. Withholding
/// it leaves the bar reading the last real turn, which is the truth.
///
/// Only the failed path is withheld: a *successful* compaction's numbers come
/// from `compact_metadata` and are the whole point of the row.
///
/// **The contract, stated once because three states meet here.** A *refusal*
/// (no verdict, or a failed one) withholds usage — nothing was compacted, so the
/// previous turn's number is still the truth. A *successful* compaction keeps
/// it. A successful compaction whose process then exits abnormally **also keeps
/// it** — that terminal is `Failed`, but the compaction demonstrably happened
/// and its post-compaction occupancy is the best measurement available, so
/// suppressing it would knowingly leave the context bar stale. The gate is
/// therefore the *verdict*, never the terminal outcome. Do not "simplify" this
/// to "failed turns carry no usage"; that would re-break the dirty-exit case,
/// which `a_folded_success_still_fails_on_a_dirty_exit` pins.
///
/// **A context report is withheld unconditionally**, for the same mechanism and
/// a simpler reason: it makes no model call at all, so its `modelUsage` is
/// always empty and its `result.usage` always schema-present zeros. It is free
/// by construction, so there is no turn it could truthfully bill — and letting
/// its zero-Some through would make "the report does not blank the very bar its
/// chevron opens" depend on the frontend suppressing the row, rather than on the
/// event never carrying the number.
fn withhold_usage_on_a_turn_that_measured_nothing(
    state: &ParserState,
    usage: Option<TurnUsage>,
) -> Option<TurnUsage> {
    if state.reporting_context() {
        return None;
    }
    if state.compacting()
        && !matches!(
            &state.compaction_verdict,
            Some(CompactionVerdict::Succeeded)
        )
    {
        return None;
    }
    usage
}

/// Pull `TurnUsage` from a `result` event.
///
/// **Token fields come from `result.modelUsage`** — per-model whole-dispatch
/// aggregates, summed across entries. That is the *billing-consistent*
/// vocabulary: the dispatch's `total_cost_usd` equals Σ
/// `modelUsage[*].costUSD` exactly (probed against claude 2.1.198,
/// multi-model), and it includes subagent work, unlike the per-cycle,
/// parent-only `result.usage`. On a background-agent dispatch (multiple
/// results), each result's `modelUsage` snapshots the whole dispatch so far —
/// kept-last by the caller, the final one is the dispatch total.
///
/// **Fallback:** when `modelUsage` is empty/missing, or any entry lacks the
/// required numeric `inputTokens`/`outputTokens`, fall back to the per-cycle
/// `result.usage` shape — where `input_tokens`/`output_tokens` are required
/// numeric fields and a miss returns `None`, never a fabricated zero-Some.
/// Zero *values* from a real harness (auth-failure synthetic responses) DO
/// produce a valid `Some` — what matters is schema presence, not non-zero.
///
/// Populated for both Completed and Failed turns. The harness charges for
/// partial work, so token counts on failure are meaningful telemetry.
/// The **occupancy** field (`context_input_tokens`) comes from neither
/// vocabulary: it is the final assistant message's per-call prompt size,
/// threaded in via `last_call_context_input_tokens`, because both aggregates
/// sum across calls and would over-report a multi-call turn's window
/// fullness (see `ParserState::last_assistant_context_input_tokens`).
fn extract_usage_from_result(
    obj: &Value,
    last_call_context_input_tokens: Option<u64>,
    last_call_context_tokens_after_turn: Option<u64>,
    context_window: Option<u32>,
) -> Option<TurnUsage> {
    let total_cost_usd = obj.get("total_cost_usd").and_then(Value::as_f64);

    if let Some(aggregate) = sum_model_usage_tokens(obj) {
        return Some(TurnUsage {
            input_tokens: aggregate.input,
            output_tokens: aggregate.output,
            cached_input_tokens: aggregate.cached_input,
            cache_creation_input_tokens: aggregate.cache_creation,
            context_input_tokens: last_call_context_input_tokens,
            context_tokens_after_turn: last_call_context_tokens_after_turn,
            // Not present in `modelUsage`; the per-cycle shape may carry it.
            reasoning_output_tokens: obj
                .get("usage")
                .and_then(|u| u.get("reasoning_output_tokens"))
                .and_then(Value::as_u64),
            context_window,
            total_cost_usd,
        });
    }

    let usage_obj = obj.get("usage")?;
    let input_tokens = usage_obj.get("input_tokens").and_then(Value::as_u64)?;
    let output_tokens = usage_obj.get("output_tokens").and_then(Value::as_u64)?;
    let cached_input_tokens = usage_obj
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .or_else(|| usage_obj.get("cached_input_tokens").and_then(Value::as_u64));
    let cache_creation_input_tokens = usage_obj
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64);
    let reasoning_output_tokens = usage_obj
        .get("reasoning_output_tokens")
        .and_then(Value::as_u64);

    Some(TurnUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        cache_creation_input_tokens,
        // Occupancy = the final model call's prompt size, NOT the summed
        // aggregate (which double-counts the shared cached prefix). See
        // `ParserState::last_assistant_context_input_tokens`.
        context_input_tokens: last_call_context_input_tokens,
        context_tokens_after_turn: last_call_context_tokens_after_turn,
        reasoning_output_tokens,
        context_window,
        total_cost_usd,
    })
}

/// Whole-dispatch token totals summed across `result.modelUsage` entries.
/// `None` (→ caller falls back to `result.usage`) when `modelUsage` is
/// empty/missing or any entry lacks the required `inputTokens`/`outputTokens`
/// — an all-or-nothing rule so a partially-malformed aggregate never yields a
/// silently-undercounted sum. Cache fields are optional per entry (`0` is a
/// legitimate real value; absent contributes nothing) and reported only when
/// at least one entry carries them.
struct ModelUsageAggregate {
    input: u64,
    output: u64,
    cached_input: Option<u64>,
    cache_creation: Option<u64>,
}

fn sum_model_usage_tokens(result: &Value) -> Option<ModelUsageAggregate> {
    let model_usage = result.get("modelUsage").and_then(Value::as_object)?;
    if model_usage.is_empty() {
        return None;
    }

    let mut aggregate = ModelUsageAggregate {
        input: 0,
        output: 0,
        cached_input: None,
        cache_creation: None,
    };
    for entry in model_usage.values() {
        if !checked_add_model_usage(
            &mut aggregate.input,
            entry.get("inputTokens").and_then(Value::as_u64)?,
            "inputTokens",
        ) || !checked_add_model_usage(
            &mut aggregate.output,
            entry.get("outputTokens").and_then(Value::as_u64)?,
            "outputTokens",
        ) {
            return None;
        }
        if let Some(cached) = entry.get("cacheReadInputTokens").and_then(Value::as_u64)
            && !checked_add_model_usage(
                aggregate.cached_input.get_or_insert(0),
                cached,
                "cacheReadInputTokens",
            )
        {
            return None;
        }
        if let Some(created) = entry
            .get("cacheCreationInputTokens")
            .and_then(Value::as_u64)
            && !checked_add_model_usage(
                aggregate.cache_creation.get_or_insert(0),
                created,
                "cacheCreationInputTokens",
            )
        {
            return None;
        }
    }
    Some(aggregate)
}

fn checked_add_model_usage(total: &mut u64, value: u64, field: &str) -> bool {
    let Some(sum) = total.checked_add(value) else {
        tracing::warn!(
            field,
            "Claude modelUsage aggregate overflow — falling back to result.usage"
        );
        return false;
    };
    *total = sum;
    true
}

#[derive(Debug, PartialEq, Eq)]
struct SelectedContextWindow {
    model: String,
    context_window: u32,
}

/// Select a model-bound `contextWindow` from `result.modelUsage`.
///
/// The final parent assistant model is authoritative. `init_model` — the model a
/// compaction's re-initialized session announces, and the only exact identifier
/// that path has — is next; the result-level model follows; and a sole-entry
/// fallback is allowed only when **none** of the three exists. Claude may qualify
/// the corresponding usage-map key with `[1m]`; that exact transport suffix is the
/// only non-exact lookup allowed. Other mismatches do not fall through because
/// guessing would risk binding an auxiliary model's window to the parent turn.
/// A numerically impossible window is also rejected before it can reach
/// persistence or the UI.
///
/// `init_model` is `None` for an ordinary send, so this ordering change is inert
/// on that path: a send always has a final assistant model, which still wins.
fn select_context_window(
    result: &Value,
    final_assistant_model: Option<&str>,
    init_model: Option<&str>,
    context_tokens_after_turn: Option<u64>,
) -> Option<SelectedContextWindow> {
    let model_usage = result.get("modelUsage").and_then(Value::as_object)?;
    if model_usage.is_empty() {
        return None;
    }

    let (model, sole_entry_fallback) = if let Some(model) = final_assistant_model {
        (model, false)
    } else if let Some(model) = init_model {
        (model, false)
    } else if let Some(model) = result.get("model").and_then(Value::as_str) {
        (model, false)
    } else if model_usage.len() == 1 {
        (model_usage.keys().next()?.as_str(), true)
    } else {
        return None;
    };
    let entry = model_usage
        .get(model)
        .or_else(|| model_usage.get(&format!("{model}[1m]")))?;
    let context_window = u32::try_from(entry.get("contextWindow")?.as_u64()?).ok()?;
    if context_window == 0
        || context_tokens_after_turn.is_some_and(|occupancy| occupancy > u64::from(context_window))
    {
        return None;
    }
    Some(SelectedContextWindow {
        model: if sole_entry_fallback {
            model.strip_suffix("[1m]").unwrap_or(model).to_owned()
        } else {
            model.to_owned()
        },
        context_window,
    })
}

/// Parse a `system` envelope. `init` carries session metadata; **every other
/// subtype maps to `Liveness`** — a denylist, deliberately. The non-init
/// vocabulary is vendor-controlled and has already drifted across CLI
/// versions (`task_started`/`task_progress`/`task_notification` documented at
/// 2.1.170; 2.1.198 added `task_updated` and `thinking_tokens`, plus
/// `status`/`hook_*`), so a fixed allowlist of names silently stops re-arming
/// the heartbeat on the next rename — flagging a healthy multi-minute
/// background-agent wait as "gone quiet." An unknown system event is still
/// evidence the process is alive; `Liveness` renders nothing, so
/// misclassifying a future content-bearing subtype degrades to exactly the
/// old silent skip, never worse.
fn parse_system_event(
    obj: &Value,
    turn_id: TurnId,
    agent_id: AgentId,
    state: &mut ParserState,
) -> ParseOutcome {
    let subtype = obj.get("subtype").and_then(Value::as_str);
    // Compaction-only: two of the subtypes that are pure liveness for a send
    // carry this turn's entire meaning. Outside compaction mode they stay
    // `Liveness` exactly as before.
    if state.compacting() {
        match subtype {
            Some("status") => record_compaction_verdict(obj, state),
            Some("compact_boundary") => record_compaction_occupancy(obj, state),
            _ => {}
        }
    }
    if subtype != Some("init") {
        return ParseOutcome::Event(AdapterEvent::Liveness { turn_id });
    }

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    // Stash the re-initialized session's model as the compaction's exact window
    // key (see `ParserState::init_model`). Kept-first: the whole point is the
    // model this dispatch ran under, and only one `init` is expected.
    if state.compacting() && state.init_model.is_none() && !model.is_empty() {
        state.init_model = Some(model.clone());
    }
    let harness_version = obj
        .get("claude_code_version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    ParseOutcome::Event(AdapterEvent::SessionMeta {
        agent_id,
        model,
        harness_version,
        inventory: parse_init_inventory(obj),
        raw: obj.clone(),
        // Claude's inventory exists only on this event — `system/init` has no
        // on-disk analog in the session file (class C) — so the dispatcher
        // caches it for restart continuity.
        source: SessionMetaSource::StreamOnly,
    })
}

/// Read the environment inventory off a `system/init` record.
///
/// **An absent key yields `None`, not an empty list.** The two are different
/// claims (see [`SessionInventory`]): only `None` lets the config loaders fill
/// the list on a later reload, which is the correct degradation for an older
/// CLI that does not emit the key at all. An `init` that *does* emit the key
/// with an empty array is reporting an authoritative zero and is preserved as
/// `Some([])`.
fn parse_init_inventory(obj: &Value) -> SessionInventory {
    SessionInventory {
        tools: string_list(obj, "tools"),
        mcp_servers: obj
            .get("mcp_servers")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(parse_mcp_server_status).collect()),
        // Claude reports skill names only; descriptions and paths stay `None`
        // (Codex's rollout is the one source that carries all three).
        skills: string_list(obj, "skills")
            .map(|names| names.into_iter().map(SkillEntry::from_name).collect()),
        agents: string_list(obj, "agents"),
        plugins: obj
            .get("plugins")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(parse_plugin_entry).collect()),
        // `memory_paths` is a **map** of kind → path (`{"auto": "<dir>"}`),
        // not a list; the paths are its values. The concrete memory *files*
        // are named only by the `/context` report.
        memory_paths: obj.get("memory_paths").and_then(Value::as_object).map(|m| {
            m.values()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        }),
        slash_commands: string_list(obj, "slash_commands"),
        // Claude reports no approved-command allowlist on `init`; that list is
        // Codex's.
        approved_commands: None,
        settings: parse_init_settings(obj),
    }
}

/// The run settings Claude reports on `init`, as display pairs. Absent keys
/// are skipped rather than rendered blank; all keys absent yields `None`, so
/// the card draws no settings line at all.
fn parse_init_settings(obj: &Value) -> Option<Vec<SettingPair>> {
    let pairs: Vec<SettingPair> = [
        ("Permission mode", "permissionMode"),
        ("Output style", "output_style"),
    ]
    .into_iter()
    .filter_map(|(label, key)| {
        let value = obj.get(key).and_then(Value::as_str)?;
        (!value.is_empty()).then(|| SettingPair {
            label: label.to_owned(),
            value: value.to_owned(),
        })
    })
    .collect();
    (!pairs.is_empty()).then_some(pairs)
}

/// An array-of-strings field, `None` when the key is absent or not an array.
fn string_list(obj: &Value, key: &str) -> Option<Vec<String>> {
    obj.get(key).and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    })
}

/// Record the compaction verdict from a `system/status` record.
///
/// Claude emits two status records: an in-progress `status:"compacting"` with no
/// `compact_result`, then the verdict with `status:null` and `compact_result`
/// set. Keying on `compact_result` being a **string** is what distinguishes
/// them — treating the in-progress record as a verdict would classify every
/// compaction by whichever field happened to be absent.
fn record_compaction_verdict(obj: &Value, state: &mut ParserState) {
    let Some(result) = obj.get("compact_result").and_then(Value::as_str) else {
        return;
    };
    state.compaction_verdict = Some(match result {
        "success" => CompactionVerdict::Succeeded,
        // Any non-success value is a failure, including one we have never seen:
        // an unrecognized verdict must not fall through to "it worked."
        //
        // A blank `compact_error` is treated as absent, not as an empty message:
        // an empty string here would win the precedence at `result` and mask the
        // result record's own diagnostic.
        _ => CompactionVerdict::Failed {
            message: obj
                .get("compact_error")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(str::to_owned),
        },
    });
}

/// Record before/after occupancy from a `system/compact_boundary`'s
/// `compact_metadata`, whenever it arrives.
///
/// Deliberately **not** conditioned on a verdict having been seen — see
/// [`ParserState::compaction_occupancy`]. Recording a boundary cannot make a
/// turn succeed: [`parse_result`] reads this only when the verdict says success,
/// so a boundary with no verdict still terminates as a failure with no usage.
fn record_compaction_occupancy(obj: &Value, state: &mut ParserState) {
    let Some(metadata) = obj.get("compact_metadata") else {
        return;
    };
    let (Some(pre_tokens), Some(post_tokens)) = (
        metadata.get("pre_tokens").and_then(Value::as_u64),
        metadata.get("post_tokens").and_then(Value::as_u64),
    ) else {
        return;
    };
    state.compaction_occupancy = Some(CompactionOccupancy {
        pre_tokens,
        post_tokens,
    });
}

fn parse_mcp_server_status(v: &Value) -> Option<McpServerStatus> {
    Some(McpServerStatus {
        name: v.get("name").and_then(Value::as_str)?.to_owned(),
        status: v.get("status").and_then(Value::as_str)?.to_owned(),
        source: v.get("source").and_then(Value::as_str).map(str::to_owned),
    })
}

/// One `init.plugins` entry. Only `name` is required — a plugin with no
/// version renders as a bare name rather than being dropped.
fn parse_plugin_entry(v: &Value) -> Option<PluginEntry> {
    Some(PluginEntry {
        name: v.get("name").and_then(Value::as_str)?.to_owned(),
        version: v.get("version").and_then(Value::as_str).map(str::to_owned),
        source: v.get("source").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Parse an `assistant` envelope: emit `ToolStarted` for each `tool_use`
/// content block. Normal model text is handled at the delta layer in
/// `parse_stream_event` (the envelope arrives after all the deltas), so replaying
/// it here would double-emit. Claude's harness-authored responses are different:
/// they use `message.model: "<synthetic>"` and may emit no deltas, so their
/// non-error text blocks are surfaced directly from the envelope as a defensive
/// fallback. Ordinary Switchboard sends bypass Claude's local-command parser.
///
/// **Auth-failure detection (state-flag pattern).** If the envelope carries
/// top-level `"error": "authentication_failed"`, stash the displayable
/// message on `state.pending_auth_failure` for `parse_result` to consume;
/// do **not** emit a terminal event here. Terminals come only from
/// `parse_result`'s fail-fast failure path or the adapter's EOF emission of
/// the folded result; the stash just refines the failure's `FailureKind`
/// from `HarnessError` to `AuthFailure`.
fn parse_assistant_envelope(obj: &Value, turn_id: TurnId, state: &mut ParserState) -> ParseOutcome {
    let mut events = Vec::new();
    // A compaction's only assistant envelope is the harness-authored
    // `<synthetic>` diagnostic on the refusal path, carrying a zero-valued
    // `usage`. Tracking it would set occupancy to `Some(0)` — and if a window
    // resolved beside it, the sidebar would render a confident 0% for a
    // conversation that was never compacted.
    if !state.compacting() {
        track_assistant_context_usage(obj, state);
    }

    // Track this message's Anthropic id as the turn's durable join key (keep
    // last → the final assistant message's id). Same envelope, same "keep last"
    // discipline as the occupancy above; subagent envelopes never reach here
    // (skipped on `parent_tool_use_id`), so this is the final *non-subagent*
    // message by construction.
    // Announce the turn's dedup identity the first time we see an assistant
    // message id, so a live turn carries its `hydration_key` while streaming.
    //
    // **Never in compaction mode**, and this is the load-bearing half of the
    // gating: the id on that envelope is minted for a synthetic refusal message
    // that exists in no session file. Letting it through would make it the
    // turn's `stable_message_id` (the cost/context sidecar join key) and
    // `first_message_id` (the live↔disk `hydration_key`) — two durable keys
    // pointing at a message nothing can ever join against. Both stay `None` on
    // every compaction terminal, which is what decision 6 and 7's "live-only"
    // properties rest on.
    let message_id = obj
        .get("message")
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .filter(|_| !state.compacting());
    if let Some(id) = message_id {
        state.last_assistant_message_id = Some(id.to_owned());
    }
    let assistant_model = obj
        .get("message")
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str);
    let is_synthetic = assistant_model == Some("<synthetic>");
    let content = obj
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);

    // Keep-last the real assistant model the same way (final non-subagent model
    // is the turn's model). `<synthetic>` identifies harness-authored local
    // output, not a model selection, so never stamp it onto turn metadata.
    if let Some(model) = assistant_model.filter(|model| *model != "<synthetic>") {
        state.last_assistant_model = Some(model.to_owned());
    }

    let envelope_error = obj.get("error").and_then(Value::as_str);
    if state.first_assistant_message_id.is_none()
        && let Some(id) = message_id
    {
        state.first_assistant_message_id = Some(id.to_owned());
        events.push(AdapterEvent::TurnIdentity {
            turn_id,
            message_id: id.to_owned(),
        });
    }
    if envelope_error == Some("authentication_failed") {
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

    // The identity event leads; tool_use blocks follow. A first assistant
    // envelope with no content array still emits the identity (it's decoupled
    // from content) rather than being dropped by an early `Skip`.
    //
    // A compaction contributes no content of any kind: the auth stash and the
    // model keep-last above are the entire reason its envelope is parsed at all.
    if let Some(content) = content.filter(|_| !state.compacting()) {
        // The compaction verdict already carries the refusal text, and the
        // frontend renders it on the failed compaction row — emitting the
        // envelope's copy as content would print the same sentence twice.
        if is_synthetic && envelope_error.is_none() && !state.compacting() {
            append_synthetic_text_events(content, turn_id, state, &mut events);
        }
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
                    facet: crate::claude_code::facets::classify_claude_tool_facet(name, &input),
                    name: name.to_owned(),
                    input,
                });
            }
        }
    }

    match events.len() {
        0 => ParseOutcome::Skip,
        1 => ParseOutcome::Event(events.into_iter().next().expect("len==1")),
        _ => ParseOutcome::Events(events),
    }
}

fn track_assistant_context_usage(obj: &Value, state: &mut ParserState) {
    let Some(usage) = obj
        .get("message")
        .and_then(|message| message.get("usage"))
        .and_then(Value::as_object)
    else {
        return;
    };
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage.get("output_tokens").and_then(Value::as_u64);
    let context_input = input
        .checked_add(cache_read)
        .and_then(|tokens| tokens.checked_add(cache_creation));
    let context_after_turn = context_input
        .zip(output)
        .and_then(|(tokens, output)| tokens.checked_add(output));
    if context_input.is_none()
        || (context_input.is_some() && output.is_some() && context_after_turn.is_none())
    {
        tracing::warn!(
            "Claude parent context-token arithmetic overflow — context utilization unavailable"
        );
    }
    state.last_assistant_context_input_tokens = context_input;
    state.last_assistant_context_tokens_after_turn = context_after_turn;
}

fn append_synthetic_text_events(
    content: &[Value],
    turn_id: TurnId,
    state: &mut ParserState,
    events: &mut Vec<AdapterEvent>,
) {
    let mut emitted_text = false;
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(text) = block.get("text").and_then(Value::as_str) else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        events.push(AdapterEvent::ContentChunk {
            turn_id,
            kind: ContentKind::Text,
            text: if emitted_text {
                format!("\n\n{text}")
            } else {
                text.to_owned()
            },
        });
        emitted_text = true;
        state.text_chunk_emitted_in_turn = true;
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

/// Swallow a context report's synthetic `assistant` envelope and emit the
/// decoded report — the only event a `/context` stream produces.
///
/// The structured object is `context_usage`; the printed table arrives as
/// `local_command_source`, wrapped in the same `<local-command-stdout>` tags the
/// session file uses. Both are handed to the decoder, which prefers the object
/// and keeps the text either way.
///
/// `Skip` when the envelope carries neither, so a future stream that interleaves
/// some other synthetic record into a report run does not produce an empty
/// panel.
fn parse_context_report_envelope(obj: &Value, agent_id: AgentId) -> ParseOutcome {
    let structured = obj.get("context_usage");
    let source = obj.get("local_command_source").and_then(Value::as_str);
    if structured.is_none() && source.is_none() {
        return ParseOutcome::Skip;
    }
    let raw = source.map_or("", |text| strip_local_command_stdout(text).unwrap_or(text));
    ParseOutcome::Event(AdapterEvent::ContextReport {
        agent_id,
        report: crate::context_report::decode(structured, raw),
        // The envelope's own `timestamp`, not our arrival clock. The disk
        // marker takes its time from the same CLI clock, so reading it here is
        // what makes a report show the same moment before and after a reload
        // rather than two readings a network hop apart. Arrival is the fallback
        // for a future stream that stops carrying the field.
        at: envelope_timestamp(obj).unwrap_or_else(Utc::now),
    })
}

/// The CLI's own `timestamp` on a stream record, when it carries a parseable
/// one.
fn envelope_timestamp(obj: &Value) -> Option<DateTime<Utc>> {
    let text = obj.get("timestamp").and_then(Value::as_str)?;
    Some(DateTime::parse_from_rfc3339(text).ok()?.with_timezone(&Utc))
}

fn strip_local_command_stdout(text: &str) -> Option<&str> {
    text.strip_prefix("<local-command-stdout>")
        .and_then(|body| body.strip_suffix("</local-command-stdout>"))
}

fn parse_rate_limit_event(obj: &Value, agent_id: AgentId, state: &mut ParserState) -> ParseOutcome {
    let info = obj.get("rate_limit_info").cloned().unwrap_or(Value::Null);

    // Stash the overage state so the terminal `result` can stamp this turn's
    // `TurnSpend`. `isUsingOverage` is the real-spend signal for Claude (the
    // only harness with cost in v1); `overageResetsAt` (epoch seconds) is the
    // credit-window reset for the marker tooltip. The opaque `info` still rides
    // the event for the Sidebar's Bucket-A rate-limit rendering.
    state.pending_is_overage = info
        .get("isUsingOverage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    state.pending_overage_resets_at = info
        .get("overageResetsAt")
        .and_then(Value::as_i64)
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single());

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

    /// The accessor the adapter reads to stamp the dedup identity onto a
    /// synthesized **failure** `TurnEnd`: `None` before any assistant message,
    /// then the adapter-selected hydration identity (normally the first assistant
    /// `message.id`, keep-first). This is what lets a crashed multi-message turn's
    /// `Failed` row dedup against its on-disk copy.
    #[test]
    fn first_assistant_message_id_accessor_is_keep_first() {
        let mut state = ParserState::default();
        let turn_id = tid();
        assert_eq!(
            state.first_assistant_message_id(),
            None,
            "no identity before any assistant message"
        );

        let first = r#"{"type":"assistant","message":{"id":"msg_first","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#;
        let _ = parse_line(first, turn_id, aid(), &mut state);
        assert_eq!(state.first_assistant_message_id(), Some("msg_first"));

        let second = r#"{"type":"assistant","message":{"id":"msg_second","content":[{"type":"text","text":"done"}]}}"#;
        let _ = parse_line(second, turn_id, aid(), &mut state);
        assert_eq!(
            state.first_assistant_message_id(),
            Some("msg_first"),
            "later assistant messages must not overwrite the first id"
        );
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

    /// Drive a turn to its folded terminal on `model`, returning the stamped effort.
    fn terminal_effort_for(model: &str, dispatched: Option<&str>) -> Option<String> {
        let mut state = ParserState::for_stream(StreamMode::Send, dispatched.map(str::to_owned));
        let turn = tid();
        let assistant = format!(
            r#"{{"type":"assistant","message":{{"id":"m1","model":"{model}","content":[{{"type":"text","text":"ok"}}]}}}}"#
        );
        let _ = parse_line(&assistant, turn, aid(), &mut state);
        let _ = parse_line(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok"}"#,
            turn,
            aid(),
            &mut state,
        );
        match state.take_final_turn_end(turn, TurnOutcome::Completed) {
            Some(AdapterEvent::TurnEnd { effort, .. }) => effort,
            other => panic!("expected a folded TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn terminal_stamps_dispatched_effort_for_models_that_record_it() {
        // Claude's live stream carries no effort at all, so the only source for
        // the live footer is the value we dispatched. Echoing it is sound only
        // where the session file corroborates it — verified for these families.
        for model in [
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-fable-5",
            "claude-fable-5-1",
        ] {
            assert_eq!(
                terminal_effort_for(model, Some("xhigh")),
                Some("xhigh".to_owned()),
                "{model} records effort on disk, so the live stamp must match"
            );
        }
    }

    #[test]
    fn terminal_withholds_effort_for_a_model_with_no_effort_axis() {
        // Haiku accepts `--effort` silently and records NO effort key, so
        // stamping would show a level live that vanishes on reopen. This is the
        // regression that made the ungated stamp wrong for a default picker
        // model (`harness-behavior.md` §3.4).
        assert_eq!(
            terminal_effort_for("claude-haiku-4-5-20251001", Some("max")),
            None
        );
    }

    #[test]
    fn terminal_withholds_effort_for_every_unverified_model() {
        // Default-closed on an EXACT id list. Each of these was admitted by the
        // earlier family-substring match, and none of them was ever probed:
        //   - older generations of a listed family, which §3.4 records as
        //     *executing* at a capped level (whether they record the requested
        //     or the effective value is unprobed — withholding sidesteps it);
        //   - a newer/unknown first-party id, e.g. Mythos, in the catalog but
        //     unreachable through the alias picker and never probed;
        //   - a third-party id that merely contains a family word.
        // All degrade to "blank live, correct on reopen", never to a wrong value.
        for model in [
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-mythos-5",
            "claude-mythos-5-1",
            "some-vendor-opus-proxy",
            "bedrock/anthropic.claude-opus-5",
            "some-vendor-model",
        ] {
            assert_eq!(
                terminal_effort_for(model, Some("max")),
                None,
                "{model} is unverified and must not be echoed live"
            );
        }
    }

    #[test]
    fn terminal_withholds_effort_when_no_model_ran() {
        // An auth/argument failure dies before any assistant record, so there is
        // no resolved model — and no honest effort to report either.
        let mut state = ParserState::for_stream(StreamMode::Send, Some("high".to_owned()));
        let turn = tid();
        match parse_line(
            r#"{"type":"result","is_error":true,"result":"boom"}"#,
            turn,
            aid(),
            &mut state,
        ) {
            ParseOutcome::Event(AdapterEvent::TurnEnd { effort, model, .. }) => {
                assert_eq!(model, None);
                assert_eq!(effort, None);
            }
            other => panic!("expected a failure TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn terminal_effort_is_none_when_the_agent_left_it_unset() {
        // "Default" in the Model settings dialog: no `--effort` is passed, so
        // Claude picks its own level and we have nothing truthful to stamp.
        assert_eq!(terminal_effort_for("claude-opus-5", None), None);
    }

    #[test]
    fn result_success_folds_terminal_and_yields_liveness() {
        // A successful `result` is NOT the terminal (a background-agent
        // dispatch emits one per internal cycle): it folds into the pending
        // stash and surfaces as turn progress. The single terminal comes from
        // `take_final_turn_end` at stream EOF, and the stash is consumed.
        let line = r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":null,"result":"4"}"#;
        let mut state = ParserState::default();
        let turn = tid();
        assert!(matches!(
            parse_line(line, turn, aid(), &mut state),
            ParseOutcome::Event(AdapterEvent::Liveness { .. })
        ));
        match state.take_final_turn_end(turn, TurnOutcome::Completed) {
            Some(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }) => {}
            other => panic!("expected the folded TurnEnd(Completed), got {other:?}"),
        }
        assert!(
            state
                .take_final_turn_end(turn, TurnOutcome::Completed)
                .is_none(),
            "the stash is consumed by the first take"
        );
    }

    #[test]
    fn take_final_turn_end_none_without_any_successful_result() {
        // No result folded → None; the adapter falls back to truncation
        // synthesis at EOF.
        let mut state = ParserState::default();
        assert!(
            state
                .take_final_turn_end(tid(), TurnOutcome::Completed)
                .is_none()
        );
    }

    #[test]
    fn second_result_overwrites_the_folded_terminal_kept_last() {
        // Kept-last is whole-dispatch-correct: each result's telemetry
        // snapshots the dispatch so far, so the final one wins.
        let mut state = ParserState::default();
        let turn = tid();
        let first = r#"{"type":"result","is_error":false,"result":"cycle 1","usage":{"input_tokens":10,"output_tokens":5},"total_cost_usd":0.05}"#;
        let second = r#"{"type":"result","is_error":false,"result":"cycle 2","usage":{"input_tokens":20,"output_tokens":7},"total_cost_usd":0.14}"#;
        let _ = parse_line(first, turn, aid(), &mut state);
        let _ = parse_line(second, turn, aid(), &mut state);
        match state.take_final_turn_end(turn, TurnOutcome::Completed) {
            Some(AdapterEvent::TurnEnd {
                usage: Some(usage), ..
            }) => {
                assert!((usage.total_cost_usd.unwrap() - 0.14).abs() < f64::EPSILON);
                assert_eq!(usage.input_tokens, 20);
            }
            other => panic!("expected folded terminal with the last result's usage, got {other:?}"),
        }
    }

    #[test]
    fn take_final_turn_end_with_failed_outcome_keeps_folded_telemetry() {
        // The exit-status gate: a dirty process exit after an intermediate
        // success result yields a Failed terminal that still carries the
        // folded telemetry (partial work is billed) and the identity keys.
        let mut state = ParserState::default();
        let turn = tid();
        let assistant = r#"{"type":"assistant","message":{"id":"msg_a1","content":[{"type":"text","text":"partial"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let result = r#"{"type":"result","is_error":false,"result":"cycle 1","usage":{"input_tokens":10,"output_tokens":5},"total_cost_usd":0.05}"#;
        let _ = parse_line(assistant, turn, aid(), &mut state);
        let _ = parse_line(result, turn, aid(), &mut state);
        let outcome = TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message: "harness was killed by a signal after an intermediate result".to_owned(),
        };
        match state.take_final_turn_end(turn, outcome) {
            Some(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed { .. },
                usage: Some(usage),
                stable_message_id,
                first_message_id,
                ..
            }) => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(stable_message_id.as_deref(), Some("msg_a1"));
                assert_eq!(first_message_id.as_deref(), Some("msg_a1"));
            }
            other => panic!("expected Failed terminal carrying telemetry, got {other:?}"),
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

    /// Drives a sequence of lines through one shared `ParserState` (one turn)
    /// and returns the folded terminal's usage — the same event the adapter
    /// emits at stream EOF on a clean exit. Occupancy is sourced from the
    /// turn's assistant messages, not the `result` events.
    fn turn_end_usage(lines: &[&str]) -> Option<TurnUsage> {
        let mut state = ParserState::default();
        let turn_id = tid();
        let agent_id = aid();
        for line in lines {
            let _ = parse_line(line, turn_id, agent_id, &mut state);
        }
        match state.take_final_turn_end(turn_id, TurnOutcome::Completed) {
            Some(AdapterEvent::TurnEnd { usage, .. }) => usage,
            other => panic!("expected a folded terminal, got {other:?}"),
        }
    }

    #[test]
    fn result_with_usage_populates_turn_usage() {
        // Token/cost fields come from the `modelUsage` whole-dispatch
        // aggregate (billing-consistent, subagents included); occupancy comes
        // from the assistant message's per-call usage.
        let assistant = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":100,"output_tokens":25,"cache_read_input_tokens":50,"cache_creation_input_tokens":30}}}"#;
        let result = r#"{"type":"result","is_error":false,"api_error_status":null,"result":"ok","model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":25,"cache_read_input_tokens":50,"cache_creation_input_tokens":30},"modelUsage":{"claude-sonnet-4-6":{"inputTokens":100,"outputTokens":25,"cacheReadInputTokens":50,"cacheCreationInputTokens":30,"costUSD":0.05,"contextWindow":200000}},"total_cost_usd":0.05}"#;
        let usage = turn_end_usage(&[assistant, result]).expect("Some(usage)");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 25);
        assert_eq!(usage.cached_input_tokens, Some(50));
        assert_eq!(usage.cache_creation_input_tokens, Some(30));
        // Occupancy from the assistant message: 100 + 50 + 30.
        assert_eq!(usage.context_input_tokens, Some(180));
        assert_eq!(usage.context_tokens_after_turn, Some(205));
        assert_eq!(usage.context_window, Some(200_000));
        assert!((usage.total_cost_usd.unwrap() - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn qualified_usage_key_keeps_assistant_model_as_context_provenance() {
        let mut state = ParserState::default();
        let turn_id = tid();
        let agent_id = aid();
        let assistant = r#"{"type":"assistant","message":{"id":"m1","model":"claude-opus-4-8","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":100,"output_tokens":25}}}"#;
        let result = r#"{"type":"result","is_error":false,"result":"ok","usage":{"input_tokens":100,"output_tokens":25},"modelUsage":{"claude-opus-4-8[1m]":{"inputTokens":100,"outputTokens":25,"contextWindow":1000000}}}"#;

        let _ = parse_line(assistant, turn_id, agent_id, &mut state);
        let _ = parse_line(result, turn_id, agent_id, &mut state);

        match state.take_final_turn_end(turn_id, TurnOutcome::Completed) {
            Some(AdapterEvent::TurnEnd {
                usage: Some(usage),
                context_window_source:
                    Some(crate::events::ContextWindowSource::StreamOnly { model }),
                model: turn_model,
                ..
            }) => {
                assert_eq!(usage.context_window, Some(1_000_000));
                assert_eq!(model, "claude-opus-4-8");
                assert_eq!(turn_model.as_deref(), Some("claude-opus-4-8"));
            }
            other => panic!("expected qualified context window on completed turn, got {other:?}"),
        }
    }

    #[test]
    fn parent_context_overflow_hides_derived_occupancy_without_losing_raw_usage() {
        let assistant = r#"{"type":"assistant","message":{"id":"m1","model":"claude-sonnet-5","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":18446744073709551615,"cache_read_input_tokens":1,"output_tokens":1}}}"#;
        let result = r#"{"type":"result","is_error":false,"result":"ok","usage":{"input_tokens":7,"output_tokens":8},"modelUsage":{"claude-sonnet-5":{"inputTokens":7,"outputTokens":8,"contextWindow":200000}}}"#;

        let usage = turn_end_usage(&[assistant, result]).expect("Some(usage)");

        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 8);
        assert_eq!(usage.context_input_tokens, None);
        assert_eq!(usage.context_tokens_after_turn, None);
        assert_eq!(usage.context_window, Some(200_000));
    }

    #[test]
    fn aggregate_overflow_falls_back_without_poisoning_parent_context() {
        let assistant = r#"{"type":"assistant","message":{"id":"m1","model":"claude-sonnet-5","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":100,"output_tokens":10}}}"#;
        let result = r#"{"type":"result","is_error":false,"result":"ok","usage":{"input_tokens":7,"output_tokens":8},"modelUsage":{"claude-sonnet-5":{"inputTokens":18446744073709551615,"outputTokens":1,"contextWindow":200000},"auxiliary":{"inputTokens":1,"outputTokens":1,"contextWindow":200000}}}"#;

        let usage = turn_end_usage(&[assistant, result]).expect("Some(usage)");

        assert_eq!(usage.input_tokens, 7, "overflowed aggregate falls back");
        assert_eq!(usage.output_tokens, 8);
        assert_eq!(usage.context_input_tokens, Some(100));
        assert_eq!(usage.context_tokens_after_turn, Some(110));
        assert_eq!(usage.context_window, Some(200_000));
    }

    #[test]
    fn multi_model_dispatch_sums_model_usage_across_entries() {
        // A background-agent dispatch on a different model: `modelUsage` gains
        // an entry per model and the turn's tokens are the whole-dispatch sum
        // (probed 2.1.198: total_cost_usd == Σ modelUsage costUSD exactly).
        let result = r#"{"type":"result","is_error":false,"result":"done","model":"claude-sonnet-5","usage":{"input_tokens":1444,"output_tokens":3},"modelUsage":{"claude-sonnet-5":{"inputTokens":6995,"outputTokens":182,"cacheReadInputTokens":87218,"cacheCreationInputTokens":12377,"costUSD":0.1241424,"contextWindow":1000000},"claude-haiku-4-5":{"inputTokens":10,"outputTokens":140,"cacheReadInputTokens":0,"cacheCreationInputTokens":11622,"costUSD":0.0152375,"contextWindow":200000}},"total_cost_usd":0.1393799}"#;
        let usage = turn_end_usage(&[result]).expect("Some(usage)");
        assert_eq!(usage.input_tokens, 7005, "6995 parent + 10 subagent");
        assert_eq!(usage.output_tokens, 322, "182 parent + 140 subagent");
        assert_eq!(usage.cached_input_tokens, Some(87_218));
        assert_eq!(usage.cache_creation_input_tokens, Some(23_999));
        assert!((usage.total_cost_usd.unwrap() - 0.139_379_9).abs() < f64::EPSILON);
        // Context window still follows `select_context_window` (primary model).
        assert_eq!(usage.context_window, Some(1_000_000));
    }

    #[test]
    fn result_usage_without_cache_fields_context_input_is_input_only() {
        // No cache fields on the assistant message → occupancy is input alone.
        let assistant = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":120,"output_tokens":5}}}"#;
        let result = r#"{"type":"result","is_error":false,"api_error_status":null,"result":"ok","usage":{"input_tokens":120,"output_tokens":5}}"#;
        let usage = turn_end_usage(&[assistant, result]).expect("Some(usage)");
        assert_eq!(usage.cached_input_tokens, None);
        assert_eq!(usage.cache_creation_input_tokens, None);
        assert_eq!(usage.context_input_tokens, Some(120));
    }

    #[test]
    fn multi_call_turn_context_input_is_final_call_not_result_sum() {
        // Verified against claude 2.1.161: a turn with a tool call makes two
        // model calls, and `result.usage` reports the per-call SUMS. Using that
        // sum for occupancy double-counts the shared cached prefix. Occupancy
        // must be the FINAL call's prompt size.
        // Call 1 (tool call): prompt 3133 + 16833 + 2422 = 22388.
        let call1 = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}],"usage":{"input_tokens":3133,"cache_read_input_tokens":16833,"cache_creation_input_tokens":2422,"output_tokens":4}}}"#;
        // Call 2 (final answer): prompt 2 + 19255 + 3220 = 22477.
        let call2 = r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":2,"cache_read_input_tokens":19255,"cache_creation_input_tokens":3220,"output_tokens":1}}}"#;
        // result: cumulative sums across both calls (the trap).
        let result = r#"{"type":"result","is_error":false,"result":"done","usage":{"input_tokens":3135,"cache_read_input_tokens":36088,"cache_creation_input_tokens":5642,"output_tokens":85},"modelUsage":{"claude-opus-4-8":{"inputTokens":3135,"contextWindow":1000000}}}"#;
        let usage = turn_end_usage(&[call1, call2, result]).expect("Some(usage)");
        // Final call's prompt, not the result sum (3135 + 36088 + 5642 = 44865,
        // which would ~2x over-report).
        assert_eq!(usage.context_input_tokens, Some(22_477));
        assert_ne!(usage.context_input_tokens, Some(44_865));
        assert_eq!(usage.context_tokens_after_turn, Some(22_478));
        assert_ne!(
            usage.context_tokens_after_turn,
            Some(22_477 + 85),
            "whole-dispatch output must not be mixed into final-parent occupancy"
        );
        assert_eq!(usage.context_window, Some(1_000_000));
    }

    #[test]
    fn result_with_empty_model_usage_falls_back_to_per_cycle_usage() {
        // Empty `modelUsage` → no aggregate to sum and no context window; the
        // per-cycle `result.usage` shape still populates the token fields.
        let line = r#"{"type":"result","is_error":false,"api_error_status":null,"result":"ok","usage":{"input_tokens":10,"output_tokens":3},"modelUsage":{},"total_cost_usd":0.01}"#;
        let usage = turn_end_usage(&[line]).expect("Some(usage)");
        assert_eq!(usage.context_window, None);
        assert_eq!(usage.input_tokens, 10);
    }

    #[test]
    fn model_usage_entry_missing_required_fields_falls_back_to_per_cycle_usage() {
        // All-or-nothing aggregation: an entry without the required
        // `outputTokens` must not yield a silently-undercounted sum — the
        // whole aggregate is rejected and `result.usage` wins.
        let line = r#"{"type":"result","is_error":false,"result":"ok","usage":{"input_tokens":10,"output_tokens":3},"modelUsage":{"claude-opus-4-8":{"inputTokens":3135,"contextWindow":1000000}}}"#;
        let usage = turn_end_usage(&[line]).expect("Some(usage)");
        assert_eq!(
            usage.input_tokens, 10,
            "per-cycle usage, not the partial aggregate"
        );
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(
            usage.context_window,
            Some(1_000_000),
            "context window still selected"
        );
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
        assert_eq!(
            turn_end_usage(&[line]),
            None,
            "no usage schema at all → None, never a fabricated zero-Some"
        );
    }

    #[test]
    fn select_context_window_prefers_final_assistant_model() {
        let result = json!({
            "model": "claude-haiku-4-5",
            "modelUsage": {
                "claude-haiku-4-5": {"inputTokens": 10_000, "contextWindow": 200_000},
                "claude-sonnet-5": {"inputTokens": 50, "contextWindow": 1_000_000}
            }
        });
        assert_eq!(
            select_context_window(&result, Some("claude-sonnet-5"), None, Some(600_000)),
            Some(SelectedContextWindow {
                model: "claude-sonnet-5".to_owned(),
                context_window: 1_000_000,
            })
        );
    }

    #[test]
    fn select_context_window_accepts_one_million_usage_key_qualifier() {
        let result = json!({
            "modelUsage": {
                "claude-opus-4-8[1m]": {
                    "inputTokens": 50,
                    "contextWindow": 1_000_000
                },
                "claude-haiku-4-5": {
                    "inputTokens": 10_000,
                    "contextWindow": 200_000
                }
            }
        });
        assert_eq!(
            select_context_window(&result, Some("claude-opus-4-8"), None, Some(600_000)),
            Some(SelectedContextWindow {
                model: "claude-opus-4-8".to_owned(),
                context_window: 1_000_000,
            })
        );
    }

    #[test]
    fn select_context_window_prefers_exact_key_over_qualified_key() {
        let result = json!({
            "modelUsage": {
                "claude-opus-4-8": {"inputTokens": 50, "contextWindow": 200_000},
                "claude-opus-4-8[1m]": {"inputTokens": 50, "contextWindow": 1_000_000}
            }
        });
        assert_eq!(
            select_context_window(&result, Some("claude-opus-4-8"), None, Some(100_000)),
            Some(SelectedContextWindow {
                model: "claude-opus-4-8".to_owned(),
                context_window: 200_000,
            })
        );
    }

    #[test]
    fn select_context_window_does_not_accept_other_key_qualifiers() {
        let result = json!({
            "modelUsage": {
                "claude-opus-4-8[beta]": {
                    "inputTokens": 50,
                    "contextWindow": 1_000_000
                }
            }
        });
        assert_eq!(
            select_context_window(&result, Some("claude-opus-4-8"), None, Some(100_000)),
            None
        );
    }

    #[test]
    fn select_context_window_does_not_guess_across_multiple_entries() {
        let result = json!({
            "modelUsage": {
                "subordinate": {"inputTokens": 50, "contextWindow": 64000},
                "primary": {"inputTokens": 5000, "contextWindow": 200_000}
            }
        });
        assert_eq!(
            select_context_window(&result, None, None, Some(10_000)),
            None
        );
    }

    #[test]
    fn select_context_window_uses_sole_entry_without_model_identifiers() {
        let result = json!({
            "modelUsage": {
                "claude-sonnet-5": {"inputTokens": 5, "contextWindow": 1_000_000}
            }
        });
        assert_eq!(
            select_context_window(&result, None, None, Some(600_000)),
            Some(SelectedContextWindow {
                model: "claude-sonnet-5".to_owned(),
                context_window: 1_000_000,
            })
        );
    }

    #[test]
    fn select_context_window_normalizes_only_one_million_qualified_sole_entry() {
        let qualified = json!({
            "modelUsage": {
                "claude-opus-4-8[1m]": {
                    "inputTokens": 5,
                    "contextWindow": 1_000_000
                }
            }
        });
        assert_eq!(
            select_context_window(&qualified, None, None, Some(600_000)),
            Some(SelectedContextWindow {
                model: "claude-opus-4-8".to_owned(),
                context_window: 1_000_000,
            })
        );

        let unrelated = json!({
            "modelUsage": {
                "claude-opus-4-8[beta]": {
                    "inputTokens": 5,
                    "contextWindow": 200_000
                }
            }
        });
        assert_eq!(
            select_context_window(&unrelated, None, None, Some(100_000)),
            Some(SelectedContextWindow {
                model: "claude-opus-4-8[beta]".to_owned(),
                context_window: 200_000,
            })
        );
    }

    #[test]
    fn select_context_window_does_not_fall_through_on_model_mismatch() {
        let result = json!({
            "model": "missing-result-model",
            "modelUsage": {
                "claude-sonnet-5": {"inputTokens": 5, "contextWindow": 1_000_000}
            }
        });
        assert_eq!(
            select_context_window(&result, Some("missing-assistant-model"), None, Some(10)),
            None
        );
    }

    #[test]
    fn select_context_window_rejects_window_smaller_than_parent_occupancy() {
        let result = json!({
            "modelUsage": {
                "claude-sonnet-5": {"inputTokens": 5, "contextWindow": 200_000}
            }
        });
        assert_eq!(
            select_context_window(&result, Some("claude-sonnet-5"), None, Some(596_970)),
            None
        );
    }

    #[test]
    fn select_context_window_empty_modelusage_returns_none() {
        let result = json!({"modelUsage": {}});
        assert_eq!(select_context_window(&result, None, None, None), None);
    }

    #[test]
    fn select_context_window_missing_modelusage_returns_none() {
        let result = json!({"result": "ok"});
        assert_eq!(select_context_window(&result, None, None, None), None);
    }

    /// The inventory off a `system/init` line, or a panic naming what came
    /// out instead.
    fn init_inventory(line: &str) -> SessionInventory {
        match parse_one_with_agent(line, tid(), aid()) {
            ParseOutcome::Event(AdapterEvent::SessionMeta { inventory, .. }) => inventory,
            other => panic!("expected SessionMeta, got {other:?}"),
        }
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
                inventory,
                source,
                ..
            }) => {
                assert_eq!(aid_out, agent_id);
                assert_eq!(model, "claude-sonnet-4-6");
                assert_eq!(harness_version, "2.1.140");
                assert_eq!(
                    inventory.tools,
                    Some(vec![
                        "Bash".to_owned(),
                        "Read".to_owned(),
                        "mcp__srv__do".to_owned()
                    ])
                );
                let servers = inventory.mcp_servers.expect("mcp_servers reported");
                assert_eq!(servers.len(), 1);
                assert_eq!(servers[0].name, "srv");
                assert_eq!(servers[0].status, "connected");
                assert_eq!(
                    servers[0].source, None,
                    "an init without `source` carries none"
                );
                assert_eq!(
                    inventory.skills,
                    Some(vec![SkillEntry::from_name("debug".to_owned())])
                );
                // Claude's inventory has no session-file analog, so it must be
                // cached for restart continuity.
                assert_eq!(source, SessionMetaSource::StreamOnly);
            }
            _ => panic!("expected SessionMeta"),
        }
    }

    #[test]
    fn system_init_carries_the_whole_environment_inventory() {
        let line = r#"{"type":"system","subtype":"init","model":"claude-fable-5-1","mcp_servers":[{"name":"tiddly","status":"needs-auth","source":"claudeai"}],"agents":["Explore","Plan"],"plugins":[{"name":"anthropic-skills","path":"/p","source":"marketplace","version":"0.0.1"}],"memory_paths":{"auto":"/home/me/.claude/memory"},"slash_commands":["init","review"],"skills":["dataviz"],"permissionMode":"bypassPermissions","output_style":"default"}"#;
        let inventory = init_inventory(line);

        let servers = inventory.mcp_servers.expect("mcp_servers reported");
        assert_eq!(servers[0].status, "needs-auth");
        assert_eq!(servers[0].source.as_deref(), Some("claudeai"));
        assert_eq!(
            inventory.agents,
            Some(vec!["Explore".to_owned(), "Plan".to_owned()])
        );
        assert_eq!(
            inventory.plugins,
            Some(vec![PluginEntry {
                name: "anthropic-skills".to_owned(),
                version: Some("0.0.1".to_owned()),
                source: Some("marketplace".to_owned()),
            }])
        );
        // `memory_paths` is a map of kind → path; the paths are its values.
        assert_eq!(
            inventory.memory_paths,
            Some(vec!["/home/me/.claude/memory".to_owned()])
        );
        assert_eq!(
            inventory.slash_commands,
            Some(vec!["init".to_owned(), "review".to_owned()])
        );
        assert_eq!(
            inventory.settings,
            Some(vec![
                SettingPair {
                    label: "Permission mode".to_owned(),
                    value: "bypassPermissions".to_owned(),
                },
                SettingPair {
                    label: "Output style".to_owned(),
                    value: "default".to_owned(),
                },
            ])
        );
        assert_eq!(
            inventory.approved_commands, None,
            "the approved-command allowlist is Codex's; Claude reports none"
        );
    }

    #[test]
    fn an_older_init_without_the_inventory_keys_reports_nothing() {
        // Absent is not empty: only `None` lets the config loaders fill the
        // list on a later reload, which is the right degradation for a CLI
        // that never emitted the key. Reporting `Some([])` here would claim an
        // authoritative zero the harness never stated.
        let inventory =
            init_inventory(r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-6"}"#);
        assert!(
            inventory.is_empty(),
            "every list must be absent, got {inventory:?}"
        );
    }

    #[test]
    fn an_init_reporting_an_empty_list_is_authoritative() {
        // The other side of the same distinction: an `init` that says "zero
        // MCP servers" means none are connected, and that must survive the
        // merge instead of being topped up from a config file.
        let inventory = init_inventory(
            r#"{"type":"system","subtype":"init","model":"m","mcp_servers":[],"skills":[]}"#,
        );
        assert_eq!(inventory.mcp_servers, Some(vec![]));
        assert_eq!(inventory.skills, Some(vec![]));
    }

    #[test]
    fn a_plugin_without_a_version_keeps_its_name() {
        let inventory = init_inventory(
            r#"{"type":"system","subtype":"init","model":"m","plugins":[{"name":"bare"},{"path":"/no-name"}]}"#,
        );
        assert_eq!(
            inventory.plugins,
            Some(vec![PluginEntry {
                name: "bare".to_owned(),
                version: None,
                source: None,
            }]),
            "a nameless entry is unrenderable and dropped; a version-less one is not"
        );
    }

    #[test]
    fn init_settings_skip_absent_and_blank_keys() {
        let only_mode = init_inventory(
            r#"{"type":"system","subtype":"init","model":"m","permissionMode":"plan","output_style":""}"#,
        );
        assert_eq!(
            only_mode.settings,
            Some(vec![SettingPair {
                label: "Permission mode".to_owned(),
                value: "plan".to_owned(),
            }]),
            "a blank value renders nothing rather than an empty row"
        );
        let neither = init_inventory(r#"{"type":"system","subtype":"init","model":"m"}"#);
        assert_eq!(
            neither.settings, None,
            "no readable setting → no settings line at all"
        );
    }

    #[test]
    fn system_non_init_subtypes_yield_liveness() {
        // Denylist posture: `init` is the only system subtype with a semantic
        // event; everything else — including subtypes that don't exist yet —
        // is proof the process is alive. The task-lifecycle vocabulary has
        // already drifted across CLI versions (`task_updated` appeared at
        // 2.1.198), so an allowlist would silently stop re-arming the
        // heartbeat on the next rename.
        for line in [
            r#"{"type":"system","subtype":"compact_boundary","data":{}}"#,
            r#"{"type":"system","subtype":"task_started","task_id":"a1","description":"work"}"#,
            r#"{"type":"system","subtype":"task_updated","task_id":"a1"}"#,
            r#"{"type":"system","subtype":"task_notification","task_id":"a1","status":"completed"}"#,
            r#"{"type":"system","subtype":"totally_new_future_subtype"}"#,
        ] {
            let turn = tid();
            match parse_one(line, turn) {
                ParseOutcome::Event(AdapterEvent::Liveness { turn_id }) => {
                    assert_eq!(turn_id, turn);
                }
                other => panic!("expected Liveness for {line}, got {other:?}"),
            }
        }
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
    fn synthetic_assistant_text_yields_content_without_model_metadata() {
        let fixture = include_str!("../tests/fixtures/claude/local-command.stream.jsonl");
        let mut lines = fixture.lines();
        let line = lines.next().expect("assistant line");
        let turn_id = tid();
        let mut state = ParserState::default();

        let events = match parse_line(line, turn_id, aid(), &mut state) {
            ParseOutcome::Events(events) => events,
            other => panic!("expected identity + synthetic content events, got {other:?}"),
        };
        assert!(matches!(
            &events[0],
            AdapterEvent::TurnIdentity { message_id, .. }
                if message_id == "synthetic-message-id"
        ));
        assert!(matches!(
            &events[1],
            AdapterEvent::ContentChunk { kind: ContentKind::Text, text, .. }
                if text == "/plugin isn't available in this environment."
        ));
        assert_eq!(
            state.first_assistant_message_id(),
            Some("synthetic-message-id")
        );

        let result = lines.next().expect("result line");
        assert!(matches!(
            parse_line(result, turn_id, aid(), &mut state),
            ParseOutcome::Event(AdapterEvent::Liveness { .. })
        ));
        assert_eq!(
            state.first_assistant_message_id(),
            Some("synthetic-message-id")
        );
        assert_eq!(state.last_assistant_model, None);
    }

    #[test]
    fn synthetic_error_text_is_not_emitted_as_content() {
        let line = r#"{"type":"assistant","error":"authentication_failed","message":{"id":"synthetic-auth","model":"<synthetic>","content":[{"type":"text","text":"Not logged in"}]}}"#;
        let turn_id = tid();
        let mut state = ParserState::default();

        let events = match parse_line(line, turn_id, aid(), &mut state) {
            ParseOutcome::Event(event) => vec![event],
            ParseOutcome::Events(events) => events,
            ParseOutcome::Skip => Vec::new(),
            ParseOutcome::Error(error) => panic!("unexpected parse error: {error}"),
        };

        assert!(
            events
                .iter()
                .all(|event| !matches!(event, AdapterEvent::ContentChunk { .. }))
        );
        assert_eq!(
            state.pending_auth_failure.as_deref(),
            Some(CLAUDE_AUTH_MESSAGE)
        );
        assert_eq!(state.last_assistant_model, None);
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

    /// Drive a turn's `rate_limit_event` then its `result` through one shared
    /// state (the stream order — rate-limit precedes result), returning the
    /// terminal `TurnEnd`'s `spend`. Models how `run_producer` feeds the parser.
    fn turn_end_spend(rate_limit_info: &str) -> Option<TurnSpend> {
        let mut state = ParserState::default();
        let turn_id = tid();
        let agent_id = aid();
        let rl = format!(r#"{{"type":"rate_limit_event","rate_limit_info":{rate_limit_info}}}"#);
        parse_line(&rl, turn_id, agent_id, &mut state);
        let result = r#"{"type":"result","is_error":false,"api_error_status":null,"result":"ok","usage":{"input_tokens":10,"output_tokens":5}}"#;
        let _ = parse_line(result, turn_id, agent_id, &mut state);
        match state.take_final_turn_end(turn_id, TurnOutcome::Completed) {
            Some(AdapterEvent::TurnEnd { spend, .. }) => spend,
            other => panic!("expected the folded terminal, got {other:?}"),
        }
    }

    #[test]
    fn overage_rate_limit_stamps_turn_as_real_spend() {
        // An overage rate-limit (isUsingOverage:true) seen before the result
        // stamps the turn as real spend, with the overage reset for the marker.
        let spend = turn_end_spend(r#"{"isUsingOverage":true,"overageResetsAt":1778701800}"#)
            .expect("Claude turns carry spend");
        assert!(spend.real_spend, "overage turn is real spend");
        assert!(spend.is_overage);
        assert!(
            spend.overage_resets_at.is_some(),
            "overageResetsAt is parsed for the marker tooltip"
        );
    }

    #[test]
    fn normal_rate_limit_stamps_turn_as_no_real_spend() {
        // A normal-quota rate-limit → not real spend → the message shows no cost
        // and no marker (subscription cost is notional unless in overage).
        let spend = turn_end_spend(
            r#"{"status":"allowed","resetsAt":1778701800,"rateLimitType":"five_hour","isUsingOverage":false}"#,
        )
        .expect("Claude turns carry spend");
        assert!(!spend.real_spend);
        assert!(!spend.is_overage);
        assert!(spend.overage_resets_at.is_none());
    }

    #[test]
    fn thinking_delta_empty_yields_liveness() {
        // An empty thinking delta (Opus 4.8 redacts reasoning to "") is not
        // surfaced as content — but it is a sign the harness is alive, so it
        // produces a non-rendering Liveness event to re-arm the frontend
        // heartbeat.
        let turn = tid();
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}}"#;
        match parse_one(line, turn) {
            ParseOutcome::Event(AdapterEvent::Liveness { turn_id }) => assert_eq!(turn_id, turn),
            other => panic!("expected Liveness, got {other:?}"),
        }
    }

    #[test]
    fn thinking_delta_with_text_yields_thinking_chunk() {
        // Non-empty reasoning (Sonnet 4.6 streams it; Opus 4.8 redacts) must
        // flow through as `Thinking` content rather than being dropped.
        let turn = tid();
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"deliberating"}}}"#;
        match parse_one(line, turn) {
            ParseOutcome::Event(AdapterEvent::ContentChunk {
                turn_id,
                kind: ContentKind::Thinking,
                text,
            }) => {
                assert_eq!(turn_id, turn);
                assert_eq!(text, "deliberating");
            }
            other => panic!("expected ContentChunk(Thinking), got {other:?}"),
        }
    }

    #[test]
    fn signature_delta_yields_liveness() {
        let turn = tid();
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}}"#;
        match parse_one(line, turn) {
            ParseOutcome::Event(AdapterEvent::Liveness { turn_id }) => assert_eq!(turn_id, turn),
            other => panic!("expected Liveness, got {other:?}"),
        }
    }

    #[test]
    fn input_json_delta_yields_liveness() {
        // Streaming a tool call's arguments is a sign the harness is alive — and
        // it can run for many seconds before `tool_started` (emitted from the
        // completed assistant envelope) arrives, so it must re-arm the heartbeat
        // rather than counting as silence. It carries no renderable content.
        let turn = tid();
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}}"#;
        match parse_one(line, turn) {
            ParseOutcome::Event(AdapterEvent::Liveness { turn_id }) => assert_eq!(turn_id, turn),
            other => panic!("expected Liveness, got {other:?}"),
        }
    }

    #[test]
    fn unknown_delta_type_is_skipped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"some_future_delta"}}}"#;
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
    fn input_json_delta_tool_input_yields_liveness() {
        // Tool-input streaming is a liveness signal (it can run for seconds
        // before `tool_started` arrives from the completed assistant envelope);
        // it carries no renderable content.
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{"}}}"#;
        assert!(matches!(
            parse_one(line, tid()),
            ParseOutcome::Event(AdapterEvent::Liveness { .. })
        ));
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
    fn result_missing_error_fields_defaults_to_success_fold() {
        // No `is_error` / `api_error_status` fields → treated as a successful
        // result: folded (Liveness), terminal Completed at EOF.
        let line = r#"{"type":"result","result":"ok"}"#;
        let mut state = ParserState::default();
        let turn = tid();
        assert!(matches!(
            parse_line(line, turn, aid(), &mut state),
            ParseOutcome::Event(AdapterEvent::Liveness { .. })
        ));
        assert!(matches!(
            state.take_final_turn_end(turn, TurnOutcome::Completed),
            Some(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            })
        ));
    }

    // --- Multi-text-block separator behaviour ---

    /// Collects the answer prose (`Text` chunks only) emitted across a turn —
    /// the surface the paragraph-separator tests below care about. `Thinking`
    /// chunks are excluded: they are not part of the answer text and have no
    /// bearing on text-block separator behavior.
    fn run_turn(lines: &[&str]) -> String {
        let mut state = ParserState::default();
        let turn_id = tid();
        let agent_id = aid();
        let mut out = String::new();
        for line in lines {
            if let ParseOutcome::Event(AdapterEvent::ContentChunk {
                text,
                kind: ContentKind::Text,
                ..
            }) = parse_line(line, turn_id, agent_id, &mut state)
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
        let events = replay_fixture(include_str!(
            "../tests/fixtures/claude/subagent-delegation.jsonl"
        ));

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

    /// Collect the events from replaying every line of a Claude live fixture
    /// through `parse_line` under one `ParserState` (one turn), then append
    /// the folded terminal exactly as the adapter does at stream EOF on a
    /// clean exit — so the returned sequence is what a consumer actually sees.
    fn replay_fixture(fixture: &str) -> Vec<AdapterEvent> {
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
        if let Some(end) = state.take_final_turn_end(turn_id, TurnOutcome::Completed) {
            events.push(end);
        }
        events
    }

    fn turn_end_stable_id(events: &[AdapterEvent]) -> Option<String> {
        events.iter().find_map(|e| match e {
            AdapterEvent::TurnEnd {
                stable_message_id, ..
            } => Some(stable_message_id.clone()),
            _ => None,
        })?
    }

    fn turn_end_first_id(events: &[AdapterEvent]) -> Option<String> {
        events.iter().find_map(|e| match e {
            AdapterEvent::TurnEnd {
                first_message_id, ..
            } => Some(first_message_id.clone()),
            _ => None,
        })?
    }

    /// An ordinary model turn emits `TurnIdentity` **once**, at the first
    /// assistant message, carrying the first non-subagent `message.id` — the same
    /// value `TurnEnd` carries — before the terminal `TurnEnd`.
    #[test]
    fn emits_turn_identity_once_before_turn_end() {
        let events = replay_fixture(include_str!("../tests/fixtures/claude/tool-use.jsonl"));
        let identities: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AdapterEvent::TurnIdentity { message_id, .. } => Some(message_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            identities,
            vec!["msg_test02"],
            "exactly one identity, the first assistant message id"
        );

        let identity_pos = events
            .iter()
            .position(|e| matches!(e, AdapterEvent::TurnIdentity { .. }))
            .expect("a TurnIdentity event");
        let end_pos = events
            .iter()
            .position(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
            .expect("a terminal TurnEnd");
        assert!(
            identity_pos < end_pos,
            "identity is announced before turn end"
        );

        // The early identity and the turn-end key are the same value.
        assert_eq!(turn_end_first_id(&events).as_deref(), Some("msg_test02"));
    }

    /// The production-common shape: a plain *text* assistant envelope that
    /// carries a `message.id` emits `TurnIdentity` (and no `ToolStarted`). The
    /// identity is decoupled from tool content — the existing "no tool event"
    /// test uses an envelope *without* an id, so it `Skip`s and doesn't exercise
    /// this path that the parser refactor introduced.
    #[test]
    fn text_only_assistant_envelope_with_id_emits_turn_identity() {
        let line = r#"{"type":"assistant","message":{"id":"msg_abc","content":[{"type":"text","text":"hello"}]}}"#;
        match parse_one(line, tid()) {
            ParseOutcome::Event(AdapterEvent::TurnIdentity { message_id, .. }) => {
                assert_eq!(message_id, "msg_abc");
            }
            other => panic!("expected a single TurnIdentity event, got {other:?}"),
        }
    }

    /// The two anchors (live side), multi-assistant tool-use turn. The turn has
    /// two non-subagent assistant messages — `msg_test02` (`tool_use`) then
    /// `msg_test03` (final answer) — and cost arrives on the terminal `result`.
    /// `stable_message_id` (cost-join) anchors on the **final** message;
    /// `first_message_id` (dedup identity → `hydration_key`) anchors on the
    /// **first**. Pairs with the session-file side
    /// (`session_file.rs::hydrated_tool_use_turn_anchors_keys_first_and_final`):
    /// both paths must agree on each anchor for live↔disk dedup and the cost
    /// join to hold on reopen.
    #[test]
    fn tool_use_turn_anchors_keys_first_and_final() {
        let events = replay_fixture(include_str!("../tests/fixtures/claude/tool-use.jsonl"));
        assert_eq!(
            turn_end_stable_id(&events),
            Some("msg_test03".to_owned()),
            "cost-join key must be the final non-subagent assistant message (not msg_test02)"
        );
        assert_eq!(
            turn_end_first_id(&events),
            Some("msg_test02".to_owned()),
            "dedup identity must be the first non-subagent assistant message (not msg_test03)"
        );
    }

    /// Join-key parity (live side), subagent-delegation turn. The *last*
    /// assistant envelope in the stream (`msg_…a003`) is a **subagent** message
    /// (`parent_tool_use_id` set) and is skipped, so keep-last must fall back to
    /// the parent's `msg_…a001`. This guards the live exclusion mechanism that
    /// keeps the join key in sync with the disk side, where Claude structurally
    /// omits subagent records from the main session file (so the disk loader
    /// never sees `a003` at all and lands on `a001` by construction).
    #[test]
    fn subagent_turn_anchors_stable_id_on_final_non_subagent_message() {
        let events = replay_fixture(include_str!(
            "../tests/fixtures/claude/subagent-delegation.jsonl"
        ));
        assert_eq!(
            turn_end_stable_id(&events),
            Some("msg_00000000000000000000a001".to_owned()),
            "the subagent envelope's id must not win the join key; keep-last skips it"
        );
        assert_eq!(
            turn_end_first_id(&events),
            Some("msg_00000000000000000000a001".to_owned()),
            "synthetic tool-use envelopes must retain message.id as their hydration identity"
        );
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

        // Absent entirely (e.g. `result` events) → process normally (a
        // successful result folds and surfaces as Liveness, not Skip).
        let line = r#"{"type":"result","is_error":false,"result":"done"}"#;
        assert!(matches!(
            parse_one(line, tid()),
            ParseOutcome::Event(AdapterEvent::Liveness { .. })
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
        // state, regardless of what happened earlier on a different state — a
        // successful result folds (Liveness) instead of failing fast.
        let mut state2 = ParserState::default();
        let turn_id_2 = tid();
        let success_line = r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":null,"result":"ack"}"#;
        match parse_line(success_line, turn_id_2, agent_id, &mut state2) {
            ParseOutcome::Event(AdapterEvent::Liveness { .. }) => {}
            other => panic!("expected the result to fold on the second dispatch, got {other:?}"),
        }
        assert!(matches!(
            state2.take_final_turn_end(turn_id_2, TurnOutcome::Completed),
            Some(AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            })
        ));
    }
}

/// Compaction-mode parsing, driven from the two real captures in
/// `tests/fixtures/claude/` (Claude 2.1.270, Switchboard's exact argv).
///
/// The property under test throughout: **the verdict is the `system/status`
/// pair, never `result`** — both captures carry `result.subtype:"success"`,
/// `is_error:false` and exit 0, so a parser that classified off `result` would
/// report the refusal as a completed turn.
#[cfg(test)]
mod compaction_tests {
    use super::*;
    use crate::events::ContextWindowSource;

    const SUCCESS: &str = include_str!("../tests/fixtures/claude/compaction-success.jsonl");
    const TOO_SMALL: &str = include_str!("../tests/fixtures/claude/compaction-too-small.jsonl");

    fn tid() -> TurnId {
        uuid::Uuid::nil()
    }

    fn aid() -> AgentId {
        AgentId::from(uuid::Uuid::nil())
    }

    /// Replay a stream in `mode`, emitting the folded terminal at EOF with
    /// `exit_outcome` — the adapter's exit-status gate, which the caller owns.
    fn replay_with(
        mode: StreamMode,
        fixture: &str,
        exit_outcome: TurnOutcome,
    ) -> Vec<AdapterEvent> {
        let mut state = ParserState::for_stream(mode, None);
        let (turn_id, agent_id) = (tid(), aid());
        let mut events: Vec<AdapterEvent> = Vec::new();
        for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
            match parse_line(line, turn_id, agent_id, &mut state) {
                ParseOutcome::Event(ev) => events.push(ev),
                ParseOutcome::Events(evs) => events.extend(evs),
                ParseOutcome::Skip => {}
                ParseOutcome::Error(e) => panic!("unexpected parse error: {e}"),
            }
        }
        if let Some(end) = state.take_final_turn_end(turn_id, exit_outcome) {
            events.push(end);
        }
        events
    }

    fn compact(fixture: &str) -> Vec<AdapterEvent> {
        replay_with(StreamMode::Compaction, fixture, TurnOutcome::Completed)
    }

    fn terminals(events: &[AdapterEvent]) -> Vec<&AdapterEvent> {
        events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
            .collect()
    }

    fn sole_terminal(events: &[AdapterEvent]) -> &AdapterEvent {
        let found = terminals(events);
        assert_eq!(found.len(), 1, "exactly one terminal per turn");
        found[0]
    }

    #[test]
    fn success_folds_and_completes_with_occupancy_from_compact_metadata() {
        let events = compact(SUCCESS);
        let AdapterEvent::TurnEnd {
            outcome,
            usage,
            context_window_source,
            model,
            stable_message_id,
            first_message_id,
            ..
        } = sole_terminal(&events)
        else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(*outcome, TurnOutcome::Completed);
        let usage = usage
            .as_ref()
            .expect("a successful compaction reports usage");
        // Straight from `compact_boundary.compact_metadata` — the turn makes no
        // assistant call, so this is the only occupancy source that exists.
        assert_eq!(usage.context_input_tokens, Some(23_423));
        assert_eq!(usage.context_tokens_after_turn, Some(4_172));
        // Resolved from the post-compaction `system/init` model, exactly.
        assert_eq!(usage.context_window, Some(1_000_000));
        assert!(matches!(
            context_window_source,
            Some(ContextWindowSource::StreamOnly { model }) if model == "claude-fable-5-1"
        ));
        // Billing telemetry still comes from `result` as usual.
        assert_eq!(usage.total_cost_usd, Some(0.099_425));
        // A compaction names no model: there is no assistant envelope to carry
        // one, and the `system/init` model is deliberately not substituted (it
        // announces the session's model, not evidence of what summarized).
        assert_eq!(*model, None);
        // No durable identity: a compaction produces no assistant message, so
        // neither the hydration key nor the cost-join key may be invented.
        assert_eq!(*stable_message_id, None);
        assert_eq!(*first_message_id, None);
    }

    #[test]
    fn a_folded_success_still_fails_on_a_dirty_exit() {
        // The exit-status gate is the adapter's, and it applies to a compaction
        // exactly as to a send: a folded success is not proof the process
        // finished cleanly.
        let dirty = TurnOutcome::Failed {
            kind: FailureKind::HarnessError,
            message: "harness exited with code 1".to_owned(),
        };
        let events = replay_with(StreamMode::Compaction, SUCCESS, dirty.clone());
        let AdapterEvent::TurnEnd { outcome, usage, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(*outcome, dirty);
        // Partial work is still billed, so the folded telemetry rides the
        // failure terminal — same rule as a send's dirty exit.
        assert!(usage.is_some());
    }

    #[test]
    fn a_refusal_fails_fast_with_the_harness_message_and_no_usage() {
        let events = compact(TOO_SMALL);
        let AdapterEvent::TurnEnd {
            outcome,
            usage,
            stable_message_id,
            first_message_id,
            ..
        } = sole_terminal(&events)
        else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(
            *outcome,
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message: "Not enough messages to compact.".to_owned(),
            },
            "the CLI's own message, classified from the verdict — not from `result`, \
             which reports success"
        );
        // The refusal's `result` carries an empty `modelUsage`, so the extractor
        // falls back to a schema-present all-zero `usage` with no window.
        // Emitting it would blank the context bar after a no-op compaction.
        assert_eq!(*usage, None);
        // The `<synthetic>` refusal envelope carries a `message.id`; it must
        // never become a hydration or cost-join key.
        assert_eq!(*stable_message_id, None);
        assert_eq!(*first_message_id, None);
    }

    #[test]
    fn a_refusal_emits_no_content_and_no_identity() {
        let events = compact(TOO_SMALL);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AdapterEvent::ContentChunk { .. })),
            "the verdict already carries the refusal text — emitting the envelope's \
             copy too would render the same sentence twice"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AdapterEvent::TurnIdentity { .. })),
            "a synthetic message id must never be announced as the turn's dedup key"
        );
    }

    #[test]
    fn a_successful_compaction_emits_no_content() {
        let events = compact(SUCCESS);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AdapterEvent::ContentChunk { .. })),
            "the recap is harness-owned and hydrates from the session file; the live \
             turn contributes no transcript content"
        );
    }

    #[test]
    fn session_meta_and_rate_limit_still_flow_through() {
        let events = compact(SUCCESS);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AdapterEvent::SessionMeta { .. })),
            "the post-compaction `system/init` still refreshes the agent's registry"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AdapterEvent::RateLimitEvent { .. })),
            "rate-limit telemetry is agent-scoped and unaffected by the mode"
        );
    }

    #[test]
    fn a_result_with_no_verdict_fails_closed_rather_than_reporting_success() {
        // `DISABLE_COMPACT=1` (and any future shape in which the command does
        // not run) produces a `result` with no preceding verdict. Written
        // against the absence of evidence, not that one cause.
        let stream = r#"{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, usage, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(
            *outcome,
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message: "compaction did not run".to_owned(),
            },
            "a named harness error, not an AdapterFailure — `FailureKind` drives the \
             frontend's recovery copy, and \"file a bug\" is wrong for something \
             Claude declined"
        );
        assert_eq!(*usage, None);
    }

    #[test]
    fn an_unrecognized_verdict_is_treated_as_a_failure() {
        let stream = r#"{"type":"system","subtype":"status","status":null,"compact_result":"something_new"}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert!(
            matches!(outcome, TurnOutcome::Failed { .. }),
            "an unknown verdict must not fall through to success, got {outcome:?}"
        );
    }

    #[test]
    fn an_auth_failed_compaction_stays_an_auth_failure() {
        // An auth failure never reaches Claude's command handler, so it carries
        // no verdict — and must not be swallowed by the no-verdict arm, which
        // would replace an actionable "run `claude auth login`" with a generic
        // harness error.
        let stream = r#"{"type":"assistant","error":"authentication_failed","message":{"id":"msg_auth","model":"<synthetic>","content":[{"type":"text","text":"Not logged in · Please run /login"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd {
            outcome,
            first_message_id,
            ..
        } = sole_terminal(&events)
        else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(
            *outcome,
            TurnOutcome::Failed {
                kind: FailureKind::AuthFailure,
                message: CLAUDE_AUTH_MESSAGE.to_owned(),
            }
        );
        assert_eq!(
            *first_message_id, None,
            "even on the auth path a compaction mints no hydration key"
        );
    }

    #[test]
    fn the_init_model_resolves_the_window_across_a_multi_entry_model_usage() {
        // The sole-entry fallback cannot help here; only the init model can.
        let stream = r#"{"type":"system","subtype":"status","status":null,"compact_result":"success"}
{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":900,"post_tokens":100}}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"contextWindow":200000},"claude-haiku-4-5-20251001":{"inputTokens":1,"outputTokens":1,"contextWindow":111111}}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { usage, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(
            usage.as_ref().and_then(|u| u.context_window),
            Some(200_000),
            "the parent model's window, not the auxiliary model's"
        );
    }

    #[test]
    fn an_init_model_absent_from_model_usage_resolves_no_window_but_keeps_usage() {
        // Fail closed: never bind a window we cannot name. The row's numbers
        // come from `compact_metadata`, so the turn is still fully reportable —
        // the bar clean-hides until the next turn rather than showing a
        // pre-compaction percentage that is now wrong.
        let stream = r#"{"type":"system","subtype":"status","status":null,"compact_result":"success"}
{"type":"system","subtype":"init","model":"claude-model-that-did-not-bill"}
{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":900,"post_tokens":100}}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"contextWindow":200000}}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd {
            outcome,
            usage,
            context_window_source,
            ..
        } = sole_terminal(&events)
        else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(*outcome, TurnOutcome::Completed);
        let usage = usage.as_ref().expect("usage survives an unresolved window");
        assert_eq!(usage.context_window, None);
        assert_eq!(usage.context_tokens_after_turn, Some(100));
        assert_eq!(*context_window_source, None);
    }

    #[test]
    fn a_window_smaller_than_the_post_compaction_occupancy_is_rejected() {
        // The impossible-window check is fed the compaction's own occupancy, so
        // it stays live on a path that has no assistant call to derive one from.
        let stream = r#"{"type":"system","subtype":"status","status":null,"compact_result":"success"}
{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":900000,"post_tokens":500000}}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"contextWindow":200000}}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { usage, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(usage.as_ref().and_then(|u| u.context_window), None);
    }

    #[test]
    fn a_success_without_a_boundary_completes_with_no_occupancy() {
        // Defensive: the boundary is the only occupancy source, so losing it
        // must cost the numbers, not the turn.
        let stream = r#"{"type":"system","subtype":"status","status":null,"compact_result":"success"}
{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"contextWindow":200000}}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, usage, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(*outcome, TurnOutcome::Completed);
        let usage = usage.as_ref().expect("billing telemetry still present");
        assert_eq!(usage.context_input_tokens, None);
        assert_eq!(usage.context_tokens_after_turn, None);
    }

    #[test]
    fn a_boundary_arriving_before_the_verdict_still_reports_the_occupancy() {
        // The observed order is verdict-then-boundary, but Claude emits both
        // from one routine and that order is not a documented contract. If it
        // ever flips, the compaction must still report its numbers — the
        // alternative is a success that silently renders no counts and hides the
        // context bar, which no offline test would otherwise catch.
        let stream = r#"{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":23423,"post_tokens":4172}}
{"type":"system","subtype":"status","status":null,"compact_result":"success"}
{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"contextWindow":200000}}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, usage, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(*outcome, TurnOutcome::Completed);
        let usage = usage.as_ref().expect("occupancy survives the reordering");
        assert_eq!(usage.context_input_tokens, Some(23_423));
        assert_eq!(usage.context_tokens_after_turn, Some(4_172));
    }

    #[test]
    fn a_boundary_without_a_verdict_never_invents_a_success() {
        // The safety property the decoupling above must not weaken: recording a
        // boundary independently of the verdict cannot make a turn succeed. Only
        // the verdict decides the terminal.
        let stream = r#"{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":23423,"post_tokens":4172}}
{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{"claude-sonnet-5":{"inputTokens":10,"outputTokens":5,"contextWindow":200000}}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, usage, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert!(
            matches!(outcome, TurnOutcome::Failed { .. }),
            "a boundary alone is not a verdict, got {outcome:?}"
        );
        assert_eq!(*usage, None, "and it carries no usage");
    }

    #[test]
    fn a_missing_verdict_carries_the_results_own_diagnostic() {
        // Classification stays fail-closed; the message is best-available.
        // Without this, every cause outside the two captured shapes collapsed to
        // a bare "compaction did not run" and told the user nothing about
        // whether a retry could work.
        let stream = r#"{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"result","subtype":"success","is_error":false,"result":"Compaction is disabled in this environment.","modelUsage":{}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(
            *outcome,
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message: "compaction did not run: Compaction is disabled in this environment."
                    .to_owned(),
            }
        );
    }

    #[test]
    fn an_api_error_before_the_verdict_keeps_its_text() {
        // This arm runs AHEAD of the generic `is_error` / `api_error_status`
        // handling, so an API error that kills a compaction reads as "did not
        // run" plus the CLI's text — the same `HarnessError` kind the generic
        // path would give, plus the fact that nothing was compacted. Asserted so
        // a later reordering of the arms cannot silently drop the API text.
        let stream = r#"{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"result","subtype":"error_during_execution","is_error":true,"result":"API Error: 500 overloaded","modelUsage":{}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        let TurnOutcome::Failed { kind, message } = outcome else {
            panic!("expected a failure, got {outcome:?}");
        };
        assert_eq!(*kind, FailureKind::HarnessError);
        assert!(
            message.contains("API Error: 500 overloaded"),
            "the API error text must survive, got {message:?}"
        );
    }

    #[test]
    fn a_failed_verdict_without_compact_error_falls_back_to_the_result_text() {
        // A blank `compact_error` is treated as absent, not as an empty message —
        // otherwise it would win the precedence and mask the better diagnostic.
        let stream = r#"{"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"   "}
{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"result","subtype":"success","is_error":false,"result":"Quota exhausted.","modelUsage":{}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(
            *outcome,
            TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message: "Quota exhausted.".to_owned(),
            }
        );
    }

    #[test]
    fn a_failure_with_no_diagnostic_anywhere_falls_back_to_authored_copy() {
        let stream = r#"{"type":"system","subtype":"status","status":null,"compact_result":"failed"}
{"type":"result","subtype":"success","is_error":false,"result":"","modelUsage":{}}"#;
        let events = compact(stream);
        let AdapterEvent::TurnEnd { outcome, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        let TurnOutcome::Failed { message, .. } = outcome else {
            panic!("expected a failure, got {outcome:?}");
        };
        assert!(
            !message.trim().is_empty(),
            "a failure must always say something"
        );
    }

    #[test]
    fn outside_compaction_mode_the_same_status_records_stay_liveness() {
        // Pins the existing send-path behavior: the compaction rules are gated
        // on the mode, not on the record shape.
        let events = replay_with(StreamMode::Send, TOO_SMALL, TurnOutcome::Completed);
        let AdapterEvent::TurnEnd { outcome, usage, .. } = sole_terminal(&events) else {
            unreachable!("filtered to TurnEnd");
        };
        assert_eq!(
            *outcome,
            TurnOutcome::Completed,
            "a send reads this stream as an ordinary completed turn — the verdict is \
             meaningless to it"
        );
        assert!(usage.is_some(), "a send keeps `result`'s zero-valued usage");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AdapterEvent::ContentChunk { .. })),
            "a send surfaces synthetic text as content, as it does for `/plugin`"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AdapterEvent::TurnIdentity { .. })),
            "a send still announces its dedup identity"
        );
    }
}

/// Context-report-mode parsing, driven from the recorded `/context` stream in
/// `tests/fixtures/claude/` (claude 2.1.274, Switchboard's exact argv).
///
/// The property under test throughout: **the report's envelope is swallowed
/// whole**. Everything the run produces downstream of the parser is the
/// `ContextReport` event and a terminal — no content, no turn identity, no
/// usage — so nothing further along has to know that report text is not an
/// answer.
#[cfg(test)]
mod context_report_tests {
    use super::*;
    use crate::context_report::ContextReport;

    const REPORT: &str = include_str!("../tests/fixtures/claude/context-report.stream.jsonl");

    fn tid() -> TurnId {
        uuid::Uuid::nil()
    }

    fn aid() -> AgentId {
        AgentId::from(uuid::Uuid::nil())
    }

    fn replay(fixture: &str) -> Vec<AdapterEvent> {
        let mut state = ParserState::for_stream(StreamMode::ContextReport, None);
        let (turn_id, agent_id) = (tid(), aid());
        let mut events: Vec<AdapterEvent> = Vec::new();
        for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
            match parse_line(line, turn_id, agent_id, &mut state) {
                ParseOutcome::Event(ev) => events.push(ev),
                ParseOutcome::Events(evs) => events.extend(evs),
                ParseOutcome::Skip => {}
                ParseOutcome::Error(e) => panic!("unexpected parse error: {e}"),
            }
        }
        if let Some(end) = state.take_final_turn_end(turn_id, TurnOutcome::Completed) {
            events.push(end);
        }
        events
    }

    fn sole_report(events: &[AdapterEvent]) -> &ContextReport {
        let found: Vec<&ContextReport> = events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::ContextReport { report, .. } => Some(report),
                _ => None,
            })
            .collect();
        assert_eq!(found.len(), 1, "exactly one report per run: {events:#?}");
        found[0]
    }

    #[test]
    fn a_report_run_completes_carrying_the_decoded_report() {
        let events = replay(REPORT);

        let report = sole_report(&events);
        assert!(!report.unparsed);
        assert_eq!(report.model.as_deref(), Some("claude-fable-5-1"));
        assert_eq!(report.max_tokens, Some(1_000_000));
        assert_eq!(report.categories.len(), 11);
        assert!(!report.raw.is_empty());

        assert!(
            matches!(
                events.last(),
                Some(AdapterEvent::TurnEnd {
                    outcome: TurnOutcome::Completed,
                    ..
                })
            ),
            "events: {events:#?}"
        );
    }

    #[test]
    fn a_report_is_stamped_with_the_clis_own_time_not_our_arrival() {
        // The disk marker takes its time from the same CLI clock, so reading the
        // envelope's is what makes one report show one moment either side of a
        // reload. Stamping arrival instead drifts by a network hop and, worse,
        // is a different clock entirely.
        let events = replay(REPORT);

        let Some(AdapterEvent::ContextReport { at, .. }) = events
            .iter()
            .find(|event| matches!(event, AdapterEvent::ContextReport { .. }))
        else {
            panic!("expected a report: {events:#?}");
        };
        assert_eq!(
            at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "2026-09-18T15:48:31.451Z"
        );
    }

    #[test]
    fn a_report_whose_envelope_carries_no_time_still_lands() {
        // The fallback exists for a stream that stops carrying the field; the
        // report is far too useful to drop over a missing timestamp.
        let stripped: String = REPORT
            .lines()
            .map(|line| {
                let Ok(mut record) = serde_json::from_str::<Value>(line) else {
                    return line.to_owned();
                };
                if let Some(object) = record.as_object_mut() {
                    object.remove("timestamp");
                }
                record.to_string()
            })
            .collect::<Vec<String>>()
            .join("\n");

        let events = replay(&stripped);

        assert!(!sole_report(&events).unparsed);
    }

    #[test]
    fn a_report_run_emits_no_content_and_no_turn_identity() {
        let events = replay(REPORT);

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AdapterEvent::ContentChunk { .. })),
            "the printed table must not reach the transcript as an answer: {events:#?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AdapterEvent::TurnIdentity { .. })),
            "the synthetic message id names a message no session file holds, so it \
             must never become a dedup key: {events:#?}"
        );
    }

    #[test]
    fn a_report_run_carries_no_usage_and_no_context_window() {
        let events = replay(REPORT);

        let Some(AdapterEvent::TurnEnd {
            usage,
            context_window_source,
            model,
            first_message_id,
            stable_message_id,
            ..
        }) = events.last()
        else {
            panic!("expected a terminal, got {events:#?}");
        };
        // The run makes no model call: `modelUsage` is empty and `result.usage`
        // is schema-present zeros. Emitting that would make the newest
        // usage-bearing turn a free one with no window, blanking the context bar
        // the panel was opened from.
        assert!(usage.is_none(), "usage: {usage:?}");
        assert!(context_window_source.is_none());
        assert!(model.is_none());
        assert!(first_message_id.is_none());
        assert!(stable_message_id.is_none());
    }

    #[test]
    fn a_report_run_still_reports_the_session_inventory() {
        // `system/init` arrives on this dispatch like any other, and the card's
        // environment row should not go stale because the user asked for a
        // breakdown.
        let events = replay(REPORT);

        assert!(
            events
                .iter()
                .any(|event| matches!(event, AdapterEvent::SessionMeta { .. })),
            "events: {events:#?}"
        );
    }

    #[test]
    fn a_report_whose_object_is_garbled_completes_with_the_raw_text() {
        let garbled: String = REPORT
            .lines()
            .map(|line| {
                let Ok(mut record) = serde_json::from_str::<Value>(line) else {
                    return line.to_owned();
                };
                if let Some(object) = record.as_object_mut()
                    && object.contains_key("context_usage")
                {
                    object.insert("context_usage".to_owned(), Value::Bool(true));
                    object.insert(
                        "local_command_source".to_owned(),
                        Value::String(
                            "<local-command-stdout>unreadable</local-command-stdout>".to_owned(),
                        ),
                    );
                }
                record.to_string()
            })
            .collect::<Vec<String>>()
            .join("\n");

        let events = replay(&garbled);

        let report = sole_report(&events);
        assert!(report.unparsed);
        assert_eq!(report.raw, "unreadable");
        assert!(
            matches!(
                events.last(),
                Some(AdapterEvent::TurnEnd {
                    outcome: TurnOutcome::Completed,
                    ..
                })
            ),
            "a report Switchboard cannot read is not a harness failure: {events:#?}"
        );
    }

    #[test]
    fn a_synthetic_envelope_carrying_no_report_is_skipped() {
        // Nothing in a `/context` run produces this today. If a future CLI
        // interleaves some other synthetic record, it must not land as an empty
        // panel that overwrites the previous report.
        let mut state = ParserState::for_stream(StreamMode::ContextReport, None);
        let line = r#"{"type":"assistant","message":{"id":"m1","model":"<synthetic>","content":[{"type":"text","text":"hi"}]}}"#;

        assert!(matches!(
            parse_line(line, tid(), aid(), &mut state),
            ParseOutcome::Skip
        ));
    }

    #[test]
    fn outside_report_mode_the_same_envelope_is_ordinary_assistant_output() {
        // The interception is mode-gated: an ordinary send that somehow carried
        // a `context_usage` key must keep its existing handling.
        let events = {
            let mut state = ParserState::for_stream(StreamMode::Send, None);
            let mut events = Vec::new();
            for line in REPORT.lines().filter(|l| !l.trim().is_empty()) {
                match parse_line(line, tid(), aid(), &mut state) {
                    ParseOutcome::Event(ev) => events.push(ev),
                    ParseOutcome::Events(evs) => events.extend(evs),
                    ParseOutcome::Skip => {}
                    ParseOutcome::Error(e) => panic!("unexpected parse error: {e}"),
                }
            }
            events
        };

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AdapterEvent::ContextReport { .. })),
            "events: {events:#?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AdapterEvent::TurnIdentity { .. })),
            "events: {events:#?}"
        );
    }
}
