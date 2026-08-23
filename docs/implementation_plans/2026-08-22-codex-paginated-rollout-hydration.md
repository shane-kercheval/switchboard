# Codex paginated-rollout hydration (G30)

**Status: complete (2026-08-22).** Branch: `fix/codex-paginated-rollout-hydration`. One PR; milestones landed as commits, each reviewed pre-commit. One deliberate deferral: naming the *argument* (not just the source) in prompt-forward invalidation copy — adjudicated non-blocking, revisit only if prompts grow many args with disjoint sources.

## Problem

Codex switched newly created threads to a new rollout format — `session_meta.history_mode: "paginated"` (TUI threads at CLI 0.147.0, `codex exec` threads at 0.148.0; we run 0.149.0). In that mode Codex's persistence policy stops writing four `event_msg` record types our disk parser reads, moving the same content onto a new `event_msg/item_completed` record our parser skips.

**The severe symptom is not the one that looks obvious.** A reopened transcript rendering empty is bad; silently dropping an agent's output from a *live* forward is worse, and it is already happening:

> Forwarding from an idle Codex agent reads its transcript **from disk** (`commands.rs:3456` → `latest_completed_agent_text`). On a paginated rollout that returns `""`, and the UI reports **"<agent> had no output"** — an affirmative false statement. Reproduced against a real session: 3 turns, **13 agent messages, 17,736 characters** present on disk, `warnings: 0`, forward text `Some("")`. A multi-agent review lost one reviewer's entire contribution this way, and the receiving agent consumed the truncated message without any signal.

No restart is required. Any forward from a finished Codex agent hits it. (A forward from a *still-running* agent uses the dispatcher's live capture and is unaffected — which is why it is intermittent rather than total.)

| Dropped record | What we used it for |
|---|---|
| `event_msg/user_message` | the hydrated `Turn::User` (prompt text) |
| `event_msg/agent_message` | the agent turn's `TurnItem::Text` — **and every forward's payload** |
| `event_msg/patch_apply_end` | structured edit content, for both reopen hydration and live `ToolFacetUpdated` enrichment |
| `event_msg/mcp_tool_call_end` | structured MCP results (`content`, `is_error`) |

**Unaffected** (verified in real 0.149.0 rollouts): `session_meta`, `task_started` (incl. `model_context_window`), `task_complete`, `token_count`, `turn_context` (`turn_id` hydration key, model, effort, cwd), `world_state`, and **every `response_item` record** — so turn boundaries, usage, model/effort, and tool *rows* still hydrate.

Full record: `docs/harness-behavior.md` **G30** + §6 "Codex 0.148.0". Review reasoning: the 2026-08-22 entry in `docs/harness-update-review.md`.

### Success criterion

**A user must not be able to tell which rollout format a Codex thread is on.** Same transcript, same ordering, loaded whole, no paged/lazy rendering. "Paginated" is Codex's name for how *its own TUI* browses history; the file we read is unchanged in completeness (verified: a 6.3 MB paginated session, 581 lines, all three turns and all user messages intact end to end). The one intentional visible difference is an improvement — see M3's batched-wrapper outcome.

## Required reading (before implementing)

Upstream at the reviewed tag — trust these over this plan's paraphrase if they disagree:

- Persistence policy: <https://github.com/openai/codex/blob/rust-v0.149.0/codex-rs/rollout/src/policy.rs> (`should_persist_event_msg`)
- `TurnItem` variant shapes: <https://github.com/openai/codex/blob/rust-v0.149.0/codex-rs/protocol/src/items.rs>
- Where `exec` opts in, and its legacy fallback: <https://github.com/openai/codex/blob/rust-v0.149.0/codex-rs/exec/src/lib.rs> (`thread_start_params_from_config`, `start_thread`)

In-repo: `docs/harness-behavior.md` §3.1/§3.6/G30/§6; `crates/harness/src/codex/session_file.rs` (both `CodexReconstruction` and the `Enrichment` path — especially `handle_patch_apply_end` and `decode_single_exec_wrapper`); `crates/harness/src/codex/mod.rs::emit_facet_upgrades`; `crates/harness/src/forward.rs`; `crates/app/src/commands.rs` forward resolution (~3240–3470).

## Verified ground truth (probes 2026-08-22, codex-cli 0.149.0)

Established empirically; the load-bearing ones must survive into code comments.

1. **The durable predicate is `session_meta.history_mode == "paginated"`** — never a CLI-version check. Old files lack the field entirely. Both upstream callers only *request* pagination and retry with `history_mode: None` if the store rejects it, so a paginated-capable CLI can still write legacy files. Resuming a legacy thread keeps writing legacy records.
2. **`item_completed` coexists with `response_item` for the same content, and IDs only partially agree.** `AgentMessage`/`Reasoning` items share their `response_item` id exactly; `CommandExecution`/`FileChange` items carry a synthetic `exec-<uuid>` matching **neither** the `custom_tool_call` id (`ctc_…`) nor its `call_id`. Tool ID-joining is impossible.
3. **A failed tool call can have no `item_completed` record at all.** Observed: a failed `apply_patch` produced `custom_tool_call` + `custom_tool_call_output` (carrying the failure text) and no `FileChange` item. `response_item` is the complete record of tool activity; `item_completed` is not.
4. **One `exec` wrapper can contain many operations.** Its `input` is JavaScript that may call several tools; observed one `custom_tool_call{name:"exec"}` whose body called `apply_patch` then `exec_command`, emitting **both** a `FileChange` and a `CommandExecution` between that call and its single output. Real sessions show this at scale (one had 80 `CommandExecution` items against 78 `custom_tool_call` records). This shape is what the currently-failing `live_codex_apply_patch_emits_edit_facet` exercises.
5. **File-order adjacency holds:** a tool's `item_completed` records land between that tool's `custom_tool_call` and its `custom_tool_call_output`.
6. **The richer-data gain is narrower than first reported — shell only.**
   - `FileChange.changes` is **at parity** with legacy `patch_apply_end.changes`: both are `{ "<abs path>": { type, unified_diff, move_path } }` (verified against `exec-wrapper.session.jsonl`). Differs only in `success` vs `status`. Neither `unified_diff` nor `move_path` is new.
   - `CommandExecution` is **genuinely new**: unwrapped `command` array, `parsed_cmd`, separate `stdout`/`stderr`, and a first-class integer `exit_code`. Legacy persisted no structured shell record at all (`ExecCommandEnd` is transient in both modes), which is why today's disk path sniffs `Script failed` / `Process exited with code` strings. Seeing *inside* a batched wrapper for shell work is likewise new.
   - `McpToolCall` (**captured 2026-08-22, M1**): `{ id, server, tool, arguments, readOnlyHint, status, result, duration }`. Success is `status: "completed"`; failure is `status: "failed"` **with `result.isError: true`** — the same `isError` key the legacy `mcp_tool_call_end` handler already reads. **The capture disproved the assumed id join:** the item id is a synthetic `exec-<uuid>`, and MCP calls ride the **same `exec` wrapper** as shell and edit work (two MCP calls observed under one wrapper). MCP is therefore not a special case — it is the same wrapper-children shape as `CommandExecution`/`FileChange`.
7. **Text-block case is inconsistent:** `UserMessage.content` blocks use `{"type":"text"}`, `AgentMessage.content` blocks use `{"type":"Text"}`. Read each block's `text` field; never gate on the exact `type` string.
8. `Reasoning` items carry `summary_text: []` and encrypted content — nothing renderable, consistent with §3.2. Skipped.
10. **A caught tool failure is unrecoverable — it leaves no trace anywhere.** When the wrapper's script wraps a failing call in `try/catch` and continues, the failed operation emits **no `item_completed`**, *and* the wrapper output reads `"Script completed"` with no diagnostic. Probe-verified: a caught `apply_patch` failure followed by a successful `exec_command` produced one `CommandExecution` child and a clean wrapper output. This is a hard ceiling on fidelity, not a parser gap — record it rather than chasing it later. The failure *is* preserved when it is **uncaught**: the script aborts and the wrapper output carries `"Script failed"` plus the diagnostic (this is what `paginated-failed-tool` and `paginated-mixed-batch` pin).
9. **Two previously-unprobed item types now captured** and worth recording even though this work does not consume them: `item_completed/ContextCompaction` (Codex's compaction shape — noted as unprobed in §3.1's bookkeeping audit and load-bearing for **G25**) and `item_completed/Extension`.

## Design (decided in review; do not re-litigate)

**Branch on mode; per-content-type canonical sources.** `CodexReconstruction` reads `session_meta.history_mode` into parser state:

| Content | Legacy (unchanged) | Paginated |
|---|---|---|
| User prompt text | `event_msg/user_message` | `item_completed/UserMessage` |
| Agent answer text | `event_msg/agent_message` | `item_completed/AgentMessage` |
| Reasoning | skipped | skipped (fact 8) |
| Tool **rows** | `response_item` | **`response_item` stays canonical** |
| Tool **enrichment** | `patch_apply_end`, `mcp_tool_call_end` | `item_completed/{CommandExecution,FileChange,McpToolCall}` |
| Boundaries / usage / model / key | unchanged | unchanged |

Rationale to carry into comments:

- **Why `response_item` stays canonical for tool rows:** fact 3 — replacing it would silently drop failed tool rows, losing error evidence.
- **Why not ID-join the enrichment:** fact 2 — tool IDs never match.
- **How enrichment attaches — new logic composed from two existing primitives, not prior art.** State it that way in the plan and the code so nobody hunts for a pattern that doesn't exist:
  - `handle_patch_apply_end` already solves the *unmatched child* case: search for a `tool_use_id` match, else **push a new `TurnItem::Tool` row** keyed by the record's own synthetic id. `exec_wrapper_fixture_hydrates_output_failure_and_structured_edit` pins the resulting wrapper-row-plus-child-row shape as today's accepted presentation.
  - `decode_single_exec_wrapper` already answers "is this wrapper exactly one call?"
  - The **new glue** is dispatching on that answer, bounded to the wrapper's `call → output` interval (fact 5): a wrapper the decoder proves is exactly one command is **enriched in place** (so a single shell command never renders twice); anything else has its `item_completed` children **pushed as their own rows** with the wrapper retained as container and failure evidence; a wrapper with zero completions is left untouched. Reusing match-else-create *alone* would be wrong — it would create a duplicate row for every ordinary single-command wrapper.
  - **Precision worth a comment:** `decode_single_exec_wrapper` recognizes only a single `tools.exec_command(...)`. A lone `apply_patch` wrapper therefore takes the children branch by construction. That matches legacy precedent (today's `patch_apply_end` also lands in its own row) but is not obvious from the function name.
  - **MCP is not a special case.** The gate this plan required has run: MCP items carry `exec-<uuid>` ids and arrive as children of an `exec` wrapper exactly like `CommandExecution`/`FileChange`, so they take the same attachment path. The earlier "joins by `call_id`" hypothesis, inferred from upstream conversion code, is **wrong** — do not implement it. Map `status: "failed"` / `result.isError` onto `is_error` through the existing MCP result handling.
- **Structured status outranks string-sniffing**, extending the precedent already in the parser (`function_call_output` `is_error` gating). On paginated files `exit_code`/`status` set `is_error`; the legacy string path is untouched.

**Both file readers get fixed.** `CodexReconstruction` (reopen) and the `Enrichment` filled by `parse_session_content` (post-terminal re-read powering live `ToolFacetUpdated`) read the same bytes for different purposes. Fixing one leaves the other broken.

**Out of scope:** no re-architecture of the legacy path; no reading of `response_item/message` (text stays single-sourced per mode); no `Reasoning`/`Extension`/`ContextCompaction` surfacing (fact 9 is recorded, not consumed); no paged or lazy rendering of any kind.

---

## M1 — Ground truth: MCP capture + fixtures

### Goal & Outcome

Close the one evidence gap and turn the captured rollouts into the fixture set every later milestone tests against.

- A real paginated rollout containing an MCP tool call exists; its `McpToolCall` shape — **including whether its `id` equals the `response_item` `call_id`** — is documented, settling the MCP join rule.
- Fixtures cover: paginated text-only turn; paginated single-command wrapper; paginated **batched** wrapper (fact 4); a **mixed** batch where one operation succeeds and a later uncaught one fails (fact 10 — the only shape that pins retaining the wrapper alongside its children); a failed wrapper with **zero** `item_completed` items (fact 3); a paginated MCP turn covering all three result envelopes; an **unknown** `history_mode` in both readable and degraded variants; and a legacy control.
- Captures are recorded before parser work begins, so an interruption loses nothing.

### Implementation Outline

Probe first: a small live `codex exec --json` turn in a scratch directory, prompting a call to one configured MCP server, then capture `~/.codex/sessions/<date>/rollout-*.jsonl`. Keep the response tiny per the live-test cost policy. Try to capture an MCP *error* variant cheaply (e.g. a nonexistent tool); if not obtainable, record it as a known gap rather than inventing a shape.

Sanitize the real captures (this one plus the 2026-08-22 probes) following the conventions of the existing `codex/` fixtures. Preserve verbatim: record order (facts 3–5 are ordering facts), `history_mode`, the ID shapes (`msg_…`, `ctc_…`, `call_…`, `exec-<uuid>`), and the text-block case split (fact 7). Document each fixture's purpose **in the test module that consumes it** — that is where such commentary lives in this crate; the fixture directory has no README convention.

### Definition of Done

- ✅ MCP capture obtained (success **and** error variants, one live turn). The `id == call_id` hypothesis is **disproved** and recorded in fact 6 and the Design section.
- ✅ Nine fixtures under `crates/harness/tests/fixtures/codex/`, every record timestamped (the parser falls back to `Utc::now()` on absence, which would make chronology assertions wall-clock dependent), documented in `session_file.rs`'s test module. No parser changes.
- ✅ The MCP transport-failure envelope (`result: null` + top-level `error`) is **source-derived** from upstream's `McpToolCallError` and labelled as such; a real transport failure could not be forced within the live-test cost discipline. M3 must reach parity with the live parser's `extract_mcp_output(result, error)`.

---

## M2 — Mode detection + paginated text hydration

### Goal & Outcome

Reopening a paginated Codex thread shows the conversation again, and an unrecognized future format can never fail silently the way this one did.

- Mode is detected per file; legacy files (including every pre-existing one, which lacks the field) parse exactly as today.
- On paginated files, `Turn::User` and agent answer text hydrate, each rendered exactly once.
- A **missing** mode selects legacy silently; an **unrecognized** mode selects legacy **and warns**.

### Implementation Outline

Model the mode as four states — `Missing`, `Legacy`, `Paginated`, `Unknown(value)` — read from `session_meta` at ingest. `Missing` → legacy, silent (old files predate the field). `Unknown` → legacy **plus a `ParseWarning`** naming the value; the enrichment-only reader, which has no warning channel, emits a tracing warning instead. This distinction is the tripwire for the next format change: collapsing missing and unknown together is exactly what would let a third format go quiet. Establish the detection as a shared helper here — M3 and M4 consume it rather than re-deriving it.

Add an `item_completed` arm to `handle_event_msg` dispatching on `item.type`. Gate the text handlers so a mode's records can only be ingested by that mode's path, making the single-source invariant structural rather than trusted. `UserMessage` follows the existing `user_message` contract (push to `self.turns`, anchored to the open builder's `started_at` — that anchoring rationale carries over); `AgentMessage` follows the `agent_message` contract (append a `TurnItem::Text` to the open builder). Extract text from each content block's `text` field, tolerant of the case split (fact 7 — comment it).

Rewrite the tripwire comment at the `response_item/message` skip arm: it currently claims the `event_msg` text records "flow alongside in every observed Codex session," which is no longer true. State the per-mode single-source rule and point at G30.

### Definition of Done

- Fixture tests: paginated text-only yields user + agent turns with correct text, exactly once; legacy control's output is unchanged (assert against expected output, not merely "no crash"); a file with no `history_mode` takes the legacy path **and is silent**; an explicit unknown mode parses via legacy **and cannot produce `warnings: []`**; text-block case tolerated both ways.
- All existing legacy fixtures and unit tests pass untouched. Never weaken or delete a test to get green.

---

## M3 — Paginated tool records

### Goal & Outcome

Tool rows on reopened paginated threads carry trustworthy status and full structured content.

- Shell rows derive `is_error` from the real `exit_code` — no string sniffing on these threads.
- Edit rows recover a content-bearing edit facet with per-file diffs.
- MCP rows recover structured results and `is_error`.
- A **batched** wrapper shows each operation as its own row, matching what the live view already displays for the same turn — the one intended visible improvement.
- A single-command wrapper shows **one** row, not two.
- A failed tool call with no `item_completed` record hydrates exactly as today.

### Implementation Outline

Extend M2's `item_completed` dispatch with `CommandExecution` / `FileChange` / `McpToolCall`, implementing the attachment contract from the Design section — the `decode_single_exec_wrapper` dispatch bounded to the wrapper's `call → output` interval, with `handle_patch_apply_end`'s match-else-push-new-row as the child mechanism. **All three item types — `CommandExecution`, `FileChange`, and `McpToolCall` — take that same path; none of them joins by id** (fact 2, confirmed for MCP by M1's capture). An item's id serves only to key the child row it produces. Follow the existing "authoritative result outranks the format-sensitive fallback" ordering rules where `patch_apply_end` / `mcp_tool_call_end` already interact with `function_call_output`-derived state. The pairing rationale must land in a comment citing facts 2–5, and must state plainly that the single-vs-batched dispatch is new logic composed from two existing primitives.

Convert `FileChange.changes[].unified_diff` into the same edit-pair representation `patch_apply_end_facet` produces, so both generations render identically. For `CommandExecution`, the load-bearing part is `exit_code` → `is_error`; whether to also upgrade the displayed command from the wrapper JS to the clean `command` string is a judgment call to make against the code — do it if it stays facet-level, skip it with a comment if it would ripple further.

### Definition of Done

- Fixture tests: exit-code-derived `is_error` (success and failure); edit facet with diff content; **batched wrapper produces a row per operation**; **single-command wrapper produces exactly one row** (the duplicate-row regression); the `item_completed`-less failed `apply_patch` unchanged; a child that matches no wrapper warns rather than mis-attaching; MCP success (and error, if M1 captured it) carrying structured results.
- Legacy fixture output unchanged.

---

## M4 — Live edit enrichment from `FileChange`

### Goal & Outcome

Edits made by a live Codex turn on a paginated thread show their diffs in the running app, not only after reopen.

- The post-terminal enrichment supplies `ToolFacetUpdated` content for live `file_change` rows on paginated threads.
- Legacy enrichment is unchanged.

### Implementation Outline

Small milestone. Make `parse_session_content`'s patch-facet extraction mode-aware via M2's shared helper: `patch_apply_end` on legacy, `item_completed/FileChange` (converted with M3's facet builder) on paginated, collected **in record order** into `enrichment.patch_facets`.

Do **not** try to recover `call_id`s here. `emit_facet_upgrades` (`codex/mod.rs`) already correlates live rows to disk facets ordinally with a path-set-equality guard and a unique-path-key fallback on count mismatch — it never consults `call_id`. Feeding it an ordered facet list is all that is required; reintroducing row association in the enrichment parse would add risk without helping the live path.

Note for the test below: a failed `apply_patch` emits neither a live `file_change` row nor a `FileChange` item (fact 3), so counts stay aligned. Comment this where the facets are collected.

### Definition of Done

- Fixture-driven adapter test (`fake_codex` pattern): a staged paginated session file plus a replayed live stream yields `ToolFacetUpdated` with diff content — the offline twin of `live_codex_apply_patch_emits_edit_facet`.
- Legacy enrichment tests untouched and green.

---

## M5 — Forward-path hardening: block on any empty source

**Sequencing is load-bearing: this must land after M2.** Until paginated text hydration is restored, every Codex source resolves empty — shipping this first would block every Codex forward *and* fail every workflow step that forwards from one.

### Goal & Outcome

No forward path can dispatch a message that omits a selected source.

- If **any** selected source resolves to zero forwardable text, nothing is dispatched: manual message forwards and manual prompt forwards invalidate with a compose-bar error; workflow steps fail the step.
- The user is told before a downstream agent consumes a truncated message — and in the workflow case, told at all.
- All forward paths share **one** empty-source policy, enforced in one place.

### Implementation Outline

**Correcting the premise this milestone was originally written on:** an earlier draft claimed the workflow path already enforced "fail on any empty," citing the doc comment on `forward_message_impl`. That comment is **stale and wrong**, and taking it at face value instead of tracing the code was the error. Verified against the current tree, **four** paths are permissive:

1. `forward_message_impl` — skips empties, fails only if *every* source is empty; records the rest for the partial-empty caption (`commands.rs` ~3330).
2. Manual prompt forwarding — skips partial empties, permits an empty optional argument and empty appended sources (`commands.rs` ~3768).
3. `resolve_workflow_forwards` — builds a `skipped` list, **never reads it**, and composes anyway (`commands.rs` ~3411).
4. The workflow runtime — `absorb` inserts `result.text` into `OutputScope` unconditionally on `Completed` (including `""`), and `build_body`'s `forward_from` fails only when a source is **absent** (`None`), composing an empty block for a present-but-empty one (`app/src/workflow.rs` ~408, ~545–570).

Path 4 is the worst of the four and the reason this milestone matters most: a workflow step forwarding from an affected Codex agent hits the exact silent drop this plan opens with, and unlike the manual path it produces **no caption at all**. Multi-agent workflows are where this class of bug does the most damage — it is what consumed a reviewer's entire contribution in the incident above. Fix the stale doc comment as part of this work so it stops misleading readers.

Introduce **one shared empty-resolution validator** and apply it before composition at all four sites. Manual paths return `ForwardOutcome::Invalidated { reason }`, reusing the existing `compose-send-error` surface — no new UI. Workflow validation/invocation returns the existing workflow error type so a step fails loudly rather than composing an empty block.

**Scope the guarantee honestly.** The check is "resolves to zero forwardable text," not "we detected data loss." `resolve_source_completed_only` collapses *no completed turn*, *completed turn with only tool output*, and *hydration loss* into the same empty string, and a **partially** parsed non-empty answer still passes. So the copy must be neutral — name the source and say it has no forwardable text and nothing was sent — never assert that output "could not be read," which claims a cause we cannot know. If distinguishing genuine read failure is wanted later, that needs typed provenance and parser warnings carried through the resolver, not an inference from emptiness.

**Frontend copy follows the policy.** `ForwardSourceChip`'s `readiness: empty` currently means "this source is skipped from the send" and is shared by the compose bar and the prompt/workflow composers; that consequence is now "this send is blocked." Update the chip plus `ComposeBar`, `PromptComposer`, `WorkflowComposer`, and `ForwardSourcePicker`.

**Remove the partial-empty caption last**, only once all four backend paths are strict — otherwise prompt forwarding loses its remaining partial-empty signal while still able to produce one. At that point no path can dispatch a partial forward, and the caption is live-session state that does not survive reload, so it is unreachable for historical messages too. Verify that reachability claim against the code before deleting `skipped`, `heldForwards`' plumbing, the `types.ts` field, and the `UnifiedTranscript` caption; if any live producer remains, keep it and fix its wording instead.

### Definition of Done

- Backend tests, one per path: manual message forward, manual prompt **argument** forward, manual prompt **appended-text** forward, and workflow **field** forward — each invalidates/fails when any one source is empty, dispatches nothing, and names the source. A workflow step forwarding from a completed-but-empty source must fail the step rather than compose an empty block.
- Rewrite (do not delete) tests encoding the old policy — `forward_all_empty_invalidates` becomes a special case of the new rule; any test asserting a partial-empty forward proceeds must be inverted.
- Frontend tests: the error surfaces and no dispatch occurs; remove partial-empty caption tests only if the removal is confirmed reachable-dead.
- **The regression test for the original bug**: hydrate the M1 paginated fixture and assert `latest_completed_agent_text` returns the agent's answer text. It belongs with M2's parser work as well — it is the parser fix, not the hardening, that closes the root cause.

---

## M6 — Codex rename display (`move_path`)

### Goal & Outcome

When an agent renames a file, the transcript shows the move instead of presenting it as an edit to the old path.

- A renamed file reads as `source → destination` in **both** the collapsed row and the expanded view.
- Ordinary edits are visually unchanged.
- Works for **both** rollout generations — legacy `patch_apply_end` already carries `move_path` (fact 6), so historical transcripts improve too.

### Implementation Outline

Independent of M2–M5; ordered last because it is an enhancement, not the repair. Scoped separately because it has **nothing to do with the version bump** — the data has always been available and is deliberately discarded today (`parse_apply_patch` consumes and drops `*** Move to:`, pinned by `move_to_line_is_consumed_and_path_stays_original`).

This is the plan's one cross-layer change: an additive **optional destination field** on `EditedFile`, its TypeScript mirror, and rendering. Prefer the optional field over a new `EditChange` variant — a `Renamed` value alone cannot carry both source and destination — and note that `ToolFacet::Mcp`'s `mutation` is the existing precedent for an additive optional facet field. Populate from `move_path` in both the legacy `patch_apply_end` facet builder and the M3 `FileChange` builder, and stop discarding `*** Move to:` in the patch-text parser; **update** that pinned test to assert the new behavior rather than deleting it. Resolve a relative `*** Move to:` value against the same cwd the source path is resolved against.

**Both render surfaces, explicitly** — updating one would leave the other presenting a rename as an ordinary edit:

- Collapsed: `toolDetail`'s edit arm currently joins `files.map(f => f.path)`; it must show the move.
- Expanded: `ToolCallWidget`'s edit section renders `file.path` alone. Its existing per-file annotation is gated on `files.length > 1 && file.change !== "modified"`, which **structurally excludes the single-file rename** this milestone targets — and single-file is the realistic case (one rename across 3,201 local file-change records). This needs a new independent conditional, not an extension of that one.
- The path row's copy action should target the **destination** — that is the file that exists after the operation.

**Codex-only by design** — record why, and mark it inferred rather than observed: Claude has no rename-capable edit tool, so it renames via `mv` in Bash, which already renders as a self-explanatory shell row. That is a reasonable reading of Claude's tool vocabulary but is not backed by a captured fixture, unlike the Codex `move_path` parity claim.

### Definition of Done

- Backend tests: destination populated from both generations; a null `move_path` leaves ordinary edits unchanged; the `*** Move to:` patch-text path retains the destination; a relative destination resolves against cwd.
- Frontend tests: a **single-file** rename renders its destination in both collapsed and expanded views; an ordinary edit is unchanged; extend the existing real-WebKit long-path wrapping test with a rename case.
- `docs/harness-behavior.md` §5's `*** Move to:` open capture updated — now represented, with the observed frequency noted (one occurrence across 3,201 local file-change records).

---

## M7 — Documentation + full verification

### Goal & Outcome

Docs match reality and the drift-detection suite is green.

### Definition of Done

- `make test`, `make lint`, then `make test-live-codex` → **13/13**, including the two currently failing. A still-failing live test is a finding to fix, not to annotate around.
- Extend `live_codex_transcript_load_via_captured_locator_round_trips` to assert the rollout it exercised was actually `history_mode: paginated`; otherwise a future upstream legacy-fallback would let it pass without exercising the new path.
- `docs/harness-behavior.md`: close **G30** with the shipped mechanism; update §3.6's Codex generation table (three generations, third supported, per-mode edit/exit-code sources); lift the §3.1 caveats this resolves and note that paginated threads now carry structured exit codes while legacy keeps the string-sniffing fallback; update §6's 0.148.0 note tail to "fixed"; record fact 9's two new item types (the `ContextCompaction` capture partially answers the compaction shape flagged unprobed in §3.1/G25 — record what was captured, do not extrapolate).
- `docs/harness-update-review.md`: update the Codex row's follow-up (live 13/13, G30 closed).
- Fix the stale doc comment on `forward_message_impl` claiming the workflow path fails on any empty source (M5) — it misled this plan's own first draft.
- Blow-by-blow reasoning goes in the PR description; git owns chronology.
- Known limitations recorded, not dropped: MCP error-variant capture status (M1); legacy threads keep string-sniffed exit status by design; `Reasoning` skipped; `Extension`/`ContextCompaction` captured but not consumed.
