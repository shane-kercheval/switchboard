use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use switchboard_core::AgentId;

use crate::events::{
    AdapterEvent, ContentKind, FailureKind, McpServerStatus, ToolKind, TurnId, TurnOutcome,
    TurnSpend, TurnUsage,
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
    /// The **first** assistant message's Anthropic `message.id`, kept-first
    /// (set once per turn, never overwritten). Emitted as the turn's
    /// `first_message_id` → the frontend `hydration_key`. Unlike the last id,
    /// this is stable from the turn's first assistant message, so a live turn and a
    /// mid-flight or completed disk re-read of it all dedup to the same key.
    /// See `AdapterEvent::TurnEnd::first_message_id`.
    first_assistant_message_id: Option<String>,
    /// The most recent assistant message's `message.model`, kept-last the same
    /// way as the id above — so at `TurnEnd` it is the **final non-subagent**
    /// assistant model (a subagent on a different model never reaches here).
    /// Stamped on the live per-turn `TurnEnd.model`; the reopen counterpart
    /// reads the same `message.model` from the session file. Claude exposes no
    /// per-turn effort, so `TurnEnd.effort` stays `None`.
    last_assistant_model: Option<String>,
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
            effort: None,
            stable_message_id: self.last_assistant_message_id.clone(),
            first_message_id: self.first_assistant_message_id.clone(),
        })
    }
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
    // Gemini's `invoke_agent` (already opaque) and Antigravity's
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
        Some("system") => parse_system_event(&value, turn_id, agent_id),
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

    let usage = extract_usage_from_result(obj, state.last_assistant_context_input_tokens);

    // Claude's context window is stream-only (`result.modelUsage`), absent from
    // the session file — so when this turn carries one, tag it `StreamOnly` for
    // the dispatcher to persist to the metadata sidecar. `None` when there's no
    // window (nothing to persist).
    let context_window_source = usage
        .as_ref()
        .and_then(|u| u.context_window)
        .map(|_| crate::events::ContextWindowSource::StreamOnly);

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
    let failure = if let Some(auth_message) = auth_failure {
        Some(TurnOutcome::Failed {
            kind: FailureKind::AuthFailure,
            message: auth_message,
        })
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
            usage,
            context_window_source,
            spend,
            model: state.last_assistant_model.clone(),
            effort: None,
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
        usage,
        context_window_source,
        spend,
    });
    ParseOutcome::Event(AdapterEvent::Liveness { turn_id })
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
) -> Option<TurnUsage> {
    let total_cost_usd = obj.get("total_cost_usd").and_then(Value::as_f64);
    let context_window = select_context_window(obj);

    if let Some(aggregate) = sum_model_usage_tokens(obj) {
        return Some(TurnUsage {
            input_tokens: aggregate.input,
            output_tokens: aggregate.output,
            cached_input_tokens: aggregate.cached_input,
            cache_creation_input_tokens: aggregate.cache_creation,
            context_input_tokens: last_call_context_input_tokens,
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
        aggregate.input += entry.get("inputTokens").and_then(Value::as_u64)?;
        aggregate.output += entry.get("outputTokens").and_then(Value::as_u64)?;
        if let Some(cached) = entry.get("cacheReadInputTokens").and_then(Value::as_u64) {
            *aggregate.cached_input.get_or_insert(0) += cached;
        }
        if let Some(created) = entry
            .get("cacheCreationInputTokens")
            .and_then(Value::as_u64)
        {
            *aggregate.cache_creation.get_or_insert(0) += created;
        }
    }
    Some(aggregate)
}

/// Pick the right `contextWindow` from `result.modelUsage` per the selection
/// rule:
///
/// 1. If `result.model` matches a `modelUsage` key, use that entry.
/// 2. Otherwise, use the entry with the largest `inputTokens` (the primary
///    model did the heavy lifting; routing / subordinate models have minimal
///    tokens).
/// 3. Empty or missing `modelUsage` → `None`.
fn select_context_window(result: &Value) -> Option<u32> {
    let model_usage = result.get("modelUsage").and_then(Value::as_object)?;
    if model_usage.is_empty() {
        return None;
    }

    let primary_model = result.get("model").and_then(Value::as_str);
    if let Some(model) = primary_model
        && let Some(entry) = model_usage.get(model)
        && let Some(cw) = entry.get("contextWindow").and_then(Value::as_u64)
    {
        return u32::try_from(cw).ok();
    }

    let max_entry = model_usage
        .values()
        .max_by_key(|v| v.get("inputTokens").and_then(Value::as_u64).unwrap_or(0))?;
    let cw = max_entry.get("contextWindow").and_then(Value::as_u64)?;
    u32::try_from(cw).ok()
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
fn parse_system_event(obj: &Value, turn_id: TurnId, agent_id: AgentId) -> ParseOutcome {
    if obj.get("subtype").and_then(Value::as_str) != Some("init") {
        return ParseOutcome::Event(AdapterEvent::Liveness { turn_id });
    }

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let harness_version = obj
        .get("claude_code_version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let tools = obj
        .get("tools")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mcp_servers = obj
        .get("mcp_servers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(parse_mcp_server_status)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let skills = obj
        .get("skills")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    ParseOutcome::Event(AdapterEvent::SessionMeta {
        agent_id,
        model,
        harness_version,
        tools,
        mcp_servers,
        skills,
        raw: obj.clone(),
    })
}

fn parse_mcp_server_status(v: &Value) -> Option<McpServerStatus> {
    Some(McpServerStatus {
        name: v.get("name").and_then(Value::as_str)?.to_owned(),
        status: v.get("status").and_then(Value::as_str)?.to_owned(),
    })
}

/// Parse an `assistant` envelope: emit `ToolStarted` for each `tool_use`
/// content block. Text content is handled at the delta layer in
/// `parse_stream_event` (the envelope arrives after all the deltas), so we
/// don't emit `ContentChunk`s from here — that would double-emit.
///
/// **Auth-failure detection (state-flag pattern).** If the envelope carries
/// top-level `"error": "authentication_failed"`, stash the displayable
/// message on `state.pending_auth_failure` for `parse_result` to consume;
/// do **not** emit a terminal event here. Terminals come only from
/// `parse_result`'s fail-fast failure path or the adapter's EOF emission of
/// the folded result; the stash just refines the failure's `FailureKind`
/// from `HarnessError` to `AuthFailure`.
fn parse_assistant_envelope(obj: &Value, turn_id: TurnId, state: &mut ParserState) -> ParseOutcome {
    // Track this model call's prompt size for context-occupancy. Done before
    // the content/early-return below so the final *text-only* answer message
    // (which produces no tool event) still updates the occupancy — its usage
    // reflects the largest, most-recent context. Overwrite → keep last.
    if let Some(usage) = obj
        .get("message")
        .and_then(|m| m.get("usage"))
        .and_then(Value::as_object)
    {
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
        state.last_assistant_context_input_tokens = Some(input + cache_read + cache_creation);
    }

    // Track this message's Anthropic id as the turn's durable join key (keep
    // last → the final assistant message's id). Same envelope, same "keep last"
    // discipline as the occupancy above; subagent envelopes never reach here
    // (skipped on `parent_tool_use_id`), so this is the final *non-subagent*
    // message by construction.
    // Announce the turn's dedup identity the first time we see an assistant
    // message id, so a live turn carries its `hydration_key` while streaming.
    let mut identity_event: Option<AdapterEvent> = None;
    if let Some(id) = obj
        .get("message")
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
    {
        state.last_assistant_message_id = Some(id.to_owned());
        // Keep-*first* (set once): the dedup identity must be the turn's first
        // assistant message, which is invariant across partial vs complete
        // parses — see the field doc.
        if state.first_assistant_message_id.is_none() {
            state.first_assistant_message_id = Some(id.to_owned());
            identity_event = Some(AdapterEvent::TurnIdentity {
                turn_id,
                message_id: id.to_owned(),
            });
        }
    }
    // Keep-last the assistant model the same way (final non-subagent model is
    // the turn's model). Stamped on `TurnEnd.model`.
    if let Some(model) = obj
        .get("message")
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
    {
        state.last_assistant_model = Some(model.to_owned());
    }

    if obj.get("error").and_then(Value::as_str) == Some("authentication_failed") {
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
    let mut events = Vec::new();
    events.extend(identity_event);
    if let Some(content) = obj
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
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
    /// then the *first* assistant `message.id` (keep-first, not overwritten by
    /// later messages). This is what lets a crashed multi-message turn's
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
        assert_eq!(usage.context_window, Some(200_000));
        assert!((usage.total_cost_usd.unwrap() - 0.05).abs() < f64::EPSILON);
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
    fn select_context_window_prefers_top_level_model() {
        // result.model points at the primary; even if the routing model has more
        // input tokens, the primary wins.
        let result = json!({
            "model": "claude-opus-4-7[1m]",
            "modelUsage": {
                "claude-haiku-4-5": {"inputTokens": 10_000, "contextWindow": 200_000},
                "claude-opus-4-7[1m]": {"inputTokens": 50, "contextWindow": 1_000_000}
            }
        });
        assert_eq!(select_context_window(&result), Some(1_000_000));
    }

    #[test]
    fn select_context_window_falls_back_to_largest_input_tokens() {
        // No top-level model field — pick the entry with the largest inputTokens.
        let result = json!({
            "modelUsage": {
                "subordinate": {"inputTokens": 50, "contextWindow": 64000},
                "primary": {"inputTokens": 5000, "contextWindow": 200_000}
            }
        });
        assert_eq!(select_context_window(&result), Some(200_000));
    }

    #[test]
    fn select_context_window_empty_modelusage_returns_none() {
        let result = json!({"modelUsage": {}});
        assert_eq!(select_context_window(&result), None);
    }

    #[test]
    fn select_context_window_missing_modelusage_returns_none() {
        let result = json!({"result": "ok"});
        assert_eq!(select_context_window(&result), None);
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
                tools,
                mcp_servers,
                skills,
                ..
            }) => {
                assert_eq!(aid_out, agent_id);
                assert_eq!(model, "claude-sonnet-4-6");
                assert_eq!(harness_version, "2.1.140");
                assert_eq!(tools, vec!["Bash", "Read", "mcp__srv__do"]);
                assert_eq!(mcp_servers.len(), 1);
                assert_eq!(mcp_servers[0].name, "srv");
                assert_eq!(mcp_servers[0].status, "connected");
                assert_eq!(skills, vec!["debug"]);
            }
            _ => panic!("expected SessionMeta"),
        }
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

    /// `TurnIdentity` is emitted **once**, at the first assistant message,
    /// carrying the first non-subagent `message.id` — the same value `TurnEnd`
    /// carries — and it precedes the terminal `TurnEnd`. This is what lets the
    /// frontend stamp a live turn's `hydration_key` while it is still streaming.
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
