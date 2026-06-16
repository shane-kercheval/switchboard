# M6 + M7 — Workflow engine, dependency resolver, pause & iteration

**Status:** plan (not started)
**Supersedes outline in:** `docs/implementation_plans/2026-05-12-v1.md` §M6 / §M7 (this is their implementation-grade expansion).
**Authoritative DSL spec:** `docs/workflow-spec.md` — the workflow file format. This plan implements it; where this plan and the spec disagree, the spec wins and this plan is wrong and should be fixed.

## What this delivers

By the end of M7, a user can: forward one agent's (possibly still-streaming) output to another from the **manual** compose bar; author a YAML workflow under `<dir>/.switchboard/workflows/`; invoke it with a per-input form; watch it run in a progress surface; cancel it mid-flight; recover an interrupted run after a crash; have a workflow pause for their input mid-run; and run a workflow that iterates over a static list. Two built-in workflows ship (`review-and-aggregate`, `sequential-handoff`).

The milestones are split M6 (1–5: engine, no pause/iteration) and M7 (6–7: pause, iteration, interactive failure-pause), matching the roadmap's `M6 → M7` sequence.

---

## Required reading before implementing

The implementing agent **must** read these before writing code. They carry contracts this plan deliberately does not restate in full.

- `docs/workflow-spec.md` — the entire file. It is implementation-grade and is the source of truth for field names, scoping, validation, helper functions, status values, and the three worked examples. **Especially:** §"Output scope", §"Variable scoping", §"Sibling-failure policy" (newly added), §"Failure handling and workflow status", §"Retry from inside a `for_each` iteration".
- `docs/system-design.md` §4 (functional primitives), §5 (workflows), §7 (Agent contention / Failure handling / Cancelling / Walking away). §7's "Agent contention" states the **binding principle** that the manual compose bar and the workflow interpreter drive **one** dependency-resolution mechanism — load-bearing for M2 and M4 below.
- `crates/dispatcher/src/lib.rs` — read the module doc and the actor model in full. M1 modifies it. The doc explains why "one turn in flight per agent" is structural, why cancellation is out-of-band, and where terminals are synthesized — all three matter for the completion signal.
- `docs/implementation_plans/2026-05-12-v1-m4-dispatcher-contention-cancel.md` — the "Shared patterns established here" section. M6 **reuses** the `CancellationToken` (`CancelSource::Workflow` already exists) and the `ConversationJournal` inject-trait shape; it does not invent parallel mechanisms.
- MiniJinja docs: <https://docs.rs/minijinja/latest/minijinja/> — the workflow templating engine. The project already uses `minijinja` 2.20 in `crates/prompts`; the workflow crate reuses the same version and the same "subset only" posture (`docs/workflow-spec.md` §Templating lists exactly which features are in/out). Read how to register functions (`Environment::add_function`) and how undefined-variable behavior is configured.
- `tokio::sync::oneshot` docs: <https://docs.rs/tokio/latest/tokio/sync/oneshot/index.html> — the completion-signal primitive in M1.
- `crates/prompts/src/service.rs` and `local.rs` — `PromptId::parse`, `PromptService::render`, and how MiniJinja is configured for prompts. The workflow templating in M3 mirrors this setup; the interpreter in M4/M5 calls `PromptService::render` for `prompt:`-typed sends.
- M7 only — Tauri notification plugin: <https://v2.tauri.app/plugin/notification/>. No notification plugin is wired today; M6 milestone 5 adds `tauri-plugin-notification` for completion/pause notifications (see that milestone).

**Dependency-management rule (from `AGENTS.md`):** add crates with `cargo add` (never hand-edit `Cargo.toml` version strings); the YAML crate is **`serde_norway`** (workspace dependency — *not* `serde_yaml`/`serde_yml`; see the project's recorded rationale), and MiniJinja is `minijinja`. Commit manifest + lockfile together.

---

## Architecture decisions (these are the planner's calls — do not re-litigate by reading code; the rationale must survive into crate/module docs)

These were decided in the design discussion that produced this plan. State each one's rationale in the relevant module/crate doc comment so it does not evaporate.

1. **New pure crate `crates/workflow` for the workflow *language*.** It holds the file model, parser, parse-time + invocation-time validation, the MiniJinja template environment + the four helper functions, variable-scope resolution, and the run-status + checkpoint *record types*. It depends only on `switchboard-core` (for `AgentId` etc.), `serde`, `serde_norway`, `minijinja`, `serde_json`, `thiserror` — **no `dispatcher`, `prompts`, `harness`, `app`, or `tauri`.** Rationale: this mirrors the workspace's existing discipline (core/harness/dispatcher are pure, Tauri-free, and heavily unit-tested); the language layer is a large self-contained body of logic with its own fixture-driven test surface, and `crates/app/src/commands.rs` is already ~12k lines. Keeping the language pure is what makes the three worked examples testable as fixtures with no app.

2. **The interpreter/runtime lives in `crates/app`, not in the workflow crate.** The interpreter is a *conductor*: it orchestrates the `Dispatcher`, `PromptService`, transcript loading, checkpoint file IO, and the event emitter — all app-owned. Rationale: it is wiring by nature; forcing it into the pure crate would require inventing an injected-trait zoo (TurnRunner, PromptResolver, OutputReader, CheckpointWriter…) with no payoff, because we can integration-test the interpreter through the **real `Dispatcher` + `MockHarnessAdapter`** exactly as `crates/dispatcher/tests/dispatcher_with_mock.rs` already does. That tests the real concurrency, not a mock of it. Do not build the trait zoo.

3. **Turn-completion observation is added to the dispatcher (M1), not reconstructed from the event stream.** The actor already owns the single authoritative terminal; the completion signal fires from there. Rejected alternative: an app-layer second `EventEmitter` that re-parses serialized `TurnStart`/`TurnEnd` JSON to resolve futures — fragile (string/JSON matching) and duplicates state the actor already holds. See M1 for the contract and the audit obligation (it must fire at *every* terminal-synthesis point). **The completion payload carries the turn's captured text output, not just its outcome** — see decision #7.

4. **One dependency resolver, two surfaces.** The "hold a send → await agent B's referenced turn → resolve B's output text → dispatch to A (or invalidate)" mechanism is built once (M2) and reused by the interpreter (M4). The manual compose bar (M2) is a first-class user of it, per the system-design §7 binding principle — not a workflow-only capability. The single per-surface difference is failure handling: **manual forward** invalidates recoverably (restores the user's text to the composer); **workflow `forward_from`** is a hard step failure. Same resolver, surface-specific terminal handling. This difference is the planner's decision; state it in the resolver's doc.

5. **Sibling-failure policy is phased** (resolved in this discussion; already propagated to `docs/workflow-spec.md` §"Sibling-failure policy" and `system-design.md` §7): M6 ships the **non-destructive floor** (never cancel surviving siblings; step `failed`; outputs retained; retry re-runs the whole step). M7 layers the **interactive failure-pause** once pause machinery exists. M6 must **not** implement the old "SIGTERM the survivors" behavior.

6. **Run checkpoints reuse the journal pattern, not a new store.** `runs/<run-id>.jsonl` under the project dir, written via an injected trait shaped like `ConversationJournal` (app owns the path, core/workflow owns the record format). Add a `RUNS_DIR` path constant alongside the existing `WORKFLOWS_DIR`.

7. **Forwarded output is captured from the live stream, never re-joined from disk by id.** A finished turn's completion payload carries the agent's text output, accumulated by the actor as it already drains content events. Rationale (load-bearing — state it in the dispatcher + resolver docs): the dispatcher's `turn_id` is **not joinable** to the harness session file's own turn ids (`crates/app/src/commands.rs` correlates the two by *order* precisely because a direct join is impossible), and the one stable per-turn key that does exist (`hydration_key`) is **absent for Antigravity**. So "hold a `turn_id`, load the transcript, find the matching turn" — the approach an earlier draft of this plan took — cannot work. Capturing text at completion sidesteps identity entirely for the just-awaited turn: no disk read, no key dependency, no race. Disk loading (`load_agent_transcript` + the existing order-correlation) is kept **only** for the "latest completed snapshot when nothing is in flight" case (M2's no-in-flight path). The per-run output-scope map (M4) therefore stores **resolved text**, not a turn-id needing later correlation; `hydration_key` may ride along as diagnostic/reference metadata but is never the load-bearing path.

8. **Workflow run checkpoints may persist resolved output text — a scoped, intentional exception to the system-design §3 "Switchboard stores no agent content" invariant.** Deterministic crash-recovery of a multi-step workflow requires re-feeding an earlier step's output after restart; the disk re-derivation alternative is exactly the fragile order-join decision #7 rejects. So `runs/<run-id>.jsonl` stores the bounded resolved outputs that entered the workflow's output scope (text-only, same filter as forwarding — no thinking, no tool output). The §3 invariant is reworded to permit this narrowly (see the §3 edit). Run checkpoints are runtime data under `.switchboard/projects/<project-id>/runs/`, **not** the transcript store §3 protects and **not** user-authored config; M5 owns their retention/abandon lifecycle (this exception raises the sensitivity of leftover run files).

---

# M6 — Workflow engine + manual dependency forwarding + workflow cancel

## Milestone 1 — Per-send turn-completion signal in the dispatcher

*Small, isolated milestone — compressed per the structure guidance. It is the substrate every later await depends on, so it goes first.*

### Goal & Outcome

A caller of `Dispatcher::send_message` can obtain a handle that resolves — when **that specific send's** turn reaches a terminal state — with both the turn's outcome **and its captured text output**.

- A caller (workflow runtime, manual-forward resolver) can `await` a send and learn whether the turn `Completed`, `Failed`, or was `Cancelled`, **and** receive the agent's text output for a completed turn (so forwarding never needs a disk lookup — decision #7).
- A send that never starts a turn (journal write fails, adapter dispatch fails) resolves the handle as a failure, not a hang.
- A turn whose stream ends with **no** terminal and **no** cancel (adapter-contract violation / upstream stream drift) still resolves the handle (as a failure) rather than stranding the awaiter forever.
- Existing callers (the compose-bar `send_message_impl`) are unaffected — the completion handle is opt-in.

### Implementation Outline

Add an **opt-in completion channel** to the dispatcher's send path. The `WorkItem` gains an optional `oneshot::Sender<CompletionResult>` (the receiver is handed back to the caller), where `CompletionResult` is a small struct `{ outcome: TurnOutcome, text: String }` (carrying the captured text-kind output for a completed turn — empty for failed/cancelled). `hydration_key`/`turn_id` may be included as reference fields but are **not** the load-bearing path (decision #7). The contract:

- The signal fires **exactly once**, carrying the same `TurnOutcome` the actor used for the terminal `TurnEnd` it emitted (or synthesized), plus the accumulated text.
- **Text accumulation:** for opt-in awaited sends only, the actor accumulates `ContentChunk` text where `kind == Text` (exclude `Thinking`; exclude tool output) as it already drains those events in `drain_turn`. Non-awaited sends accumulate nothing (no cost on the compose-bar path).
- It must fire at **every** terminal-synthesis point in `run_turn` / `drain_turn`. Audit these against the current code: (a) the journal `record_send` fail-closed path (→ a synthesized `Failed`), (b) the adapter `dispatch()` error path (→ `Failed { AdapterFailure }`), (c) the normal `TurnEnd` from the stream, (d) the synthesized `Cancelled` terminal after a fired cancel token, (e) the force-failed locator-persist path, **(f) the stream-ends-without-terminal-and-without-cancel path** (currently `crates/dispatcher/src/lib.rs:1284` only `warn!`s and advances the backlog — under an awaited handle this strands the caller forever; synthesize a `Failed { AdapterFailure }` terminal here, consistent with (b), and resolve the handle with it). Missing any one turns an awaited send into a hang — this audit is the milestone's main risk.
- A dropped `Sender` (caller stopped awaiting) must not panic the actor — `oneshot` send returning `Err` is ignored, consistent with how the actor already treats dropped reply channels.

Decide the surface shape against the code, but the **contract** is fixed: opt-in (the `Enqueue` compose path passes `None` and is byte-for-byte unchanged in behavior), one-shot, fires-on-every-terminal. Workflow sends will use `OnBusy::FailFast`; a `Busy` outcome means no turn started and therefore no completion handle is meaningful — model this so the caller cannot confuse "Busy, nothing queued" with "queued, awaiting completion" (FailFast already returns `SendOutcome::Busy` synchronously, so the handle is only created once acceptance is certain).

Why a per-send `oneshot` rather than a broadcast/watch keyed by `turn_id`: the caller holds the `MessageId`/handle at send time and there is exactly one awaiter per send; a `oneshot` is the minimal primitive and needs no keying, no cleanup, no missed-event window. Record this rationale in the dispatcher module doc next to the existing "wire contract" paragraph.

### Definition of Done

- Unit/integration tests in `crates/dispatcher/tests/` (extend `dispatcher_with_mock.rs`) covering: completion resolves `Completed` **with the captured text** on a normal turn; thinking-kind chunks are excluded from the captured text; resolves `Failed` when the adapter dispatch errors; resolves `Cancelled` when the turn is cancelled mid-flight; resolves a failure when `record_send` fails (use a journal stub that errors); **a stream that ends with no terminal and no cancel resolves the handle as `Failed` rather than hanging** (case (f) — a mock adapter that drops its stream without a `TurnEnd`); the `Enqueue` path with `None` is unchanged (existing tests still pass) and accumulates no text; dropping the receiver does not disturb the turn or the actor.
- Dispatcher module doc updated with the completion contract + the per-send-`oneshot` rationale + the capture-text-not-disk-join rationale (decision #7).
- Known limitation to record if present: a queued (not FailFast) send that is later removed via `remove_queued_message` must resolve its completion handle as a failure/cancellation rather than hang — cover it or explicitly note manual-forward (M2) only uses the immediate path. (Manual forward dispatches to A only after B completes, and to an idle A, so it should not hit the queue; confirm and document.)

---

## Milestone 2 — Cross-agent dependency resolver + manual compose-bar forwarding

*This ships standalone user value before any workflow YAML exists, and builds the resolver M4 reuses. It is the milestone the system-design §7 binding principle is about.*

### Goal & Outcome

From the compose bar, a user can forward agent B's output to agent A — and if B is mid-turn, the send waits for B's turn to finish, then forwards the **finished** output.

- The compose bar offers a way to reference another agent's latest output as forwarded content (the `@`-mention menu is the natural host — it already lists agents).
- If the referenced agent (B) has an in-flight turn, A's send is **held** (visible as a distinct "waiting for B…" state, not the existing "queued — agent busy" state) until B's turn reaches a terminal state, then dispatched with B's completed output composed in.
- If B has no in-flight turn, B's latest completed output is forwarded immediately (snapshot — the pre-M6 behavior, preserved as the no-wait case).
- The forwarded body uses the **canonical `=== START forwarded from B === / === END ===` composition** from `docs/workflow-spec.md` §`send` `forward_from`, so manual and workflow forwarding look identical to the receiving agent.
- If B's referenced turn **fails or is cancelled**, A's held send is **invalidated** (never dispatched with empty/stale input) and the user's composed text is **restored to the compose bar** so they can decide what to do.

### Implementation Outline

Three pieces, in order:

**(a) Two output-text sources — live-captured (primary) and disk-snapshot (fallback).** Decision #7 governs this. There are two distinct cases and they use different sources — do **not** try to unify them behind a disk-`turn_id` lookup (that join is impossible — see decision #7):

- **In-flight / just-awaited turn (the common workflow + manual-forward case):** the text comes from the **M1 completion payload** — the resolver awaits B's turn and reads `CompletionResult.text` directly. No disk read, no id correlation. This is the load-bearing path.
- **No-in-flight-turn snapshot (manual forward to an idle B):** the latest completed output is read from disk via `load_agent_transcript` (`AgentRecord → session file → LoadedTranscript`), taking the agent's most-recent completed `Turn::Agent` and concatenating its `TurnItem::Text { kind: Text, .. }` items (exclude `Thinking`; exclude tool output). This reuses the existing order-based correlation — it does **not** need a `turn_id` join because "latest completed turn" is positional, not id-keyed.

Give the text-concatenation logic (`Turn::Agent` items → text-only string) a clear shared home so M4's helpers apply the identical filter. Document that the in-flight path never touches disk.

**(b) The dependency resolver (backend, shared).** A function/service that takes: a target agent A, a composed message (text/prompt), and a set of forward-source references (agent + the turn to wait on). It: holds outside any queue; for each source with an in-flight turn, awaits its completion via the M1 signal; on all-sources-terminal-and-successful, resolves each source's output text via (a), composes the canonical body, and dispatches to A via `Dispatcher::send_message` with `OnBusy::FailFast`; on any source failing/cancelling, returns an invalidation result without dispatching. This is the single mechanism; expose it so both the manual command (this milestone) and the interpreter (M4) call it. Its doc must state the surface-specific failure rule (decision #4): the *caller* decides what invalidation means — manual restores text, workflow fails the step.

**(c) Tauri command + frontend.** A command for "forward from B to A" that drives the resolver. Frontend: extend the `@`-mention menu (`ComposeBar.svelte`) with a "forward output from {agent}" entry that inserts a forward reference; render the held send's "waiting for {agent}…" state (new, distinct from "queued — agent busy"); on invalidation, restore the composed text + any chips to the composer (reuse the existing removed-queued-message restore path as the model). The default is **wait-for-finished** when B is in-flight (system-design §7: the user wants the *finished* response ~99% of the time) — no "snapshot vs wait" prompt.

Sequencing note: (a) and (b) are backend and can land before (c). The held-send state and the invalidation-restore are the parts most likely to harbor bugs — they get their own frontend tests per the DoD.

### Definition of Done

- Backend tests (extend the dispatcher mock harness or app integration tests): wait-for-finished forwards B's completed text to A; no-in-flight-turn forwards the latest snapshot immediately; B failing invalidates A's send (no dispatch); B cancelled invalidates A's send; the canonical sentinel composition matches the spec's shape exactly.
- **Frontend tests are mandatory for this milestone** (the system-design and the M6 outline both call this out explicitly): a component test that mounts the compose bar, mocks `invoke`/`listen`, and exercises "wait for B, then forward to A" including the dependency-failure path (B fails → composed text restored, send not delivered). Cover the held "waiting for {agent}" state rendering and the distinction from "queued — busy".
- The output-text reader has unit tests over a fixture transcript (text-only turn, turn with interleaved tool calls, turn with thinking blocks excluded).
- Resolver doc states the one-mechanism-two-surfaces decision and the per-surface failure rule.
- `docs/system-design.md` §7 "Sequencing" note about "until M6 the compose bar resolves only a completed snapshot" is now satisfied — update it to reflect that in-flight forwarding has landed.

---

## Milestone 3 — `crates/workflow`: the workflow language (parser, validation, templating, helpers)

### Goal & Outcome

A pure crate that turns a workflow `.yaml` file into a validated, executable in-memory model, and renders its templates — with no runtime/dispatch concerns.

- A workflow file is parsed into a typed model; malformed files and spec violations are rejected at **parse time** with clear errors (the spec's "Parse-time" validation list).
- **Invocation-time** validation is supported: given parsed inputs + the project's agents + the prompt providers, missing required inputs, non-existent agents, unresolvable prompt ids, empty/duplicate `[agent]` lists, and unresolvable template variables are caught before any dispatch.
- Templates render through the MiniJinja subset the spec defines, with the four helper functions (`responses_from`, `aggregated_responses`, `last_output`, `agent_names`) and the documented scope-precedence order.
- The three worked examples in `docs/workflow-spec.md` parse and validate.
- Run-status (`complete`/`cancelled`/`failed`/`interrupted`) and checkpoint **record types** are defined here (the interpreter in M4 writes them; the format lives with the language).

### Implementation Outline

Create the crate per architecture decision #1 (`cargo new`-equivalent within the workspace; deps via `cargo add`: `serde`, `serde_norway`, `minijinja`, `serde_json`, `thiserror`, `switchboard-core`). The crate is the authoritative implementation of `docs/workflow-spec.md` — read it as the spec, not this section.

- **Model + parser.** Top-level `name`/`description`/`inputs`/`steps`; the input type grammar (`agent`, `[agent]`, `prompt_id`, `text`, `text?`, `[text]`, shorthand + long form); the step enum (`send`, `wait_for`, `wait_for_all`, `pause_for_user`, `for_each`). Use `serde_norway` for the YAML layer. Each step is a mapping with exactly one step-type key (the spec is explicit) — model the "exactly one key" and "unknown step type" rules.
- **Parse-time validation** — implement the spec's full "Parse-time" bullet list, including: reserved top-level/step keys (the forward-compat table) are errors; no nested `for_each`; `item:` not colliding with an input name or `user_input`; hardcoded `[agent]` literals checked for emptiness/duplicates (after hyphen→underscore normalization); all template strings parse as valid templates; `name` slug matches and equals the filename. Use `thiserror` typed errors at the crate boundary (workspace convention).
- **Templating.** Build a MiniJinja `Environment` configured to the spec's **subset**. Two distinct enforcement mechanisms — do not conflate them:
  - *Undefined variables* → render error (strict-undefined). `crates/prompts/local.rs` configures undefined behavior but uses **lenient**-undefined; workflows require **strict** per the spec — set strict.
  - *Unsupported tags* (`{% set %}`, `{% raw %}`, macros, inheritance `{% extends %}`/`{% block %}`, includes, the `do` extension, custom filters) → these are **valid MiniJinja syntax and will NOT self-reject** (strict-undefined does nothing for them). They require an **explicit validation pass** at parse time that rejects the specific tag/feature set. Prefer MiniJinja AST inspection if exposed; otherwise a conservative scan of the parsed template that does not false-positive on the tag text appearing inside string literals or comments. This is a *bounded* check (a fixed list of tags), not a general linter. (Earlier wording implied these "produce render errors" automatically — they do not; this is the correction.)
  - Register the four helper functions. The helpers read from an **injected output-scope map** keyed `agent → resolved completed-turn text` (decision #7 — resolved text, never a turn-id requiring later disk correlation). The crate defines the helper *logic* and the map's shape; the interpreter (M4) populates it. Keep the crate pure: the helper does no IO; it receives already-resolved text. Implement the scope-precedence chain (step-local `template_vars` → iteration var → `user_input` → workflow inputs) and the canonical `aggregated_responses` sentinel shape (identical to M2's forward composition).
- **Checkpoint + status types.** Define the `runs/<run-id>.jsonl` record enum and the run-status enum here as serde types. The on-disk schema is the planner's to specify at the *shape* level — it must capture, per the spec's retry rules: step index, and (for `for_each`) iteration index + iteration value + the per-run output-scope map + the current-scope `user_input`. **The output-scope map persists resolved text** (decision #7/#8 — the scoped §3 exception: text-only, same filter as forwarding, no thinking/tool output), so a crash-recovered run can re-feed an earlier step's output without the impossible disk join. Do not design iteration fields away — M7's retry-from-inside-iteration depends on them existing in the record from the start, even though M6 doesn't write iterations. (Defining them now avoids an M7 schema migration; this is the "establish the pattern early" instruction.)

### Definition of Done

- Heavy unit tests (this is the pure, high-value test surface): the three worked examples parse + validate; each parse-time validation rule has a positive and negative case; **each unsupported MiniJinja tag is rejected by the explicit validation pass, and a template with that tag's text inside a string literal/comment is NOT a false positive**; undefined variable is a render error; scope precedence resolves to the innermost binding; `aggregated_responses`/`responses_from` produce the canonical shape and the name→underscore mapping; helper errors when an agent has no completed output.
- Checkpoint record types round-trip through serde, **including the resolved-text output-scope map** and the iteration fields (unused in M6).
- Crate-level doc states decision #1's rationale (why pure, why separate from the interpreter) and points at `docs/workflow-spec.md` as the spec.
- Known limitation recorded: the crate validates template *parsing* at parse time but variable *resolution* only at invocation/render time (matches the spec) — state this so M4 knows render errors surface at step execution.

---

## Milestone 4 — Workflow interpreter / runtime

### Goal & Outcome

The app can execute a parsed workflow against a project's agents: dispatch steps, synchronize, resolve forwarded/aggregated output, persist checkpoints, handle failures non-destructively, and cancel.

- `send` (single + parallel list), `wait_for`, `wait_for_all` execute with correct parallelism (list dispatches issued in order, agents run concurrently) and synchronization (barrier waits on all).
- `forward_from` and the helper functions resolve against the **per-run output-scope map** (only turns this run dispatched and observed reaching terminal — the spec's "Output scope" rule), reusing the M2 resolver and the M2 output-text reader.
- A workflow run terminates in exactly one of `complete`/`cancelled`/`failed`, per the spec's status table; trailing in-flight turns hold the run open until they settle.
- **Sibling-failure non-destructive floor** (decision #5): a participating agent failing marks the step `failed` but never cancels surviving siblings; their output is retained.
- Step-boundary checkpoints are written to `runs/<run-id>.jsonl`; a process that dies mid-run leaves a checkpoint that M5's UI surfaces as `interrupted`.
- Workflow-level cancel stops orchestration and cancels in-flight turns via `CancelSource::Workflow`; cancelling a participating agent's turn marks the whole run `cancelled`.
- Runs are **project-scoped and concurrent across projects** (separate runtime instance per run; no cross-project coordination).

### Implementation Outline

The interpreter lives in `crates/app` (decision #2). It consumes the M3 model, drives the M1 completion signal and the M2 resolver, calls `PromptService::render` for `prompt:` sends, and writes checkpoints.

- **Execution model.** A step-based interpreter (the spec and system-design §4 are explicit that a general DAG scheduler is *not* required for v1 — do not build one). Walk the snapshotted steps; for a list `send`, issue all dispatches (FailFast) in declared order and collect their completion handles; `wait_for`/`wait_for_all` await the relevant handles. On a handle resolving `Completed`, store its `CompletionResult.text` (decision #7) into the per-run output-scope map keyed by agent — `agent → resolved text this run`, **not** a turn-id needing disk correlation. This map is what the M3 helpers read through. It is per-run and sees only this run's turns (the spec's out-of-band-invisibility rule); implement it so a manual send or another run targeting the same agent is not observed. The map is also what the checkpoint persists (decision #8).
- **Workflow file snapshot at invocation** (spec §"Workflow file snapshot"): the run executes against an immutable copy of the parsed workflow + bound inputs captured at invoke time. Edits to the file mid-run do not affect the run or its retries. Prompt resolution still happens at each step's dispatch (so prompt edits take effect next invocation). Capture the snapshot; do not re-read the file per step.
- **Contention = step failure** (system-design §7): workflow sends use `OnBusy::FailFast`; a `Busy` is a step failure with the spec's error ("agent X is busy…"), not a queue. This is the deliberate difference from the manual compose bar's queue.
- **Sibling-failure floor.** When a parallel/fan-in participant fails, mark the step `failed`, do **not** cancel siblings, let them settle, retain their output. M6 stops here — no pause (that is M7). Implement so the M7 interactive pause can wrap this without re-architecting: the floor's "collect all sibling outcomes, then fail" shape is what the pause will hook.
- **Status state machine + trailing settle.** The run does not go `complete` until every turn it dispatched is terminal, including turns still in flight after the last step (spec §"Failure handling"). A trailing failure → `failed`. Implement the trailing-settle hold.
- **Checkpointing.** Write `runs/<run-id>.jsonl` via an injected writer trait shaped like `ConversationJournal` (decision #6) — app owns the path (`<dir>/.switchboard/projects/<project-id>/runs/<run-id>.jsonl`), the M3 record types own the format. Add the `RUNS_DIR` path constant in `crates/core/src/paths.rs`. Checkpoint at step boundaries. Reuse the project's existing fsync/durability posture from the journal (`crates/core/src/io.rs`).
- **Cancel.** Workflow-level cancel fires the **same** per-turn `CancellationToken` with `source = Workflow` (decision: reuse M4.2, not a new path) on whichever agents have in-flight turns for the run, and stops the interpreter. A user cancelling a participating agent's turn (observed via the completion signal resolving `Cancelled`) marks the run `cancelled` uniformly (spec §"Failure handling").

### Definition of Done

- Integration tests through the **real `Dispatcher` + `MockHarnessAdapter`** (model: `dispatcher_with_mock.rs`): sequential handoff (worked example 1) runs end-to-end; fan-in review (example 2) dispatches reviewers in parallel, waits for all, aggregates, dispatches to primary; `forward_from` composes the canonical body; helper output-scope correctly ignores an out-of-band manual send to a participating agent; contention on a `send` step fails the step; **sibling failure marks the step failed without cancelling the surviving sibling** (assert the survivor's turn still completes and its output is retained); workflow-level cancel marks the run `cancelled` and fires `CancelSource::Workflow`; cancelling one participant's turn marks the whole run `cancelled`; the trailing-settle hold keeps a run open until a fire-and-forget final turn completes, and a trailing failure marks it `failed`.
- Checkpoint records are written at step boundaries and round-trip; a simulated mid-run drop leaves a checkpoint whose last entry identifies the step (M5 consumes this); **a run resumed from a checkpoint re-feeds an earlier step's output to a later `forward_from`/helper purely from the persisted resolved text — no harness-file read** (proves decision #8 makes recovery work where the disk join would fail).
- `RUNS_DIR` constant added; checkpoint path matches the spec.
- Interpreter module doc states: step-based-not-DAG rationale, the output-scope-map rule, the snapshot-at-invocation rule, and the reuse of M1/M2 + `CancelSource::Workflow`.
- **Capability gating (not parse rejection).** M3 parses the **full** v1 DSL — `pause_for_user` and `for_each` are syntactically valid and the spec's worked example 3 (which uses both) validates. They are simply **not executable until M7**. So the gate is a *runtime* capability check, not a parse error: M5's invocation-validation rejects a workflow containing either step type with a clear "step type not supported in this version" message (list it as syntactically valid; block invoke), and the M4 interpreter errors clearly as defense-in-depth if ever handed one. (This corrects an earlier internal contradiction in this plan that said these were "rejected at parse time" — they are not; that would break worked example 3.)

---

## Milestone 5 — Workflow Tauri commands, invocation + progress UI, built-in workflows

### Goal & Outcome

A user can discover, invoke, watch, cancel, and recover workflows from the desktop UI, and two built-in workflows ship.

- The UI lists the directory's workflows (parsed from `<dir>/.switchboard/workflows/`), shows invalid ones with their parse error.
- Invoking a workflow presents a form with one field per declared input (agent pickers for `agent`/`[agent]`, prompt pickers for `prompt_id`, text fields for `text`/`text?`/`[text]`), validates inputs (invocation-time rules), and launches the run.
- A **workflow-progress surface** shows each active run's name, current step / total, and per-step status; multiple concurrent runs are listed; cancelling from here stops the run.
- On app start, a run that was interrupted by a crash is surfaced as "interrupted at step N" with retry/abandon.
- `review-and-aggregate.yaml` and a sequential-handoff workflow ship as built-ins the user can copy.
- `docs/agent-instructions/workflows.md` exists so a user can point an agent at it to author a workflow.

### Implementation Outline

- **Commands** (`crates/app`): list/parse workflows for a project's directory (returns parsed metadata + per-file parse errors); validate invocation inputs (invocation-time rules from M3, against the project's agents and `PromptService`); invoke (spawns the M4 runtime for the project, returns a run id); cancel a run; query active/interrupted runs. Follow the existing `*_impl` free-function + thin `#[tauri::command]` shim convention (`commands.rs`), and the tagged-error pattern already used at the IPC boundary.
- **Capability gate (M6).** Invocation-validation rejects a workflow that contains a `pause_for_user` or `for_each` step with a clear "step type not supported in this version" error (per the M4 gating note — these parse as valid but aren't runnable until M7). The UI lists such a workflow as syntactically valid but blocks/disables invocation with that message. The two shipped built-ins use neither step type, so they invoke normally.
- **Progress events.** Decide the channel: a dedicated `workflow:<run-id>` (or `project`-scoped) emission for step transitions, separate from the per-agent `agent:<id>` stream (agent turns already flow there and render in the transcript — the progress surface needs *step* granularity, not token granularity). Keep the payload minimal (run id, name, step index/total, per-step status, run status). The agent *turns* a workflow produces already appear in the unified transcript via the existing event path — do not duplicate them.
- **Frontend.** Workflow list + invocation form (model it on `PromptComposer.svelte`'s structured-args + preview lifecycle — the M5-prompts precedent), and the progress surface (the shape — header row vs. side panel vs. modal — is left to the implementer per system-design §7 "shape TBD"; the *content* is fixed above). Reuse the agent recipient-picker for `agent`/`[agent]` inputs and the prompt menu for `prompt_id` inputs. Crash-recovery surfacing reads the M4 checkpoints on project open and presents interrupted runs.
- **Notifications.** Add `tauri-plugin-notification` (via `cargo add` + the JS plugin) and fire an OS-native notification on run completion and on failure/interruption. (Pause notifications come in M7. The plugin is wired here because completion notification is in M6's "watch a workflow run" scope per system-design §7.)
- **Run-file retention / abandon.** Because run checkpoints now contain resolved agent output text (the scoped §3 exception, decision #8), leftover `runs/<run-id>.jsonl` files are more sensitive than empty bookkeeping. Give the run files a deliberate lifecycle: a completed/cancelled run's checkpoint can be pruned or retained per a clear rule, and "abandon" on an interrupted run removes its checkpoint. Decide the exact retention policy against the code, but do not leave abandoned run files (with agent text) accumulating silently — this is the M5-owned tail of decision #8.
- **Built-in workflows.** Ship `review-and-aggregate.yaml` (worked example 2) and a sequential-handoff `.yaml` (worked example 1) as files the app can seed/copy into a directory's `workflows/`. Decide the seeding mechanism against the code (how prompts ship examples is the precedent).
- **`docs/agent-instructions/workflows.md`** — tutorial-style authoring doc written *for an AI coding agent to consume* (per system-design §2), pointing at `docs/workflow-spec.md`. This is a listed M6 prerequisite; a focused draft is in scope here.

### Definition of Done

- This milestone is the **M6 acceptance test** (v1 plan): invoke `review-and-aggregate` against three real-or-mock agents (one implementer, two reviewers) and confirm end-to-end execution, correct step transitions in the progress surface, mid-flight cancel marks the run `cancelled` and terminates the in-flight subprocess, and a force-kill + restart surfaces "interrupted at step N" with retry/abandon.
- Command-layer tests for list/validate/invoke/cancel including the error paths (invalid input, non-existent agent, unresolvable prompt id, busy agent → step failure).
- Frontend component tests for the invocation form (per-input rendering + validation) and the progress surface (step transitions, concurrent runs, cancel). Mock `invoke`/`listen` per the project's component-test convention.
- Both built-in workflow files parse + validate (assert in a test so a future spec change can't silently break the shipped examples).
- Capability gate: invoking a workflow containing `pause_for_user` or `for_each` is refused with the "not supported in this version" error (not a parse failure, not a hang); a `for_each`/`pause` workflow still appears in the list as syntactically valid.
- Run-file retention: an abandoned/completed run's checkpoint is pruned or retained per the chosen rule (assert the agent-text-bearing file does not linger past its defined lifecycle).
- `docs/agent-instructions/workflows.md` drafted; `README.md` "Harness support and limitations" / onboarding updated only if a user-visible limitation emerged.

---

# M7 — Pause for user input + iteration + interactive failure-pause

## Milestone 6 — `pause_for_user` (Modes 1 & 2) + interactive sibling-failure pause

### Goal & Outcome

A workflow can suspend mid-run for the user, and the M6 sibling-failure floor gains the interactive human-in-the-loop pause from the design discussion.

- **Mode 1 (no `recipient`):** the run suspends, an OS notification fires, the user submits (or skips), `user_input` is bound, the next step runs — no dispatch.
- **Mode 2 (with `recipient`):** the run suspends, the compose bar is pre-targeted at the recipient, the user's response is dispatched to it, and the step **implicitly waits** for that turn to terminate before continuing. A Mode-2 dispatch failure marks the run `failed` and retry re-enters the pause pre-filled with the prior `user_input`, requiring explicit re-submit.
- Both modes honor `required: true` (skip → `cancelled`) and `required: false` (skip → empty `user_input`, proceed).
- **Interactive failure-pause** (decision #5, M7 half): when a parallel/fan-in participant fails **and ≥1 sibling is still alive or has produced output**, the run enters a pause presenting the failed agent + error and each sibling's status/output, offering: retry the failed agent and continue / continue with survivors' output only / cancel the workflow. All-agents-failed (no survivor) falls back to ordinary step `failed`.

### Implementation Outline

- **Pause machinery.** Build the suspend/resume primitive: the runtime parks at a `pause_for_user` step, emits a "paused, awaiting input" progress state + OS notification (reuse the M5 notification plugin), and resumes when a new command delivers the user's input (or a skip). `user_input` binds into the scope per the spec (and is per-iteration-scoped inside `for_each`, which lands next milestone — build the scoping so M7's iteration reuses it). Mode 2 reuses the **M2 resolver / M4 dispatch path** for the dispatch + implicit wait — it is "capture, then a normal dependency-aware send to one agent." Implement the Mode-2 dispatch-failure → `failed` + retry-prefill rule (spec §`pause_for_user`).
- **Compose-bar pre-targeting.** The pause surfaces the compose bar pre-targeted at the configured recipient (Mode 2) or as a plain capture (Mode 1). Reuse existing compose-bar targeting; the new piece is the runtime binding the submitted text back to the suspended step.
- **Interactive failure-pause.** Wrap the M4 non-destructive floor: instead of "collect sibling outcomes → fail the step," when ≥1 sibling survives, route into the pause surface with the three options. "Continue with survivors" feeds the fan-in helpers only the succeeded agents (the per-run output-scope map already keys by agent — omit the failed one). "Retry failed and continue" re-dispatches just the failed agent and resumes. Reuse the pause machinery built above (same suspend/resume + notification); this is the payoff of deferring it from M6. The boundary cases (no survivor → ordinary `failed`; single-recipient send → ordinary `failed`; user-cancel during the step → `cancelled`) are in the spec's §"Sibling-failure policy" — implement exactly those.
- **Checkpoint interaction.** A pause is a checkpointable boundary; the captured `user_input` and the pause's pending state must survive into the checkpoint so an interrupted-while-paused run recovers coherently. (Full iteration-aware checkpoint fields land next milestone; the `user_input`-in-scope field already exists in the M3 record.)

### Definition of Done

- Integration tests (mock harness): Mode 1 binds `user_input` and runs the next step with no dispatch; Mode 2 dispatches to the recipient and blocks until that turn terminates; Mode 2 dispatch failure → `failed` and retry re-enters the pause pre-filled; `required: true` skip → `cancelled`; `required: false` skip → empty `user_input` + proceed.
- Interactive failure-pause tests: sibling failure with a live survivor enters the pause (not bare step-failure); "continue with survivors" aggregates only succeeded agents; "retry failed" re-dispatches only the failed agent and resumes; "cancel" → `cancelled`; all-failed → ordinary `failed`; single-recipient failure → ordinary `failed` (no pause).
- Frontend tests for the pause surface (Mode 1 capture, Mode 2 pre-targeted dispatch, the failure-pause option buttons) with mocked `invoke`/`listen`.
- Notification fires on pause entry.
- `docs/workflow-spec.md` §"Sibling-failure policy" M7 half is now implemented — verify the doc matches the build; no doc edit expected unless a detail shifted.

## Milestone 7 — `for_each` iteration

### Goal & Outcome

A workflow can iterate a sub-sequence of steps over a static, invocation-time list, with correct scoping, checkpointing, and crash recovery.

- `for_each` runs its body once per item in a `[text]`/`[agent]` list, binding the iteration variable for that body only; iterations are sequential.
- The iteration variable is available in templates; `user_input` is scoped per-iteration (iteration N+1 does not see iteration N's input); cross-iteration **agent output** remains visible (spec §"Cross-iteration visibility").
- The progress surface shows the iteration dimension ("iteration 2 of 3 (milestone = 'X'), step 3 of 8").
- An interrupted run inside iteration K step N recovers: iteration variable, output-scope map, and `user_input` restored from the checkpoint; execution resumes at step N within iteration K; steps 1..N-1 of iteration K are not re-run (spec §"Retry from inside a `for_each` iteration").

### Implementation Outline

- **Iteration execution.** Bind the `item` variable per iteration (scope layer already modeled in M3); run the body steps (which reuse all existing primitives). Empty list → zero iterations (no-op, not an error). A failure inside iteration N halts the whole run (no per-iteration error handling in v1). Nested `for_each` is already a parse-time error (M3).
- **Scoping.** Per-iteration `user_input` reset; cross-iteration agent output preserved in the output-scope map (the map is per-run, not per-iteration — so later iterations see earlier ones' turns, which is the documented behavior; the author uses an explicit fresh `wait_for` to avoid stale reads).
- **Iteration-aware checkpointing.** The M3 checkpoint record already carries iteration index + value + output-scope map + `user_input` (defined-but-unused since M3). Now write and read them. On retry, rebind the iteration variable, restore the output-scope map (so post-failure-step helpers resolve correctly without re-running earlier dispatches) and `user_input`, and resume at the failed step's index within the iteration. The two `user_input` retry cases (non-pause step after a completed pause vs. the Mode-2 pause that failed at dispatch) are spelled out in the spec — implement both.
- **Progress.** Extend the M5 progress payload with the iteration dimension (variable name, value, index/total) and the M6/M7 surface to render it.

### Definition of Done

- This milestone + Milestone 6 together are the **M7 acceptance test** (v1 plan): the milestone-iteration worked example (spec example 3) runs over a 3-item list, the progress surface shows the iteration index, and a run interrupted at iteration K step N resumes with the iteration variable, output-scope map, and `user_input` restored and steps 1..N-1 of iteration K not re-executed.
- Tests: iteration over a 3-item list runs the body 3×; empty list is a no-op; per-iteration `user_input` isolation; cross-iteration agent-output visibility; mid-iteration interrupt + recovery restores all checkpoint fields and resumes at the right step without re-running earlier steps; a failure inside an iteration halts the run.
- Progress surface renders the iteration dimension (frontend test).
- Spec §"Retry from inside a `for_each` iteration" is implemented; verify the build matches the doc.

---

## Cross-cutting notes

- **No milestone/pass references in code** (`AGENTS.md`): describe rules directly; this plan's "M4"/"M6" labels are for the plan, not for code comments. The *rationale* (decisions #1–#6) must survive into code docs; the milestone *numbers* must not.
- **Live tests:** the interpreter and resolver are tested fixture-driven via `MockHarnessAdapter` (hermetic, in CI). A small `make test-live` workflow run against a real harness (per the live-test naming convention `live_<harness>_…`) is worth adding once M5 lands, to catch the end-to-end dispatch+wait path against a real CLI — but it is additive, not a gate.
- **Open items deliberately deferred (not in this plan):** the M7 automated-handoff-guard classifier (the v1 outline marks it "may defer to v2" — leave it filed, do not build); workflow-progress ring buffer / backpressure (M8); polished walk-away (close-to-tray, quit-with-confirmation) — M8. If implementing any milestone surfaces a need for one of these, **stop and ask** rather than pulling it in.
