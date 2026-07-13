# Per-message metadata & cache-aware context formula

Fixes three related defects the user observed on agent cards and consolidates two stale investigation notes into one buildable plan. The unifying idea is a **taxonomy decision**: some harness metadata is *current agent state* and belongs on the agent card; some is *per-turn telemetry* and belongs with the message that produced it. Today everything is jammed onto the card, which produces a confusing accumulating cost total, a context bar stuck near 0%, and an overage flag detached from the turns it describes.

> **Sequencing:** [`2026-05-31-session-identity-into-registry.md`](2026-05-31-session-identity-into-registry.md) executes **before** this plan. It cleans up the per-agent `sessions/` directory (deletes the session-link sidecars, consolidates session identity into the registry), so Milestone 4's new `<agent-id>.turnmeta.jsonl` lands in the cleaned-up structure. Milestone 4 specifically depends on that refactor having landed.

> **Post-implementation correction (2026-07-13):** Claude's live `output_tokens` is a whole-dispatch billing aggregate, not necessarily the final parent call's output. The current normalized numerator is therefore `context_tokens_after_turn`, computed from the final parent assistant envelope's input + cache-read + cache-creation + own output. The context-window snapshot carries both the exact selected `modelUsage` key and final assistant `message.id`; hydration requires both to match the source turn, and legacy snapshots without either provenance field clean-hide. The sidebar evaluates the newest non-streaming usage-bearing agent turn and never falls back to an older percentage when that telemetry is missing or invalid. Derived token arithmetic is checked; parent-occupancy overflow hides context telemetry, while a separate whole-dispatch aggregate overflow falls back without poisoning valid parent context. See `docs/harness-behavior.md` G29.

## Supersedes

This plan absorbs and replaces two 2026-05-18 notes (both "investigation needed," never implemented). Delete them when this plan's milestones land; do not leave them as live notes.

- `2026-05-18-note-claude-context-cache-formula.md` (deleted — resolved by Milestone 1) — the context bar ignored prompt-cache tokens; the cache-aware occupancy formula landed in M1.
- `2026-05-18-note-claude-cost-context-persistence.md` (deleted — resolved by Milestones 2 and 4) — cost/context vanished on project reopen because they're stream-only; context-window persistence landed in M2 and per-turn cost/overage persistence in M4.

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

- [`docs/harness-behavior.md`](../harness-behavior.md) §3.1 (event ⟂ on-disk parity — the class-A/B/C model this plan's persistence decisions rest on). **Read first.**
- The two superseded notes above — they contain the root-cause analysis (Anthropic prompt-cache token accounting; why cost/context aren't in the session file) that this plan compresses.
- [`docs/system-design.md`](../system-design.md) §2 (cost surface — no pricing tables, no cross-harness aggregation, no per-token display) and §3 (split source of truth: harness files own completed-turn content, the journal owns the user's side; the `meta.json` sidecar from the failure-metadata plan owns stream-only metadata).
- [`docs/ui-conventions.md`](../ui-conventions.md) — semantic tokens + `ui/` primitives for the Sidebar and transcript changes.
- Anthropic prompt caching & usage accounting (for Milestone 1's formula): <https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching> and the `usage` object in <https://docs.anthropic.com/en/api/messages>. Confirm empirically that `cache_creation_input_tokens` and `cache_read_input_tokens` are *additive, distinct* contributors to context occupancy (they are) before writing the formula.

---

## Milestone 1 — Cache-aware context-utilization formula

### Goal & Outcome

The "context after last turn: X%" bar reflects *true* context occupancy, not just the marginal uncached input. The per-harness token-accounting reconciliation lives in the adapter (where harness knowledge belongs); the frontend formula stays harness-agnostic and operates on a normalized occupancy value.

Outcomes:
- A Claude agent deep into a conversation shows a context percentage that grows with the conversation (e.g. tens of percent), not a near-0% that barely moves. The current bug — caching makes Claude's `input_tokens` count only the new prompt, so `(input + output)/window` underestimates by ~10× — is gone.
- **Codex is not regressed.** Claude and Codex report cached tokens with *opposite* semantics: for Claude `cache_read`/`cache_creation` are **disjoint** additions to `input_tokens`, but for Codex (OpenAI) `cached_input_tokens` is a **subset already inside** `input_tokens`. So a single "sum all the token fields" rule would be correct for Claude and *double-count* Codex (whose bar is correct today). The fix reconciles this **per-harness in each adapter**, not with a `match harness` in the frontend — see the Implementation Outline. Harnesses without a context window stay cleanly hidden (Gemini, Antigravity) — unchanged.
- This milestone fixes the *live-session* bar fully. The bar still blanks on **reopen** until Milestone 2 (the denominator `context_window` is stream-only); that's expected and called out, not a regression.

### Implementation Outline

**The numerator is "total tokens occupying the window after the final parent call," and is computed per harness.** The input side differs because of the cache-accounting asymmetry below. The output side must also come from that same parent call: Claude's live `output_tokens` is whole-dispatch billing telemetry and can include auxiliary/subagent output that never entered the parent's context. So the reconciliation happens in each adapter, which already owns its harness's usage shape:

- For **Claude**, the input-side total is `input_tokens + cache_read + cache_creation` (disjoint contributors). Today the parser captures `cache_read_input_tokens` (as `cached_input_tokens`) but **drops `cache_creation_input_tokens` entirely** — read on neither the live `result` path (`crates/harness/src/parser.rs`, `extract_usage_from_result`) nor the session-file path (`crates/harness/src/claude_code/session_file.rs`, `parse_usage`). Both must capture it.
- For **Codex**, the input-side total is just `input_tokens` (it already includes the cached subset; `cache_creation` doesn't exist for Codex).

**Carry the reconciled values as normalized fields, not a frontend computation.** `context_input_tokens` retains the harness-reconciled input side. `context_tokens_after_turn` carries the complete final-parent occupancy: that input side plus the same call's own output. Every session-file enrichment path populates it from per-turn data; Claude's live parser populates it from the final non-subagent assistant envelope, not the aggregate terminal result. Gemini leaves it `None` because it exposes no window. The frontend does one harness-agnostic `context_tokens_after_turn / context_window` — **no `match harness`**, consistent with the `RateLimitSource` / real-spend-signal pattern elsewhere in this plan.

- **Keep the raw fields faithful.** `input_tokens` / `cached_input_tokens` / `cache_creation_input_tokens` continue to mirror exactly what the harness reported (existing parser tests assert this). `context_input_tokens` is the *derived* reconciliation; do not mutate the raw fields to make a frontend sum work — that would break the faithful-mirror contract those fields document.
- **Also add the `cache_creation_input_tokens` raw field** to `TurnUsage` + TS mirror (`Option<u64>` / `number | null`, absence representable) — Claude's parsers read it exactly as they read `cache_read_input_tokens`, and the Claude `context_input_tokens` computation consumes it. (M2/M4 don't need this raw field, but it makes Claude's occupancy reconstructable on reopen and keeps the telemetry complete.)
- Update `contextUtilization` (`src/lib/components/Sidebar.svelte`) to `context_tokens_after_turn / context_window` from the newest non-streaming agent turn carrying terminal usage. If either operand is absent/zero, or occupancy exceeds the window, return `undefined` and clean-hide the bar; do not scan backward to an older turn, fabricate a denominator, or clamp invalid telemetry to 100%. A currently streaming turn has no terminal usage and leaves the last completed value visible while it runs.
- The bar's label keeps its meaning but the interpretation sharpens to "how full is the conversation's context right now"; no label change needed (verify the existing copy still reads correctly — it should).
- **Record the rationale in a code comment** at each adapter's `context_input_tokens` computation (why Claude sums the cache fields but Codex does not — the disjoint-vs-subset asymmetry) and at the frontend formula (it consumes a pre-reconciled value; do not re-add per-harness summation here). This asymmetry is the single most likely thing to silently regress later.

### Definition of Done

- **Verify Codex's cache semantics empirically first.** Confirm against a real Codex `turn.completed.usage` (or a captured session file) that `cached_input_tokens` is a subset of `input_tokens`, not an addition, before finalizing the Codex computation. This is the Codex-side analogue of the Claude additive-tokens check in Required Reading. If Codex turns out to report disjoint cached tokens, its computation matches Claude's instead — record the finding either way.
- **Unit tests (Claude parser, both paths):** a `result`/session-file record carrying `cache_creation_input_tokens` populates the raw field *and* yields `context_input_tokens == input + cache_read + cache_creation`; a record lacking the cache fields yields `None` for the raw field (not `0`-Some) and a `context_input_tokens` of just `input` (no fabricated cache). Mirror the existing `cache_read` test shape.
- **Unit tests (Codex parser, both paths) — the no-double-count guard:** a `turn.completed` with `input_tokens` and a `cached_input_tokens` *subset* yields `context_input_tokens == input_tokens` (NOT `input + cached`). This is the regression this milestone must not introduce.
- **Frontend unit test (`contextUtilization`):** a turn with large `context_input_tokens` relative to a tiny raw `input_tokens` produces a meaningfully-non-zero utilization (the Claude bug fixture); a Codex-shaped turn does not over-report; a turn with no `context_window` still yields `undefined` (bar hidden); a turn with `context_input_tokens == null` yields `undefined`.
- **Docs:** none beyond the code comments, plus deleting the superseded `context-cache-formula` note once merged.
- **Known limitation (record):** the bar still blanks on reopen until Milestone 2 — note it where the reader will look (the formula comment or the note's deletion commit).

---

## Milestone 2 — Persist context-window for restart continuity (per-agent snapshot)

### Goal & Outcome

The context bar survives project reopen. With Milestone 1, the numerator tokens are already on disk in the harness session file; the only missing piece on reopen is the denominator, `context_window`, which is stream-only (lives in Claude's `result.modelUsage.<model>.contextWindow`, never in the session file). Persist it.

Outcomes:
- Reopen a Claude project with prior conversation → the context bar renders immediately (sourced from a persisted snapshot), instead of blanking until the next send.
- The latest snapshot is joined back only to the final assistant `message.id` that produced it, with the selected model key as a second integrity check. This reuses the existing per-agent snapshot rather than retaining a historical window log.
- Codex is unaffected (its `context_window` is already in its session file — class B, durable). Gemini/Antigravity have no window — still hidden.

### Implementation Outline

**Why a snapshot, not a per-turn record.** The context bar reflects only the latest terminal turn, so retaining one last-write-wins window snapshot is sufficient. Effective capacity is not safely reusable by model name alone: capability/beta state can change for the same resolved model, and the harness transcript can advance outside Switchboard. The snapshot therefore carries the source turn's final assistant `message.id` and selected model key and overlays only onto that exact turn. This is still the shape of the existing `meta.json` sidecar (`crates/harness/src/meta_sidecar.rs`); no historical window log is needed.

- Add a `context_window` field (with `captured_at`, selected `model`, and final assistant `message_id`) to `MetaSidecar` as an **additive `Option`** whose provenance fields use `#[serde(default)]`. **Do not bump `schema_version`.** Existing v1 files remain readable; snapshots without both provenance fields are represented but never overlaid.
- Write path: the dispatcher routes stream-only metadata to `MetadataCache::record_context_window` only when `TurnEnd` carries a selected window, valid parent occupancy, and `stable_message_id`. Same best-effort, warn-and-drop-on-error posture — the sidecar is a UX improvement, not load-bearing.
- Read path: `apply_meta_sidecar_overlay` fills `usage.context_window` only on the hydrated `Turn::Agent` whose `stable_message_id` and model match the snapshot and whose occupancy fits the window. A missing match, missing usage, existing durable window, or invalid value is a no-op. Never create a synthetic turn or `TurnUsage`.
- No age qualifier is needed. Turn identity, model identity, and numeric plausibility determine whether the snapshot applies; otherwise it clean-hides.

**Boundary with Milestone 4.** This milestone persists *only* the latest `context_window` snapshot. Its message-id field validates which turn owns that one value; it does not turn the snapshot into the historical append-log used for cost/overage in Milestone 4.

### Definition of Done

- **Backend tests:** a valid `result` writes a model- and message-bound snapshot; hydration restores it only to the exact turn; legacy, message/model-mismatched, impossible, missing-occupancy, and overflowed telemetry clean-hide. A transcript advanced outside Switchboard must not inherit the older snapshot.
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

**Ordering verified → stamp in the Claude parser, not the dispatcher (deviation from the "dispatcher stamps" wording above).** The stamp needs the turn's latest `rate_limit_event` to be seen before the terminal event. Verified against claude 2.1.161 across normal + tool-use turns: the `rate_limit_event` always precedes the terminal `result`, and none arrive after it. Because the Claude parser sees both lines in order with a shared `ParserState`, it stashes the overage (`isUsingOverage` + `overageResetsAt`) on the `rate_limit_event` and stamps `TurnSpend` onto the `TurnEnd` it builds from the `result`. This keeps **all** Claude knowledge in the Claude parser and leaves the **dispatcher entirely free of overage logic** (it just forwards the `TurnEnd`, whose `spend` rides through `into_normalized`) — strictly cleaner than the dispatcher-stamps idea, and squarely on the "adapter owns the signal, no `match harness` in the dispatcher" principle. Non-Claude parsers and all synthesized terminals set `spend: None`. **Guarded by a live test** (`live_claude_rate_limit_precedes_result`): since the stamp moves an *upstream-CLI* ordering+presence assumption onto the money-facing path, a fixture (which replays the assumed order) can't catch drift — the live test asserts, against the real CLI, that a `rate_limit_event` is emitted and precedes the terminal `TurnEnd`. The `isUsingOverage == true` branch itself is **not** live-coverable (overage can't be forced on demand), so that residual risk is accepted knowingly.

**Define the canonical per-turn metadata type — do not let it land ad hoc.** Today the per-turn surface carries only `usage`: live `NormalizedEvent::TurnEnd { usage }` (`crates/harness/src/events.rs`) and hydrated `Turn::Agent { usage }` (`crates/harness/src/transcript.rs`), mirrored in TS `LoadedTurn` and state `Turn` (`src/lib/state/types.ts`). There is **nowhere** for the overage stamp to land without a decision, and an implementer left to improvise could tuck it into `usage`, a raw JSON blob, or separate runtime state — producing live↔hydrated drift where the marker shows live but is lost or misread on reopen. Add one explicit, additive field carried on **all** of: live `TurnEnd`, hydrated `Turn::Agent`, TS `LoadedTurn`, state `Turn`. Suggested shape (implementor refines): `TurnSpend { real_spend: bool, is_overage: bool, overage_resets_at: Option<DateTime<Utc>> }`. The renderer reads **this single field** — never `last_rate_limit` (which stays Bucket-A card state). Keep `real_spend` (the harness-agnostic render gate) distinct from `is_overage` (the Claude-derived source) so the documented cost-visibility seam stays honest. The M4 join key is a *separate* field (different lifecycle — internal plumbing, not rendered); see Milestone 4.

- **Render the inline cost + overage surface.** The per-message meta row is `messageMeta` in `src/lib/components/UnifiedTranscript.svelte` (currently timestamp + copy only). Add cost (`usage.total_cost_usd`) and the "using credits" marker, both rendered only when the turn's real-spend/overage signal is set. **Scope to agent turns only:** `messageMeta` is a *shared* snippet (user rows, outcome rows, agent turns, and the fan-out column layout all call it). Render the cost/marker at the agent-turn call sites only — do not add it to the shared snippet unconditionally, or it leaks onto user/outcome rows that have no spend. Reuse the amber `warning` semantic token already used for the card's overage line; keep the cost visually subordinate per `ui-conventions.md`. **Scoped to the single-agent `agentRow` path:** the fan-out column renders one column-level meta for potentially several turns, where per-column cost attribution is ambiguous, so the fan-out meta deliberately does **not** show per-message cost in M3 (revisit if fan-out cost display is wanted).
- **Delete the card cost total.** Remove `sessionTotalCost` and its `agent-cost` render in `src/lib/components/Sidebar.svelte`, including the now-dead summing logic and the `harness === "claude_code"` gate. Do not replace it with a different aggregate — per system-design §2 there is no cross-turn cost aggregation surface in v1.
- **Keep the current overage status on the card** as Bucket-A current state, distinct from the per-turn attribution. It continues to read the latest `last_rate_limit` snapshot's `isUsingOverage`.
- **Record the seam in a comment** at the gate: the frontend renders on a real-spend signal, currently sourced from per-turn overage; a harness that bills differently sets the signal differently in its adapter. No policy-config object (see the cost-visibility decision).
- **Fix the stale `last_rate_limit` type comment** in `src/lib/types.ts` if still present (it claims Claude never populates it — false; it's an opaque payload populated by both Claude and Codex). *(As of this writing the `types.ts` comments describe `last_rate_limit` accurately, so this is likely already a no-op — verify and move on.)*

### Definition of Done

- **Backend/dispatcher tests:** a turn completing while the latest rate-limit shows `isUsingOverage` is stamped with the overage snapshot; a normal-quota turn is stamped with no overage. `grep` confirms no `match harness` in the stamping path.
- **Component tests (`UnifiedTranscript`):** an overage Claude turn renders both cost and the "using credits" marker in its meta row; a normal-quota Claude turn renders neither (assert absence — no empty label); a Codex turn renders no cost regardless.
- **Component tests (`Sidebar`):** the `agent-cost` card total is gone (assert the testid no longer renders); the context bar, rate-limit window, and current-overage status still render under their existing conditions.
- **Docs:** update `harness-behavior.md` §3 + the G7 entry — overage/cost attribution now renders **per-message gated on real-spend**, not as a sidebar line; the sidebar keeps only the neutral rate-limit window + a current overage status. Note in the component the deliberate absence of a card cost total (so it isn't "helpfully" re-added). **`system-design.md` was updated ahead of implementation to match this reframe** (done in the planning change, not pending M3): §7's sidebar list no longer claims "last-turn cost (Claude Code)"; the §2 per-harness sidebar table row "Cost $ (per turn + session aggregate)" became "Overage status (current)" (per-turn cost is now a per-message transcript surface, no card total); §7's status item is now "Rate-limit / quota signal"; and §2's cross-harness-aggregation bullet notes the removed per-agent card total. **Note this means system-design describes the target until the M3 code lands — the docs lead the code through this milestone.** No further system-design edits needed for M3; verify they still read correctly when the code merges.

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

- **Recommended key: Claude's per-message id.** Claude stamps an id on each message that appears in *both* the live stream and the on-disk session file. The captured data shows **two candidates** (`crates/harness/tests/fixtures/claude/with-usage.jsonl`): a top-level per-record `uuid`, and the Anthropic `message.id` (e.g. `msg_test31`) inside the assistant message. The implementing agent picks whichever is stable across live↔disk for the same message (the `message.id` is the API-level message identity and is the stronger prior, but verify). **Reconcile this with the Definition of Done:** the DoD currently words the gate around `uuid`, while this outline favors `message.id`. Validate *both* against a real captured session file and **commit to one in the plan/commit before building** — do not leave the outline and the DoD pointing at different keys. The session-file parser reads neither today (it only reads tool-use block `id`s), so this milestone adds: (1) the live path captures the turn's assistant-message id and emits it on `AdapterEvent::TurnEnd` (e.g. `stable_message_id: Option<String>` — a field distinct from the rendered `TurnSpend` of Milestone 3, since it's internal join plumbing, not displayed); (2) the session-file parser exposes that same id on the hydrated `Turn::Agent`; (3) persisted metadata is keyed by it; (4) hydration joins by it. Exact, per-turn — strongly preferred over timestamp/ordinal heuristics.
- **The anchor rule is load-bearing — pin it, don't leave it as "e.g."** A Claude turn with tool use contains *several* assistant messages, each with its own `message.id`/`uuid`, but `total_cost_usd` arrives on the per-turn `result` record that names none of them. The live and hydration paths **must choose the same anchor message**, or cost lands on the wrong turn (or fails to join). Specify: anchor on the turn's **final non-subagent assistant message**. This aligns with existing behavior on the disk side — the session-file parser already overwrites `builder.usage` on each assistant record (`session_file.rs`), so the *last* assistant message is already the de-facto usage anchor; keying the join to the same message keeps live and disk consistent by construction. A tool-use fixture with ≥2 assistant messages must assert the final answer-bearing id is the key.
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

- **Join validation (gate, do first): ✅ PASSED — key locked to `message.id`.** Verified against a real captured Claude session file (multi-call tool-use turn, claude latest): for every assistant message, *both* the Anthropic `message.id` and the record-level `uuid` round-trip identically between the live stream and the on-disk session file, in the same order. Chose **`message.id`** (the final non-subagent assistant message's) over `uuid` because `message.id` is per-*logical-message* while `uuid` is per-*record* — a tool-call message was observed spanning two records that shared one `message.id` but had two different `uuid`s, so `message.id` is the robust anchor (and aligns with the disk parser's existing "last assistant record wins" usage anchor). Cost lives on the `result` record (no `message.id`), so it's attached to the final assistant message's id at turn-end. The gate held, so the join is built; no escalation.
- **Backend tests: ✅ done.** Dispatcher path persists `{message.id → cost, overage}` at `TurnEnd` (`real_spend_turn_with_message_id_is_persisted_to_metadata_cache`), and is gated on the join key + `real_spend` — a non-real-spend turn and a real-spend turn lacking a `stable_message_id` are both *not* persisted (`non_real_spend_turn_is_not_persisted`, `real_spend_turn_without_message_id_is_not_persisted`). The turn-metadata sidecar round-trips and skips corrupt lines (`turnmeta_sidecar` unit tests). App hydration joins by `message.id` onto the matching hydrated turn (`apply_turnmeta_overlay` unit tests + the e2e `load_transcript_rejoins_persisted_turn_spend_for_claude_agent`); a turn with no record hydrates with no cost/overage; empty/corrupt store → no-op (no panic). `grep` confirms no `match harness {…}` in the dispatcher/persistence path (gate on key presence + capability, not harness identity).
- **Component/reducer tests: ✅ done.** The hydrate reducer carries a `LoadedTurn.spend` onto the state turn (`carries a hydrated turn's persisted spend onto the state turn`) and leaves it undefined when absent (`leaves spend undefined for a hydrated turn with no persisted record`); the existing `UnifiedTranscript` render tests prove a state turn with `spend.real_spend` renders the inline cost + "using credits" marker and one without renders neither — so the reopen chain (persist → re-join → hydrate → render) is covered end-to-end across layers.
- **Manual verification (or can't-run note):** in `make dev`, incur overage on a Claude turn, quit, reopen → that turn still shows its cost and an overage marker; a normal (normal-quota) turn shows **no cost and no overage marker** — matching the cost-visibility decision (cost appears only on real-spend turns) and the M4 component tests above ("a turn without renders neither"). *(This line previously read "a normal turn shows cost," contradicting the load-bearing decision in three other places; corrected.)*
- **Docs: ✅ done.** `harness-behavior.md` — §3.1 parity table gained a per-turn cost/overage row (class C, closed by the turn-metadata sidecar), a new **§3.1-cost** subsection documents the `message.id` join + verification + no-backfill limitation, and G7 notes M4 makes the per-turn `spend` durable. The `2026-05-18-note-claude-cost-context-persistence.md` note (whose context half was already done in M2, cost half in M4) is deleted and its inbound links updated. `system-design.md` §3 — the per-agent metadata section + the directory-layout block + §10.3 now describe the per-turn telemetry append-log (`.turnmeta.jsonl`) as distinct from the `.meta.json` snapshot.

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
