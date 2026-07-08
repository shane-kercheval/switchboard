# Durable send↔turn correlation (fix transcript prompt-duplication)

**Status:** proposed
**Date:** 2026-07-06

## Why this exists

A user hit a reproducible bug: in a long-running Claude project, the *latest* user
message renders **twice** — once correctly (before the agent's response) and once
as a stray duplicate *after* it. As newer messages arrive, the duplicate follows
the newest prompt (the earlier one "heals").

We traced it end-to-end against the real files (parser + merge run over the actual
session). The mechanism, precisely:

1. The project's conversation is assembled by correlating two independently-produced
   streams — the **journal** (`journal.jsonl`, the user's sends) and the **harness
   session file** (the agent's turns, which also echo each prompt). The journal's
   `turn_id` and the harness file's ids are different id spaces, so
   `merge_project_conversation` (`crates/app/src/commands.rs`) correlates them **by
   position/count**.
2. Positional correlation is only correct when the two streams stay the same length.
   A `/compact` (context auto-compaction) in the Claude session desynced them two ways:
   - The bare `/compact` record parses into a `Turn::User` with
     `source = Unknown` (the parser's housekeeping filter catches the
     `<command-name>/compact</command-name>` sibling but **not** the bare `/compact`).
     One in-window `Unknown`-source prompt trips the merge's "straddling" guard
     (`ambiguous_unknown`, `commands.rs`), which **abandons** the reliable
     provenance-based correlation and falls back to the fragile **count-based** path.
   - Compaction also leaves a **continuation agent turn with no preceding prompt**.
     That is one extra agent turn (125 agent turns vs 124 journal sends), so the
     count-based path runs out of sends and renders the **last** prompt as an
     "imported" user message (`send_id: None`), which the frontend's send-anchoring
     floats *below* the response.

Confirmed: running the real count-path classifier over the real turns imports exactly
one prompt ("Proceed", the newest), matching the report.

**Two independent problems, both real:**
- **Classification** — a non-dispatched record (`/compact`) was treated as a
  correlating prompt. (This is a denylist-of-housekeeping treadmill: every new Claude
  bookkeeping shape is a fresh hole.)
- **Correlation** — matching by position couples every record to every other, so any
  single stray record shifts everything after it.

This plan fixes both, for the harnesses we ship: Claude Code, Codex, and Antigravity.
(Gemini is being decommissioned and is out of scope for this work.)

## Design principles (establish here, reused by every milestone)

These are the load-bearing decisions from the discussion. They must survive into the
code as doc-comments on the types/functions that embody them — not just live here.

1. **Separate "render" from "correlate."** Whether a harness record is *shown* in the
   transcript is a different question from whether it *participates in the send↔turn
   join*. Non-prompt events (compaction, slash-commands, background-task notices,
   continuations) may render but must **never** consume a send slot or advance a
   correlation counter. The codebase already does this for the compaction *summary*
   (`Turn::System` → `SystemMarker`, explicitly "invisible to prompt↔send
   correlation"). The bug is that `/compact` wasn't routed into that category. This
   principle is the direct answer to "we want `/compact` to show without it breaking
   the join": show it as a non-correlating marker.

2. **A correlating prompt is defined by a positive dispatch signal, not by the absence
   of known junk.** Where a harness gives us provenance (Claude `promptSource: sdk`),
   trust it; classify everything without that signal as non-correlating (render-only),
   not as a reason to abandon provenance.

3. **Correlate by a durable key where one exists; fall back to position only where it
   doesn't.** The structural fix is to join each send to its harness turn by a shared,
   stable per-turn id (`hydration_key`) that Switchboard already observes live —
   eliminating counting for harnesses that have such an id. Position remains the
   fallback for harnesses/turns with no key (Antigravity; pre-feature history), and is
   *only* exercised there.

   Why a fallback at all (and why the durable join was deferred until now): **not every
   harness has a per-turn id.** Antigravity has none; the id is also only knowable
   *after* a turn emits content, so a turn cancelled before output never gets one.
   A universal key-join is therefore impossible — the count path cannot be deleted,
   only demoted. That is the honest reason this wasn't done earlier, and it is why the
   plan stages the join per harness.

## Required reading (read before implementing)

Internal (primary — this is where harness ground-truth lives):
- `docs/system-design.md` §3 ("conversation source-of-truth is split") and §7
  ("Sends and turns", "Unified history after restart"). The correlation contract and
  the "no agent content in the journal" invariant live here; both are touched.
- `docs/harness-behavior.md` — the gap register and the per-harness behavior matrix.
  Especially **G22** (Claude background-agent `<task-notification>` continuations) and
  the cost sidecar's `message.id` re-join (§3.1) — the closest existing precedent for a
  durable per-turn key-join.
- `docs/implementation_plans/2026-06-07-cancelled-mid-turn-reopen-dedup.md` — where the
  order/count correlation and its known residuals were established. This plan supersedes
  the "correlate by order" posture for key-bearing harnesses.
- `docs/research/archive/claude-code-observed.md` and the
  `claude-background-agent-*.jsonl` probe fixtures — the raw record shapes for
  compaction, `promptSource`, and continuation turns.
- Source of truth for the pieces you'll change: `crates/harness/src/claude_code/session_file.rs`
  (`handle_user`, `is_user_housekeeping`, `classify_prompt_source`),
  `crates/harness/src/transcript.rs` (`Turn`, `SystemMarker`, `UserPromptSource`),
  `crates/harness/src/events.rs` (`AdapterEvent`/`NormalizedEvent` `TurnEnd`,
  `first_message_id`/`hydration_key`), `crates/core/src/journal.rs`,
  `crates/dispatcher/src/lib.rs` (the turn-drive loop, `record_send`/`record_outcome`),
  `crates/app/src/commands.rs` (`merge_project_conversation`,
  `classify_turns_by_provenance`, `classify_turns_by_count`),
  `src/lib/state/unified.ts` (render merge).

External (for harness contracts; verify against the archived probes, which are the
authority for what we actually observe):
- Claude Code headless / SDK mode and session files (`promptSource`, `isMeta`,
  `isCompactSummary`): https://docs.anthropic.com/en/docs/claude-code — this is the
  origin of the `sdk`/`typed`/`queued` provenance we key on. If the exact page has
  moved, trust `docs/research/archive/claude-code-observed.md`.

## One product decision to confirm before starting M1

We **want `/compact` (and similar) to remain visible** (user's explicit ask). The clean
way is a new non-correlating `SystemMarker` variant (`SlashCommand`-style) so the record
renders as a marker but never correlates — one mechanism that also covers future slash
commands. Wrinkle: for `/compact` specifically, the compaction **summary** already
renders its own `Compaction` marker, so the session would show *both* a "ran /compact"
marker and the compaction summary — mildly redundant but informative.

**Recommendation:** add the non-correlating marker and accept the minor double-marker.
It honors "show it," generalizes, and is strictly better than today (where `/compact`
corrupts correlation). If you'd rather not double up, the alternative is to **drop** the
bare `/compact` record (the compaction stays visible via its existing summary marker) —
smaller, but then a slash command that has *no* summary (e.g. `/clear`) would show
nothing. Confirm which you want; M1 is written for the marker approach and notes where
the drop-only variant differs.

---

## Milestone 1 — Non-dispatched records don't correlate (Part A)

### Goal & Outcome

Stop non-prompt harness records from participating in send↔turn correlation, at the
source, and render them as non-correlating markers. This alone fixes the reported bug
for Claude and keeps the count-based fallback (which Antigravity and legacy history
still use) clean.

Outcomes:
- A Claude session containing `/compact` (or another slash-command bookkeeping record)
  no longer duplicates any user message, live or after reload.
- `/compact` remains **visible** in the transcript as a non-correlating marker (pending
  the decision above).
- The provenance-based correlation is no longer abandoned just because a bookkeeping
  record appears inside the journaled window.
- No regression for genuinely user-typed prompts that happen to start with `/`
  (the `typed`/`queued` provenance exemption still protects them).

### Implementation Outline

The fix belongs in the **parser** (`claude_code/session_file.rs`), because
classification is a source-level concern and this keeps the merge's provenance guard
protecting only the case it was meant for (genuine pre-`promptSource` dispatches).

1. **Reclassify bare slash-command / bookkeeping user records that currently leak
   through as `Turn::User { source: Unknown }`.** Today `is_user_housekeeping` catches
   the `<command-name>…</command-name>` wrapper and `isMeta`, but the sibling **bare
   `/compact`** record (no `isMeta`, `promptSource` absent, text literally `/compact`)
   slips through into a real user turn. Route these to a non-correlating outcome:
   - **Marker approach (recommended):** add a `SystemMarker` variant for a slash
     command (the enum's own doc already anticipates "a state-changing slash command"
     as an additive variant) and emit `Turn::System` for these records, exactly like
     the compaction summary path. Carry the command text.
   - **Drop approach (alternative):** treat them as housekeeping and emit no turn.

   The detection predicate is **narrow and two-part**: reclassify only when
   **`promptSource` is absent** (the record parses to `source: Unknown`) **AND** the
   trimmed text is a **bare slash-command** (e.g. `/compact`). Both parts are
   load-bearing:
   - *Absent, not `∉ {typed,queued}`*: a dispatched prompt carries `promptSource: sdk`
     and a bare-TUI prompt carries `typed`/`queued`. Keying on **absent** protects all
     three (a user who types `/compact` into Switchboard's compose bar dispatches as
     `sdk` and must stay a correlating prompt). This is stricter than the plan's earlier
     wording, which wrongly included `sdk`.
   - *Bare-slash shape required*: `promptSource`-absent alone is **not** housekeeping — a
     genuinely dispatched prompt on a **pre-`promptSource` Claude CLI** is also absent
     (the straddling case). Those render as normal prose, never `/compact`, so the shape
     check is what separates housekeeping from a real pre-marker dispatch. Do **not** drop
     it to "absent ⇒ reclassify."

   Ground truth (traced from the reported session, shape confirmed): the leaking record
   is `{"type":"user","message":{"role":"user","content":"/compact"}}` with **no**
   `promptSource` and **no** `isMeta`; its `<command-name>/compact</command-name>` sibling
   is already dropped by the existing prefix denylist. A committed fixture with these
   exact bytes backs the parser test (see DoD).

2. **Confirm the merge now stays on the provenance path.** With no in-window
   `Unknown`-source *user* turn, `ambiguous_unknown` is false and
   `classify_turns_by_provenance` runs. Do **not** remove the `ambiguous_unknown` guard
   or the count path — they still protect genuinely-straddling pre-`promptSource` files
   and drive keyless harnesses. Add a doc-comment at the guard clarifying that
   *bookkeeping* records are now filtered upstream, so the guard fires only for true
   pre-marker dispatch history. The continuation agent turn (no preceding prompt) is
   already handled by provenance (`Agent(None)`, rendered un-grouped) — verify, don't
   rebuild.

3. **Frontend (marker approach only):** the new `SystemMarker` variant flows to the UI
   as a `ConversationItem::SystemMarker` and through `unified.ts` unchanged (it already
   carries `marker` opaquely). Add a render case for the new variant in the marker
   component; unknown variants already degrade gracefully, so this is additive.

Rationale to preserve in code: a one-line "why" at the reclassification site — *this
record is harness bookkeeping, not a dispatched prompt; it renders but must not consume
a send slot* — with a pointer to the render/correlate split principle.

### Definition of Done

- **Unit tests (parser):** a bare `/compact` record produces a non-correlating outcome
  (marker or no turn), **not** a `Turn::User`; a `promptSource: sdk` record whose text is
  `/compact` (user typed it into the compose bar) **stays** a correlating `Turn::User`;
  a `typed`/`queued` `/…` prompt stays a correlating `Turn::User`. Assert against a
  committed fixture carrying the real bytes (below) — compaction is absent from the
  existing archived probes (they're all background-agent captures), so this is the
  ground truth to add, not reconstruct.
- **Integration test (merge):** a fixture transcript reproducing the reported scenario
  — N `sdk` prompts = N sends, plus a `/compact` and a post-compaction continuation
  turn — yields **zero** imported (`send_id: None`) user messages and the newest prompt
  renders once. This is the regression test for the reported bug; assert it against the
  merge output directly.
- **Frontend test (marker approach):** the new marker renders; unrelated rows are
  unaffected.
- **Docs:** note in `docs/harness-behavior.md` that bare slash-command records are
  classified non-correlating (with the reason). If a new `SystemMarker` variant lands,
  it's self-documenting via its doc-comment.
- **Known limitation recorded:** this is Claude-specific classification; Codex/Antigravity
  bookkeeping shapes are audited in M3.

---

## Milestone 2 — Durable send↔turn key-join for Claude (Part B core)

### Goal & Outcome

Replace positional correlation with a **key-join** for turns that carry a durable
per-turn id, and persist that id so the join survives restart. Claude works end-to-end
in this milestone because its live `TurnEnd` already carries the disk-matching
`hydration_key`. This milestone establishes the join contract that M3 extends to the
other harnesses.

Outcomes:
- After restart, each Claude turn that produced output (completed **or**
  failed-with-content) is matched to its originating send by a shared id, not by
  counting — so an extra/missing/mis-ordered harness record can no longer shift a
  *different* send's correlation.
- The reported duplication class cannot recur for Claude even if a future bookkeeping
  shape slips past M1's classifier (the join doesn't depend on stream alignment).
- Turns with no resolvable key (in-flight, pre-feature, keyless harness) fall back to
  the existing positional path with **no behavior change** from today.
- The journal still stores **no agent content** — only an opaque id (a `message.id`),
  consistent with the system-design §3 invariant.

### Implementation Outline

Three pieces, in dependency order. The join key is the harness's **first non-subagent
assistant `message.id`** — already surfaced live on `NormalizedEvent::TurnEnd.hydration_key`
(Claude) and computed at parse time by the Claude parser as `Turn::Agent.hydration_key`.
The two are equal by construction (events.rs documents "live-matched"), which is exactly
what makes the join possible.

1. **New journal record: the send↔key link (`crates/core/src/journal.rs`).** Add a
   `#[non_exhaustive]` variant carrying `send_id`, `turn_id` (the dispatcher's, for
   symmetry with the other records), `agent_id`, the durable `hydration_key`, and a
   timestamp. Contract and rationale (put in the variant's doc-comment):
   - Written on **any real adapter terminal that carries a `hydration_key`** —
     `Completed` **or** `Failed`. A crash-truncated turn still tags its `Failed`
     terminal with the first assistant `message.id` (`claude_code/mod.rs` truncation
     test), and that partial content *is on disk* and needs the same correlation as a
     completed turn. Restricting the link to completed turns would leave failed-with-
     content turns on the fragile positional path — the exact class of mis-grouping the
     key dissolves.
   - This is the first journal record written for a turn that isn't non-completed —
     until now the journal wrote only `Send` (turn-start) and `Outcome` (non-completed).
     It carries **no agent content**; a `message.id` is an identifier, not content, so
     the §3 invariant holds. Say this explicitly in the doc-comment (a future reader
     will otherwise think it violates the invariant).
   - Absent for keyless harnesses (Antigravity) and terminals that ended before emitting
     a key (a cancel before any assistant output) — those keep positional correlation.

2. **Capture and persist the link at the turn terminal (`crates/dispatcher/src/lib.rs`).**
   In the turn-drive loop, the dispatcher already owns the mapping `send_id → this turn`
   and observes the terminal `TurnEnd` (which carries `hydration_key`). When a turn
   completes with a `hydration_key`, write the link via a new method on the journal
   handle (sibling to `record_send`/`record_outcome`). Write it whenever the terminal
   carries a `hydration_key`, regardless of `Completed` vs `Failed` — the same terminal
   handler that already calls `record_outcome` for non-completed turns
   (`dispatcher/src/lib.rs`, where both `outcome` and the adapter's `first_message_id`
   are in hand).
   - Sequencing/contract: this write is **not** fail-closed (unlike `record_send`). A
     failed link write must not fail the turn — the turn already produced content; the
     merge simply falls back to positional for it. Log and continue. State this
     (it's the opposite of `record_send`'s posture, and the difference is deliberate).
   - A non-completed turn still writes its `Outcome`. A **failed-with-content** turn
     therefore writes **both** — the link correlates its partial on-disk content, the
     `Outcome` supplies the failed badge. They are complementary, exactly as the
     dispatcher already treats non-completed outcomes and content-bearing turns. A
     cancel/pre-start terminal with no key writes only the `Outcome`.

3. **Key-join in the merge (`crates/app/src/commands.rs::merge_project_conversation`).**
   Build a per-agent **render plan over the original turn indices** — do **not**
   physically pre-filter the arrays and re-run the classifier naively. That naive
   approach has two verified defects the plan must foreclose:
   - `classify_turns_by_count`'s `turn_offset` / `dangling_journaled` /
     `agent_turn_count` are precomputed from the agent's **full** arrays and then used
     to index `all_sends` by position (`commands.rs:4179-4200`). Feed the classifier a
     shortened "residual" and that arithmetic silently misaligns — potentially *worse*
     than today in the mixed-history case. So any residual pass must have **all** of
     those counts **recomputed from the residual view**, never inherited.
   - "Drop the user prompt immediately preceding a key-matched agent turn" is fragile
     adjacency. Suppress the harness's echoed prompt via the classifier's existing
     **reply/provenance** pairing instead. Reuse `classify_turns_by_provenance`'s walk,
     whose `pending_send` **already survives an intervening `Turn::System`** (it clears
     only on a non-consuming `Turn::User`) — do not hand-roll a weaker rule than what
     already exists.

   Mechanics:
   - Build `hydration_key → send_id` from the agent's link records.
   - A `Turn::Agent` whose `hydration_key` matches gets its linked `send_id` immediately
     and marks that send **claimed** (removed from the positional pool).
   - Residual provenance/count classification runs only over **unclaimed** turns and
     sends, offsets recomputed from that residual view.

   Contract/decisions the agent must not have to guess:
   - **Key-join is authoritative;** a claimed send never re-enters positional.
   - **Continuation turns do not consume a send.** An agent turn with no paired
     correlating prompt — a post-`/compact` continuation is the live example — must
     render `Agent(None)` (un-grouped) and claim **no** send. On the provenance path
     (Claude, post-M1) this is already the behavior (`pending_send` is `None`); the plan
     only needs to **not regress it** here. (The count/residual path — Codex, legacy —
     must uphold the same invariant; that is called out in M3, where the count path is
     actually exercised.) Note: this is the *only* multi-turn-per-dispatch case that
     matters — background-agent (`Agent`-tool) dispatches are **already reunified to one
     turn / one key** by the G22 fix, so they leave no unlinked continuation.
   - **Steady state is all-linked:** once shipped, every new key-bearing terminal has a
     link, so the positional path is inert for current history. Positional exists only
     for (a) pre-feature turns, (b) in-flight turns (no link yet), (c) keyless harnesses.
   - **Mixed transition** (one agent with both pre- and post-feature turns) must be
     **provably no-worse** than positional-only — assert against the *recomputed*
     offsets, since "no worse" is not automatic from filtering.
   - Do not change the frontend: agent turns still carry `send_id`; the merge just
     assigns it more reliably. Reload grouping improves for free.

Rationale to preserve in code: at the merge, a comment stating *key-join is exact and
authoritative; the positional classifiers are the fallback for keyless/legacy turns
only* — and that this supersedes the "correlate by order" note from the 2026-06-07 plan
for key-bearing harnesses.

### Definition of Done

- **Unit tests (journal):** the new record round-trips; a journal without link records
  reads back exactly as today (backward compatibility).
- **Unit/integration tests (dispatcher):** a `Completed` turn **and** a `Failed`
  turn that carries a `hydration_key` (crash-truncated) each write a link with the
  correct `send_id`; a cancel/pre-start terminal with no key writes an `Outcome` and
  **no** link; a link-write failure does **not** fail the turn.
- **Integration tests (merge):**
  - with link records present, agent turns match their sends by key even when the
    positional path *would have* mis-aligned (construct a transcript with a spurious
    extra turn and assert the key-join still pairs correctly);
  - with **no** link records, output is byte-identical to today (the count/provenance
    path is untouched);
  - mixed legacy+linked history for one agent behaves no-worse than positional,
    asserted against the **recomputed** residual offsets (not merely "no worse");
  - a **failed-with-partial-content, key-linked** turn renders as **one** conversation
    item carrying *both* its content and its failed status — not a duplicate (this is
    the seam where double-render bugs like the reported one hide);
  - a post-`/compact` **continuation** turn renders `Agent(None)`, unlinked and
    un-grouped, and consumes no send — unchanged by the key-join.
- **Live test** (`live_claude_*`, per the naming convention): a real dispatch's
  `hydration_key` on live `TurnEnd` equals the value the parser computes from the
  written session file (guards the equality the join depends on against CLI drift).
- **Docs:** update `docs/system-design.md` §3 to describe the link record and why it
  does not violate "no agent content" (it stores an id, not content); update §7's
  correlation description to "key-join with positional fallback."

---

## Milestone 3 — Extend key-join to Codex; Antigravity + legacy fallback; audit & docs

### Goal & Outcome

Make every shipped harness correct: Claude and (newly) Codex use the durable key-join;
Antigravity and pre-feature history use the positional fallback, documented as such.
Includes the per-harness classification audit that M1 flagged and the cross-cutting
documentation.

Outcomes:
- Codex turns are matched to sends by key after restart (not by counting), closing the
  same duplication class M1/M2 closed for Claude.
- Antigravity is explicitly, knowingly on positional correlation (it has no per-turn
  id), and that limitation is recorded where users and developers will find it.
- Each harness's non-prompt bookkeeping records are confirmed non-correlating (the M1
  principle applied per harness), so the positional fallback stays aligned wherever it
  still runs.

### Implementation Outline

1. **Surface the disk-matching key on the live `TurnEnd` for Codex.** The Codex *parser*
   already computes `Turn::Agent.hydration_key` from **`turn_context.turn_id`** — **not**
   `task_started.turn_id`, which the code explicitly rejects because its
   per-turn-uniqueness is unconfirmed and "a non-unique dedup key drops new turns
   silently" (`codex/session_file.rs:757-772`). But the **live** adapter emits
   `hydration_key: None` — live↔disk parity is marked **unprobed**.
   - **Probe** (a `make test-live-codex` round-trip) that the id on the live stream
     equals the parser's `turn_context.turn_id`. Record the finding in
     `docs/harness-behavior.md` (probe-verified vs. inferred).
   - If parity holds, surface that id on the live `TurnEnd`. The live adapter **already
     parses `turn_context`** for model/effort (`codex/mod.rs`), so exposing its
     `turn_id` alongside is a small addition, not new plumbing — and then the M2
     dispatcher capture writes links for Codex with **no change to M2's dispatcher or
     merge code** (the point of the M2 contract).
   - If the live stream genuinely cannot produce the disk-matching id, leave it `None`
     and document Codex as positional-fallback like Antigravity — do not invent a
     synthetic or `task_started`-derived key.

2. **Antigravity + legacy + count-path invariant: confirm clean fallback.** No key-join
   for Antigravity (no per-turn id — confirmed in `dispatcher/src/lib.rs` and the
   antigravity research notes). Verify the M2 merge composition leaves Antigravity and
   pre-feature history on exactly today's positional path. **Because Codex and legacy
   use the count/residual path, explicitly verify the M2 "continuation turns consume no
   send" invariant holds there** — the provenance path gets it for free via
   `pending_send`, but the count path assigns a send to every agent turn by position, so
   a continuation-shaped turn could steal a residual send unless handled. Record
   Antigravity's positional-only correlation as a **known limitation** (gap register in
   `docs/harness-behavior.md`; a one-line `README.md` note only if a user would observe
   it — e.g. "after a compaction, Antigravity history may mis-group" — otherwise keep it
   developer-facing).

3. **Per-harness classification audit (M1 principle, applied out).** For Codex and
   Antigravity, identify any non-prompt user-side records their session files contain
   (compaction/continuation/tool-continuation/notification analogs) and confirm the
   parser classifies them non-correlating. Fix any that leak into `Turn::User` the way
   bare `/compact` did. Scope this to shapes we can evidence from the probes — do **not**
   speculatively invent categories.

### Definition of Done

- **Live test** (`live_codex_*`): live `TurnEnd` key == parsed `turn_context.turn_id`;
  a multi-turn dispatch reloads with every turn key-matched.
- **Integration tests:** Codex transcripts with link records key-join correctly; an
  Antigravity transcript (no keys) uses the positional path unchanged; a continuation
  turn on the count/residual path consumes no send; any newly-classified bookkeeping
  record for Codex/Antigravity is covered by a parser test.
- **Docs:**
  - `docs/harness-behavior.md`: per-harness key-join status (which harness has a
    live-matching key, probe-verified), the Antigravity positional-only limitation, and
    the bookkeeping-record classifications found in the audit.
  - `docs/system-design.md`: the capability matrix (§9) reflects durable correlation per
    harness.
  - `README.md`: user-facing limitation entry only if user-observable.
- **Known limitations recorded:** Antigravity (and any harness whose live stream lacks
  the disk key) remains on positional correlation; the residuals of the positional path
  (from the 2026-06-07 plan) persist only where the fallback still runs.

---

## Out of scope (do not build)

- Retroactive migration of old journals to add link records — the fallback handles
  legacy history and the join self-heals going forward. Explicitly not needed.
- A synthetic per-turn id for Antigravity — there is no disk-matching id to join on;
  inventing one would be a fake key that can't survive reload. Positional stays.
- Any workflow/forward correlation changes — this plan is scoped to the
  send↔turn transcript join. If the audit (M3) surfaces a workflow-side correlation
  issue, stop and raise it rather than expanding scope here.
