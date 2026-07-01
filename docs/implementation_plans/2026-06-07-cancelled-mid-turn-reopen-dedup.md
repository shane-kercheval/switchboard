# Cancelled-mid-turn reopen: stop duplicating the prompt, render partial content labelled cancelled

**Status:** Implemented
**Date:** 2026-06-07
**Branch:** `fix/in-flight-turn-hydration-dedup` (same PR as the M1–M4 in-flight-turn work)

A focused change with **two loci** — the backend conversation merge
(`merge_project_conversation`) and one frontend badge function
(`columnState`) — plus tests. No schema change. Reviewed three ways; the first
draft's approach was wrong on the correlation key, the rendering mechanism, and
the scope claim — this rewrite incorporates those corrections.

## Problem (verified against a real session)

On a **fan-out** send (one prompt → N agents, shared `send_id`) where some
recipients are **cancelled after they already produced output**, the
conversation **on switch-back** (re-activation runs `maybeRefreshProject` →
`merge_project_conversation`) shows the **user's prompt duplicated** (once per
cancelled-mid-turn agent) plus stray/empty user rows, compounding each switch.
**Live is unaffected** — cancelling does not reload the conversation; the live
fan-out prompt correctly renders once. Switch-back is the *only* trigger.

**Root cause** — `crates/app/src/commands.rs::merge_project_conversation`. The
prompt has two on-disk sources: the **journal** `Send` records (canonical,
grouped by `send_id`) and the **harness session file** (which also records the
prompt as a `Turn::User`). They can't be joined directly (journal `turn_id` =
dispatcher's; harness `turn_id` = its own), so the merge correlates **by order**,
on the assumption *"a non-completed [cancelled/failed] send leaves no clean
harness turn — only an Outcome marker."*

That assumption is false for a turn cancelled **after** the agent wrote content:
the harness file holds a **partial agent turn**. The cancelled send is excluded
from `completed` (`:3050-3057`) while `agent_turn_count` still counts the partial
turn (`:3107-3110`), so `turn_offset = agent_turn_count − completed.len() ≥ 1`
(`:3116`). A non-zero offset makes the merge treat the partial turn as
**pre-journaling history** and its prompt as an **"imported" `UserMessage`**
(`send_id: None`, un-grouped, never deduped — `:3159-3170`) — a duplicate of the
journal's prompt. Confirmed in the repro: both cancelled agents had substantial
disk content (codex 8 assistant messages, antigravity 5 planner steps).

## Product decision (settled)

A cancelled turn renders the **same on reopen as live**: the agent's **partial
output, labelled cancelled**, grouped under the single prompt — *not* a bare
content-free marker. Live already does this; this milestone makes the reopen
merge + render produce the same. The bare marker remains the fallback only for
**cancelled-before-any-output** (nothing on disk).

## Approach

Two independent pieces. Do **not** suppress the journal Outcome marker or stamp a
cancelled status onto the disk turn — `TurnStatus` has no `Cancelled` by design
(`commands.rs:2918`, "the harness never persists a cancelled turn"), `AgentTurn`
has no `reason` field (the marker carries failure detail), and same-turn
`AgentTurn` + `Outcome` co-presence is an **explicit, intentional contract**
(`commands.rs:2830-2837`, "Consumers render both"). Keep that contract.

### 1. Backend — correlate by the journal outcome, not by disk status (`merge_project_conversation`)

The authoritative split of sends into completed vs. non-completed is the
**journal** (Outcome marker present ⇒ non-completed). Drive the correlation off
that — **never** off the harness `TurnStatus`, which is an unreliable proxy in
*both* directions:
- **cancel-after-end_turn race:** a cancelled send's turn can read `Complete` on
  disk (the model finished writing before the process kill) — a disk-status
  partition routes it to the completed bucket, never pairs it with its cancelled
  send, and reproduces the duplicate-prompt bug.
- **Streaming-completed tail:** a *completed* send's last turn can read
  `Streaming` (M2's `eof_tail_status`, no `end_turn`) — a disk-status partition
  routes it to the non-completed bucket and strands the matched completed send.

**Algorithm — generalize the existing front-aligned offset from completed-only to
all sends.** This is *one substitution*, not a new algorithm: the current code
already correctly handles the two excess directions; it just pairs against the
wrong list. The all-sends list already exists — `agent_sends[agent_id]`
(`commands.rs:2995`) is every `Send` in dispatch order; `sends_by_agent`
(`:3049-3057`) is that list filtered to completed-only, which is the bug (it
excludes cancel-mid sends whose partial turns *are* on disk). Pair against the
unfiltered `agent_sends` instead.

Let `N` = the agent's disk **agent** turns, `M` = its **all-sends** count. The
offset is on the **disk-turn side** (this is the load-bearing detail the prior
draft's prose got wrong):
- `turn_offset = N.saturating_sub(M)` — the first `turn_offset` disk turns are
  **pre-journaling history** (attached session, older than the first send) →
  `send_id: None`. (Leading *turn*-excess.)
- A turn at agent-index `i ≥ turn_offset` pairs with `all_sends[i − turn_offset]`,
  **regardless of harness status** (a `Complete`-on-disk cancelled turn pairs to
  its cancelled send; a `Streaming` completed tail pairs to its completed send).
- **Trailing *send*-excess is dropped** — a send with no disk turn (an **in-flight**
  send still running, or a contentless cancelled-before-output) is not paired. An
  in-flight send shows only its journal prompt; a cancelled one shows its bare
  Outcome marker. **In-flight is handled, not a residual** — front-alignment drops
  trailing send-excess, which is exactly the existing invariant at `:3186-3191`
  and its gate test `merge_in_flight_send_does_not_mislabel_completed_turns`
  (`:10191`). *Do not* re-introduce tail-anchoring (a `send_offset` that skips
  leading sends) — it pairs the last completed turn with the in-flight send and
  regresses that common path.

A disk turn paired to a **completed** send renders as today (its `send_id`, its
harness status). A disk turn paired to a **non-completed** send renders its
partial content with that send's `send_id` so it lands in the right fan-out
column; its cancelled/failed badge comes from the coexisting Outcome marker
(piece 2), not from a stamped status.

**The prompt-drop (user-turn) half — get this exactly right; it's where the bug
and the existing edges live.** A disk `Turn::User` is **dropped** (journaled —
the journal owns the prompt) iff it corresponds to a journaled send, mirroring
the current two-branch split (`:3159-3170`) but over the *all-sends* zip:
- A user turn **with a following reply** (the next agent turn): journaled iff that
  reply is *not* pre-journaling history (its paired agent turn index `≥
  turn_offset`).
- A **dangling** user turn (no following reply — a cancelled-before-output send
  whose prompt was recorded but produced no agent turn): journaled while unmatched
  trailing sends remain to account for it.
This half is **coupled** to the agent-turn change — `turn_offset` and
`dangling_journaled` must move to the all-sends basis **in lockstep** (compute
`dangling_journaled` from `all_sends.len()`, not `completed.len() +
non_completed_count`), or a cancel-mid prompt won't drop. With all-sends pairing,
a cancel-mid prompt now drops through the *main* pairing (its turn pairs with its
send), so the dangling branch *simplifies* — it only covers cancelled-before-output
and in-flight. The **existing bare-CLI characterization tests at `:3089-3106` are
a gate this must pass**, not an assumption.

**Documented residual (a known-bound, not a silent gap).** Front-alignment cannot
disambiguate a **content-less non-completed send positioned *before* a
content-bearing turn** — the leading (or interior) cancelled-before-output shape
shifts every subsequent label by one. Pin it the way the existing comment pins its
edges (`:3089-3106`; the existing `merge_cancel_before_harness_flush_overcounts…`
test at `:10151` already characterizes a neighbor). Crucially, this residual is a
**content mis-grouping** (a turn lands in the wrong column / gets the wrong
`send_id`), **not** prompt duplication — the journal still owns the prompt, so the
headline bug stays fixed. Its **user-visible symptom** is a *completed answer
rendered under a `cancelled` badge* (wrong status on a real answer) — not a
duplicated or missing prompt; worth knowing because "wrong status" is more
alarming than "mis-grouped" sounds. It is strictly narrower than the in-flight
breakage tail-anchoring would cause, and it is the precise trigger for the
deferred key-join (below).

### 2. Frontend — make the journal marker authoritative for the cancelled badge (`columnState`)

Today `columnState` (`UnifiedTranscript.svelte:202-211`) returns the agent row's
status whenever an agent row exists, falling back to the Outcome marker only when
there is none. So a cancelled-mid-turn — persisted by the parser as `Streaming`
(Claude) or `Failed` (Codex/Gemini/Antigravity) — plus its cancelled marker
renders as a **live spinner** (and `streaming` keeps the cancel button active on
a dead turn) or **mislabeled "failed."**

Change `columnState` so a **non-completed Outcome marker is authoritative** for
the column's badge — it outranks the harness-derived agent status (`streaming`
*and* `failed`). The journal is the source of truth for non-completed outcomes by
the whole split-source design; failed turns already carry a `failed` marker, so
`failed`+`failed` stays consistent.
- This resolves the **cancel-after-end_turn** race toward **"cancelled"**
  (disk `Complete` + cancelled marker → badge `cancelled`) — deliberately, for
  **live↔reopen parity**: in that race the dispatcher synthesized `Cancelled`, so
  *live* showed cancelled; reopen must match. (If product later prefers a
  genuinely-finished answer to keep a "complete" badge, this is the single knob;
  recommendation is parity = cancelled.)
- **Scope:** the override is inherently per-turn — a fan-out **column is a single
  `(send_id, agent_id)` pair**, so it holds exactly one turn plus its own marker;
  a marker can never paint a *different* live turn cancelled. Document that
  single-send-column invariant in `columnState` so a future change that lets a
  column span sends trips a flag (it would need send/turn-scoped matching then).
- **`columnState` alone is necessary but not sufficient** — it sets the column's
  badge (`data-state`), but the column's agent row independently renders the
  live "Working…" footer (spinner + live cancel button) for any turn whose
  *harness* status is `streaming`. A cancelled-mid Claude turn reopens as
  `streaming`, so without a second gate the column reads `cancelled` yet still
  shows a phantom spinner on a dead turn. Gate `turnBody`'s streaming footer on a
  `live` flag and pass `live = (columnState === "streaming")` at the column site,
  so the live affordance only renders when the column is *genuinely* live (no
  authoritative non-completed marker).
- **Single-recipient (standalone) path — fixed too** (initially deferred, then
  pulled in: the deferral's premise was wrong). A single-agent cancelled-mid turn
  reopens as a standalone `agentRow` plus a sibling `outcomeRow`, hitting the same
  two bugs (phantom footer for Claude `streaming`; `failed`+`cancelled` double
  badge otherwise) on the *more common* path. The authority signal is already
  available — the backend stamps `send_id` on both the turn and the marker — so a
  render-time `Set<${agent_id}:${send_id}>` of marker-owned turns (`hasOutcomeFor`)
  gates the standalone `turnBody` footer and `turnStatusLabel`, mirroring
  `colHasOutcome`/the column gate. Turns with no `send_id` (pre-journaling/
  imported) are never in the set and render unchanged.

## Tests

**Backend (`merge_project_conversation`, fixture-driven):**
- Fan-out, one recipient cancelled-mid-turn (has disk content): **one** grouped
  prompt; the cancelled agent's partial content grouped under it with its
  `send_id`; the cancelled Outcome marker still present (render-both); **no**
  imported/duplicate prompt; **no** phantom bare marker for a "missing" turn.
- **cancel-after-end_turn** (the race that breaks a disk-status partition):
  cancelled send + disk turn that reads `Complete` → paired to the cancelled
  send, one prompt, no duplicate, no orphan.
- **Trailing interleave `[completed, cancel-after-end]`** (both disk turns
  `Complete`, one completed + one cancelled send): correct cross-boundary
  assignment — completed turn to completed send, the `Complete`-on-disk cancelled
  turn to the cancelled send; one prompt each, no duplicate/orphan.
- **`[cancel(partial), completed]`** (cancel-mid then completed): correct
  send_ids, one prompt each, no duplicate.
- **Cancelled-before-output with the prompt recorded on disk (dangling user
  turn)** — a `Turn::User` with no following agent turn for a cancelled send:
  prompt **dropped** (journaled), bare cancelled marker rendered, no duplicate,
  and sends after it still align. (Distinct from the "nothing on disk" case
  below; this is the prompt-drop half's subtle shape.)
- **Streaming-completed tail** (one completed send, disk one `Streaming` turn, no
  Outcome marker): asserts **grouping** only — paired to the *completed* send,
  one prompt, no duplicate. (With no marker, `columnState` still returns
  `streaming`; that spinner is the pre-existing M2 running-vs-finished limitation,
  **out of scope** here — this test does not assert it's resolved.)
- **Cancelled-before-any-output, nothing on disk:** one prompt + a bare cancelled
  marker, no content, no duplicate.
- **Idempotent re-merge** (switch-back): re-running the merge yields the same
  items — no growth.
- **Documented residual — `[cancel-pre-output, completed]` (leading contentless):**
  *characterizes* the known mis-grouping (the completed answer lands under the
  cancelled send's column / gets its `send_id`) so the bound is a conscious
  decision — **not** a correctness assertion. The prompt is still journal-owned
  (no duplication).
- **Gate (must stay green — these pin front-alignment / in-flight):**
  `merge_in_flight_send_does_not_mislabel_completed_turns` (`:10191`),
  `merge_trailing_in_flight_prompt_*` (`:9989`),
  `merge_cancel_before_harness_flush_*` (`:10151`), and the bare-CLI / pre-journaling
  characterization tests (`:3089-3106`). **Run the rewrite against them**, don't
  assume — they are exactly what tail-anchoring would have broken.
- The documented residual (`[completed, cancel-pre-output, completed]`):
  characterize current behavior so the known-bound is a conscious decision.
- **Gate:** the existing `merge_project_conversation` characterization tests
  (completed turns, imported bare-CLI prompts at `:3089-3106`, pre-journaling
  history) must pass unchanged — run the rewrite against them, don't assume.

**Frontend (`columnState`, component-level):**
- A reopened cancelled-mid-turn column — agent row `Streaming` **and** `Failed`
  variants — plus a cancelled marker → badge `cancelled` (not a spinner, cancel
  button not live).
- A failed-mid-turn column keeps its `reason` (marker not suppressed).

## Alternative considered — durable key-join (deferred, breadcrumb only)

Record the cancelled turn's `hydration_key` (the first-message-id from M1, which
the disk partial **already carries**, and which the dispatcher sees via the M4
`TurnIdentity` event) on `JournalRecord::Outcome`, then join the disk partial to
its outcome by key — *exact*, no ordering heuristic, and it dissolves the
documented residual above. Deferred because: it needs a **core-schema** field +
dispatcher state, and it's **go-forward only** (existing Outcome records have no
key), so the forward-walk heuristic is needed for old journals regardless —
making the key-join *additive*, not a replacement. It is cheaper than the first
draft implied (M1 already persists the key on disk). **Revisit it when the
documented residual proves to bite** — that mis-grouping shape is its trigger.
Note this in `docs/harness-behavior.md`.

## Additional bug found during real-data verification: Gemini tool-result fragmentation

Verifying the fix against real captured sessions (`~/repos/temp`, projects
`dedup-test` / `dedup-test-23`) surfaced a **separate, pre-existing Gemini
session-parser bug** that produced the same user-visible symptoms (blank user
rows + fragmented/"duplicated" fan-out tool rows) and was being mistaken for the
merge bug.

**Root cause** — `crates/harness/src/gemini/session_file.rs::handle_user`. Gemini
echoes every tool result back into the transcript as a standalone `type:"user"`
record carrying a `functionResponse` block (no prompt text), *in addition* to the
inline `toolCalls[].result` on the agent's `gemini` record. `handle_user` treated
each one as a fresh user prompt → it closed the current agent turn and pushed a
**blank-text `Turn::User`** on every tool round-trip. One logical agent turn with
N tool calls became N+1 turns (each a one-tool fragment) interleaved with N blank
user rows. The merge's order-correlation then scattered the fragments (one send
vs ~12 disk turns → `turn_offset` ≫ 0) as standalone rows *outside* the fan-out
column — the exact stack of repeated `GEMINI` tool rows in the repro.

**Fix** — skip `functionResponse` user records in `handle_user` (don't close the
turn, don't emit a `Turn::User`); the inline `toolCalls[].result` already carries
the output, so nothing is lost. After the fix the real session parses to **2
turns** (one prompt + one whole agent turn with all 17 tool items) instead of
25+. Regression test:
`function_response_user_records_do_not_fragment_the_agent_turn`.

**Isolated to Gemini.** Codex binds tool results via `function_call_output`
response items and Antigravity distinguishes `is_tool_result()` from prompts —
neither fragments. No merge/journal change; this is purely a read-side parser fix,
so existing on-disk sessions render correctly once rebuilt.

## Scope / non-goals

- **Two loci** (cancelled-mid dedup): backend `merge_project_conversation` +
  frontend `UnifiedTranscript.svelte` (`columnState` badge authority **and** the
  `turnBody` live-footer gate at the column site — both needed; see piece 2).
  (The first draft's "backend-only, no component change" was wrong — without the
  badge fix the marker is inert and the turn renders as a spinner.) Plus the
  Gemini parser fix above, found during verification.
- **No schema change:** no `TurnStatus::Cancelled`, no journal field (that's the
  deferred key-join), no `ConversationItem` change. Render-both is preserved.
- Live cancel behavior is unchanged (already correct).
