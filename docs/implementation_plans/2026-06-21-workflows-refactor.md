# Workflows refactor: hardcoded prompts + auto-derived prompt arguments

**Status:** proposed (2026-06-21). Follow-on to the v1 workflows work (`2026-06-15-workflows.md`).

**Audience:** an AI coding agent who will read the relevant code before acting but was not part of the design discussion. Read this whole document, then the "Docs to read first" list, before editing.

---

## Why this change

Today a workflow takes a **user-selectable prompt** as an input (`review_prompt: prompt_id`) and the author hand-wires that prompt's arguments via `template_vars` (`context: "{{ context }}"`). The workflow form shows a standalone `context` input regardless of which prompt the user picks. This is broken in two directions:

- **Over-supply:** pick a prompt that has no `context` argument (e.g. a user's own `code-review` variant) and the `context` field does nothing — a dead form field.
- **Under-supply / drift:** pick a prompt that *requires* an argument the workflow doesn't pass, or a computed handoff (`review ← aggregated_responses(...)`) whose target argument the prompt has since renamed, and the workflow **fails at run time** (render error → `failed` run). Invocation validation only checks the prompt *resolves*, not that its arguments line up.

The root cause is the combination of (a) a user-chosen prompt with (b) author-guessed arguments. The fix removes (a): **workflows hardcode their prompts**, and the form is **derived from the (now-known) prompt's arguments**.

### The model we're building

1. **Prompts are hardcoded literals** in `send.prompt` (`prompt: "builtin:code-review"`), not workflow inputs. The `prompt_id` input *type* is removed from the DSL.
2. A prompt's arguments are filled from **two sources**:
   - **Computed bindings** — author-wired `template_vars` whose value is a workflow expression the user can't type (`review: "{{ aggregated_responses(reviewers) }}"`). These stay in `template_vars`.
   - **User-fillable arguments** — every other argument of the hardcoded prompt. These are **auto-derived** from the resolved prompt and surfaced as form fields. They are *not* declared in `inputs:`.
3. **At invocation**, the app resolves each hardcoded prompt and:
   - **Validates** that every `template_vars` key is a real argument of that prompt (block with a clear error on drift — this is the "incompatible prompt" guard, now tractable because the prompt is fixed).
   - **Derives** the user-fillable argument fields for the form.
4. **At render**, each hardcoded-prompt `send` is given `template_vars ∪ {user value for each user-fillable arg of that prompt}` — exactly the prompt's declared arguments (the prompt renderer rejects unknown args).

### Key decision flagged for confirmation

**Remove the `prompt_id` input type entirely** (rather than keep it for power-user "user-selectable prompt" workflows). Rationale: this is a **scope simplification, not a correctness necessity**. Selectability does not by itself reintroduce drift — a selectable prompt whose argument fields are re-derived *after* the pick would be fine (that is exactly what `PromptComposer` already does on prompt-pick). What we avoid by removing it is the *cost* for v1: a two-stage pick-then-derive form and a second validation path, for a use case nobody has requested. (One wrinkle if it's ever reintroduced: a selectable prompt at a step that receives a **computed handoff** — `review ← aggregated_responses(...)` — could be picked incompatibly, so it would need a pick-time compatibility check, not just post-pick derivation.) The decision is therefore **reversible** — selectable prompts can be added back later without reopening the drift problem. This is a **breaking DSL change** (workflows declaring a `prompt_id` input no longer parse); blast radius is zero since workflows are unreleased. *Confirm the removal before implementing M1.*

### What is explicitly NOT in scope

- Resume/retry of failed runs (already deferred beyond v1).
- Per-step namespacing of derived arguments. We use a **flat, merge-by-name** model (see Conventions). The built-ins have no name collisions; per-step namespacing is speculative until a real workflow needs it.
- Forward-from on workflow inputs is **Milestone 5**, separable and deferrable — it has an open design question and the core fix ships without it.

---

## Docs to read first

- `docs/agent-instructions/workflows.md` — the authoring guide (you will rewrite parts of it in M6). Note the "⚠️ prompt model under revision" markers — those mark exactly the sections this plan changes.
- `docs/workflow-spec.md` — the formal DSL spec (§"Available template variables", §"Step-local variables", §"Validation rules", the worked examples). Same markers.
- `docs/system-design.md` §5 (workflows) and §8 (the walkthrough) — same markers.
- `docs/implementation_plans/2026-06-15-workflows.md` — the original milestones (M5/M6 are the workflow command + interpreter layers this plan modifies).
- MiniJinja undefined-behavior docs (the workflow render is strict-undefined, the prompt render is lenient): <https://docs.rs/minijinja/latest/minijinja/enum.UndefinedBehavior.html>

---

## Cross-cutting conventions (establish in M2, reuse in M3/M4)

These decisions are load-bearing across milestones; define them once and reuse.

- **Prompt-schema resolution primitive (with a freshness contract).** A single app-layer function resolves a prompt id (`provider:name`) to its declared `arguments` (or a not-found/unresolved result), via `PromptService`. Do **not** treat `PromptService.list()` as authoritative — it is a cache that can be cold or stale, especially for MCP prompts after startup or a provider change. Resolve local/built-in prompts directly; for MCP prompts, await or trigger a sync (or use an on-demand provider schema lookup) rather than inferring "missing" from a cold cache. Add a `get`-style method on `PromptService` if cleaner than filtering `list()`. This primitive is used by validation (M2), the form descriptor (M2), and the runtime arg-assembly (M3). Do not re-derive prompt schemas three different ways.

- **Binding classification.** For one hardcoded-prompt `send` with declared prompt arguments `A` and `template_vars` keys `T`:
  - **Computed args** = `A ∩ T` — filled by the workflow expression; hidden from the user.
  - **Invalid bindings** = `T \ A` — a `template_vars` key that is not an argument of the prompt. **Any non-empty `T \ A` is an invocation-blocking incompatibility** (the workflow is trying to inject into an argument the prompt doesn't have).
  - **User-fillable args** = `A \ T` — surfaced as form fields (required if the prompt marks the argument required).

- **Flat, merge-by-name argument namespace (type-aware collisions).** Derived user-fillable args share one flat namespace with declared inputs, keyed by argument name. A given name appears in the form **once**; at render it is passed to *every* hardcoded prompt that declares it as a user-fillable arg. Prompt arguments are **strings** (`render_template` requires string values and rejects unknown args), so collisions resolve by type:
  - A scalar **`text`/`text?` declared input** may shadow-and-satisfy a prompt argument of the same name: one field, the declared input's label/description wins, and its value also feeds the prompt (the intended escape hatch for custom-labeling a field that feeds a prompt).
  - **Two hardcoded prompts** with a same-named, same-typed (string) user-fillable arg **share** one field: its value feeds both, and it is required if *either* prompt marks it required.
  - A **non-text declared input** (`agent`/`[agent]`/`[text]`) colliding with a (string) prompt-arg name is a **descriptor/validation error** at form-build time — it would feed a non-string into a string slot. This is the **only** collision that is an error rather than a share.
  Every share above must be **surfaced** in the descriptor (or a dev-log) so it's diagnosable, not silent. These rules must be documented in code and in the spec.

- **Rationale must survive into code.** Every non-obvious decision above (why `prompt_id` is gone, why bindings are validated at invocation, the merge-by-name rule and its two consequences) must appear as a comment/docstring at the relevant code site, not just here. A reviewer reading `build_body` or the validation function should find the "why" without this plan.

---

## Milestone 1 — DSL: hardcode prompts, remove the `prompt_id` input type

### Goal & Outcome
The workflow file format expresses "this step runs *this specific* prompt," and a prompt is never a user-supplied input.
- A workflow author writes `prompt: "builtin:code-review"` (a literal id); there is no `prompt_id` input.
- A file declaring a `prompt_id` input fails to parse with a clear error.
- The two shipped built-ins are rewritten to hardcode their prompts and declare only their genuine non-prompt inputs (`reviewers`, `worker`).
- All workflow-crate fixtures/examples parse under the new rules.

### Implementation Outline
- **Remove `InputType::PromptId`** (`crates/workflow/src/model.rs`) and its handling (`parse.rs` `parse_input_type` ~line 247 + the long-form path, and the `InputType::PromptId` arm in `invocation.rs` `validate_value`). A `prompt_id` type token becomes an "unknown type" parse error; update the error message listing valid types.
- **Make `send.prompt` a static literal** (this corrects an earlier draft that left it templatable — that was a hole: if the prompt id could depend on an input value, the app couldn't statically resolve the schema to build the form, making "derive the form from the prompt" circular). **Enforce it respecting the crate-purity boundary:** `crates/workflow` depends only on `switchboard-core` and must **not** reach for `PromptId` (which lives in `crates/prompts` — pulling it in would break the purity rule the fixture-test strategy rests on). Split accordingly:
  - *Workflow crate (parse time):* reject template delimiters (`{{` / `{%`) in `send.prompt` and store it as a **validated literal string** — the invariant the pure crate can own without knowing what a `PromptId` is.
  - *App layer (`build_body`, `crates/app/src/workflow.rs`):* keep the existing `PromptId::parse` + resolution, but **drop the `render()` call** on the prompt field (there is nothing to template now). Do not move `PromptId` parsing into the workflow crate.
- **Rewrite the two resource YAMLs** (`crates/workflow/resources/workflows/`):
  - `review-and-aggregate.yaml`: `inputs` = `reviewers: [agent]`, `worker: agent` (keep the long-form descriptions). The first `send` hardcodes `prompt: "builtin:code-review"` with no `template_vars` (code-review's `context` is user-fillable, auto-derived — not wired here). Steps otherwise unchanged.
  - `review-analyze-discuss.yaml`: `inputs` = `reviewers`, `worker`. Step 1 hardcodes `prompt: "builtin:code-review"`. Step 3 hardcodes `prompt: "builtin:ai-review-feedback"` and keeps `template_vars: { review: "{{ aggregated_responses(reviewers) }}" }` (a computed binding — `review` is `ai-review-feedback`'s required arg). Drop the `review_prompt`/`analysis_prompt`/`context` inputs.
  - Note: `context` (code-review's optional arg) is now **derived**, not declared — it appears in the form via M2/M4, not via `inputs:`.
- **Update workflow-crate tests/fixtures** that encode `prompt_id` inputs: `crates/workflow/tests/fixtures/*.yaml`, `worked_examples.rs`, and the `parse.rs` unit test asserting `prompt_id` parses (it should now assert it's rejected). Keep the fixtures meaningful — convert them to hardcoded-prompt form rather than deleting coverage.
- **`recommended_prompts` removal** is an M2 concern (it lives in the app layer); leave a note but don't touch it here.

### Definition of Done
- `prompt_id` is gone from the type grammar; a unit test asserts a `prompt_id` input is a parse error with the updated message.
- `send.prompt` is literal-only: a unit test asserts a templated `prompt: "{{ x }}"` is a parse error, and a literal `builtin:code-review` parses and round-trips as a string. (The workflow crate does **not** reference `PromptId`.)
- Both built-ins parse, are runnable (`gated_step_kind() == None`), and hardcode their prompts; the existing `builtin.rs` tests pass (they assert names/runnable, not inputs).
- All workflow-crate fixtures/worked-examples parse.
- Rationale comment at the removed type site and in the built-in YAMLs (a short "prompt is hardcoded; `context` is auto-derived from the prompt" note).

---

## Milestone 2 — Invocation: resolve prompts, validate bindings, describe the form

### Goal & Outcome
At invocation time the app knows exactly what the user must fill and refuses to run a workflow whose hardcoded prompt has drifted.
- Picking a workflow yields a **form descriptor**: the declared inputs (agents/text) plus the auto-derived user-fillable prompt-argument fields, each with name/required/description.
- If a workflow's `template_vars` targets an argument its prompt no longer has (or the prompt can't be resolved), the descriptor reports a **blocking incompatibility** with a message naming the prompt and argument — the form can show it and disable run.
- Invocation pre-flight enforces the same checks, so a run never starts in a knowingly-broken state.

### Implementation Outline
- **Add the prompt-schema resolution primitive and binding classification** (Conventions) in the app workflow layer (`crates/app/src/workflow_commands.rs` or a sibling module). Inputs: the parsed `Workflow` + `PromptService`. Output per hardcoded-prompt `send`: `{computed: [..], invalid_bindings: [..], user_fillable: [PromptArgument..]}`, plus an unresolved-prompt signal.
- **Form descriptor command.** Add a command (e.g. `describe_workflow_form`) that, given a workflow identity (`name`, `is_builtin`), resolves its prompts on demand and returns: declared inputs (as today's `WorkflowInputInfo`) + derived user-fillable arg fields (reuse the same field shape so the frontend renders them uniformly) + a compatibility result (ok | list of `{prompt, argument}` invalid bindings | unresolved-prompt-needs-sync). **Resolution happens here, per picked workflow — not in `list`** (resolving every workflow's prompts on every menu render is wasteful and fails on cold remote caches). `list_workflows_impl` stays lightweight (metadata + declared inputs + `invocable`); it loses `recommended_prompts`.
  - Project scope: prompt resolution is **user-global** (prompts aren't project-scoped), so the descriptor command does **not** need `project_id`. The agent roster for agent-typed inputs already comes from the frontend's loaded project. *Confirm `PromptService` is reachable without a project before finalizing the signature.*
  - Cold MCP cache: if a prompt isn't yet resolvable (sync pending), report it as a distinct "unresolved" state, not a hard incompatibility — mirror how the prompt menu tolerates a cold cache, and let invoke pre-flight be the authoritative gate.
- **Validation — partition the invocation payload; keep `validate_invocation` intact.** The flat payload carries two kinds of keys, validated by two paths. Conflating them is a correctness regression: `validate_invocation` rejects any key not declared in `workflow.inputs`, so routing derived args through it would refuse the workflow's own `context`; and gutting it would drop the required-input / agent-roster / `[agent]` checks that must stay.
  - *Declared-input keys* → the existing `validate_invocation`, **retained as-is** except for removing the now-dead `prompt_id` handling (drop the `prompt_resolves` predicate from its signature and the `prompt_id` arm of `validate_value`). Its required-input, agent-roster, and `[agent]` non-empty/non-dup checks stay.
  - *Derived-arg keys* → the new prompt-arg validator (binding classification + required-fill check), run from the app layer where `PromptService` lives.
  Under the type-aware collision rule (Conventions), a name that is both a declared input and a prompt arg is validated by the declared-input path *and* also fed to the prompt as that arg. A non-empty `invalid_bindings` set, or a still-required derived arg left unfilled, blocks invocation. **Invoke is authoritative:** it re-resolves and re-validates prompt schemas at invocation (descriptor-time resolution is for UX; a prompt definition can change between form-open and invoke). Reuse the resolution primitive — do not duplicate.
- **Remove `recommended_prompts_for`** and the `recommended_prompts` field from `WorkflowListing` (it existed only to pre-seed `prompt_id` inputs).
- **`WorkflowInputValue` / input plumbing**: the invocation payload now carries declared-input values **and** derived user-fillable arg values in one flat map (Conventions). Keep the existing `string | string[]` value shape; derived args are strings.

### Definition of Done
- Unit/integration tests for the resolution+classification primitive: a prompt with an optional user arg → that arg is user-fillable; a `template_vars` key not on the prompt → invalid binding; a required prompt arg with no binding → user-fillable+required; an unresolved prompt id → unresolved state.
- A test that `describe_workflow_form` on `review-and-aggregate` returns `reviewers`, `worker`, and a derived optional `context`; on `review-analyze-discuss` returns `reviewers`, `worker`, `context` (and that `review` is **not** surfaced — it's computed).
- A test that invocation is **blocked** when a hardcoded prompt is missing a `template_vars`-targeted argument (simulate via a fixture prompt), with the error naming the prompt+argument.
- **Collision rules:** a declared `text` input shadowing a prompt arg → one field whose value feeds the prompt; a declared `[agent]` input colliding with a prompt-arg name → descriptor error; two hardcoded prompts with a same-named string user arg → one shared field (required if either prompt requires it).
- **Partition (regression guard):** a derived arg value (e.g. `context`) is accepted at invocation, not rejected as "not declared"; a missing required *declared* input still fails; a value naming a non-roster agent still fails; a required *derived* arg left unfilled blocks invocation.
- **Invoke is authoritative:** a workflow whose descriptor validated cleanly is still re-validated at invoke and blocked if the prompt's schema changed in between (simulate by mutating the fixture prompt's arguments between describe and invoke).
- **Freshness:** a local/built-in prompt resolves under a cold cache (never falsely "unresolved"); an unresolvable id yields the unresolved state. (If an MCP prompt stub is available, also cover cold → resolved-after-sync; otherwise record it as covered only at the integration boundary.)
- The existing app-layer invoke/validate tests updated to the new input shape (no `review_prompt`/`primary_agent`-style prompt inputs; `worker`/`reviewers` + derived `context`).
- Rationale comments at the validation site (why drift is caught here, not at run).

---

## Milestone 3 — Runtime: pass derived argument values at render

### Goal & Outcome
When a workflow runs, each hardcoded-prompt step renders with the right arguments: the user's typed values plus the workflow's computed handoffs, and nothing the prompt doesn't declare.
- A `send` with a hardcoded prompt renders successfully using the user-supplied values for that prompt's user-fillable args.
- Computed handoffs (`aggregated_responses`, etc.) still fill their bound args.
- No "unknown argument" render failure from passing an arg the prompt doesn't declare.

### Implementation Outline
- In `WorkflowRun::build_body` (`crates/app/src/workflow.rs` ~429): today `args` = rendered `template_vars` only. Change to `args` = rendered `template_vars` **∪** `{user value for arg a : a ∈ this prompt's user-fillable args}`. Concretely: resolve the prompt's declared arguments (the M2 primitive), and for each declared arg **not** already provided by `template_vars`, pull its value from the invocation's user-fillable values (the flat map bound into the run). Pass exactly those — `render_template` rejects unknown args, so passing the whole user map blindly would fail for any prompt that doesn't declare a given name.
- The run must hold the user-fillable values: thread them in alongside the existing bound inputs at run construction (`bind_invocation` / wherever the run captures invocation inputs). They do **not** go into the MiniJinja workflow scope as template variables (they're prompt args, not workflow-template vars) — keep them separate to avoid a derived arg accidentally shadowing a workflow input in a `text:` template.
- Optional-arg semantics: an unfilled optional user arg should be **omitted** from the args map (so the prompt's lenient-undefined renders it empty / falsy in `{% if %}`), matching `code-review`'s `{% if context %}`. Do not pass empty strings for unfilled optionals if that changes `{% if %}` truthiness — verify against `render_template`'s lenient behavior.

### Definition of Done
- Integration test through `WorkflowRun`: invoke `review-and-aggregate` with a `context` value → the dispatched body to reviewers contains the rendered `code-review` prompt *with* the context section; invoke without `context` → renders the no-context branch. (Reuse the existing `RecordingDispatch`-style harness in `workflow.rs` tests.)
- A test that `review-analyze-discuss`'s analysis step still renders `ai-review-feedback` with the aggregated `review` arg.
- **Per-prompt argument isolation** (the built-ins don't exercise this — needs a synthetic two-prompt fixture): a workflow whose step A hardcodes a prompt with user arg `foo` and step B a prompt with user arg `bar`. Filling both, step A renders with only `foo` and step B with only `bar`, and neither fails on an unknown argument — proving each prompt receives exactly its own declared args, not the whole user-value map (`render_template` rejects unknown args, so a leak would fail the run).
- Rationale comment in `build_body` explaining the two arg sources and why only the prompt's declared args are passed.

---

## Milestone 4 — Frontend: dynamic invocation form

### Goal & Outcome
The workflow form shows the user exactly what the chosen workflow needs — agent slots plus the real arguments of its prompts — and refuses to run an incompatible one.
- Selecting a workflow renders its agent/list inputs (as today) **plus** the derived prompt-argument fields, each a describable text field. (The per-field forward-from picker is Milestone 5; M4 without M5 is complete — the fields are plain, fillable text inputs.)
- The prompt-picker control disappears from the workflow form (prompts are no longer chosen here).
- If the workflow is incompatible (drifted prompt), the form shows the error and disables Run.
- Running passes all values (declared + derived) to the backend.

### Implementation Outline
- **On pick**, call the M2 `describe_workflow_form` command instead of seeding from `WorkflowListing.inputs` + `recommended_prompts`. `ComposeBar.pickWorkflow` builds form state from the descriptor; drop the `recommended_prompts` seeding and the `prompt_id` branch. **Re-fetch the descriptor on `prompts:synced`** (mirroring the prompt menu's cold-cache handling) so a workflow that hardcodes an MCP prompt resolves once sync lands, instead of stranding the user on an "unresolved" form until they re-pick.
- **`WorkflowComposer`**: remove the `prompt_id` field rendering (the embedded prompt menu / `promptMenuFor` state). Render derived prompt-arg fields as describable text fields, reusing the existing field pattern. (The per-field `ForwardSourcePicker` wiring — same as `PromptComposer` — arrives in M5; M4 fields are plain text inputs.) Surface the compatibility error from the descriptor as a blocking message (sibling to the existing non-`invocable` message) that disables Run.
- **Pending vs. error for the unresolved state (latency UX).** Resolving a prompt's *schema* reads the synchronous in-memory cache (`PromptService.list()`) — it is **not** a network call, so building the form is instant. The only slow window is a **cold cache before the first background `sync()` lands**, where an MCP-hardcoded prompt isn't cached yet and the descriptor reports `unresolved`. Treat `unresolved` as a **non-error pending affordance** — a "resolving prompts…" spinner with Run disabled and **no** incompatibility styling — *not* the blocking drift error. It self-heals via the existing `prompts:synced` re-fetch. Only escalate `unresolved` to the blocking-error presentation once resolution has **settled** (a `prompts:synced` has fired and the prompt is still unresolved → genuinely missing). The `invalid_bindings` incompatibility is always a hard error regardless of sync state. (Distinguishing pending from settled-missing is the frontend tracking "has a sync settled since pick"; `PromptService` already exposes per-provider `ProviderStatus` if a richer signal is wanted — don't over-build it.) Note: the *dispatch-time* MCP `render()` call (M3) is a genuine network round-trip, but it happens inside a running workflow where the run indicator / per-step progress already absorbs the latency; no form spinner is involved there.
- **Grouping/labeling**: derived fields belong to a specific prompt; a light label/hint indicating which prompt a field feeds is enough (decide presentation against the code). The contract is "derived prompt args render as fillable, describable fields"; don't over-build grouping the built-ins don't need (each built-in has a single derived `context`).
- **`api.ts` / `types.ts`**: add the `describe_workflow_form` binding and its descriptor type; drop `recommended_prompts` from the `WorkflowListing` type; the invoke payload carries the flat declared+derived value map (shape unchanged: `Record<string, string | string[]>`).
- **Missing-required gating** (`WorkflowComposer.missingRequired` / `ComposeBar.workflowMissing`): extend to include required derived args.

### Definition of Done
- Component tests: a workflow descriptor with a derived optional `context` renders a `context` field; a descriptor with a required derived arg blocks Run until filled; an incompatible descriptor shows the error and disables Run; no prompt-picker control renders.
- A `ComposeBar` test that picking a workflow calls `describe_workflow_form` and invoking passes the combined declared+derived values.
- A test that an **unresolved** descriptor re-fetches on `prompts:synced` and resolves into a fillable form (no manual re-pick needed).
- A test that the **unresolved** state renders as a non-error pending affordance (spinner / "resolving…", Run disabled, no incompatibility error) while a sync may be in flight, and only escalates to the blocking error once a `prompts:synced` has settled with the prompt still unresolved.
- Update existing `WorkflowComposer`/`ComposeBar` tests that referenced `prompt_id` fields or `recommended_prompts`.

---

## Milestone 5 — Forward-from on text & derived prompt-argument fields (separable)

### Goal & Outcome
A workflow's user-fillable text fields (genuine `text` inputs and derived prompt args) can be filled by forwarding an agent's or pane's **already-completed** latest output, like prompt-composer arguments.
- The user can attach an agent/pane forward source to a workflow text field; at invocation that field's value is the forwarded output (composed like a prompt forward).
- **Completed-source semantics (decided):** the source's latest *completed* turn is captured at invoke and the run starts immediately. If a chosen source still has an **in-flight turn**, invocation is **rejected with a clear message** ("agent X is still responding — wait for it to finish, then run the workflow"). The workflow launch is never held open waiting on a streaming agent.

This is **decided, not open** — see "Decision: completed-only, no holding" below. M5 is a small, well-understood addition layered on M1–M4; it can ship after them, but carries no unresolved design question.

### Implementation Outline
- **Resolution at invoke (backend).** Reuse the existing source resolver (`resolve_all_sources` / `resolve_source` in `commands.rs`), but in **completed-only** mode: instead of awaiting a source's in-flight turn, detect it (the resolver already distinguishes idle-with-completed-transcript from an active turn via `dispatcher.wait_for_current_turn`) and **reject** the invoke if any chosen source is mid-turn. For idle sources, read the latest completed output exactly as the resolver does today. Compose typed-lead + forwarded blocks via the existing `compose_forwarded_message` / `compose_resolutions` helpers. The resolved field values are bound as the field's value — they are *field inputs*, not a held send.
- **Invoke payload.** Gains per-field forward sources for text/derived fields (mirror `ForwardArg`: field name → source agent ids). Resolution + the completed-only check run in the app layer at invoke, **before** the run is spawned. Because resolution is synchronous-ish (no holding), it does not change the existing fast invoke→run-id return contract in any user-visible way.
- **Reuse, don't reinvent (frontend).** The per-field `ForwardSourcePicker` + chip UI from `PromptComposer`, and `expandForwardSources`. `WorkflowComposer`'s derived/text fields gain the same picker the prompt composer fields already have.

### Decision: completed-only, no holding (rationale, and the deferred path)
Manual forwarding (`forward_prompt`) *holds* the send until sources finish. A code investigation (2026-06-21) confirmed the held resolver and the run's cancel/registry lifecycle are both reusable, so holding is **technically feasible** — but its cost is **UX, not backend**: holding the invoke open for a still-streaming agent means a minutes-long, hung-looking invoke with no cancel affordance on the gated button (the manual held-forward has dedicated "waiting/[cancel]/restore" UI precisely for this; a workflow invoke does not). The clean version of holding would make the wait a first-class **run phase** (return the run id immediately, resolve sources as the run's first step with normal progress + cancel) — a meaningfully larger change to the interpreter and scope model. Since a workflow forward source is almost always an already-completed turn, completed-only serves the real case at near-zero risk. **If real usage shows users genuinely forwarding from still-streaming agents, revisit with the run-phase approach (not an IPC-held invoke).** This supersedes the earlier "open question — hold semantics" framing.

### Definition of Done
- Component tests: attaching/removing a forward source on a workflow text/derived field (reusing the `ForwardSourcePicker` pattern).
- Backend test: invocation resolves a forwarded field to the composed output from the source's completed turn.
- Backend test: a still-streaming source is **rejected at invoke** with a clear message, and the run does not start.
- The completed-only limitation is documented in `workflows.md` ("workflow forward fields use an agent's latest *completed* output; if the agent is mid-response, wait for it to finish") so the behavior isn't surprising.
- If M5 ships after M1–M4 rather than with them, record the interim "workflow fields don't yet support forward-from" note in `workflows.md` so the gap isn't silent.

---

## Milestone 6 — Documentation

### Goal & Outcome
The authoring guide and spec teach the hardcoded-prompt model; no stale prompt-model examples or "under revision" markers remain.

### Implementation Outline
- `docs/workflow-spec.md`: remove the `prompt_id` type from the input-type grammar; rewrite the `prompt`/`template_vars` semantics to "hardcoded prompt + computed `template_vars` + auto-derived user args"; document the binding classification, the merge-by-name rule and its two consequences, and the invocation-time incompatibility check; rewrite the worked examples to hardcoded form; remove the revision marker.
- `docs/agent-instructions/workflows.md`: update the input-types table (drop `prompt_id`), add a section on hardcoded prompts + auto-derived arguments (what the author writes vs. what the form derives), update both shipped-built-in YAML blocks to the M1 form, remove the revision marker. While here, fix the pre-existing **retry/checkpoint contradiction** (the failure-handling section describes resume/retry that v1 doesn't support — align it with "abandon + re-invoke").
- `docs/system-design.md` §5 illustrative YAML and §8 walkthrough: rewrite to hardcoded prompts; remove the revision markers and the `aggregation_prompt`/`prompt_id` framing.
- Update `2026-06-15-workflows.md` with a short pointer to this refactor (as the M5/M6 command + interpreter model is superseded here).

### Definition of Done
- No remaining `prompt_id`-as-input or `aggregation_prompt`-input examples in the authoritative docs; no "under revision" markers.
- The retry/checkpoint contradiction in `workflows.md` is resolved.
- The doc presents the **two ways a step produces its message**, side by side: (a) **named prompt** (`prompt: builtin:…`) — fixed prompt, arguments auto-derived from its schema; (b) **inline text** (`text: "… {{ arg }}"`) — literal body, `{{ }}` vars resolved from workflow scope, made user-fillable by declaring them in `inputs:`. Includes a worked inline-text-with-arguments example so the inline path isn't lost behind the named-prompt arg-derivation prose.
- An author following `workflows.md` alone can write a correct hardcoded-prompt workflow without reading code.

---

## Risks & sequencing notes

- **M1 is a breaking parser change** (the flagged decision). Land it only after the `prompt_id`-removal decision is confirmed.
- **M2 establishes the shared resolution + classification primitive**; M3 and M4 must reuse it, not re-derive prompt schemas. A plan where M3 re-implements schema lookup has failed the "establish patterns early" rule.
- **M4 depends on M2's descriptor command and M3's invoke shape.** M4 can render derived fields as plain text inputs even if M5 (forward pickers) is deferred.
- **M5 is separable and deferrable** without weakening the core fix. Its design is **decided** (completed-only forward, no holding — see M5); it is small and carries no open question.
- The whole change is one logical unit but **need not be one PR** — M1–M4 + M6 are the shippable core; M5 can follow.
