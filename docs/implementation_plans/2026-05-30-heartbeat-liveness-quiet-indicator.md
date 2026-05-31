# Heartbeat liveness + "gone quiet" indicator

Fix for healthy turns being marked **failed** by the frontend liveness timer
("heartbeat") when an alive harness is briefly silent. Two coordinated changes;
neither is M4.10 (rendering) nor M8 (distribution) — this is a standalone
correctness fix, cross-referenced to both.

## Problem

The frontend arms a per-turn liveness timer (`HEARTBEAT_TIMEOUT_MS = 60_000`,
`src/lib/types.ts:384`) that re-arms only on `turn_start | content_chunk |
tool_started | tool_completed` (`src/lib/state/index.svelte.ts:561-595`). On
expiry it synthesizes a `heartbeat_timeout` event that marks the turn **failed**
(`src/lib/state/reducers.ts:242-255`; runtime mirror `:421-434`).

This fires on *healthy* turns: a turn that is alive but produces no listened-for
event for 60s gets marked failed, then completes cleanly seconds later (the
"heartbeat-timeout resurrection" mode, `2026-05-12-v1-m2-agent-adaptors.md:1190`).
Marking it failed on the frontend is doubly wrong: the backend subprocess is
still running and still holds the agent's busy-lock, so the "failure" is a lie
*and* frees nothing (the user still can't send).

## Evidence (live probe, Claude CLI 2.1.158, production flags)

Probed with Switchboard's exact spawn flags (`-p … --output-format stream-json
--include-partial-messages --verbose`, `crates/harness/src/claude_code/mod.rs:147-155`):

1. **Claude's thinking text is redacted to empty.** Every `thinking_delta`
   during an extended-thinking block carried **0 characters** (signature-only).
   There is nothing to render — confirms the M4.10 finding on the current CLI.
2. **Claude emits events throughout the thinking window** — empty `thinking_delta`s
   roughly every ~1.5s, plus periodic `system` events, then a `signature_delta`.
   **The parser drops all of them** (`crates/harness/src/parser.rs:161` returns
   `Skip` for any non-`text_delta`). So the harness was emitting a heartbeat the
   whole time; we manufactured the silence by discarding it.

Conclusion: the reported failure is the parser starving the frontend timer of
liveness signals the harness genuinely sent.

## Change B — emit a liveness keepalive on content-free activity

The harness *is* alive during thinking and tool-input streaming; treat those
events as the heartbeat they are. A content-free `Liveness` event re-arms the
timer and never becomes a transcript item.

- **`crates/harness/src/events.rs`:** add `Liveness { turn_id: TurnId }` to both
  `AdapterEvent` and `NormalizedEvent`, and a pass-through arm in
  `impl From<AdapterEvent> for NormalizedEvent`. Turn-scoped, carries no content.
- **`crates/harness/src/parser.rs` (`parse_content_block_delta`):**
  - `signature_delta` and `input_json_delta` → `Liveness` (both are content-free
    signs of life; tool-input can stream for seconds before `tool_started`
    arrives from the completed assistant envelope, so it must not count as
    silence).
  - `thinking_delta` → **branch on the `"thinking"` field**: empty (today's
    server-redacted reality) → `Liveness`; non-empty → `ContentChunk { kind:
    Thinking, text }`. We deliberately do **not** depend on the redaction: if it
    lifts, real reasoning flows through as `Thinking` content rather than being
    silently dropped, matching what the M4.10 reasoning renderer expects to
    "pick up for free." (Until M4.10's `ThinkingWidget` lands, such text would
    render unstyled and not survive reload — a visible degraded state, far
    better than silent loss; flagged for M4.10.)
- **`crates/dispatcher/src/lib.rs`:** `event.into()` forwards `Liveness`
  generically; no dispatcher change.
- **`src/lib/types.ts`:** add `{ type: "liveness"; turn_id: TurnId }` to the
  `NormalizedEvent` union.
- **`src/lib/state/index.svelte.ts` (`manageHeartbeat`):** add `"liveness"` to
  the re-arm set (same `heartbeat?.turn_id === event.turn_id` guard).

This fixes the *reported* (thinking) failure — periodic keepalives re-arm the
timer continuously.

## Change A — stop failing on silence (transient "quiet" indicator)

The safety net for the one genuinely-silent case a keepalive can't cover: a
single long tool *execution* emits `tool_started`, then **no event at all** until
`tool_completed`. Don't fail it — surface it, after a threshold.

- **Threshold: 1 minute.** `HEARTBEAT_TIMEOUT_MS = 60_000` (kept short because
  the indicator is harmless and the user can always cancel). The heartbeat now
  drives only the soft indicator (no failure), so the threshold is "when to
  surface the silence." Below it, the footer shows plain `Working...` — no
  counter (a counter would otherwise reset on every event and add noise during
  normal work).
- **State on `AgentRuntime`** (not the transcript turn): a transient
  `quiet_since?: string` — the ISO instant the timer fired (when the turn went
  quiet). `undefined` = not quiet.
- **On timer expiry** (`armHeartbeat`'s callback): set `runtime.quiet_since` to
  the fire time via the runtime reducer. Do **not** change the turn's `status`,
  write `last_error`, or clear `in_flight_turn_id`. Keep the heartbeat watch
  alive across expiry (retain the `heartbeats` entry; clear only the fired timer
  handle) so the next activity event re-arms and clears quiet. The load-bearing
  comment at the fire site is updated to describe this.
- **Repurpose the `heartbeat_timeout` reducer cases:** remove the `→ failed`
  transcript case and the runtime mirror that wrote `last_error`; the synthetic
  event now only sets `runtime.quiet_since`.

**Clearing `quiet_since` has an explicit owner: `runtimeReducer`.** Added
`content_chunk` / `liveness` / `tool_started` / `tool_completed` cases clear it
when `event.turn_id === runtime.in_flight_turn_id`; `turn_start` / `turn_end` /
`agent_idle` clear it too. `manageHeartbeat` owns *only* the timer map. The
transcript reducer ignores `liveness` via its default branch (graceful-unknown).

### UI

- The footer (`workingFooter` in `UnifiedTranscript.svelte`) renders per-turn.
  Show the quiet variant only when `rt?.quiet_since !== undefined &&
  rt?.in_flight_turn_id === turn.turn_id` (`rt = runtimes[turn.agent_id]`) —
  `quiet_since` is agent-scoped, so this join prevents painting an unrelated
  streaming turn.
- **Counting-up variant:** `No response (Xm Ys)...` in the amber/warning
  **semantic token** (`text-warning`), never red/failed. Elapsed silence is
  `now - quiet_since + HEARTBEAT_TIMEOUT_MS` (the timer fired one threshold after
  the last activity), so it appears at `1m 00s` and ticks up via a 1s clock.
  Reverts to `Working...` the instant activity resumes.

## Tests

- **Harness, fixture (`parser.rs`):** `thinking_delta_empty_yields_liveness`,
  `thinking_delta_with_text_yields_thinking_chunk`, `signature_delta_yields_liveness`,
  `input_json_delta_yields_liveness`, `unknown_delta_type_is_skipped`; `run_turn`
  helper scoped to `Text` chunks (separator tests are about answer prose). A
  `From` test that `Liveness` maps to `NormalizedEvent::Liveness`.
- **Harness, live (`crates/harness/tests/live.rs`, `live_claude_thinking_emits_liveness`):**
  a thinking-inducing prompt asserts the *adapter* emits a thinking sign-of-life
  (`Liveness` **or** a `Thinking` `ContentChunk` — both re-arm the heartbeat, so
  a benign upstream un-redaction doesn't read as a regression) and the turn
  completes. Adapter-only — harness live tests don't run `manageHeartbeat`/reducers.
- **Frontend state (`index.test.ts`, fake timers):** past-threshold with only
  `liveness` → not quiet (re-armed); past-threshold silence → `quiet_since` set,
  turn `status` unchanged, `in_flight_turn_id` retained, no `last_error`; activity
  after quiet clears `quiet_since` **and** re-arms (second silence re-triggers —
  latch regression); stale-turn event doesn't re-arm.
- **Frontend reducer (`reducers.test.ts`):** `heartbeat_timeout` sets
  `quiet_since` (= event `at`) without failing; non-in-flight timeout is a no-op;
  activity / `turn_end` clear `quiet_since`; transcript reducer leaves the turn
  streaming.
- **Frontend component (`UnifiedTranscript.test.ts`):** static render of the
  counting-up variant (turn-scoped); and an end-to-end fake-timer test —
  `Working... → No response → Working...` across the live timer through the real
  listener path.
- **Replaced** the old `heartbeat_timeout → failed` assertions; corrected the
  stale "reserved; not currently emitted" comments.

## Decisions locked

- **Liveness when redacted; content when not.** The Claude parser branches on the
  thinking field: empty → `Liveness`; non-empty → `ContentChunk { Thinking }`.
  Correctness no longer depends on the redaction holding.
- **Both changes ship together**, standalone — not folded into M4.10 (rendering,
  gated on the Markdown plan) and not deferred to M8.
- `quiet_since` on `AgentRuntime`; **1-minute threshold**; counting-up indicator;
  no frontend hard-fail; **cancel is the backstop** for a wedged-but-alive
  process (no escalation wording — the ticking counter is the escalation signal).

## Out of scope / cross-references

- **Unattended auto-recovery** (kill a truly-wedged subprocess after N minutes)
  can only work on the **backend** (the frontend can't release the lock);
  intentionally deferred to the M8 backend stall-timeout (system-design open
  question 10.18). Recorded as a decision, not an oversight.
- **M4.10 cross-reference:** Claude thinking is consumed as a liveness signal
  while redacted (empty). If redaction lifts, non-empty thinking flows as
  `ContentChunk { Thinking }` — which M4.10's `ThinkingWidget` is meant to render
  "for free." Until that widget lands, such text renders unstyled and does not
  survive reload (the Claude session-file parser drops thinking) — a visible
  degraded state, flagged for M4.10, far better than silent loss.
- **Residual uncertainty:** the probed thinking block was short, so the
  inter-`thinking_delta` gap is not proven to stay under the 1-minute threshold
  on a very long think. The cadence looks like an API-level keepalive, so it's
  unlikely — and Change A's quiet indicator backstops that case harmlessly
  regardless, so the combination is robust without further probing.
