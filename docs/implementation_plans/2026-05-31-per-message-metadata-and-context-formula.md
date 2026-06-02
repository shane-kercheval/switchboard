# Per-message metadata & cache-aware context formula

Fixes three related defects the user observed on agent cards and consolidates two stale investigation notes into one buildable plan. The unifying idea is a **taxonomy decision**: some harness metadata is *current agent state* and belongs on the agent card; some is *per-turn telemetry* and belongs with the message that produced it. Today everything is jammed onto the card, which produces a confusing accumulating cost total, a context bar stuck near 0%, and an overage flag detached from the turns it describes.

> **Sequencing:** [`2026-05-31-session-identity-into-registry.md`](2026-05-31-session-identity-into-registry.md) executes **before** this plan. It cleans up the per-agent `sessions/` directory (deletes the session-link sidecars, consolidates session identity into the registry), so Milestone 4's new `<agent-id>.turnmeta.jsonl` lands in the cleaned-up structure. Milestone 4 specifically depends on that refactor having landed.

## Supersedes

This plan absorbs and replaces two 2026-05-18 notes (both "investigation needed," never implemented). Delete them when this plan's milestones land; do not leave them as live notes.

- [`2026-05-18-note-claude-context-cache-formula.md`](2026-05-18-note-claude-context-cache-formula.md) — the context bar ignores prompt-cache tokens (→ Milestone 1).
- [`2026-05-18-note-claude-cost-context-persistence.md`](2026-05-18-note-claude-cost-context-persistence.md) — cost/context vanish on project reopen because they're stream-only (→ Milestones 2 and 4).

It also picks up the **per-message metadata attribution** that [`2026-05-27-harness-failure-metadata-surfacing.md`](2026-05-27-harness-failure-metadata-surfacing.md) explicitly deferred ("Correlating which messages happened during overage was considered and deferred; if ever needed it lands additively in the journal at turn-start"). That plan's Milestone 3 built a per-*agent* `meta.json` snapshot for the rate-limit payload; this plan adds the per-*turn* dimension it left out.

## The taxonomy (established here, reused by every milestone)

Two buckets. State which bucket a field is in before deciding where it renders.

- **Bucket A — current agent state → the agent card (Sidebar).** Latest-value, "as of now." Answers *"what is this agent's situation right now."* Members: rate-limit window + reset time, context-window fullness, current overage status. These legitimately live on the card.
- **Bucket B — per-turn telemetry → the message/turn that produced it.** Answers *"what did this specific exchange cost / consume."* Members: per-turn cost and the overage state in effect when that turn ran — which collapse into **one inline surface** (see the cost-visibility decision below): cost is shown only on turns that incurred real, user-billed spend, which is exactly the overage condition. These belong inline next to a turn's timestamp, **not** as card state.

The defect being corrected is that per-turn cost was placed in Bucket A (summed into an accumulating card total) and overage was placed in Bucket A as a bare current flag with no per-turn attribution. This plan moves both to Bucket B where they belong, while keeping genuine Bucket-A state (context fullness, rate-limit window) on the card.

### Cost-visibility decision (load-bearing — gate on a per-turn signal, not on harness identity)

Per-turn cost is shown **only on turns that incurred real, user-billed spend** — never as an always-on figure. For subscription Claude, `total_cost_usd` is a *notional* API-equivalent cost that isn't money actually charged unless the agent is in overage; showing it on every turn implies spend that didn't happen. So the rule is: **render the per-turn cost (and the "using credits" marker) when, and only when, the turn's real-spend signal is set.** This merges cost and overage into the single inline surface above.

The gate is a **per-turn semantic signal ("this turn incurred real spend"), not a `harness === "claude_code"` branch.** The frontend applies one harness-agnostic rule; the **adapter owns what sets the signal** (where harness knowledge belongs). For Claude, real-spend == `isUsingOverage`, so the Claude adapter derives the signal from the overage state. A future pay-per-use harness would set it whenever cost is present; harnesses that report no cost never set it. This removes the existing frontend harness-identity gate (`agent.harness === "claude_code" && cost > 0`), which is the smell.

**Scope discipline — do not build a configurable per-harness cost-display *policy* object.** For v1 the per-turn real-spend signal *is* the overage state we already need; there is no second harness with a different rule, so a policy-config system would be speculative machinery. Document the seam in a comment ("gate is real-spend, currently == overage; a harness that bills differently sets the signal differently in its adapter") so the extension point is obvious without being prematurely built. Revisit only when a second harness actually needs a different rule (e.g. Claude pricing changes, or a pay-per-use harness lands).

### Per-harness reality (don't over-index on Claude)

The mechanism is harness-agnostic; the *data availability* is not. A field renders only where the harness actually reports it (the existing clean-hide-on-absence convention — never a blank label).

| Field | Bucket | Claude | Codex | Gemini | Antigravity |
| --- | --- | --- | --- | --- | --- |
| Per-turn cost (`total_cost_usd`) | B (per-message) | ✅ stream-only | ✗ none | ✗ none | ✗ none |
| Overage state (`isUsingOverage`) | A (status) + B (attribution) | ✅ stream-only | ✗ | ✗ | ✗ |
| Rate-limit window + reset | A (card) | ✅ stream-only | ✅ session-file | ✗ | ✗ |
| Context-window fullness | A (card) | ✅ window stream-only | ✅ session-file | ✗ no window | ✗ no window |

"stream-only" = arrives in a live event, **absent from the harness's own session file**, so it dies on app restart unless Switchboard persists it. This is the crux that Milestones 2 and 4 exist to solve. Cost and overage are Claude-only realities in v1 — the per-message work is, in practice, a Claude surface, but nothing in the design hard-codes that (a future harness that reports cost gets it for free).

## Required reading (before implementing)

- [`docs/research/harness-behavior.md`](../research/harness-behavior.md) §3.1 (event ⟂ on-disk parity — the class-A/B/C model this plan's persistence decisions rest on). **Read first.**
- The two superseded notes above — they contain the root-cause analysis (Anthropic prompt-cache token accounting; why cost/context aren't in the session file) that this plan compresses.
- [`docs/system-design.md`](../system-design.md) §2 (cost surface — no pricing tables, no cross-harness aggregation, no per-token display) and §3 (split source of truth: harness files own completed-turn content, the journal owns the user's side; the `meta.json` sidecar from the failure-metadata plan owns stream-only metadata).
- [`docs/ui-conventions.md`](../ui-conventions.md) — semantic tokens + `ui/` primitives for the Sidebar and transcript changes.
- Anthropic prompt caching & usage accounting (for Milestone 1's formula): <https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching> and the `usage` object in <https://docs.anthropic.com/en/api/messages>. Confirm empirically that `cache_creation_input_tokens` and `cache_read_input_tokens` are *additive, distinct* contributors to context occupancy (they are) before writing the formula.

---

## Milestone 1 — Cache-aware context-utilization formula

### Goal & Outcome

The "context after last turn: X%" bar reflects *true* context occupancy, not just the marginal uncached input. Backend captures one currently-dropped field; the frontend formula includes the cached prefix.

Outcomes:
- A Claude agent deep into a conversation shows a context percentage that grows with the conversation (e.g. tens of percent), not a near-0% that barely moves. The current bug — caching makes `input_tokens` count only the new prompt, so `(input + output)/window` underestimates by ~10× — is gone.
- The fix is correct for any harness that reports cached tokens; harnesses without a context window stay cleanly hidden (Gemini, Antigravity) — unchanged.
- This milestone fixes the *live-session* bar fully. The bar still blanks on **reopen** until Milestone 2 (the denominator `context_window` is stream-only); that's expected and called out, not a regression.

### Implementation Outline

The utilization numerator must represent everything occupying the window: `input_tokens + cache_read + cache_creation + output_tokens`. Today the parser captures `cache_read_input_tokens` (as `cached_input_tokens`) but **drops `cache_creation_input_tokens` entirely** — it's read on neither the live `result` path (`crates/harness/src/parser.rs`, `extract_usage_from_result`) nor the session-file path (`crates/harness/src/claude_code/session_file.rs`, `parse_usage`). Both paths must capture it.

- Add a `cache_creation_input_tokens` field to `TurnUsage` (`crates/harness/src/events.rs`) and its TS mirror (`src/lib/types.ts`), as an `Option<u64>` / `number | null` so absence stays representable (older agents, harnesses that don't report it). Wire both parsers to read `cache_creation_input_tokens` exactly as they already read `cache_read_input_tokens`.
- Update `contextUtilization` (`src/lib/components/Sidebar.svelte`) to sum all four token contributors over the denominator. Keep the existing guard: if `context_window` is absent/zero, return `undefined` and the bar stays hidden (do not fabricate a denominator).
- The bar's label keeps its meaning but the interpretation sharpens to "how full is the conversation's context right now"; no label change needed (verify the existing copy still reads correctly — it should).
- **Record the rationale in a code comment at the formula** (why cached + cache-creation tokens are included: they occupy the window even though they're billed differently; excluding them is only ever correct for a *cost* calc, which this is not). This is the single most likely thing to silently regress later.

### Definition of Done

- **Unit tests (parser, both paths):** a Claude `result`/session-file record carrying `cache_creation_input_tokens` populates the new field; a record lacking it yields `None` (not `0`-Some). Mirror the existing `cache_read` test shape.
- **Frontend unit test (`contextUtilization`):** a turn with large `cache_read`/`cache_creation` and tiny `input_tokens` produces a meaningfully-non-zero utilization (the bug fixture); a turn with no `context_window` still yields `undefined` (bar hidden).
- **Docs:** none beyond the code comment, plus deleting the superseded `context-cache-formula` note once merged.
- **Known limitation (record):** the bar still blanks on reopen until Milestone 2 — note it where the reader will look (the formula comment or the note's deletion commit).

---

## Milestone 2 — Persist context-window for restart continuity (per-agent snapshot)

### Goal & Outcome

The context bar survives project reopen. With Milestone 1, the numerator tokens are already on disk in the harness session file; the only missing piece on reopen is the denominator, `context_window`, which is stream-only (lives in Claude's `result.modelUsage.<model>.contextWindow`, never in the session file). Persist it.

Outcomes:
- Reopen a Claude project with prior conversation → the context bar renders immediately (sourced from a persisted snapshot), instead of blanking until the next send.
- No per-turn join is required for this milestone (see rationale) — it reuses the existing per-agent snapshot mechanism, keeping it cheap and low-risk.
- Codex is unaffected (its `context_window` is already in its session file — class B, durable). Gemini/Antigravity have no window — still hidden.

### Implementation Outline

**Why a snapshot, not a per-turn record.** `context_window` is effectively a per-*model* constant (Claude's window is fixed per model; it changes only if the agent switches models — see [`2026-05-30-per-agent-model-selection.md`](2026-05-30-per-agent-model-selection.md)). The context bar reflects the *latest* turn's fullness, so the *latest-known* window is the right value. That means this needs only a last-write-wins per-agent snapshot — exactly the shape of the existing `meta.json` sidecar the failure-metadata plan built for the rate-limit payload (`crates/harness/src/meta_sidecar.rs`). **Extend that sidecar; do not invent a parallel store.**

- Add a `context_window` field (with its own `captured_at`, following the existing `RateLimitSnapshot` precedent) to `MetaSidecar`. Bump `schema_version` per that module's convention; an unrecognized version already reads as empty (forward-compatible).
- Write path: the dispatcher already routes stream-only metadata to the `MetadataCache` trait. Add a sibling to `record_rate_limit` (e.g. `record_context_window`) and call it when a `result` event carries a `contextWindow`. Same best-effort, warn-and-drop-on-error posture — the sidecar is a UX improvement, not load-bearing.
- Read path: the existing `apply_meta_sidecar_overlay` (hydration) already fills `last_rate_limit`/`last_rate_limit_as_of` "only if empty." Extend it to also surface the persisted `context_window` so `contextUtilization` finds a denominator on the latest hydrated agent turn. The implementing agent decides the cleanest overlay mechanics against the code — the contract is: *after hydration, the most recent Claude agent turn has a usable `context_window` sourced from the snapshot when the session file lacks one.*
- Apply the existing **staleness** convention if it's cheap and meaningful — but note the window value itself doesn't really go stale (a model's window is fixed), so an "as of" qualifier on the *context bar* is likely unnecessary. Don't add one unless it falls out naturally; this is a Bucket-A state value, not a volatile one.

**Boundary with Milestone 4.** This milestone persists *only* `context_window`, and *only* as a per-agent snapshot. It deliberately does **not** touch cost or overage (those need per-turn attribution → Milestone 4). Keeping them separate isolates this cheap, no-join win from the one milestone that carries join risk.

### Definition of Done

- **Backend tests:** a `result` event with `contextWindow` writes the snapshot; hydration with a pre-existing snapshot yields a `context_window` on the latest agent turn; missing/corrupt sidecar → no panic, bar simply hidden (best-effort). Mirror the existing rate-limit sidecar tests.
- **Manual verification (or can't-run note):** in `make dev`, hold a Claude conversation, quit, reopen → context bar shows immediately. Confirm a Codex agent writes no `context_window` snapshot (its window is session-file-backed).
- **Docs:** `harness-behavior.md` §3.1 — note `context_window` joins the rate-limit payload as a persisted class-C field; delete the `cost-context-persistence` note's *context* half (its cost half is closed by Milestone 4).

---

## Milestone 3 — Per-message cost + overage surface & agent-card cleanup (live-session)

### Goal & Outcome

Move per-turn cost and overage attribution off the card and next to the message that incurred them, gated by the per-turn real-spend signal (the cost-visibility decision); delete the confusing accumulating card total; keep genuine Bucket-A state on the card. Delivers the reframe for the *live session* (reopen-survival comes in Milestone 4).

Outcomes:
- A Claude agent turn that ran **in overage** shows its cost **and** a "using credits" marker together, inline next to its timestamp. A normal-quota turn shows neither (no cost, no marker) — matching the decision that cost appears only on real-spend turns. Codex/Gemini/Antigravity never show cost (clean-hide).
- The accumulating per-agent `$` total is **removed** from the card — it was a partial, non-durable, resets-on-reopen aggregate that read as a running total but wasn't one.
- The card retains genuine Bucket-A state: the context bar (now correct + — post-M2 — durable), the rate-limit window + reset time, and a current overage *status* ("this agent is currently spending overage credits"). The "5-hour limit resets …" line stays.
- The frontend gates on a per-turn real-spend signal, not on harness identity — the existing `harness === "claude_code"` cost gate is removed.

### Implementation Outline

**The per-turn real-spend signal.** Cost is shown only on real-spend turns, and for Claude real-spend == the turn's overage state. The turn's `usage` doesn't carry overage today (the rate-limit arrives as a separate event), so this milestone introduces **stamping each turn with its overage state on the live path** — at turn end the dispatcher knows the latest rate-limit, so it tags the completing turn with the overage snapshot (`isUsingOverage` + the reset times needed to render the marker). This new per-turn datum is what both the marker and the cost gate read. (Persisting it across reopen is Milestone 4; here it's live-only.) Keep this harness-agnostic: the dispatcher stamps from whatever the adapter emitted, no `match harness`.

- **Render the inline cost + overage surface.** The per-message meta row is `messageMeta` in `src/lib/components/UnifiedTranscript.svelte` (currently timestamp + copy only). Add cost (`usage.total_cost_usd`) and the "using credits" marker, both rendered only when the turn's real-spend/overage signal is set. Reuse the amber `warning` semantic token already used for the card's overage line; keep the cost visually subordinate per `ui-conventions.md`.
- **Delete the card cost total.** Remove `sessionTotalCost` and its `agent-cost` render in `src/lib/components/Sidebar.svelte`, including the now-dead summing logic and the `harness === "claude_code"` gate. Do not replace it with a different aggregate — per system-design §2 there is no cross-turn cost aggregation surface in v1.
- **Keep the current overage status on the card** as Bucket-A current state, distinct from the per-turn attribution. It continues to read the latest `last_rate_limit` snapshot's `isUsingOverage`.
- **Record the seam in a comment** at the gate: the frontend renders on a real-spend signal, currently sourced from per-turn overage; a harness that bills differently sets the signal differently in its adapter. No policy-config object (see the cost-visibility decision).
- **Fix the stale `last_rate_limit` type comment** in `src/lib/types.ts` if still present (it claims Claude never populates it — false; it's an opaque payload populated by both Claude and Codex).

### Definition of Done

- **Backend/dispatcher tests:** a turn completing while the latest rate-limit shows `isUsingOverage` is stamped with the overage snapshot; a normal-quota turn is stamped with no overage. `grep` confirms no `match harness` in the stamping path.
- **Component tests (`UnifiedTranscript`):** an overage Claude turn renders both cost and the "using credits" marker in its meta row; a normal-quota Claude turn renders neither (assert absence — no empty label); a Codex turn renders no cost regardless.
- **Component tests (`Sidebar`):** the `agent-cost` card total is gone (assert the testid no longer renders); the context bar, rate-limit window, and current-overage status still render under their existing conditions.
- **Docs:** update `harness-behavior.md` §3 + the G7 entry — overage/cost attribution now renders **per-message gated on real-spend**, not as a sidebar line; the sidebar keeps only the neutral rate-limit window + a current overage status. Note in the component the deliberate absence of a card cost total (so it isn't "helpfully" re-added).

---

## Milestone 4 — Durable per-turn cost & overage attribution (reopen survival)

> **Required milestone, with one engineering gate.** This is the only milestone that needs a per-turn join between Switchboard-persisted metadata and turns hydrated from a harness session file, and that join has no automatic key today (see below). The gate is technical, not a scope decision: **the implementing agent must validate the join key against real live + on-disk data before building, and escalate to the human if it doesn't hold.** Do not paper over an unreliable join with timestamp/order guessing — a wrong join silently puts "this turn cost $X / was overage" on the wrong message.

### Goal & Outcome

The inline cost + overage surface from Milestone 3 survives project reopen, re-attaching to the correct message. (Milestone 3 already renders it live and already stamps each turn with its overage state; this milestone makes that stamp + cost durable and re-joins them on reopen.)

Outcomes:
- Reopen a project → past Claude overage turns still show their cost + "using credits" marker, not just live-session turns.
- The re-attachment is per-message-correct — the right cost lands on the right message, not smeared by timestamp/order guessing.
- Turns from before this feature shipped have no persisted metadata and render nothing (no backfill — documented, expected).
- Mechanism stays harness-agnostic and the dispatcher stays free of `match harness {…}`; cost/overage are Claude-only *data* today, so **only the Claude adapter derives a join key and writes records in v1.** The storage is keyed on Switchboard's `AgentId`, so a future harness that reports real-spend cost plugs in its own per-harness key without reshaping the store. Do not build Codex/Gemini/Antigravity join machinery — they carry no such data (Codex's rate-limit is already durable in its own session file; none of the three report cost).

### Implementation Outline

**What's persisted.** Per completed turn: `total_cost_usd` and the overage snapshot Milestone 3 already stamps onto the live turn (`isUsingOverage` + reset times). Both are stream-only and absent from the harness session file, so they're lost on reopen unless Switchboard persists them itself.

**The join problem (the load-bearing decision).** On the live path the dispatcher mints a `turn_id` (UUID v7) and knows the cost/overage at `TurnEnd`. On **reopen**, turns are reconstructed from the harness session file and the parser assigns *fresh* `turn_id`s (`crates/harness/src/claude_code/session_file.rs`) — the dispatcher's `turn_id` is never written to the session file, so it cannot be the join key. A stable key must exist on **both** the live write side and the hydrated read side.

- **Recommended key: Claude's per-message id.** Claude stamps an id on each message that appears in *both* the live stream and the on-disk session file. The captured data shows **two candidates** (`crates/harness/tests/fixtures/claude/with-usage.jsonl`): a top-level per-record `uuid`, and the Anthropic `message.id` (e.g. `msg_test31`) inside the assistant message. The implementing agent picks whichever is stable across live↔disk for the same message (the `message.id` is the API-level message identity and is the stronger prior, but verify). The session-file parser reads neither today (it only reads tool-use block `id`s), so this milestone adds: (1) the live path captures the turn's assistant-message id at `TurnEnd`; (2) the session-file parser exposes that id on the hydrated turn; (3) persisted metadata is keyed by it; (4) hydration joins by it. Exact, per-turn — strongly preferred over timestamp/ordinal heuristics.
- **Cost lives on a different record than the id.** `total_cost_usd` (and `contextWindow`) arrive on Claude's stream-only `result` record, *not* on the assistant message; the `result` record may not even be written to disk. So at `TurnEnd`, attach the `result`'s cost to *the turn's assistant-message id* (a turn may have several assistant messages across tool calls — anchor on a consistent one, e.g. the final answer-bearing message; the agent decides against the parser's turn-grouping). Key the sidecar on that id.
- **The agent must first verify** the chosen id read live equals the one on disk for the same message, using a *real captured session file* (not only the stream fixtures, which differ in shape from the on-disk file). If it holds, build the join. **If it does not hold, stop and escalate** — do not fall back to a heuristic without sign-off. Fallback candidates, for the escalation discussion only: `(session_id, ordinal-within-session)` (breaks on out-of-order writes / multi-session resume) and `(agent_id, started_at)` (breaks on same-second turns and clock skew). A timestamp is **not** an id.

**Where it's stored — a new per-agent turn-metadata sidecar.** Append-JSONL, one record per completed turn, in the existing per-agent sidecar directory: `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.turnmeta.jsonl` (final filename the implementor's call). It's a *third* per-agent concern in `sessions/`, distinct from what's already there:
- `<agent-id>.jsonl` / `<agent-id>.antigravity.jsonl` — **session-link sidecars** that exist *only* for Codex / Antigravity (their harness session-id is assigned at runtime, not knowable at creation; Claude/Gemini store theirs in the `AgentRecord` and have no such file). Different filenames are a legacy-vs-suffix convention quirk, not a per-harness scheme this milestone should mimic.
- `<agent-id>.meta.json` — the harness-agnostic metadata *snapshot* (Milestone 2).

For a Claude agent (the only harness with cost/overage in v1), the new file therefore sits beside that agent's `.meta.json` — no session-link sidecar is involved. Simplified record:

```jsonc
{ "message_id": "msg_test31", "total_cost_usd": 0.0125, "is_overage": true,
  "overage_resets_at": "2026-05-31T22:00:00Z", "captured_at": "2026-05-31T18:42:11Z" }
```

Two stores were rejected, for reasons that are load-bearing — do not revisit without cause:
- **Not `meta.json`** (the Milestone 2 sidecar): that is a *snapshot* (last-write-wins, one value per field). This is *many records per agent* (one per turn). Different shape; overloading it would break the snapshot semantics M2 relies on.
- **Not the journal** (`crates/core/src/journal.rs`): the journal is per-*project* and owns the *user's* side (sends) + non-completed outcome markers. This is per-*agent* *agent-side* telemetry. Despite the failure-metadata plan's "lands additively in the journal" aside, keeping a dedicated per-agent sidecar preserves the journal's clean role and keys naturally on the same `AgentId` the other per-agent sidecars use.

Best-effort throughout: corrupt/missing sidecar → render nothing for affected turns (clean-hide), never fails hydration; atomic append per the existing sidecar posture.

**Rendering.** No new render work — Milestone 3 already renders the inline cost + overage surface from the per-turn signal. This milestone only changes the *source* of that signal on the reopen path: instead of a live stamp, it comes from the persisted-then-rejoined record. The same `messageMeta` slot displays it.

### Definition of Done

- **Join validation (gate, do first):** a documented check that the live assistant-message `uuid` matches the on-disk session-file `uuid` for the same turn, against a real captured session file. Result recorded in the plan/commit. If it fails, the milestone pauses for human input — that outcome is an acceptable milestone result, not a failure to push through.
- **Backend tests:** live path persists `{uuid → cost, overage}` at `TurnEnd`; hydration joins persisted metadata onto the matching hydrated turn by `uuid`; a turn with no persisted record (pre-feature, or non-Claude) hydrates with no cost/overage (no panic); corrupt store → best-effort empty. `grep` confirms no `match harness {…}` in the dispatcher persistence path (gate on capability/key presence, not harness identity).
- **Component tests:** a hydrated turn with persisted cost/overage renders both in its meta row after reopen; a turn without renders neither; the per-message overage marker renders only when the turn's overage state is set.
- **Manual verification (or can't-run note):** in `make dev`, incur overage on a Claude turn, quit, reopen → that turn still shows its cost and an overage marker; a normal turn shows cost (per the M3 product decision) and no overage marker.
- **Docs:** `harness-behavior.md` G7/§3.1 — per-turn cost + overage now persisted and attributed; record the `uuid` join key and its verification; note the no-backfill limitation. Delete the `cost-context-persistence` note's remaining (cost) half. `system-design.md` §3 — one line that the per-turn metadata store (journal or sidecar, per the chosen home) carries stream-only per-turn telemetry for restart continuity, distinct from the per-agent `meta.json` snapshot.

---

## Out of scope (do not build)

- Cross-turn / cross-agent cost aggregation, or any per-agent cost total — explicitly removed in Milestone 3, per system-design §2.
- A new `FailureKind`, pricing tables, or per-token-count display surfaces (system-design §2).
- Backfilling cost/overage for turns that predate Milestone 4 — pre-feature turns stay blank.
- A historical metadata timeline (quota/cost over time). The stores hold latest-snapshot (Bucket A) or per-turn-attributed (Bucket B) values, not a time series.
- Context-window persistence for Gemini/Antigravity via a hardcoded per-model table — they expose no window; clean-hide is correct, not a gap.
- Reset-time parsing into a retry/queue affordance, or any auto-retry on overage.

## Decisions resolved during planning (recorded so they aren't re-litigated)

- **Per-turn cost shows only on real-spend/overage turns**, gated by a per-turn adapter-owned signal rather than a harness branch; no configurable cost-display-policy object for v1 (see the cost-visibility decision).
- **All four milestones are in scope, including M4.** Per-message cost/overage surviving reopen is a requirement, not optional polish. The only gate on M4 is the *technical* join-key verification inside the milestone — if Claude's per-message id doesn't match live↔disk, the implementing agent escalates rather than shipping a guess. No backfill of pre-feature turns (accepted).
