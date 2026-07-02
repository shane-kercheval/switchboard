# Fix: background-agent `<task-notification>` records fragment a Claude turn on reload

**Date:** 2026-07-02
**Status:** Planned
**Scope:** `crates/harness/src/claude_code/session_file.rs` (turn-boundary fix), `crates/harness/src/forward.rs` (separator parity fix + doc correction + regression coverage), `docs/harness-behavior.md` (record the upstream change).

## Problem

Claude Code's background-agent feature (the `Agent`/Task tool run in the background) changed what lands in the **main** session file. When a background sub-agent completes, Claude Code injects a `<task-notification>…</task-notification>` record into the parent session file as a **`user`-role record with *string* content**, `promptSource: "sdk"`, *mid-turn* — and the same `claude -p` dispatch keeps responding afterward. So a single logical send now produces, on disk:

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

The **disk parser** (`session_file.rs`) treats *every* `user`-string record as a turn boundary — it calls `close_current_agent(Complete)` **before** the housekeeping check, so even though the `<task-notification>` is correctly dropped (it never renders as a user bubble), the pending agent turn has already been closed. One dispatch fragments into **three** `Turn::Agent`s.

This produces two user-visible defects, both verified:

1. **Rendering:** on reload the one response renders as three separate agent messages (the "3 messages after navigating away and back"). The **live** path does not fragment (see below), so live and disk disagree on turn count for the same conversation.
2. **Forwarding — the painful one:** the manual cross-agent forward resolves an *idle* source from disk via `latest_completed_agent_text` (`forward.rs:30`), which returns only the **most-recent completed** agent turn. That's the last fragment ("Both research passes are done…") — the earlier inventory and reasoning are silently dropped from what gets forwarded to the next agent.

### Why this is disk-only (the live path is already correct)

The live stream is bounded by the dispatcher's single `TurnStarted`/`TurnEnd` per `claude -p` process, so a dispatch is one live turn regardless of internal `end_turn` markers. The live parser's `parse_user_envelope` (`parser.rs:648`) only processes **array** content (tool_results); a `<task-notification>` carries **string** content, so it hits the `as_array` guard and returns `Skip` — it never closes anything. The dispatcher's `captured_text` accumulates *all* `Text`-kind chunks across the whole dispatch, so a forward taken from a live/just-finishing source already gets the whole thing. The asymmetry is entirely between "live = one turn" and "disk = three turns." **The fix makes disk match live.** No live-parser or dispatcher change is needed.

### Why the fix is safe / correct against Switchboard's turn model

- A background-agent dispatch is **one send → one turn** by Switchboard's own send/turn vocabulary (system-design §7). The journal records exactly one send for it. Reunifying the three disk fragments into one turn re-aligns the harness side with the one journal send (fewer merge-correlation surprises), and matches the live turn the user already saw.
- `<task-notification>` is the **only** known housekeeping record that is a *mid-turn continuation*. The other denylisted prefixes are genuine between-turn boundaries and must keep closing the turn:
  - `<command-message>` / `<command-name>` and the `<local-command-*>` trio are user-initiated slash / `!bash` invocations that happen *between* turns.
  - Compaction summaries (`isCompactSummary`) are already diverted to a `Turn::System` marker earlier in `handle_user`, before the housekeeping drop.

## Non-goals (explicitly out of scope)

- **The within-turn "separate bubbles" rendering.** Even as one turn, consecutive `Text` items render as separate `<Markdown>` blocks (the deliberate text/tool/text ordering contract in `reducers.ts` / `UnifiedTranscript.svelte`). This is pre-existing, applies to the live path too, and is a *separate* UI question (whether to visually coalesce adjacent text blocks within a turn). This plan does **not** change it. It is flagged here as an open follow-up, not built in.
- Surfacing that background work happened (a marker for the dropped notifications). Not requested; keep dropping them.
- Any change to how foreground sub-agents are handled — those come back as `tool_result` (array) blocks, are not turn boundaries, and already work.

## Reference reading (read before implementing)

- `crates/harness/src/claude_code/session_file.rs` — the whole file, especially the module doc (record mapping + lifecycle), `handle_user` (the `Value::String` arm and the pre-close ordering), `is_user_housekeeping`, `HOUSEKEEPING_PREFIXES`, `close_current_agent`, `finalize`/`eof_tail_status`.
- `crates/harness/src/parser.rs` — `parse_user_envelope` (648) and the `parent_tool_use_id` short-circuit (145–177), to confirm the live path needs no change.
- `crates/harness/src/forward.rs` — `latest_completed_agent_text` + `concat_text_items` (the consumer that the bug breaks; already correct *within* a turn).
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

**This is no longer a pure-test milestone.** The plan originally assumed `forward.rs` needed no change; that was based on the same false premise the reviewers and I corrected — that the disk join "mirrors live." It does not. The live path bakes a `\n\n` separator into each new text block's first chunk (`parser.rs:202-205`, `:273-278`; `pending_separator` persists across intervening tool calls), and the dispatcher accumulates that verbatim (`dispatcher/src/lib.rs:1507`), so live-forwarded text carries the breaks. The disk path's `concat_text_items` (`forward.rs:44-56`) joins with nothing. For any multi-block turn — which M1 now makes the common shape — the two diverge, violating the module's own "yield the same string / byte-identical bodies" promise (system-design §7 one-mechanism principle).

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

## Milestone 3 — Record the upstream behavior change

### Goal & Outcome

Document that background-agent `<task-notification>` records now appear mid-turn in the main Claude session file, and how Switchboard handles them, so the next harness-update review has the context.

Outcomes:
- `docs/harness-behavior.md` gains a gap-register entry (in the existing `Gnn` style, marked closed) describing: the upstream change, that it fragmented disk turns and broke idle-source forwarding, and the fix (task-notifications are mid-turn continuations on the disk path; live was already correct). Cross-reference the `session_file.rs` handling and the M1/M2 tests.

### Implementation Outline

Follow the existing gap-register entry format (see the ✅-closed entries like G19/G21). Keep it operational: what the record looks like, why it's a continuation not a boundary, where the handling lives, and the live-vs-disk asymmetry that made it disk-only. Note the within-turn multi-block rendering as a **known limitation / separate open question**, not something this change addressed. No README entry — the fix is transparent to users (nothing new for them to configure or work around).

### Definition of Done

- `harness-behavior.md` entry added and internally consistent with the code comment from M1.
- The "separate follow-up" (visual coalescing of adjacent within-turn text blocks) is recorded as an open item, not silently closed.

---

## Verification

- `make test` (Rust + frontend jsdom) green, including the new fixtures.
- `make lint` clean.
- No live-test change required (this is a fixture-only reconstruction concern), but a manual sanity check is worth doing once: load the real `019f2398-…` session in a dev instance, confirm the research turn now renders as one agent turn and that forwarding it carries the full inventory + conclusions.
