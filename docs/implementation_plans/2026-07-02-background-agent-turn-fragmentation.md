# Fix: Claude background-agent dispatches fragment on reload and truncate live

**Date:** 2026-07-02 (revised same day after live probes — see Evidence)
**Status:** Implemented (M1–M4 complete; operational record: `docs/harness-behavior.md` G22)
**Scope:** `crates/harness/src/claude_code/session_file.rs` (turn-boundary fix), `crates/harness/src/forward.rs` (separator parity fix + doc correction + regression coverage), `crates/harness/src/parser.rs` + `crates/harness/src/claude_code/mod.rs` (live terminal fix), `crates/harness/src/events.rs` (`AdapterEvent::is_turn_scoped` lifecycle classification), `crates/dispatcher/src/lib.rs` (defense-in-depth guard), `docs/harness-behavior.md` (record the upstream change).

## Problem

Claude Code's background-agent feature (the `Agent`/Task tool with `run_in_background: true`) changed what one `claude -p` dispatch produces. When a background sub-agent completes, Claude Code injects a completion notification and the **same process keeps responding** — an additional internal response cycle per notification. One logical send now produces N+1 cycles (N = background completions), and the two representations of that dispatch speak **different vocabularies**:

- **Disk** (main session file): each cycle ends with `stop_reason: end_turn`, and each notification lands as a `<task-notification>…</task-notification>` **`user`-role record with *string* content**, `promptSource: "sdk"`, mid-dispatch.
- **Live stream** (`--output-format stream-json`): each cycle is a full `system/init` → content → **`result`** sequence — multiple `result` and `init` events in one process, one `session_id`. Notifications surface as `system` events (a task-lifecycle family whose names have already drifted across CLI versions: `task_started` / `task_progress` / `task_updated` / `task_notification` are all observed, plus unrelated `status` / `thinking_tokens` / `hook_*` subtypes); the sub-agent's relayed records are `parent_tool_use_id`-tagged (already correctly skipped).

Each representation breaks Switchboard's "one send = one turn" model (system-design §7) in its own way: disk **fragments** the turn (Defect 1), live **truncates** it (Defect 2).

On disk, a single logical send now produces:

```
user (real prompt, string)
assistant … stop_reason=end_turn        ← "I'll start by grounding myself…" + "Both research agents are running…"
user  <task-notification> (string, 23 KB, background agent #1 done)
assistant … stop_reason=end_turn        ← "The codebase inventory is complete…"
user  <task-notification> (string, 31 KB, background agent #2 done)
assistant … stop_reason=end_turn        ← "Both research passes are done…"
user (next real prompt, string)
```

(Confirmed against a real captured session: `~/.claude/projects/-Users-shanekercheval-repos-bookmarks-migrate-auth/019f2398-…jsonl`.)

### Evidence (probe-verified vs. inferred)

Two raw captures of real `claude -p` runs (CLI 2.1.198) using Switchboard's exact flags (`-p --output-format stream-json --include-partial-messages --verbose --dangerously-skip-permissions`):

1. **One background agent** (archived in-repo as a **sanitized minimization** — `init` reduced to grammar-relevant keys, disk `attachment` records and `hookInfos` dropped, because the raw records embed local environment config: MCP server/connector lists, plugin paths, skills, slash commands, hook command lines; raw captures kept local only): `docs/research/archive/claude-background-agent-stream-probe.jsonl` (45-record stream) and `claude-background-agent-session-file-probe.jsonl` (its 14-record disk counterpart). Stream: 2 `init`; **both `result` events at the very end, back-to-back**, *after* all content. Disk: real prompt → assistant `end_turn` ("waiting") → `<task-notification>` user-string (`promptSource:"sdk"`) → assistant `end_turn` ("done"). The same sanitization rule applies to any fixture or future capture derived from a real run.
2. **Two background agents** (245-line stream, not committed — its task bodies embed repo content): `init` at 3/189/218; `result` at **216/217 (mid-stream, back-to-back), then another full cycle, then 245**.

Probe-verified:
- N background completions → **N+1 `init`/`result` cycles** in one process, one `session_id`.
- **`result` delivery timing is irregular**: exit-batched in capture 1, interleaved in capture 2. No in-stream marker reliably announces "this result is the last": capture 2 shows a new `init` *after* the result count matched the inits seen so far, and capture 1 shows an intermediate result arriving with zero pending background tasks — which together refute both boundary-detection heuristics (init/result count-matching and pending-task counting). **The only invariant both captures satisfy: the stream ends shortly after the final `result`.** This is what makes the EOF terminal in M3 the only sound design.
- Per-result `usage` is per-cycle (capture 1: `output_tokens` 144, then 3); the relayed sub-agent `assistant` record is `parent_tool_use_id`-tagged and correctly skipped.

Inferred, to be confirmed by M3 step 0:
- `total_cost_usd` appears **cumulative** across a dispatch's results (0.0899 → 0.1663 in capture 1), i.e. the last result carries the dispatch total. Single observation, not yet proof.
- Failure shapes (a failing background agent; cancellation between cycles) are unprobed.

### Defect 1 — disk: one dispatch reconstructs as N+1 turns

The **disk parser** (`session_file.rs`) treats *every* `user`-string record as a turn boundary — it calls `close_current_agent(Complete)` **before** the housekeeping check, so even though the `<task-notification>` is correctly dropped (it never renders as a user bubble), the pending agent turn has already been closed. One dispatch fragments into **three** `Turn::Agent`s (in the motivating session).

User-visible, both verified:

1. **Rendering:** on reload the one response renders as three separate agent messages (the "3 messages after navigating away and back").
2. **Forwarding — the painful one:** the manual cross-agent forward resolves an *idle* source from disk via `latest_completed_agent_text` (`forward.rs:30`), which returns only the **most-recent completed** agent turn. That's the last fragment ("Both research passes are done…") — the earlier inventory and reasoning are silently dropped from what gets forwarded to the next agent.

### Defect 2 — live: the turn ends at the first `result`, and later cycles are never read

An earlier draft of this plan asserted the live path was already correct ("the dispatcher emits a single `TurnStarted`/`TurnEnd` per process"). That was an unverified inference, and the probes disproved it. The actual live behavior:

- `parser.rs::parse_result` emits a `TurnEnd` on **every** `result` envelope — it is documented as the sole-terminal emitter, and the upstream change broke that contract's premise.
- `run_producer` (`claude_code/mod.rs`) **breaks out of its read loop at the first terminal** and stops reading stdout entirely. Later cycles never reach the dispatcher at all; the producer parks in `child.wait()`. A large post-result burst can fill the stdout pipe and stall the child indefinitely.
- User-visible: the turn completes early in the UI; the closing synthesis (written after the last background agent reports in) never streams; a forward of the turn is truncated to the cycles read before the first result; the agent looks idle while still generating. Whether a given run *looks* broken depends on Claude's irregular result timing — exit-batched runs (capture 1) escape by luck; interleaved runs (capture 2) lose their tail.
- Latent, currently unreachable only because the producer breaks first: the dispatcher forwards every `TurnEnd` and overwrites its post-terminal `terminal` stash with freshly-taken (usually empty) `captured_text` on each one (`lib.rs:1552-1572`) — `WaitForCurrentTurn` during the post-terminal drain would answer with wrong text, and `PeekCurrentTurn`/`FailFast` would treat the agent as free mid-generation. The frontend reducer is already idempotent (first `turn_end` wins; later chunks/terminals dropped).
- Cost/usage: the turn reports the **first** result's per-cycle usage and cost; `parse_result` also `.take()`s `stable_message_id`/`first_message_id`, so any second terminal carries `None` — and in interleaved orderings the persisted spend joins to a mid-dispatch message id that won't match the reunified disk turn's final-message key after M1.

### Why the fix target is "one send = one turn = the process"

- A background-agent dispatch is **one send → one turn** by Switchboard's own send/turn vocabulary (system-design §7). The journal records exactly one send for it. Both paths must converge on one `Turn::Agent` spanning the whole process: disk by treating `<task-notification>` records as mid-turn continuations (M1), live by ending the turn at process exit rather than at any individual `result` (M3). Reunifying re-aligns the harness side with the one journal send and repairs live↔disk dedup (both paths' `hydration_key` becomes the dispatch's first assistant `message.id`).
- The live parser's `parse_user_envelope` (`parser.rs:648`) only processes **array** content; the disk-vocabulary `<task-notification>` user-string never appears in the stream anyway (probe-verified — live uses `system` events). The live defect is purely the multi-`result` terminal handling.
- `<task-notification>` is the **only** known housekeeping record that is a *mid-turn continuation*. The other denylisted prefixes are genuine between-turn boundaries and must keep closing the turn:
  - `<command-message>` / `<command-name>` and the `<local-command-*>` trio are user-initiated slash / `!bash` invocations that happen *between* turns (assistant output can follow a slash-command echo and must not merge backward).
  - Compaction summaries (`isCompactSummary`) are already diverted to a `Turn::System` marker earlier in `handle_user`, before the housekeeping drop.

## Non-goals (explicitly out of scope)

- **The within-turn "separate bubbles" rendering.** Even as one turn, consecutive `Text` items render as separate `<Markdown>` blocks on the disk path, while live coalesces adjacent chunks into one block with `\n\n` paragraphs (the deliberate text/tool/text ordering contract in `reducers.ts` / `UnifiedTranscript.svelte`). Visually near-identical (paragraph breaks either way); pre-existing for all multi-block turns. This plan does **not** change it. Flagged as an open follow-up, not built in.
- **Surfacing background-agent progress in the UI** beyond keeping the heartbeat honest (M3 maps the task-lifecycle events to `Liveness`). The `task_progress` events carry human-readable descriptions ("Reading auth/session.py…") — a "background task running" hint is cheap future work, recorded in M4, not built here. The dropped `<task-notification>` bodies stay dropped.
- Any change to how **foreground** sub-agents are handled — relayed records are `parent_tool_use_id`-tagged and already skipped (probe-verified), and their results come back as `tool_result` (array) blocks, not turn boundaries.

## Reference reading (read before implementing)

- `crates/harness/src/claude_code/session_file.rs` — the whole file, especially the module doc (record mapping + lifecycle), `handle_user` (the `Value::String` arm and the pre-close ordering), `is_user_housekeeping`, `HOUSEKEEPING_PREFIXES`, `close_current_agent`, `finalize`/`eof_tail_status`.
- `crates/harness/src/parser.rs` — `parse_result` (the sole-terminal contract M3 reworks), `parse_system_event` (init-only today; gains the task-event `Liveness` mapping), `parse_content_block_delta` (the `\n\n` block separator M2 mirrors), the `parent_tool_use_id` short-circuit (145–177), `parse_user_envelope` (648).
- `crates/harness/src/claude_code/mod.rs` — `run_producer`: break-on-terminal, the `Ok(None)` EOF arm, `synthesize_truncation_turn_end`, and the cancel path (M3's main seam).
- `crates/dispatcher/src/lib.rs` — the turn loop's `TurnEnd` arm: `terminal_seen`, the `captured_text` take, the `terminal` stash, `WaitForCurrentTurn`/`PeekCurrentTurn`/`FailFast` (M3's defense-in-depth guard).
- `crates/harness/src/forward.rs` — `latest_completed_agent_text` + `concat_text_items` (the consumer that Defect 1 breaks; already correct *within* a turn).
- The two archived captures (see Evidence) — ground truth for the stream and disk shapes; `crates/harness/src/bin/fake_claude.rs` + `crates/harness/tests/claude_adapter.rs` — the fixture-driven harness M3's tests plug into.
- `docs/harness-behavior.md` — the gap register format (`Gnn`, ✅-closed style) and §3 split-source model, for the doc milestone.
- `docs/system-design.md` §7 (sends and turns) — the one-send-one-turn framing the fix restores.

---

## Milestone 1 — Disk parser: `<task-notification>` no longer closes the agent turn

### Goal & Outcome

Make the session-file reconstruction treat a background-agent `<task-notification>` as a mid-turn continuation, not a turn boundary, so one dispatch reconstructs as one `Turn::Agent`.

Outcomes:
- Loading a session file where a dispatch contains one real prompt, N interleaved `<task-notification>` records, and assistant content before/after each, yields **exactly one** `Turn::Agent` for that dispatch, with **all** text blocks present, in order.
- The `<task-notification>` records still produce **no** `Turn::User` (unchanged — they remain housekeeping and are counted in `housekeeping_skipped`).
- Genuine between-turn housekeeping (`<command-*>`, `<local-command-*>`) **still** closes the turn (no regression).
- A following real user prompt still closes the reunified turn as `Complete`; EOF still derives tail status from the last `stop_reason`.

### Implementation Outline

The bug is an ordering problem in `handle_user`'s `Value::String` arm: `close_current_agent` runs unconditionally at the top, ahead of the housekeeping determination. The fix: a `<task-notification>` record must return **without** closing the current agent turn (and without flushing deferred tool_results — they belong to the still-open turn).

Load-bearing decisions:
- **Scope the exception to task-notifications specifically**, not to all of `is_user_housekeeping`. The other denylisted prefixes are real boundaries (see rationale above).
- **The predicate must be housekeeping-AND-prefix, not prefix-only.** A user can legitimately *type* a prompt beginning with `<task-notification>`; `is_user_housekeeping` already exempts `promptSource ∈ {typed, queued}` denylist-prefixed records (they have no journal copy, so dropping one is silent data loss — pinned by `typed_prompt_starting_with_a_tag_is_preserved`). The continuation predicate must inherit that exemption, so define it as `is_user_housekeeping(record, text) && text.trim_start().starts_with("<task-notification>")` — **not** a bare prefix check. A prefix-only predicate placed before `close_current_agent` would drop a real typed prompt *and* fail to close the prior turn. Name it to encode the boundary rule (e.g. `is_task_notification_housekeeping(record, text)`), so the narrow bypass is self-evident at the call site; final naming is the implementer's against the code.
- **Pinned `Value::String(text)` arm order** (the exception must be visibly narrow — only task-notification housekeeping bypasses close/flush; everything else keeps the old boundary behavior):
  1. `is_task_notification_housekeeping(record, text)` → `housekeeping_skipped += 1; return` — **no `close_current_agent`, no `flush_deferred_as_warnings`.**
  2. else `close_current_agent(TurnStatus::Complete)` + `flush_deferred_as_warnings()` (unchanged).
  3. `isCompactSummary` → emit `Turn::System` compaction marker, return (unchanged).
  4. `is_user_housekeeping` → count + return (unchanged; catches `<command-*>` / `<local-command-*>` / other bookkeeping).
  5. real prompt → emit `Turn::User` (unchanged).

  (This supersedes an earlier draft note about "compaction must run before the task-notification check." Because the continuation predicate is prefix-specific, it cannot match a compaction summary, so that concern was moot — the clean five-step order above is the contract.)
- **Do not flush deferred tool_results** on the step-1 return — the current turn is still open, so its pending deferreds are still live. (Flushing is correct only when a turn actually closes.)
- Keep counting task-notifications in `housekeeping_skipped` so the "denylist stopped matching" warning at `finalize` stays meaningful.
- Put the *why* in a code comment at the branch: task-notifications are background-agent completions injected mid-dispatch, so they continue the same logical turn rather than bounding it — cite the behavior, not a date/milestone (per AGENTS.md).

No change to `close_current_agent`, `handle_assistant`, `finalize`, or the builder's id/usage/model kept-first/kept-last logic. Spanning the whole dispatch, the builder naturally ends with the final assistant record's `message.id` (→ `stable_message_id`, the cost-join key) and usage, and keeps the first record's id (→ `hydration_key`) — which is the correct, whole-turn span.

### Definition of Done

Unit tests in `session_file.rs`'s `#[cfg(test)] mod tests`, using the existing inline `json!` fixture style (do **not** commit the real captured transcript — its `<task-notification>` bodies contain repo auth detail; build a minimal synthetic fixture of the same shape):

- **Reunification (core):** prompt → assistant(`text "a"`, `end_turn`) → user-string `<task-notification>` (`promptSource:"sdk"`) → assistant(`text "b"`, `end_turn`) → user-string `<task-notification>` → assistant(`text "c"`, `end_turn`) → real prompt. Assert: exactly one `Turn::Agent` between the two real prompts; its `Text` items are `["a","b","c"]` in order; the trailing real prompt produced its own `Turn::User`; `housekeeping_skipped == 2`.
- **Mid-turn continuation across a tool call:** a task-notification sandwiched by assistant records that include a `tool_use` + later `tool_result`, to confirm tools still pair and the turn still doesn't split.
- **Typed prompt starting with the tag is NOT a continuation (data-loss guard):** a `promptSource:"typed"` record whose text starts with `<task-notification>`, arriving mid-conversation, must **close** the open agent turn (two `Turn::Agent`s) *and* emit a `Turn::User`. Also keep the existing `typed_prompt_starting_with_a_tag_is_preserved` test in the DoD list explicitly, so the conjunction predicate isn't "fixed" by weakening it.
- **No open turn when the notification lands:** `prompt → <task-notification> → assistant → next prompt` yields `User / Agent / User` with `housekeeping_skipped == 1` (exercises the step-1 early return with `current_agent == None`).
- **Boundary records unchanged (regression):** a `<command-name>`/`<command-message>` pair between two assistant chunks still closes the first turn (assert two `Turn::Agent`s). Keep/confirm the existing housekeeping-drop tests pass.
- **EOF tail:** a dispatch that ends on a `<task-notification>` with no following real prompt closes as one `Complete` turn at `finalize` (status from the last `end_turn`).
- **Compaction still diverts:** an `isCompactSummary` string record still yields a `Turn::System` compaction marker (guard the ordering didn't regress).

---

## Milestone 2 — Forward captures the whole reunified turn, with live-matching spacing

### Goal & Outcome

M1 already restores *what* gets forwarded (the whole dispatch, not the last fragment). This milestone restores *how it reads* — the paragraph breaks — and repairs the parity invariant `forward.rs` currently only approximates.

Outcomes:
- A forward from an idle background-agent source carries **all** the dispatch's `Text`-kind output (reasoning and tool output still excluded), not only the final block.
- The disk-read forward is **byte-identical to the live-captured forward** for the same turn: paragraph breaks (`\n\n`) sit between text blocks in both, instead of the disk path gluing blocks together (`"…running.The codebase…"`).

### Implementation Outline

**This is no longer a pure-test milestone.** The plan originally assumed `forward.rs` needed no change; that rested on a false premise — that the disk join "mirrors live." It does not. The live path bakes a `\n\n` separator into each new text block's first chunk (`parser.rs:202-205`, `:273-278`; `pending_separator` persists across intervening tool calls), and the dispatcher accumulates that verbatim (`dispatcher/src/lib.rs:1507`), so live-forwarded text carries the breaks. The disk path's `concat_text_items` (`forward.rs:44-56`) joins with nothing. For any multi-block turn — which M1 now makes the common shape — the two diverge, violating the module's own "yield the same string / byte-identical bodies" promise (system-design §7 one-mechanism principle).

Load-bearing decisions:
- **Join *non-empty* `Text`-kind items with `"\n\n"`** in `concat_text_items`, not all items and not with an empty separator. Skipping empties is required for exact parity: the disk parser emits a `TurnItem::Text` even for an empty-string text block, and the live side does **not** let an empty block consume the pending separator (`empty_text_block_does_not_consume_pending_separator`, `parser.rs`). A naive all-items join would yield `"first\n\n\n\nsecond"` where live yields `"first\n\nsecond"`. No leading separator before the first non-empty block (matches live's "separator only *between* blocks").
- **Keep the separator in the forward join only** — do **not** bake `\n\n` into the on-disk `TurnItem::Text` values. Those render as individual `<Markdown>` blocks (`UnifiedTranscript.svelte`); adding whitespace to the item text would introduce stray blank lines in the rendered transcript. The disk items stay clean; only the forward concatenation gains the separator.
- **Correct the now-false doc claims in both places:** the module doc (`forward.rs:5-13`, the "yields the same string" / "byte-identical bodies" language) and the `latest_completed_agent_text` doc comment (`forward.rs:25-28`, "accumulates the same `Text`-kind `ContentChunk`s with no separators"). After the change these become *true*; state that the disk join reproduces live's inter-block `\n\n`.

`latest_completed_agent_text` itself and the turn-selection logic are unchanged; M1 is what makes "most-recent completed turn" span the whole dispatch again.

### Definition of Done

- Update the existing `forward.rs` unit tests that encode the old glued behavior (`latest_text_is_text_only_turn_concatenated`, `latest_text_excludes_thinking_and_tool_output`) to expect the `\n\n` separator. This is an **intentional behavior change**, not test-weakening — the new expectation is the correct, live-matching output.
- Add a **boundary-sensitive** regression test at the parser→forward seam: reconstruct a background-agent dispatch via the same inline fixture shape as M1, then assert `latest_completed_agent_text` returns the blocks joined by `\n\n` (e.g. `"…running.\n\nThe codebase…"`), **not** run-together and **not** just the last block. Shape the fixture as **text → tool → text** (not merely consecutive text blocks), since separator-persistence-across-a-tool-call is the specific live behavior being mirrored.
- Add an empty-block parity case: a turn containing an empty `Text` item between two non-empty ones forwards as a single `\n\n` join (no doubled separator).
- Keep any loader-path test hermetic (injected `home_dir`, synthetic fixture) — no real `~/.claude` reads.

---

## Milestone 3 — Live path: the turn ends at process exit, not at any `result`

### Goal & Outcome

One `claude -p` dispatch = one `TurnEnd`, emitted when the process's stdout closes. All cycles stream into one live turn (text, tools, heartbeat), the forward/await waiters fire once with the whole text, the agent stays busy until genuinely done, and folded usage/cost lands on the single terminal.

Design choice, made deliberately: **the turn is the process** (EOF terminal), not in-stream boundary detection. The Evidence section shows both practical boundary detectors are refuted by the existing captures, while "stream EOF follows the last result" holds in both. EOF also collapses every downstream symptom at once — captured text, waiters, idle-peek, `FailFast`, reducer, cost — because there is exactly one terminal again.

Outcomes:
- A background-agent dispatch streams as one turn end-to-end: cycle-1 text, the `Agent` launch tool call, a live spinner through the background wait (no false `quiet_since`), later cycles' text appended, one `TurnEnd` at exit.
- A live forward/await of that turn carries the whole text (byte-identical to the M2 disk forward).
- Non-background dispatches: the terminal moves from the `result` line to process exit — **measured at ~0.5s** (step 0), because `Stop` hooks run *before* the result record is emitted, not after. The only multi-second window observed is abandoned-background-task teardown (~5s), during which task events keep the heartbeat alive.
- The pipe-fill hang risk is gone: stdout is drained to EOF on every completed dispatch.

### Step 0 — targeted probes ✅ DONE (2026-07-02, CLI 2.1.198)

Findings recorded in `docs/harness-behavior.md` §6 (2.1.198 entry); sanitized captures archived (`claude-background-agent-multimodel-stream-probe.jsonl`, `claude-background-agent-abandoned-task-stream-probe.jsonl`, next to the original two):

1. **Terminal telemetry — settled.** The final `result`'s `total_cost_usd` **equals the sum of `modelUsage[*].costUSD` exactly** (multi-model verified: sonnet parent + haiku subagent, 0.1241424 + 0.0152375 = 0.1393799) — whole-dispatch cost including subagents. `modelUsage` is a per-model whole-dispatch aggregate whose fields map 1:1 onto `TurnUsage` (`inputTokens`/`outputTokens`/`cacheReadInputTokens`/`cacheCreationInputTokens`), byte-identical across batched results; `result.usage` is per-cycle parent-only. **Fold rule (pinned):** from the **final** result — `total_cost_usd` top-level; token fields summed across `modelUsage` entries; `context_window` via `select_context_window` (unchanged); occupancy unchanged.
2. **Failing background agent — partially observed, decision-irrelevant.** A pending Bash-background task torn down at process exit emits `task_notification` `status:"stopped"` (new value — the status vocabulary is open); a genuine *failed* status remains unobserved. Handling is status-agnostic (`Liveness`), so nothing hinges on it. Bonus finding: **`system` task events can arrive *after* the final `result`** (~5s teardown window), and the process **exits 0 with the task abandoned** — so exit-0 + stash → `Completed` is correct, and post-result system events must be tolerated (they are: `Liveness`, stash undisturbed).
3. **Kill mid-wait — gate validated.** SIGKILL during the background wait → signal exit (137) and **zero `result` events on the stream** (batched results never flushed) → truncation-synthesis path. The folded-result-then-killed shape is hard to produce live (results batch at exit) — the `fake_claude` `// exit:1` fixtures are the deterministic coverage for the gate.
4. **Stop hooks — latency concern resolved.** A 5s `Stop` hook runs **before** the `result` record is emitted (`duration_ms` includes it; result→EOF gap ~0.5s). The EOF terminal therefore adds **sub-second** latency to normal turns — hook users already wait out the hook today, before the result. The one multi-second case is abandoned-task teardown (~5s observed), during which task events keep the heartbeat alive.

### Implementation Outline

**Parser (`parser.rs`):**
- `parse_result` no longer returns a `TurnEnd` for a successful result. It **folds** the result into `ParserState`, and the fold reads **whole-dispatch aggregate telemetry, not summed per-cycle fields**: token counts and cost from the final result's `modelUsage` (exact mapping pinned by step-0 probe 1 — note this is **new extraction surface**, not a tweak to `extract_usage_from_result`: `modelUsage` uses different field names (`inputTokens`/`cacheReadInputTokens`, camelCase) and needs the same pick-by-`result.model`-or-max-`inputTokens` selection rule `select_context_window` already implements — extend/reuse that path rather than duplicating it); context-window kept-last; **occupancy (`context_input_tokens`) stays what it is today** — the final parent assistant call's context, kept-last, conceptually separate from dispatch totals (the context bar must not silently become an aggregate-token display); spend/overage consumed as today; message ids read **without** `.take()` (kept-first / kept-last, matching the disk builder — this also removes the `None`-on-second-terminal trap). Returns `Liveness` — a result is turn progress, and the heartbeat must stay armed between cycles.
- **Exception — fail fast:** an `is_error` / `api_error_status` / stashed-auth-failure result still returns `TurnEnd { Failed }` immediately, preserving today's failure semantics (the producer's break-on-terminal then stops the read, which is correct for a failed dispatch).
- New `ParserState::take_final_turn_end(turn_id) -> Option<AdapterEvent>`: the stashed Completed terminal; `None` if no result was ever folded.
- `parse_system_event` gains the `turn_id` parameter and inverts to a **denylist**: `init` keeps its special handling (emitting `SessionMeta` — now N+1 per dispatch; the consumer overwrite is idempotent, assert in tests, don't change); **every other `system` subtype maps to `Liveness { turn_id }`**. A fixed allowlist of task-event names is the wrong shape against a vendor vocabulary already observed to drift (G20 documents `task_started`/`task_progress`/`task_notification` at 2.1.170; the 2.1.198 captures add `task_updated` and `thinking_tokens`, and the trivial-task capture contains **no** `task_progress` at all). An unknown system event is still evidence the process is alive; `Liveness` renders nothing, so misclassifying a future content-bearing subtype degrades to exactly today's silent skip — never worse. This is what keeps multi-minute background waits from tripping the frontend's `quiet_since` indicator on a healthy turn.

**Producer (`run_producer`, `claude_code/mod.rs`):**
- The happy-path terminal moves to stdout EOF, **gated on the process exit status**: after the read loop, when `!terminal_seen && !cancelled`, first `child.wait().await` (a reorder of the existing reap, which already sits a few lines below — stdout has closed, so exit is imminent), then:
  - **exit success + stashed result** → emit the stashed `Completed` terminal;
  - **non-zero exit / signal + stashed result** → emit `TurnEnd { Failed, HarnessError }` carrying the stderr tail and a note that N cycles completed before the interruption. A folded intermediate success is **not** proof the dispatch finished — a kill between cycles must not mark the turn `Completed` with partial text, silently feeding forwards/workflows a partial answer as authoritative;
  - **no stashed result** → `synthesize_truncation_turn_end` regardless of exit status (unchanged crash semantics).
- The fail-fast path (error result mid-stream) keeps today's emit-then-reap with log-only exit reconciliation.
- Break-on-terminal stays; it now only fires for failure terminals and malformed-JSON, where stopping early is correct.
- Cancel path unchanged (kill, no terminal, dispatcher synthesizes Cancelled — the stashed pending terminal is simply dropped with the parser state).

**Dispatcher (`lib.rs`) — defense-in-depth only:**
- Guard the `TurnEnd` arm: if `terminal_seen` is already set, warn-log and drop the event instead of re-taking `captured_text`, re-firing waiters, and overwriting the `terminal` stash. Unreachable for a correct adapter; the cost of the sole-terminal contract being violated again is exactly the bug class this plan fixes.

**Frontend:** no changes. A single `turn_end` restores the reducer's assumptions; `liveness` already re-arms the heartbeat.

Comment discipline: the *why* on the EOF terminal cites the observed grammar (irregular result timing; EOF as the only reliable boundary) and points at the archived captures — behavior, not chronology.

### Definition of Done

- **Fixtures** in `crates/harness/tests/fixtures/claude/`: `background-agent.jsonl`, derived from the archived single-agent capture (exit-batched ordering; same sanitization rule as the archive), and a synthesized `background-agent-interleaved.jsonl` mirroring capture 2's sharp shape (result, result, init, more content, result — content *after* intermediate results). The interleaved fixture carries **realistically divergent** `usage`/`modelUsage`/`total_cost_usd` values per result (not copies), so the fold assertions mean something.
- **Adapter integration tests** (via `fake_claude`, `tests/claude_adapter.rs`), for **both** fixtures: exactly one `TurnEnd`, outcome Completed; all cycles' text chunks present in the event stream (so `captured_text` spans every cycle, `\n\n`-separated); usage folded per the fold rules; `Liveness` events for the task-lifecycle records; N `SessionMeta` events tolerated.
- **Exit-status gate tests** (via `fake_claude`'s existing `// exit:<N>` directive): `result(success) → // exit:1` and `result(success) → task_notification → // exit:1` (the shape a real kill-between-cycles produces) both yield `Failed`, not `Completed`; `result(success)` + clean exit yields `Completed` (the gate doesn't over-fail healthy runs).
- **Parser unit tests:** two results fold (aggregate telemetry per the pinned fold rule, kept-last ids — no `.take()` regression); occupancy remains the final parent assistant's context, not an aggregate; an **unknown** system subtype (`"subtype":"totally_new"`) maps to `Liveness`; a failure result still fails fast; `take_final_turn_end` returns `None` with no results folded.
- **Dispatcher test** (`MockHarnessAdapter`): a contract-violating stream emitting two `TurnEnd`s produces one terminal downstream (second dropped + logged), and `WaitForCurrentTurn` after the first answers with the full captured text.
- **Live test** (`crates/harness/tests/live.rs`): `live_claude_background_agent_completes_as_one_turn` — the single-background-agent one-word probe prompt; assert one `TurnEnd` and captured text containing all cycle outputs. Named per the `live_claude_*` convention. Cost note: inherently above the one-word-reply discipline (~$0.10–0.20/run — it must genuinely run a sub-agent); acceptable in `make test-live-claude`, and the reason the default suite relies on the fixtures.

---

## Milestone 4 — Record the upstream behavior change

### Goal & Outcome

`docs/harness-behavior.md` gains a gap-register entry (existing `Gnn` style, marked closed) covering **both** defects and their one root cause, so the next harness-update review has the context.

### Implementation Outline

Follow the existing gap-register entry format (see the ✅-closed entries like G19/G21). Keep it operational:

- The upstream change: background agents run the parent as N+1 init→result cycles, and the **live/disk vocabulary split** (stream: `system/task_*` events + multiple `result`s/`init`s; disk: `<task-notification>` user-strings + multiple `end_turn`s).
- The grammar facts with explicit **probe-verified vs. inferred** markers, citing the archived captures (including step 0's) by path — especially the irregular result timing that rules out in-stream boundary detection. The distinction matters: this plan's first draft shipped an unverified inference as fact, and the register entry is where the next reviewer learns which claims to re-probe after a CLI bump.
- How Switchboard handles it: disk = continuation predicate in `session_file.rs` (M1); live = exit-status-gated EOF terminal in `run_producer` + result folding in `parser.rs` (M3); forward = `\n\n` join parity (M2). Cross-reference the tests. Document the chosen telemetry contract: **`TurnUsage` = whole-dispatch totals as the harness's terminal aggregate reports them** (matching billing and the TUI), occupancy separate.
- **Reconcile G20 in the same pass** — it is the existing record of this exact channel and now disagrees with the newest evidence: add `task_updated` (and `thinking_tokens`) with a verified-at-2.1.198 note, and update its "the progress channel was simply never wired up / Switchboard surfaces none of it" text — the channel now feeds the heartbeat via the denylist `Liveness` mapping (rich progress display remains the open UX decision G20 records).
- Residuals recorded as open items, not silently closed: within-turn block coalescing (cosmetic); a kill mid-background-work reads as `Complete` with partial content **on reload from disk** (`eof_tail_status` sees the last cycle's `end_turn`; the *live* path now correctly fails via the exit-status gate — the residual is disk-side only, surfacing after a hard app kill where the hydrate-merge protection is gone); surfacing `task_progress` descriptions as a UI hint (future work); **historical cost re-attachment** — a pre-fix background-agent turn persisted at most one spend-sidecar entry (overage-gated), keyed to the `stable_message_id` seen at its first result, which in interleaved orderings is a mid-dispatch id — after M1 reunification that key no longer matches the turn's final-message key, so the cost badge silently stops re-attaching for that narrow historical slice (self-healing for all post-fix dispatches; documented, not migrated).
- No README entry — the fix is transparent to users (nothing to configure or work around), and the resulting UX (spinner stays on through quiet background work) is expected behavior, not a limitation.

### Definition of Done

- `harness-behavior.md` entry added, internally consistent with the M1/M3 code comments, step-0 findings folded in.
- The residuals above are recorded as open items, not silently closed.

---

## Verification

- `make test` (Rust + frontend jsdom) green, including the new fixtures; `make lint` clean.
- `make test-live-claude` — the new background-agent live test plus the existing Claude live suite (M3 touches spawn/stream handling, so the adapter-touching-PR rule applies).
- Manual sanity check in a dev instance:
  - Dispatch a background-agent prompt; watch it stream as **one** turn including the post-notification synthesis, with no `quiet_since` flicker during background work.
  - Switch projects away and back mid-turn (live turn protected by the hydrate merge) and again after completion (one identical turn from disk).
  - Load the real `019f2398-…` session; the research turn renders as one agent turn, and forwarding it carries the full inventory + conclusions with paragraph breaks intact.
