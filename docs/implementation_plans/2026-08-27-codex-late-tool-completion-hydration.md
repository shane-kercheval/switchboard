# Codex asynchronous tool hydration and transcript diagnostics

**Status:** revised plan proposed; second implementation review incorporated · **Created:** 2026-08-27 · **Revised:** 2026-08-27

## Problem

Switchboard reconstructs Codex tool rows from two persisted channels in a
paginated rollout:

- `response_item/custom_tool_call` plus its matching
  `custom_tool_call_output` are the canonical record that an `exec` wrapper ran;
  their shared `call_id` identifies the wrapper and preserves a row even when
  the nested operation never emits a structured item.
- `event_msg/item_completed` carries richer structured operation data such as
  the real command argv, exit code, stdout/stderr, edit changes, or MCP result.

The structured item's id does not match the wrapper id or `call_id`. The
original parser therefore associated structured items only while a wrapper was
open: wrapper call → structured item → matching wrapper output. Codex 0.149 can
persist the same logical operation in a different order because Code Mode and
the nested tool lifecycle finish on separate asynchronous paths:

```text
wrapper call → wrapper output → token/reasoning bookkeeping → structured completion
```

This is not random transcript corruption. A 2026-08-27 scan of 995 local
rollouts found 16 late completions in four files:

- 15 were `CommandExecution`, all with `source: "unified_exec_startup"`;
- 12 of those wrapper outputs exposed a `session_id`, while three were shorter
  persistence races without one;
- one was an `McpToolCall` whose wrapper output reported a running Code Mode
  cell;
- every case crossed `token_count`; four also crossed the completed Reasoning
  item and its response-item twin; and
- the observed output-to-completion delay was approximately 0.2–2.1 seconds.

The wrapper output does not contain a hidden exact join key: none of the 16
structured item ids appeared in its wrapper output, and none matched an exposed
session/cell id. For canonical one-command wrappers, however, Switchboard can
already decode the nested `exec_command` arguments; six of the observed late
commands used that canonical shape, and all six command strings matched the
structured completion exactly. That is a useful semantic identity when it is
unique, but it must not become fuzzy text matching.

The structured event's producer-turn field required separate verification
because Switchboard deliberately does not use `task_started.turn_id` as its
durable hydration key. A 2026-08-27 audit of the same 995-rollout corpus found:

- all 6,627 `item_completed` events carried `payload.turn_id`;
- all 6,627 matched the active `task_started.payload.turn_id` in their rollout;
- 6,618 also matched the current `turn_context.turn_id`; the remaining nine
  occurred without a current `turn_context` but still matched `task_started`;
- every one of 5,714 comparable `task_started`/`turn_context` pairings was
  equal; and
- no rollout file reused a `task_started.turn_id` for two turns.

This establishes `item_completed.turn_id == task_started.turn_id` as a narrow,
file-local routing relation for persisted completion association. It does not
replace `turn_context.turn_id` as the reparse-stable frontend hydration key.
Cross-file repetitions are irrelevant because association never crosses a
rollout file. Missing, mismatched, or within-file-duplicated ids must disable
post-turn association rather than trigger a guess.

The first implementation attempted to protect against a delayed completion
being applied to a newer wrapper by entering a turn-wide `Ambiguous` state.
That design failed manual acceptance. A single childless wrapper followed by a
new wrapper disabled structured association for the rest of the turn. One real
agent rose from a handful of late-item warnings to 194 warnings; a metadata-only
replay of the subsequently grown rollout reproduced approximately 199 new
outside-interval warnings versus approximately three under the old strict
interval behavior. Some turns accumulated 37–40 warnings. The implementation
is uncommitted and must be replaced, not patched around.

There is a second product problem independent of Codex association. The
frontend copies every `ParseWarning` into `AgentRuntime` and renders the
cumulative count on the agent card. Most of these diagnostics are not
actionable, cannot be cleared, and describe salvaged or redundant data rather
than an unhealthy agent. Agent cards must not display transcript warning
counts. However, two existing warning paths mean that no history was recovered:
a stale Codex locator whose recorded session file no longer exists, and a
Gemini file containing records from multiple sessions that cannot be separated
safely. Those are real degraded-history conditions and must remain visible
through the existing per-agent hydration-error presentation. A recoverable
warning belongs on a specific tool row only when the parser knows which tool is
degraded; an unowned salvage diagnostic remains backend/test diagnostic data
and is not user-facing agent health. This warning-UX work is a deliberate
product-scope decision made after manual acceptance exposed the aggregate
counter's behavior; it is not an implementation detail required by the parser
fix.

## Goals

- Recover all currently observed healthy Codex paginated completion orderings
  without making final tool status depend on record timing.
- Use producer turn identity and exact semantic identity where available,
  rather than treating physical adjacency as the only association signal.
- Never let one uncertain completion disable otherwise normal tools later in a
  turn.
- Never attach a structured result to a different tool merely because it is the
  most recent wrapper.
- Preserve the canonical wrapper row whenever enrichment cannot be associated
  safely.
- Remove transcript-warning counts and tooltips from every agent card.
- Preserve a visible per-agent history/hydration error when a known loader
  condition prevents recovery of any transcript history.
- Surface a recoverable parser warning inside a tool row only when its owning
  tool is known and the warning materially explains degraded tool data.
- Keep genuine execution failure, cancellation, and hard transcript-load error
  states distinct from parse warnings.

## Non-goals

- Changing Codex or adding an upstream correlation id. That would be the ideal
  producer fix but is outside this repository.
- Perfect recovery of multiple overlapping operations that have no stable id
  and the same semantic signature. Switchboard must preserve canonical rows and
  decline to guess in that genuinely ambiguous case.
- Fuzzy command matching, output-text matching, unbounded nearest-neighbor
  scans, or a general event-reordering framework.
- Migrating every existing non-Codex `ParseWarning` onto a transcript item.
  Existing harness parsers may continue returning aggregate warnings for
  diagnostics; this milestone removes their agent-card presentation globally.
- Promoting recoverable per-line warnings into hard load errors merely to keep
  them visible. Existing `LoadTranscriptError`/hydration-error behavior remains
  the boundary for failures that prevent history from loading; this milestone
  only reclassifies the two known zero-recovery cases currently mislabeled as
  warnings.
- Frontend controls for dismissing or clearing warnings. The inappropriate
  card indicator is removed rather than made stateful.

## Required reading before implementing

The implementing agent must read these sources before changing code:

- `AGENTS.md`, especially the fixture/live-test policy, frontend component-test
  requirements, and the role of `docs/harness-behavior.md`.
- `docs/system-design.md` §3, for the split transcript source of truth.
- `docs/harness-behavior.md` §3.6 and G30, for Codex's persisted tool
  generations, structured-result precedence, and wrapper collapse rules. Its
  current turn-wide-ambiguity follow-up reflects the failed implementation and
  must be corrected by this milestone.
- `docs/ui-conventions.md`, for semantic warning/error tokens, icon treatment,
  tool-row primitives, and component conventions.
- `docs/implementation_plans/2026-08-22-codex-paginated-rollout-hydration.md`,
  especially facts 2–5 and the tool-attachment milestone. It is a frozen
  historical record and must not be edited.
- `crates/harness/src/transcript.rs`, especially `TurnItem`,
  `LoadedTranscript`, `ParseWarning`, and `LoadTranscriptError`.
- `crates/harness/src/codex/session_file.rs`, including the current uncommitted
  `WrapperAssociation` implementation, `decode_single_exec_wrapper`, wrapper
  collapse, all structured item handlers, turn finalization, and their tests.
- `crates/app/src/commands.rs` at the per-agent transcript response/merge path.
- `src/lib/types.ts`, `src/lib/state/types.ts`, the hydrate reducer and state
  wrapper, `Sidebar.svelte`, and `ToolCallWidget.svelte` with their tests.
- OpenAI Codex's rollout persistence policy, which establishes paginated
  `ItemCompleted` as the durable structured-item channel:
  https://github.com/openai/codex/blob/main/codex-rs/rollout/src/policy.rs
- OpenAI Codex's thread-history reconstruction, whose command-end handler says
  background completion can arrive out of order and routes it by producer
  `turn_id`:
  https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/thread_history.rs
- The upstream nested Code Mode reproduction showing a yielded command
  completing after subsequent work and after `task_complete`:
  https://github.com/openai/codex/issues/40041

Upstream source establishes the asynchronous lifecycle, but the sanitized
local corpus remains the compatibility evidence for the exact rollout records
Switchboard must parse. Do not infer an operation-level join key that the file
does not contain.

## Shared contracts established across the milestones

These decisions are load-bearing and must survive in production type/method
documentation, not only in this plan:

1. `response_item` wrapper call/output records remain canonical. A structured
   item enriches or splits those rows; absence or ambiguity never deletes the
   canonical evidence that the wrapper ran.
2. Association is scoped first by the structured event's producer `turn_id`.
   Within one rollout, it may route to a wrapper turn only when it exactly
   equals that turn's `task_started.turn_id` and that id is unique in the file.
   If the turn also has `turn_context.turn_id`, the two ids must agree. Missing,
   mismatched, or duplicated ids cannot route a completion into an already
   finalized turn. A completion emitted after `task_complete` can still enrich
   its original turn when this relation holds; physical builder finalization
   must not make persisted ordering decide correctness. This routing relation
   is deliberately separate from the existing use of `turn_context.turn_id`
   as the frontend hydration key.
3. A canonical one-command wrapper exposes a narrow semantic signature from
   the already-parsed `exec_command` arguments. A `CommandExecution` may match
   it only through exact normalized command identity and only when the match is
   unique within that producer turn. No fuzzy, substring, output, or timing
   match is acceptable.
4. The existing open-wrapper interval remains valid when no competing pending
   wrapper can accept the item. Before enriching the open wrapper, the resolver
   must reject a semantic mismatch and consider a unique older candidate; it
   must not blindly assign delayed A to currently open B.
5. A `CommandExecution` without a provable semantic signature, or an
   `McpToolCall`, may use the observed bounded sequence only while one wrapper
   is uniquely eligible: matching output, followed by `token_count` and
   optionally the Reasoning item/response pair, with no intervening wrapper
   call. Late `FileChange` has not been observed and is not admitted by this
   positional fallback. A new wrapper expires the candidate but starts its own
   ordinary association; it never blackouts the rest of the turn.
6. Each structured completion and each wrapper slot are single-use. Repeated
   commands with the same signature, multiple compatible background
   candidates, mixed children, and unexpected extra items remain ambiguous
   unless interval/order evidence uniquely resolves them.
7. An ambiguous/unowned structured item is not rendered as a standalone row:
   the wrapper is the complete audit record and a standalone item would
   duplicate a possibly different operation. Preserve the wrapper and retain
   the aggregate diagnostic for tests/developer inspection without presenting
   it as agent health.
8. A tool-local parse warning is attached only when the owning row is known.
   Its Rust wire field, TypeScript wire type, reducer mapping, and rendering
   land atomically in Milestone 2. It reuses the existing line-number/reason
   shape and explains material degradation such as an unreadable status or
   malformed structured fields. A missing structured child is normal and
   receives no tool warning by itself.
9. Structured status/output remains authoritative when association succeeds.
   A late nonzero exit or failed/declined status overrides wrapper text that
   looked successful; a successful structured empty output may replace wrapper
   boilerplate. Existing wrapper failure text remains when structured failure
   has no diagnostic.
10. Agent cards show hard hydration/listener/turn errors through their existing
    paths, but never render aggregate `ParseWarning` counts. The stale Codex
    locator and ambiguous multi-session Gemini file are hydration/history
    errors because they recover no transcript; recoverable parser diagnostics
    are not agent status.

## Milestone 1 — Order-independent Codex tool association

### 1. Goal & Outcome

Replace the failed turn-wide state machine with a per-turn association phase
that recovers observed asynchronous completions and contains genuine ambiguity
to the affected operation.

Once complete:

- All 16 observed late corpus shapes hydrate without false orphan warnings when
  their owning wrapper is uniquely recoverable.
- A completion after `task_complete` can enrich the correct original turn.
- A yielded command A completing after wrapper B starts cannot overwrite B when
  A and B have distinguishable command signatures.
- Starting wrapper B never disables structured enrichment for wrappers C, D,
  and later in the same turn.
- An unresolvable overlap preserves every canonical wrapper row and does not
  invent a result.

### 2. Implementation Outline

1. **Replace, do not extend, the failed association state.** Remove the
   turn-wide `Ambiguous` behavior and tests that require a blackout until the
   next turn. Reintroduce sanitized late-order fixtures, result-precedence
   coverage, and the independent live paginated-mode assertion as required by
   this plan, without restoring the superseded state machine or its tests.

2. **Resolve association after enough file context is available.** Collect the
   canonical wrapper facts and structured completion facts needed for each
   producer turn, then resolve them after the full rollout has been ingested (or
   through an equivalent deferred mechanism that remains able to mutate a
   completed turn). Build the file-local routing index from
   `task_started.turn_id`; reject duplicates, and require an available
   `turn_context.turn_id` to agree before using the id for post-turn routing.
   Do not replace the established hydration key or expose `task_started` as a
   frontend identity. The contract is outcome-based: `task_complete` must not
   discard the association context required by a later event carrying a proven
   producer id. Keep ordinary text/usage reconstruction streaming if that
   remains simpler; do not turn this into a general two-pass transcript parser.

3. **Add exact command identity to the existing decoder seam.** Retain the
   canonical `exec_command` argument data already parsed by
   `decode_single_exec_wrapper` so a wrapper and `CommandExecution` can produce
   the same narrow signature. Normalize only producer-defined shell wrapping
   needed to compare the literal `cmd` value with the structured argv; include
   working-directory identity only if both records provide it in a directly
   comparable form. Matching must be exact and unique within the producer turn.
   The rationale against fuzzy matching must remain in code comments.

4. **Apply one association precedence everywhere.** Route both ordinary and
   late structured items through one resolver:

   - constrain by producer turn id;
   - prefer a unique exact semantic match;
   - otherwise use an uncontested open-wrapper interval;
   - otherwise, for the explicitly admitted command/MCP variants only, use the
     captured post-output bookkeeping sequence when no newer wrapper
     intervened; and
   - otherwise leave the completion unowned.

   This order may be adjusted locally only if code inspection proves an
   equivalent ordering is necessary to preserve existing batched-wrapper slot
   behavior. There must still be one shared association decision, not separate
   ordinary/late mutation paths.

5. **Reuse existing row mutation and collapse rules.** After association, use
   the current `WrapperSlot` behavior and structured handlers so command status,
   output, MCP decoding, FileChange facets, single-command in-place enrichment,
   batched child rows, successful-wrapper supersession, and failed-wrapper
   retention remain single-sourced.

6. **Keep unresolved enrichment non-destructive.** When association is not
   unique, do not mutate a row, do not create a standalone operation, and do
   not enter persistent turn state. The canonical wrapper remains. Record an
   aggregate parser diagnostic where existing tests need drift visibility, but
   absence of a completion alone remains silent.

The associated code comments must explicitly preserve the file-local
producer-turn relation, its separation from the hydration key, exact-signature,
no-fuzzy-match, canonical-wrapper, and local-not-turn-wide rationales. These
decisions must not live only in the implementation plan.

### 3. Definition of Done

- Unit tests cover the two captured late command sequences and the captured
  late MCP sequence, asserting correct rows, structured status/output, and no
  false warning.
- Table-driven signature tests prove all currently observed late canonical
  command wrappers produce an exact command signature, including success and
  failure. They also cover different commands, the same command with different
  working directories, a working directory missing on one side, malformed or
  dynamic scripts, and multiple wrappers with the same signature. Every case
  without one unique directly comparable identity fails closed.
- A cross-wrapper test places completion A before and after B's output. With
  distinct exact command signatures, A enriches A and B's own completion
  enriches B in both orders.
- A repeated-command test makes A and B semantically identical and proves the
  resolver declines to guess when interval/order evidence cannot distinguish
  them. Both canonical rows survive.
- A post-`task_complete` test uses the event's producer turn id plus a unique
  command signature to enrich the already completed original turn without
  moving the row chronologically.
- A cross-turn test completes turn A, starts turn B and opens B's wrapper, then
  delivers A's delayed completion before B's own completion. A must enrich only
  A, B must hydrate normally, and neither turn may move or borrow the other's
  status/output.
- A checked-in sanitized fixture/test pins the observed relation among
  `item_completed.turn_id`, `task_started.turn_id`, and
  `turn_context.turn_id`. Synthetic negative fixtures prove a missing event id,
  an id mismatch, a task/context mismatch, and a duplicated task id cannot
  associate into a completed turn.
- A duplicate-record test presents the same structured completion id twice and
  proves it cannot consume two wrapper slots or create a duplicate row.
- A childless wrapper followed by a normal multi-tool sequence proves there is
  no turn-wide cascade: later commands and file changes hydrate normally and do
  not generate one warning per item.
- A late `FileChange` test proves the unobserved variant is not positionally
  attached after wrapper output without supported identity evidence; its
  canonical wrapper survives unchanged.
- Boundary tests preserve single-use slots, mixed-child warnings, malformed
  item behavior, failed wrapper retention, and batched-wrapper collapse.
- The sanitized fixtures contain no real user content, paths, ids, commands,
  outputs, or credentials.
- The previously completed 2026-08-22 plan remains unchanged.
- Focused harness tests and `make test` pass before Milestone 2 begins.

## Milestone 2 — Put recoverable diagnostics at the tool, not the agent

### 1. Goal & Outcome

Remove aggregate transcript warnings from agent cards, preserve actual
zero-history failures as hydration errors, and display only actionable, owned
parse degradation on the affected tool row.

Once complete:

- Agent cards never show `N transcript warnings`, regardless of harness or
  warning count.
- Existing hydration errors, listener errors, failed turns, and cancelled turns
  retain their current UI and behavior.
- A stale Codex locator or ambiguous multi-session Gemini file produces a
  visible per-agent history/hydration error rather than an empty unexplained
  transcript.
- A tool with known degraded parsed data can show a warning state and details
  inside that tool row.
- Successful/failed tools without parse degradation look exactly as they do
  today.
- Aggregate unowned parser diagnostics do not enter user-facing runtime state.

### 2. Implementation Outline

1. **Reclassify the known zero-recovery cases.** Route the stale Codex locator
   and ambiguous multi-session Gemini file through the existing per-agent
   transcript load-error/hydration-error path instead of returning a successful
   empty transcript with a `ParseWarning`, extending the typed load-error
   boundary and its documentation as needed. Do not infer that every empty
   transcript is an error: missing/unstarted sessions and legitimately empty
   histories retain their current behavior. Preserve secret/path-safe error
   conventions and keep failure isolated to the affected agent.

2. **Add the tool-local warning contract atomically.** Extend the Rust tool
   variant of `TurnItem` with an empty-by-default collection of existing
   `ParseWarning` values, mirror it immediately on `LoadedTurnItem` and runtime
   `ToolCall`, and map it through the hydrate reducer in the same milestone.
   Populate it only after the parser identifies the owning row and that row's
   structured data is materially degraded. An unowned result never annotates a
   nearby tool. Live tool events produce an empty/absent warning collection;
   this is persisted-transcript degradation, not a new live event type.

3. **Remove aggregate warning state from the frontend path.** Stop copying
   `LoadedTranscript.warnings` into `AgentRuntime`, remove the runtime field and
   hydrate-input plumbing that exists only for the sidebar indicator, and delete
   the sidebar warning count/tooltip. Backend `LoadedTranscript.warnings` and
   app response metadata may remain because parsers, tests, and non-frontend
   consumers use them; do not expand this milestone into a backend diagnostics
   redesign.

4. **Render warning semantics inside `ToolCallWidget`.** Keep the existing
   execution-status precedence:

   - running remains running;
   - failed and cancelled remain failed/cancelled;
   - a terminal non-failed tool with tool-local parse warnings uses the semantic
     warning icon/token instead of the ordinary completion check; and
   - otherwise it remains completed.

   Put concise warning text and source line information inside the expandable
   row, using existing tooltip/content primitives and semantic tokens. Do not
   show aggregate warning counts, raw parser dumps, or a warning badge on every
   tool in the turn.

5. **Keep warning ownership stable during hydration.** Tool-local diagnostics
   must deduplicate with the same loaded tool row and must not overwrite newer
   live execution status/output when hydrate loses the existing live-wins race.
   Follow the current reducer and transcript-revision conventions rather than
   introducing warning-specific state outside the tool.

No browser test is required unless implementation changes measurable layout or
overflow behavior. The warning glyph/details behavior is otherwise covered by
the normal jsdom component suite.

### 3. Definition of Done

- Sidebar component tests prove agent cards render no parse-warning indicator
  for one warning, hundreds of warnings, or warnings from multiple harnesses.
  Existing hydration/listener error tests remain green.
- Loader/app/frontend tests prove the stale Codex locator and ambiguous
  multi-session Gemini file reach the affected agent's existing hydration-error
  presentation, while a legitimately empty or not-yet-started transcript does
  not become an error.
- Reducer/state tests prove aggregate `LoadedTranscript.warnings` do not become
  agent runtime state and tool-local warnings hydrate onto only their tool.
- `ToolCallWidget` component tests cover completed-with-warning, failed with a
  parse warning (failure remains primary), cancelled/running precedence,
  expandable warning details, and the no-warning control.
- A hydrate/live ordering test proves tool-local warnings cannot erase newer
  live status or output.
- Type and serialization tests pin the additive tool-warning wire shape and
  empty/default compatibility across Rust and TypeScript in the same completed
  milestone.
- No dismiss/clear state, warning counter, or parallel diagnostic store is
  introduced.
- `make test` and `make check` pass.

## Documentation and acceptance

Implementation is complete only after all of the following:

- Update `docs/harness-behavior.md` §3.6 and G30 with the asynchronous lifecycle,
  corpus evidence, exact-signature/bounded association rules, retained
  ambiguity ceiling, and removal of the failed turn-wide blackout claim.
- Record in `docs/system-design.md` only the durable product rule: agent cards
  report operational/hydration state (including history that could not be
  recovered), while recoverable owned transcript diagnostics belong to
  transcript items. Do not copy parser mechanics there.
- Do not edit the frozen 2026-08-22 plan.
- `make test-live-codex` passes against the installed Codex CLI. Each live
  transcript hydration test must independently assert that its captured rollout
  is paginated. The captured rollout must also assert the accepted producer-id
  relation for every `item_completed` record it contains. Report whether the
  run happened to exercise a late completion; sanitized tests remain the
  deterministic ordering proof.
- Manual acceptance uses the previously affected `coder` agent: sync its real
  transcript and confirm the agent card has no warning count, ordinary later
  tools retain structured status/output, and no turn-wide warning cascade
  remains.
- Manual acceptance also loads a sanitized tool-local degradation fixture (or
  an equivalent deterministic test surface) and confirms the warning is inside
  the affected tool row, not the agent card.

## Known limitation retained deliberately

Codex does not persist one operation id shared by Code Mode wrappers and
structured completions. Switchboard can recover ordinary intervals, the
observed bounded late sequences, and unique canonical command signatures, but
cannot prove ownership when multiple overlapping operations have the same or no
semantic signature. In that case it preserves canonical rows and omits
uncertain enrichment. This is a fidelity ceiling, not agent failure, and it is
not represented as an agent-card warning.
