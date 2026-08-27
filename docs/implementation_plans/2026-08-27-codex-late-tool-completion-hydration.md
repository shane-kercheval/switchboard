# Codex late tool-completion hydration

**Status:** proposed · **Created:** 2026-08-27

## Problem

Switchboard currently shows `N transcript warnings` on otherwise healthy Codex
agents whose rollout files use `session_meta.history_mode: "paginated"`. The
visible tool call and its wrapper output already hydrate, but the structured
`event_msg/item_completed/CommandExecution` record is sometimes written
**after** the matching `response_item/custom_tool_call_output`. The parser
closes its wrapper-association interval when that output arrives, so it treats
the later structured completion as an orphan, warns, and drops the richer
status, exit code, command, and output data.

This is a parser ordering defect, not transcript corruption and not a reason to
hide all transcript warnings. Other warnings report real loss or drift, such as
malformed JSON, unknown history modes, unmatched output records, and malformed
structured tool items. The fix must make the newly observed healthy ordering
silent while preserving those signals.

The initial 2026-08-27 local corpus check found 16 tool completions that the
current parser treats as post-output orphans across 4 of 982 Codex rollout
files, all written by Codex CLI 0.149. A review-time re-scan after one more
rollout was created classified the 16 precisely:

- 10 `CommandExecution` records followed wrapper output with one intervening
  `event_msg/token_count`;
- 4 `CommandExecution` records followed wrapper output with intervening
  `token_count`, `item_completed/Reasoning`, and its
  `response_item/reasoning` twin;
- 1 `McpToolCall` followed wrapper output with one intervening `token_count`;
- 1 `CommandExecution` arrived after the turn's `task_complete` and is not
  eligible for this fix because its builder was already finalized.

No in-turn late completion had another `custom_tool_call` between its wrapper
output and the completion. Two active Switchboard agents accounted for 11 and
2 of the command warnings, respectively. This evidence corrects two assumptions
in the first draft of this plan: the healthy in-turn shape is not physically
adjacent, and the observed late item set is not exclusively
`CommandExecution`.

No real transcript content, paths, commands, session identifiers, credentials,
or tool output from that corpus may be committed. Tests must use a minimal
sanitized fixture or inline JSONL that preserves only the record shape.

## Scope and chosen approach

This is intentionally one milestone. The defect and its correction are both
contained in Codex paginated session-file reconstruction; splitting parser
state, tests, and the accompanying contract correction into separate milestones
would create sequencing ceremony without an independently useful outcome.

In scope:

- Preserve existing pre-output behavior while association is unambiguous, and
  accept the observed
  post-output `CommandExecution` / `McpToolCall` shapes when one wrapper remains
  unambiguously eligible within the same turn.
- Attach post-output completions through the same single-command versus
  batched-wrapper rules already used for pre-output completions.
- Preserve structured status, exit-code, facet, and output precedence in either
  order.
- Keep warnings for genuinely unassociated or malformed items.
- Correct the project documentation that currently states tool completions
  always precede wrapper output.
- Fixture-driven, unit, full CI, and live Codex verification.

Out of scope:

- Hiding, filtering, regrouping, or restyling transcript warnings in the
  frontend.
- Changing the `LoadedTranscript`, `ParseWarning`, Tauri IPC, reducer, or
  sidebar contracts.
- Reconstructing associations by synthetic item id. Codex tool-item ids do not
  match wrapper ids or `call_id` values.
- Late `FileChange` support or a mixed/multi-item post-output run; neither shape
  was observed.
- General transcript reordering, look-ahead over an arbitrary number of
  records, or guessing across wrapper calls or turns.
- Changes to legacy Codex rollout parsing or to other harnesses.
- New warning severities, telemetry, or a generalized event-correlation
  framework.

The chosen design extends the existing wrapper association with one bounded,
single-use post-output candidate. It survives only the non-tool bookkeeping
records present in the captures (`token_count` and the two reasoning records),
and only while no later wrapper or turn boundary has made the association
ambiguous. It was chosen over suppressing the warning because the late record
contains authoritative execution data, and over a most-recent-wrapper or
unbounded-pending heuristic because Codex provides no stable operation
identifier with which to prove an ambiguous match.

## Required reading before implementing

The implementing agent must read these sources before changing code:

- `AGENTS.md`, especially the harness fixture/live-test policy and the rule that
  `docs/harness-behavior.md` is the operational source of truth.
- `docs/system-design.md` §3, for the split transcript source-of-truth.
- `docs/harness-behavior.md` §3.6 and G30, for Codex's three persisted tool
  generations, structured-result precedence, wrapper collapse rules, and known
  fidelity ceilings.
- `docs/implementation_plans/2026-08-22-codex-paginated-rollout-hydration.md`,
  especially facts 2–5 and the tool-attachment milestone. It is a frozen
  historical record: Fact 5 explains the evidence behind the shipped interval
  design, but the newer evidence in this plan and `docs/harness-behavior.md`
  supersedes that assumption. Do not edit the completed plan.
- `crates/harness/src/codex/session_file.rs`, including `CodexReconstruction`,
  the wrapper state, `claim_wrapper_slot`, all three structured tool-item
  handlers, response-output pairing, turn close, and their tests.
- `crates/harness/tests/transcript_load.rs`, especially
  `live_codex_transcript_load_hydrates_tool_items` and the paginated-mode guard
  in `live_codex_transcript_load_via_captured_locator_round_trips`.
- OpenAI Codex's rollout persistence policy, which establishes
  `ItemCompleted` as the durable paginated tool-item channel:
  https://github.com/openai/codex/blob/main/codex-rs/rollout/src/policy.rs
- OpenAI Codex app-server protocol documentation, whose item lifecycle calls a
  completed item the authoritative execution/result state:
  https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md
- OpenAI Codex's thread-history reconstruction, which explicitly documents
  that command completions can arrive out of order and after a newer turn has
  started; this is why a most-recent-wrapper heuristic is insufficient:
  https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/thread_history.rs

The upstream links describe the producer's current model, but the sanitized
0.149 rollout shape and Switchboard's fixture/live tests remain the compatibility
evidence for record ordering. Do not infer an ordering guarantee from upstream
source that the observed files contradict.

## Shared parser contract

The following decisions are load-bearing and must be expressed in production
code comments or type/method documentation, not only in this plan:

1. `response_item` remains the canonical source of tool rows because failed
   calls can emit no `item_completed` record. Structured items enrich or split
   those rows; they do not replace the canonical record stream.
2. Tool-item ids cannot join to wrapper ids. Association is based on a bounded
   wrapper-local record sequence.
3. Pre-output association is unchanged while the turn remains unambiguous. A
   wrapper may become a post-output candidate only when it attached **no**
   structured tool item before its matching output. The candidate is
   single-use: the first eligible late item consumes it. Mixed before/after
   children and multi-item late runs remain warned because neither shape was
   captured.
4. Only the two item kinds observed late are eligible after output:
   `CommandExecution` and `McpToolCall`. A late `FileChange` remains an orphan
   until a real capture establishes that producer behavior. All three kinds
   retain their existing pre-output support.
5. The post-output candidate may survive only the intervening records present
   in the healthy captures: `event_msg/token_count`,
   `event_msg/item_completed/Reasoning`, and `response_item/reasoning`. These
   records carry no competing wrapper identity. Any other intervening record
   expires the candidate.
6. Expiration without consumption makes tool-item association ambiguous for the
   remainder of that turn. In particular, when a later wrapper starts, the
   parser must not simply replace A with B and then attach a delayed completion
   from A to B—whether that completion lands before or after B's output. While
   ambiguous, later wrappers still hydrate canonically from their
   `response_item` call/output pairs, but structured tool items warn/drop rather
   than enriching them. The next turn resets the ambiguity state.
7. `task_complete`, a defensive turn close, a new `task_started`, and EOF clear
   all wrapper association state. An item after the original turn closed keeps
   the current orphan warning/drop behavior; this milestone does not mutate an
   already-finalized turn.
8. Once a candidate expires or the turn becomes ambiguous, a later tool item
   remains an orphan: warn and drop it rather than guessing. A plausible but
   wrong exit code, MCP result, or output on a different tool row is worse than
   an explicit warning and unenriched row.
9. For a safely associated late item, the existing single-command versus
   batched-wrapper behavior remains unchanged. A proved one-command wrapper is
   enriched in place; a dynamic wrapper receiving the one observed late MCP
   item creates its operation row and supersedes the successful container;
   failed or childless wrappers retain their existing protections.
10. Structured result evidence remains authoritative even when it arrives after
   the wrapper's string-derived fallback. A late structured success may replace
   wrapper boilerplate with real output (including a genuinely empty output),
   and a late nonzero exit or failed/declined status must override a wrapper
   that looked successful. For a failed item with no structured diagnostic, the
   wrapper's existing failure text remains available.
11. Absence of a structured child is not itself warning-worthy. A rejected or
   failed operation may legitimately have only the wrapper output.

## Milestone 1 — Order-tolerant paginated wrapper association

### 1. Goal & Outcome

Make Codex paginated transcripts hydrate identically for the two observed
wrapper/completion orderings without weakening real parse-warning detection.

Once complete:

- Reopening or syncing affected Codex agents no longer shows false
  `CommandExecution item outside any exec wrapper interval` warnings.
- A late structured command completion enriches the existing tool row rather
  than creating a duplicate or being dropped.
- Command success/failure, exit code, command facet, and output are the same
  whether the completion appears before wrapper output or after the observed
  bookkeeping records.
- The observed late MCP completion receives its own operation row without
  duplicating its successful wrapper.
- A delayed completion can never be silently applied to a later wrapper; when
  association is ambiguous, the existing warning remains.
- Truly orphaned or malformed records still produce developer-visible transcript
  warnings.
- The frontend warning indicator remains unchanged and continues to surface
  genuine transcript problems.

### 2. Implementation Outline

1. **Add the real regression shapes before changing association behavior.** Add
   minimal sanitized paginated transcripts for:
   - output → `token_count` → `CommandExecution`;
   - output → `token_count` → completed Reasoning item → reasoning response item
     → `CommandExecution`; and
   - output → `token_count` → `McpToolCall`.

   Prefer inline unit-test JSONL when a sequence is compact; use a fixture only
   when it materially improves readability or is reused. All values must be
   invented. The tests must fail against the current parser for the expected
   orphan warning, not for unrelated malformed setup.

2. **Extend wrapper state with a conservative post-output phase.** Preserve the
   current open-wrapper metadata and slot accounting. Record whether any
   structured child attached before output. On the matching output, create a
   post-output candidate only for a wrapper with zero attached children and
   only while the current turn's late-association state is unambiguous. The
   candidate is consumed after one eligible late item; do not create a queue or
   support mixed/multiple late children.

3. **Make the safe bookkeeping allowlist structural at record dispatch.** A
   post-output candidate survives `token_count` and the paired Reasoning records
   because those are the only intervening records in the healthy captures and
   cannot identify another tool wrapper. Every other record expires it before
   that record is handled. An eligible late `CommandExecution` or `McpToolCall`
   consumes the candidate; a late `FileChange` does not.

4. **Carry ambiguity forward for the rest of the turn.** If an unconsumed
   candidate is expired—especially because another wrapper starts—clear it and
   disable all structured tool-item association until the next turn. This is
   the guard against both `call A → output A → call B → completion A → output
   B` and `call A → output A → call B → output B → completion A`: B still
   hydrates from its canonical call/output pair, but A's delayed completion
   cannot be guessed onto it on either side of B's output. Turn close and the
   next `task_started` reset the state defensively.

5. **Route safe late items through the existing slot contract.** Do not create
   separate late-item mutation code. Once the association state has admitted a
   late item, use the same in-place command enrichment or own-row/supersession
   behavior as the existing pre-output path. This keeps status parsing, output
   handling, MCP result decoding, wrapper retention, and malformed-item warnings
   single-sourced.

6. **Preserve result precedence when output arrived first.** Confirm the shared
   attachment path overwrites format-sensitive wrapper-derived status/facet and
   successful structured output exactly as it does when the item arrives first.
   Preserve wrapper diagnostic text when a failed/unknown item supplies no
   structured output. The implementation must not make final state depend on
   the supported before/after order.

7. **Keep every unsupported or ambiguous shape visible.** A structured tool
   item outside an open wrapper or an eligible single-use candidate keeps the
   current warning/drop behavior. This includes post-turn items, late
   `FileChange`, mixed/multiple late children, and any item arriving after the
   turn became ambiguous. Do not scan backward, pick the most recent wrapper,
   or render an orphan as a new standalone operation.

8. **Pin the live tool test to paginated persistence.** In
   `live_codex_transcript_load_hydrates_tool_items`, locate the rollout captured
   by that test and assert `session_meta.history_mode == "paginated"` before
   checking warnings and hydrated tool rows. The neighboring text round-trip's
   assertion does not prove the mode of this separate dispatch. Reuse the local
   lookup pattern where appropriate rather than coupling the two tests.

9. **Correct only the living documentation.** Update
   `docs/harness-behavior.md` §3.6 and the G30 record with the two actual record
   orders, the safe bookkeeping allowlist, single-use/ambiguity boundaries, the
   unsupported post-turn outlier, and the dated 0.149 corpus counts. Update
   production comments that currently assert strict pre-output ordering. Do not
   edit the completed 2026-08-22 implementation plan; it remains the historical
   record of the assumption that led to the original interval design.

No frontend implementation is part of this milestone. If parser tests pass but
the sidebar still shows these warnings for the same sanitized record shape,
trace the warning propagation to confirm the diagnosis before proposing any UI
change; do not suppress the display as a workaround.

### 3. Definition of Done

The milestone is done when all of the following are true:

- Unit tests for a proved single-command wrapper cover output → `token_count` →
  completion and output → `token_count` → Reasoning item/response → completion.
  Each asserts exactly one tool row, no orphan warning, the structured shell
  facet/output, and `is_error: Some(false)`.
- A unit test proves late structured failure is authoritative: a nonzero exit or
  failed status arriving after wrapper text that looks successful leaves the
  single row failed.
- The equivalent existing call → completion → output tests remain unchanged in
  outcome, establishing order independence rather than replacing one supported
  order with another.
- A unit test for the captured output → `token_count` → `McpToolCall` shape
  asserts one MCP operation row with its structured result/status, no duplicate
  successful wrapper, and no false orphan warning.
- Adversarial multi-wrapper tests place completion A both before and after B's
  output. Both assert that it does not mutate B, does warn, and leaves B's
  canonical wrapper-derived state intact. A separate next-turn assertion proves
  the ambiguity state resets and normal structured attachment resumes.
- Boundary tests prove an unsupported intervening record expires the candidate
  and disables later post-output association for that turn; `task_complete`, a
  new turn, and EOF clear the state. Extend the existing post-turn orphan test
  for the captured command-after-`task_complete` shape.
- Tests prove each deliberately unsupported generalization remains warned and
  unattached: a late `FileChange`, a second consecutive late completion, and a
  wrapper with one child before output plus another after output.
- Existing tests continue to cover childless failed wrappers, malformed
  structured items, surprise extra children on a proved single-command wrapper,
  missing item ids, and wrapper failure retention. None of those warnings may be
  globally filtered to make the new tests pass.
- The sanitized regression data contains no real user content, filesystem
  paths, session/thread ids, credentials, tool arguments, or outputs.
- Production comments/documentation retain the non-obvious bounded-association,
  single-use, ambiguity, no-id-join, orphan-safety, and structured-precedence
  rationales listed above.
- `docs/harness-behavior.md` no longer claims strict pre-output ordering as
  universal, and records the supported/unsupported late shapes. The completed
  paginated-hydration plan is unchanged.
- `live_codex_transcript_load_hydrates_tool_items` explicitly asserts its own
  captured rollout is paginated before checking the hydrated tool result.
- `make test` passes.
- `make check` passes.
- Before merge, `make test-live-codex` passes against the installed Codex CLI.
  In particular, the existing live tool-hydration test must load a paginated
  rollout with no parser warnings and recover the shell tool's successful
  structured result. If Codex does not emit a post-output completion during that
  run, report that limitation; the sanitized regression remains the deterministic
  proof of this ordering.
- A manual app smoke check syncs one previously affected Codex agent and
  confirms its false warning count disappears while the tool row remains
  present with the expected status/output. This is verification of the existing
  UI path, not authorization to change it.

## Known limitation retained deliberately

Codex still provides no stable identifier joining a structured tool completion
to its wrapper, and upstream explicitly permits command completions after newer
work has begun. Switchboard therefore supports only one late completion for one
unambiguous wrapper within the same turn, across the three captured bookkeeping
record types. It deliberately leaves post-turn, post-ambiguity, late
`FileChange`, and multi-item late shapes warned and unenriched. A future capture
that makes one of those shapes routine requires a new association contract; it
must not be pre-solved here with a most-recent-wrapper or unbounded-lookback
heuristic.
