# Cancelled-mid-turn reopen: stop duplicating the prompt, render partial content labelled cancelled

**Status:** Proposed (rewritten after review)
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

**Algorithm — end-aligned (tail-anchored) zip of disk turns to all sends.** Merge
each agent's sends back into one dispatch-ordered list `S` (length `M`), tagged
completed/non-completed from the journal — **all** sends, not just completed.
Take the agent's disk **agent** turns `T` (length `N`). Align the **tails**, not
the heads — the most recent disk turn answers the most recent send. Concretely:
- `pairs = min(N, M)`
- `turn_offset = N − pairs = max(0, N − M)` — the first `turn_offset` disk turns
  are **pre-journaling history** (attached session, older than the first send) →
  `send_id: None`, as today.
- `send_offset = M − pairs = max(0, M − N)` — the first `send_offset` sends have
  **no disk turn**; a non-completed one renders its content-free Outcome marker
  alone (cancelled-before-output), a completed/in-flight one is dropped as today.
- For `k in 0..pairs`: `T[turn_offset + k] ↔ S[send_offset + k]`, **regardless of
  the turn's harness status** — a `Complete`-on-disk cancelled turn still pairs to
  its cancelled send, a `Streaming` completed tail still pairs to its completed
  send.

Head-anchoring instead (zip from the front) is the trap: it pairs a *leading*
contentless cancelled send to the next *completed* answer, mislabeling a finished
answer as cancelled and reproducing the offset bug. Tail-anchoring is what the
current code already does for the completed-only correlation; this generalizes it
to all sends.

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
- A **dangling** user turn (no following reply — a cancelled/failed/in-flight send
  whose prompt was recorded but produced no agent turn): journaled while
  unmatched non-completed/in-flight sends remain to account for it. The subtle
  shape is **cancelled-before-output where the prompt *was* recorded on disk** —
  a dangling user turn with no agent turn; the walk must still consume a send
  slot for it or everything after mis-aligns.
Re-derive both branch formulas against the new `turn_offset`/`send_offset`; the
**existing characterization tests at `:3089-3106` are a gate this rewrite must
pass**, not an assumption.

**Documented residual (a known-bound, not a silent gap).** The tail-aligned zip
cannot disambiguate a **content-less non-completed send sandwiched between two
completed sends** — nothing positional distinguishes which completed turn is
which (tail-anchoring fixes the leading/trailing contentless shapes but not the
interior one). Pin this the way the existing comment pins its edges (`:3089-3106`).
Crucially, this residual is a **content mis-grouping** (a turn lands in the wrong
column / gets the wrong `send_id`), **not** prompt duplication — the journal
still owns the prompt, so the headline bug stays fixed. This residual is the
precise trigger for the deferred key-join (below).

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
- **`[cancel(partial), completed]`** (reverse interleave): correct.
- **`[cancel-pre-output, completed]` (leading contentless)** — the fixture that
  passes under tail-anchoring and **fails under head-anchoring**: the lone disk
  turn pairs to the *completed* send (not the leading cancelled one), one prompt,
  no duplicate, no mislabel. This locks the anchor direction.
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
Note this in `docs/research/harness-behavior.md`.

## Scope / non-goals

- **Two loci:** backend `merge_project_conversation` + frontend `columnState`.
  (The first draft's "backend-only, no component change" was wrong — without the
  badge fix the marker is inert and the turn renders as a spinner.)
- **No schema change:** no `TurnStatus::Cancelled`, no journal field (that's the
  deferred key-join), no `ConversationItem` change. Render-both is preserved.
- Live cancel behavior is unchanged (already correct).
