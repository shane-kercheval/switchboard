# Switchboard workflow DSL spec (v1)

## Purpose

This document defines the workflow file format Switchboard parses and executes. It is the formal version of the illustrative YAML in [`system-design.md`](system-design.md) §5 and resolves open question 5.1.

This spec is **implementation-grade**: it pins down field names, types, scoping rules, error conditions, and built-in template functions concretely enough that a parser and runtime can be implemented from it.

It is **not** a user tutorial. Tutorial-style authoring guidance lives in `docs/agent-instructions/workflows.md` (separate artifact, written for AI coding agents to consume per system-design §2 "Agent-friendly authoring").

## Scope

- File structure: top-level keys, inputs declaration, steps array
- Step types: one per workflow-runtime primitive (system-design §4 Primitives 2–6 — `send`, auto-forward [as `forward_from` on `send`], fan-in, pause for user input, iterate). Spawn (Primitive 1) is not a workflow step — agents are spawned through the UI before the workflow runs and supplied as workflow inputs. Plus the `wait_for` and `wait_for_all` synchronization helpers
- Templating: MiniJinja subset, available variables, built-in template functions
- Variable scoping rules
- Error and validation conventions
- Three worked examples
- Reserved keys for v2+ (forward-compat)

What is **out of scope** for this doc:

- Parser internals (recursive descent vs YAML library choice — M6 implementation)
- Persistence-layer encoding of workflow runs (`<directory>/.switchboard/projects/<project-id>/runs/<run-id>.jsonl` schema — M6 expansion)
- UI rendering of workflow progress (system-design §7)

## File location and naming

- Workflow files are **user-global**: they live in one folder under the OS config dir (e.g. `~/Library/Application Support/switchboard/workflows/` on macOS), shared across every project in every working directory; see system-design §3. A workflow's `agent`/`[agent]` inputs bind to whichever project's agents it's run against, so the definition isn't tied to any one directory.
- One workflow per file; filename matches the workflow's declared `name` (e.g., `review-and-aggregate.yaml`)
- File extension: `.yaml` (preferred) or `.yml`

## Top-level structure

Every workflow file is a YAML mapping with these keys:

| Key | Required | Type | Notes |
|---|---|---|---|
| `name` | yes | string (slug) | Matches `[a-z][a-z0-9-]*`. Must equal the filename minus extension. |
| `description` | yes | string | One-line human description. Surfaced in invocation UI. |
| `inputs` | optional | mapping | Declared inputs the user supplies at invocation time. See "Inputs" below. Omit if the workflow takes no inputs. |
| `steps` | yes | sequence | Ordered list of steps. Must be non-empty. See "Steps" below. |

Top-level keys not in this table are an error in v1. (Reserved keys for v2 listed under "Forward-compat reservations" below.)

## Inputs

`inputs` is a mapping of input name → input declaration. Each input declaration is either a type shorthand (string) or a mapping with metadata.

### Input types

| Type | Shorthand | Meaning |
|---|---|---|
| Agent | `agent` | A single agent name (must exist in the project at invocation time) |
| Agent list | `[agent]` | A list of agent names |
| Text | `text` | Free-form string |
| Text (optional) | `text?` | Free-form string; user may leave blank |
| List of text | `[text]` | List of strings (used by Primitive 6 iteration) |

There is no `prompt_id` input type. A step names its prompt as a **hardcoded literal** (see §`send` and §"Hardcoded prompts and auto-derived arguments"); declaring a `prompt_id` input is a parse error (unknown type). A prompt's user-fillable arguments are auto-derived from the resolved prompt and surfaced as form fields at invocation — they are **not** declared in `inputs`.

In v1 the only optional type variant is `text?`. `agent?` is deferred to v2+.

### Shorthand form

```yaml
inputs:
  primary_agent: agent
  reviewer_agents: [agent]
  user_context: text?
```

### Long form (when metadata is needed)

```yaml
inputs:
  user_context:
    type: text?
    description: Optional context the reviewers should focus on
    default: ""
```

Long-form keys:

| Key | Required | Notes |
|---|---|---|
| `type` | yes | One of the type shorthands above. |
| `description` | optional | Human description shown in the invocation form. |
| `default` | optional | Default value if the user leaves the field blank. Providing `default` implicitly makes the input optional; the `?` suffix on a type is shorthand for an optional input with a default of `""`. **`default` is only valid on a `text` input in v1** — it is the optional-input mechanism, and optional non-`text` inputs (`agent?`, and optional `[agent]`, which would contradict the non-empty-fan-in rule) are deferred to v2. A `default` on any non-`text` type is a parse error. |

Long-form `type` is required when long form is used; mixing shorthand and long form across different inputs is allowed.

### Validation rules

- Input names must match `[a-z][a-z0-9_]*`
- An input named with a reserved built-in name (`user_input`) is an error
- Required inputs (no `?` suffix, no `default`) must be supplied at invocation; missing values fail invocation pre-flight
- Agent-typed inputs are validated at invocation time — referenced agents must exist in the project

## Steps

`steps` is a YAML sequence. Each entry is a mapping with exactly one top-level key naming the step type. The value of that key is the step's parameters.

```yaml
steps:
  - send:
      to: "{{ reviewer_agents }}"
      prompt: "builtin:code-review"
  - wait_for_all:
      agents: "{{ reviewer_agents }}"
  - send:
      to: "{{ primary_agent }}"
      prompt: "builtin:ai-review-feedback"
      template_vars:
        review: "{{ aggregated_responses(reviewer_agents) }}"
```

Each step type is documented below. Unknown step types are a validation error.

### `send` (Primitive 2)

Dispatch a message to one or more agents. Returns immediately; does not wait for the agents to complete (see `wait_for` / `wait_for_all` for synchronization).

| Field | Required | Type | Notes |
|---|---|---|---|
| `to` | yes | agent or [agent] | Recipient(s). Single agent or a list. Templated. |
| `prompt` | yes (or `text` or `forward_from`) | literal string | A hardcoded prompt id (e.g., `"builtin:code-review"`). **Not templated** — a `{{ }}`/`{% %}` delimiter in `prompt` is a parse error (see §"Hardcoded prompts and auto-derived arguments"). |
| `text` | yes (or `prompt` or `forward_from`) | text | Literal text to send (no prompt resolution). Templated. Mutually exclusive with `prompt`. |
| `template_vars` | optional | mapping | **Computed bindings only** — workflow expressions wired to a prompt argument (e.g., `review: "{{ aggregated_responses(reviewers) }}"`). Mapping of name → templated value. Each key must be a real argument of the hardcoded prompt (see §"Binding classification"). User-fillable arguments are **not** put here — they are auto-derived. |
| `forward_from` | optional | agent or [agent] | Auto-forward source(s). When set, the latest output(s) of the named agent(s) are composed into the message body per the canonical shape below. Equivalent to Primitive 3 (auto-forward). If any referenced agent has no completed output from the current workflow run (per §"Output scope"), the step fails with a clear error ("no in-workflow completed output for agent X"). |

If `prompt` is set, the hardcoded prompt is resolved and its template rendered with the step's `template_vars` (computed bindings) plus the user's values for the prompt's auto-derived user-fillable arguments, then dispatched. If `text` is set, the literal text is dispatched (after templating). At least one of `prompt`, `text`, or `forward_from` is required.

#### Hardcoded prompts and auto-derived arguments

A `send` step names its prompt as a **hardcoded literal id**, never a user-supplied input. The id cannot be templated: the app must statically resolve the prompt's argument schema to build the invocation form, so allowing the id to depend on an input value would make form-derivation circular. A `prompt` field containing `{{` or `{%` is a parse error.

Because the prompt is fixed, the app resolves it at invocation and fills its declared arguments from **two sources**:

- **Computed bindings** — the step's `template_vars`, whose values are workflow expressions the user can't type (`{{ aggregated_responses(reviewers) }}`, etc.). These stay in `template_vars` and are hidden from the user.
- **User-fillable arguments** — every *other* argument of the prompt. These are **auto-derived** from the resolved prompt and surfaced as invocation-form fields (required iff the prompt marks the argument required). They are **not** declared in `inputs`.

At render, each hardcoded-prompt `send` receives exactly its prompt's declared arguments: its `template_vars` plus the user's value for each user-fillable argument the prompt declares — and nothing else (the prompt renderer rejects unknown arguments). An unfilled optional argument is omitted, so the prompt's lenient-undefined `{% if arg %}` branch renders empty.

#### Binding classification

For one hardcoded-prompt `send`, let `A` = the resolved prompt's declared argument names and `T` = the step's `template_vars` keys:

| Set | Meaning | Surfaced to user? |
|---|---|---|
| `A ∩ T` | **Computed args** — filled by the workflow expression in `template_vars`. | No (hidden). |
| `T \ A` | **Invalid bindings** — a `template_vars` key the prompt has no argument for. | Blocks invocation (see below). |
| `A \ T` | **User-fillable args** — auto-derived form fields, required iff the prompt requires them. | Yes (fillable field). |

**Invocation-time incompatibility check.** Any non-empty `T \ A` — or a `prompt` id that doesn't resolve — is an **invocation-blocking incompatibility**: the form reports it (naming the offending prompt and argument) and disables Run. This catches the drift case where a workflow's computed binding targets an argument the prompt has since renamed or removed. The check runs both when the form is built and again at invoke (invoke is authoritative — a prompt definition can change between form-open and invoke).

#### Flat, merge-by-name argument namespace

Auto-derived user-fillable arguments share **one flat namespace** with the declared `inputs`, keyed by name. A given name appears in the form **once**; at render it is passed to *every* hardcoded prompt that declares it as a user-fillable arg. Because prompt arguments are strings, name collisions resolve by type:

1. A scalar **`text`/`text?` declared input** may shadow-and-satisfy a same-named prompt argument: one field, the declared input's label/description wins, and its value also feeds the prompt (if the prompt marks the argument required, the input is enforced required). This is the escape hatch for custom-labeling a field that feeds a prompt.
2. **Two hardcoded prompts** that declare a same-named (string) user-fillable argument **share** one field: its value feeds both, required if *either* prompt marks it required.
3. A **non-text declared input** (`agent`/`[agent]`/`[text]`) colliding with a (string) prompt-argument name is a **validation error** at form-build time — it would feed a non-string into a string slot. This is the only collision that errors rather than shares.

When `to` is a list of agents, dispatches are issued in declared order; agents run in parallel. The step returns once all dispatches have been issued (not when any has completed) — to synchronize, use `wait_for_all` in the next step.

**Partial-dispatch failure:** If any dispatch in the list fails pre-flight (contention refusal, agent deleted, render error), remaining dispatches in the list are not attempted. Dispatches in the same step that have **already** been issued are **not** cancelled — they run to their natural terminal state and their output stays visible for inspection. The step is marked `failed`. Retry-from-step re-runs the whole step (re-issuing every dispatch), so the runtime never needs partial-state-reuse semantics; not cancelling the survivors avoids discarding work that already cost its quota and is frequently independently useful. See §"Sibling-failure policy" under "Failure handling" for the full resolved policy (the earlier "SIGTERM the survivors" rule is superseded) and its M6/M7 phasing.

#### Canonical composition with `forward_from`

When `forward_from` is set, the dispatched message body is composed deterministically so that workflow files remain portable and reviewable:

1. The rendered `text` or `prompt` body (if either is set) appears first.
2. Each forwarded agent's latest completed output appears below, in declared order, each delimited by a sentinel line on its own line:

```
<rendered text or prompt body, if any>

=== START forwarded from <agent_name> ===
<agent's latest completed output verbatim>
=== END forwarded from <agent_name> ===

=== START forwarded from <next_agent_name> ===
...
```

The `=== START / END ===` sentinel is plain-English-readable to the receiving agent and unlikely to collide with markdown headers, code fences, or other content agents commonly produce. If only `forward_from` is set (no `text` or `prompt`), the body is the forwarded composition alone with no leading content.

### `wait_for` (synchronization, single agent)

Block until the named agent's in-flight turn completes (or fails).

| Field | Required | Type | Notes |
|---|---|---|---|
| `agent` | yes | agent | Templated. |

Failure of the awaited agent is a step failure (per system-design §7 Failure handling).

**No-in-flight-turn behavior:** `wait_for` distinguishes two cases when the agent has no in-flight turn at the moment the step is reached:

- If this workflow run has previously dispatched to this agent (via a prior `send`, `pause_for_user` with `recipient`, or auto-forward) and that turn has already reached terminal state, `wait_for` succeeds immediately — the barrier was already cleared before the runtime advanced to it.
- If this workflow run has never dispatched to this agent, `wait_for` is a step failure with a clear error ("no turn to wait on for agent X"). This catches the authoring bug of writing `wait_for` for an agent the workflow never sent to.

### `wait_for_all` (synchronization, multiple agents — used as the wait phase of Primitive 4 fan-in)

Block until all named agents' in-flight turns complete. Failure of any one is a step failure for the whole `wait_for_all`. Per-agent no-in-flight-turn behavior follows the same rule as `wait_for` above.

| Field | Required | Type | Notes |
|---|---|---|---|
| `agents` | yes | [agent] | Templated. |

`responses_from(agents)` (see "Built-in template functions") is callable in subsequent steps whenever every agent in the argument has a completed turn — typically immediately after a `wait_for_all`, but also valid after a single-agent `wait_for` or after natural completion from prior steps.

### `pause_for_user` (Primitive 5)

Suspend workflow execution and wait for the user to respond via the compose bar. Fires an OS-native notification when entered.

| Field | Required | Type | Notes |
|---|---|---|---|
| `context` | optional | text | Templated message shown to the user (e.g., "Reviews are in. What direction do you want to take?"). |
| `recipient` | optional | agent | If set, the user's response is dispatched to this agent as part of the same step (see "Mode 2" below). Templated. |
| `required` | optional | bool | Default `true`. If `true`, the user choosing "skip" cancels the workflow. If `false`, the user may skip without supplying input. |

The user's typed text is captured into the built-in variable `user_input` (scoped per "Variable scoping" below). If the user combined a prior agent's output or applied a wrapping prompt in the compose bar, those affect what is dispatched to `recipient` — they do not change `user_input`. This keeps `user_input` predictable for subsequent template references.

If the user typed nothing but combined a prior agent's output and dispatched, `user_input` is the empty string; in Mode 2, the dispatch still happens with the combined content as the message body.

The step has two modes depending on whether `recipient` is set:

**Mode 1: without `recipient` — capture only.**

The step suspends, captures the user's typed text into `user_input`, and completes. No dispatch happens; no agent turn is initiated by the step. The next step in the workflow runs immediately after the user submits. If `required: false` and the user skips, `user_input` is bound to the empty string and the step still completes normally.

**Mode 2: with `recipient` — capture, dispatch, and implicitly wait.**

The step suspends, captures the user's typed text into `user_input`, dispatches `user_input` (along with any compose-bar combining/wrapping the user applied) to `recipient`, and **implicitly waits** for the recipient's turn to reach terminal state before the next workflow step runs. The pause + dispatch + wait are bundled into one step. This is the only step type in the spec that bundles wait with dispatch — the rationale is ergonomic: the user has just answered a question and the natural expectation is to see the agent's response before the workflow proceeds, and pause-with-recipient targets exactly one agent (no fan-out parallelism to preserve). Authors wanting true fire-and-forget after a pause should drop `recipient` (use Mode 1) and write a separate `send` step using `user_input`.

If `required: false` and the user skips in Mode 2, no dispatch occurs and no wait is applied; `user_input` is bound to the empty string and the step completes immediately.

If `required: true` and the user skips in either mode, the workflow is marked `cancelled` (per "Failure handling" below).

**Mode 2 dispatch failure (contention refusal, agent deleted, render error, etc.):** If the dispatch to `recipient` fails at dispatch time for any reason — most commonly because `recipient` is mid-turn from another workflow or a manual send — the workflow is marked `failed` (per the contention rule in system-design §7). On retry-from-step, the runtime re-enters the pause: the compose bar is shown again, **pre-filled with the text captured in `user_input` before the failed dispatch**. The user must re-submit explicitly — the pre-fill is a convenience, not an automatic re-dispatch. This lets the user re-send unchanged or revise given whatever context shifted, and avoids both the silent-input-loss UX cliff and the "stale intent silently re-dispatched" surprise.

### `for_each` (Primitive 6)

Repeat a sub-sequence of steps once for each item in a list.

| Field | Required | Type | Notes |
|---|---|---|---|
| `item` | yes | string | The iteration variable name. Must match `[a-z][a-z0-9_]*`. |
| `in` | yes | [text] or [agent] | The list to iterate over. Templated. Must resolve to a list. |
| `steps` | yes | sequence | Sub-steps to execute per iteration. Same structure as the top-level `steps`. |

The iteration variable is bound for each iteration's body and accessible in template substitution (e.g., `{{ milestone }}`). Iterations are sequential, not parallel. Iterating over an empty list is a no-op (the body executes zero times); not an error. A failure inside iteration N halts the whole workflow per system-design §4 (no per-iteration error handling in v1). Nested `for_each` is an error in v1.

The iteration variable name (`item:`) must not collide with any workflow input name or with the reserved built-in name `user_input`; the collision is a parse-time validation error (consistent with the agent-name uniqueness rule — silent shadowing is a footgun and is rejected at the boundary).

## Templating

All string values in a workflow file are passed through the templating engine before use. The engine is a **MiniJinja subset**.

### Supported MiniJinja features

- Variable substitution: `{{ var }}`
- Member access: `{{ obj.field }}`, `{{ list[0] }}`
- For loops: `{% for x in list %}...{% endfor %}` (including `loop.index`, `loop.first`, `loop.last`)
- If conditions: `{% if expr %}...{% elif %}...{% else %}...{% endif %}`
- Expression operators: comparison (`==`, `!=`, `<`, `>`, `<=`, `>=`), boolean (`and`, `or`, `not`), and basic arithmetic (`+`, `-`, `*`)
- Whitespace control: `{%-`, `-%}`, `{{-`, `-}}`
- Comments: `{# ... #}`
- Built-in filters: `length`, `lower`, `upper`, `default`, `join`, `trim`

### Enforcement boundary (what parse-time validation rejects)

Parse-time validation enforces the **tag** subset and the **filter** allowlist above: an unsupported tag (`{% set %}`, `{% raw %}`, macros, inheritance, includes, the `do` tag) or a filter outside the six listed is a parse error. These are the constructs whose availability or behavior **differs across Jinja engines**, so blocking them is what preserves cross-engine portability (MiniJinja ↔ Tiddly's Jinja2, per system-design §6).

Expression-level syntax — the comparison / boolean / arithmetic operators, member access, and indexing above — is **accepted and not parse-rejected**. These operators are core Jinja syntax that renders identically across engines, so enforcing them would buy no portability while requiring a full expression parser. *Author guidance (not enforced):* keep expressions simple for cross-engine fidelity; obscure numeric semantics (e.g. division / floor-division, type coercion) are not guaranteed identical across engines and are not part of the committed v1 contract.

### Unsupported in v1 (tags/filters → parse error; see "Enforcement boundary")

- Custom filters (Tiddly's project-specific filters)
- The `do` extension
- `{% raw %}` blocks
- Macros (`{% macro %}`)
- Template inheritance (`{% extends %}`, `{% block %}`)
- Includes (`{% include %}`)
- Set assignments (`{% set %}`) — workflow-level state should come from inputs or step outputs, not template-local variables

These are deferred so that prompts move cleanly between Switchboard's local rendering and Tiddly's Jinja2 server-side rendering (per system-design §6 cross-agent normalization). v2 may expand the supported subset.

### Available template variables

Variables are resolved in this scope order, listed innermost (highest precedence) to outermost (lowest):

1. **Step-local variables** — the `template_vars` of the currently-rendering `send` step (only visible inside that step's prompt template render, not other steps). Most local because they exist only for the duration of one render call.
2. **Iteration scope** — when inside a `for_each` body: the iteration variable (e.g., `{{ milestone }}`)
3. **Pause scope** — after a `pause_for_user` step in the current iteration / workflow: the variable `user_input`
4. **Workflow inputs** — names declared in the top-level `inputs` mapping

A variable name that resolves in two scopes uses the innermost. A variable name that resolves in none is a render error (no silent empty-string fallback).

### Built-in template functions

| Function | Returns | Notes |
|---|---|---|
| `responses_from(agents)` | mapping name → text | Maps each agent's name (with hyphens normalized to underscores) to that agent's latest completed turn output **for the current workflow run** (see "Output scope" below). The mapping preserves the input agent list order during iteration. Errors if any agent has no completed output yet from this workflow run. Agent-name uniqueness after hyphen→underscore normalization is enforced at agent-creation time (per system-design §3 Primitive 1), so collisions cannot occur here. Use this when authoring a Switchboard-aware aggregation prompt that wants to iterate over responses with custom formatting. |
| `aggregated_responses(agents)` | text | Returns the same data as `responses_from(agents)` pre-formatted into a single string in the canonical aggregation shape (defined below). Use this when the receiving prompt takes a single text-blob argument — typical of cross-platform prompts (Tiddly, MCP servers, hand-authored prompts not aware of Switchboard's data shape). Same workflow-scope and ordering rules as `responses_from`. Errors if any agent has no completed output yet from this workflow run. |
| `last_output(agent)` | text | Single agent's latest completed output **for the current workflow run** (see "Output scope" below). Errors if the agent has no completed output yet from this workflow run. |
| `agent_names(agents)` | [text] | Maps a list of agent references to their string names. Useful when iterating in a template. |

These functions are callable inside `{{ ... }}` expressions and `{% ... %}` control structures.

Functions accepting `agents` arguments accept either a single agent reference or a list of agents; a single agent is treated as a one-element list.

#### Output scope

`forward_from`, `responses_from`, `aggregated_responses`, and `last_output` see only turns dispatched by the current workflow run and observed reaching terminal state via a `wait_for` / `wait_for_all` (or via a `pause_for_user` with `recipient`'s implicit wait). Out-of-band turns are invisible to these helpers — specifically:

- Manual compose-bar dispatches the user makes to a participating agent between workflow steps.
- Turns from prior workflow runs against the same agent.
- Turns from concurrent workflow runs targeting the same agent.

Rationale: deterministic, predictable behavior. The author writes the workflow as a sequence of dispatches with declared dependencies; the helpers should reflect what the workflow itself orchestrated, not silently couple to whatever the user (or another workflow) did out-of-band. Implementation: the workflow runtime maintains a per-run map of agent → most-recent-completed-turn-this-workflow-saw, updated on `wait_for` / `wait_for_all` success **and on `pause_for_user` Mode-2 implicit-wait completion**; the helpers read from this map. The map stores the agent's **resolved output text**, captured from the turn's live event stream at completion — *not* a turn-id to be re-joined against the harness session file later. (An earlier design stored turn-id references and read bodies from disk on resolve; that join is unreliable — the dispatcher turn id and the harness file's turn ids are different id spaces, and one harness has no per-turn id at all — so the runtime captures the text when the turn completes instead.) This map is **in-memory only** and lives for the duration of a single run; it is **not persisted**. (An earlier design persisted it so a crash-recovered run could re-feed an earlier step's output — but resume/retry is deferred beyond v1, so no agent content is written to disk and the system-design §3 "no agent content" invariant stands unmodified. See "Failure handling" above.)

**Cross-iteration visibility within `for_each`:** Turns from earlier iterations of the same `for_each` body are workflow-run turns and remain visible to helpers in later iterations — only `user_input` is scoped per-iteration. Authors who don't want stale cross-iteration values should explicitly `wait_for` after a fresh `send` at the start of each iteration so the helper sees the new turn rather than the prior iteration's.

#### Canonical aggregation shape (`aggregated_responses`)

`aggregated_responses(agents)` composes the agents' outputs in declared order, each delimited by a `=== START / END ===` sentinel line. The pattern matches `forward_from`'s shape (per §`send`) so users see one canonical aggregation format throughout Switchboard:

```
=== START response from <agent_a_name> ===
<agent_a's latest completed output verbatim>
=== END response from <agent_a_name> ===

=== START response from <agent_b_name> ===
<agent_b's latest completed output verbatim>
=== END response from <agent_b_name> ===
```

A receiving prompt that simply wraps the aggregation in a single text argument (e.g., `builtin:ai-review-feedback`'s `{{ review }}`) gets this canonical shape with no Switchboard-specific authoring required.

**Sentinel collision policy:** there is no escaping of `=== START` / `=== END` in agent output. If an agent's output literally contains a sentinel-shaped line, the receiving agent sees it as part of the forwarded content. This is judged acceptable: collisions are rare in practice (the sentinel pattern is distinctive), agents are good at recovering from minor delimitation noise, and escaping would obscure the structure for the common case. Authors who need strict delimitation can use `responses_from` and a custom template that wraps content explicitly (e.g., XML-style tags).

#### Choosing between `responses_from` and `aggregated_responses`

- **`aggregated_responses`** — default for the common case. Use when the receiving prompt has a single text argument and just wants the aggregated content. Works with any cross-platform prompt that takes a string.
- **`responses_from`** — use when you're authoring a Switchboard-aware prompt and want full control over formatting (custom delimiters, XML tags, per-agent conditional logic, etc.). Returns structured data so the prompt template does the formatting.

## Variable scoping

### Workflow inputs

Bound at invocation time after pre-flight validation. Visible throughout the workflow's lifetime. Cannot be reassigned.

### Iteration variables (`for_each`)

Bound for each iteration's body. The variable is **only visible inside that iteration's `steps`** — not before the `for_each` and not after it concludes. Sibling steps to the `for_each` cannot see the iteration variable.

Each iteration is independent — variables set in iteration N (including `user_input`) are not visible to iteration N+1.

### `user_input`

Bound after a `pause_for_user` step completes. Holds the user's most recent pause-step input as text.

- Outside any `for_each`: visible from the pause step until either (a) workflow ends or (b) another `pause_for_user` reassigns it
- Inside a `for_each`: scoped to that iteration; not visible in subsequent iterations

If `user_input` is referenced before any `pause_for_user` has run in the current scope, that's a render error.

If a v1 workflow needs distinct inputs from multiple pauses, the workflow must consume `user_input` before the next pause runs (e.g., embed it into a step's `text` field). Named pause outputs (`output_var`) are a v2+ extension; not in v1.

### Step-local variables (`template_vars`)

Visible only inside the prompt template being rendered for that one `send` step. Do not leak to other steps.

## Failure handling and workflow status

A workflow does not transition to `complete` until every turn it dispatched has reached terminal state, including turns still in flight after the last step is issued. Authors do not need a trailing `wait_for` on the final dispatch — the runtime holds the workflow open until all turns it initiated settle.

A turn that fails during this trailing settle period marks the workflow `failed`, even if no step explicitly awaited it. Authors relying on fire-and-forget should accept that any participating turn's failure marks the workflow `failed` — silently swallowing trailing errors would be worse.

A workflow run terminates in one of these statuses:

| Status | Meaning |
|---|---|
| `complete` | All steps executed; all turns dispatched by the workflow reached terminal state |
| `cancelled` | User cancelled (via cancel-workflow OR by cancelling an agent's turn while the agent was in a workflow step) |
| `failed` | A step failed (harness error, template render error, pre-dispatch resolution error, contention refusal, fan-in per-agent failure) |
| `interrupted` | Switchboard exited uncontrollably (crash / OS reboot / force-kill) mid-workflow with an in-flight step. Explicit quit cleanly cancels in-flight workflows → `cancelled`, not `interrupted` (per system-design §7 "Walking away"). |

Per system-design §7:
- A pre-dispatch failure (prompt ID not found, MCP server unreachable, agent deleted, contention refusal, template render error) → `failed`
- A harness-level error during a turn (`is_error: true`, `turn.failed`, non-zero exit) → `failed`
- A user manually cancelling an agent's turn that is part of a workflow step → `cancelled`. Applies uniformly — cancelling any one participating agent in a fan-in step also marks the whole workflow `cancelled`.
- A user clicking cancel-workflow → `cancelled`
- Any agent failure within a `wait_for_all` → step `failed`
- Switchboard process death mid-step → `interrupted` (v1: surfaced as an interrupted run the user can **abandon**; no resume — see below)

**v1 does not support resuming or retrying a run.** A `failed`, `interrupted`, or `cancelled` run is terminal: the user **abandons** it (which clears its run record) and, if they still want the work done, re-invokes the workflow from the start. The status values above are *display* states — they describe how a run ended, not resumable states. Resume / retry-from-step, and the snapshot + checkpoint-replay machinery it would require, are **deferred beyond v1** (rationale: a crash leaves participating agents' in-flight turns dead and a half-run transcript, so a correct resume needs both runtime replay and a non-trivial UI to convey what ran; the value is low because crashes are rare, and the cost spans the interpreter and the progress UI). See the v1 plan (M5) for the deferred scope.

### Sibling-failure policy (parallel `send` / fan-in)

When a step dispatches to multiple agents in parallel (a list `send`, or the fan-out feeding a later `wait_for_all`), and one agent fails — pre-dispatch, mid-turn, or at fan-in completion — Switchboard **never cancels the surviving siblings.** (This supersedes the earlier "SIGTERM the survivors" rule.) The resolved policy is phased:

- **M6 — non-destructive floor.** Surviving siblings run to their natural terminal state and their output is retained and visible. The step is marked `failed`. Retry re-runs the whole step (re-dispatching every agent), so the runtime needs no partial-state-reuse semantics. Rationale: a sibling's turn already cost its quota the moment it was dispatched, the failure is usually routine and recoverable (rate limit, transient error, soft refusal), and the survivors' output is frequently independently useful — auto-cancelling discards all of that.
- **M7 — interactive failure-pause.** Once the pause machinery (Primitive 5 `pause_for_user`) exists, a sibling failure with **≥1 surviving sibling** enters a workflow-level pause instead of a bare step failure. The pause surfaces the failed agent + its error, each sibling's status/output, and offers: **(1)** retry the failed agent and continue, **(2)** continue with the surviving agents' output only, **(3)** cancel the workflow (→ `cancelled`). Option (2) feeds the fan-in helpers (`aggregated_responses` / `responses_from`) **only the agents that succeeded** — the failed agent is omitted from the aggregation. The pause is entered once all siblings have settled (no live-updating pause UI in v1).

Boundary cases:
- **No surviving sibling** (every agent in the step failed, or the step targeted a single agent): an ordinary step `failed` — retry-all or abandon. No pause (there is nothing to salvage).
- **User cancels a participating agent's turn** during such a step: the whole workflow is `cancelled` (intent-bearing), per the rule above — not a sibling-failure pause.

Manual cancel remains the user's escape hatch if they want to stop still-running siblings of a doomed step (e.g. coding agents mid-edit); Switchboard does not do this automatically.

**Workflow file snapshot during a live run:** A workflow run executes against an immutable snapshot of the workflow file and its bound inputs, captured at invocation time and held **in memory** for the life of the run, so editing the file on disk mid-run does not change the program the running workflow executes. (Prompt resolution still happens at each step's dispatch per system-design §6 — editing a referenced *prompt* takes effect on the next invocation, not the in-flight run.) Because v1 has no resume, the snapshot does **not** need to be persisted: it exists only to keep a single live run coherent, and a crashed run is abandoned rather than re-executed.

### Retry from inside a `for_each` iteration — deferred beyond v1

Resume/retry is not in v1 (see "Failure handling" above), so the iteration-level retry mechanics — checkpointing the iteration index + variable, restoring the per-run output-scope map and `user_input`, and resuming at the failed step within the iteration — are **deferred**. The design intent is recorded here for the future milestone that adds resume: a checkpoint would capture the iteration index, the iteration variable's value, the per-run output-scope map (agent → resolved text), and the current-scope `user_input`, so that `forward_from` / `last_output` / `aggregated_responses` / `responses_from` resolve correctly in steps after the failed step but before a re-completed dispatch, and so a side-effecting iteration body is not re-run from its start. None of this executes in v1.

## Validation

A workflow file is validated at two times:

### Parse-time (file save / load)

- YAML well-formed
- Top-level keys are exactly `name`, `description`, optionally `inputs`, `steps`
- `name` matches the slug regex and equals the filename
- `inputs` declarations use valid types
- No `default` (and thus no optionality) on a non-`text` input (per §Inputs — `text`-only in v1)
- Each `steps` entry has exactly one step-type key with a known type
- Each step's required fields are present
- All template strings parse as valid templates and stay within the **tag/filter** subset (per §Templating "Enforcement boundary"); expression-level operators are accepted. Referenced variable names need not be declared yet — that's an invocation-time check.
- No nested `for_each`
- No reserved built-in names used as input names
- No `for_each` `item:` name that collides with a workflow input name *or* with the reserved built-in name `user_input` (per §`for_each` — silent shadowing is rejected at the boundary)
- Hardcoded `[agent]` literals in step bodies (YAML literals, not template substitution) are checked at parse time: empty literals (e.g., `to: []`) and literals containing duplicate references (after hyphen→underscore normalization) are rejected.
- A `send.prompt` value containing a template delimiter (`{{` or `{%`) is a parse error: the prompt id must be a literal so its argument schema can be statically resolved (see §"Hardcoded prompts and auto-derived arguments").

The literal `prompt` id (e.g., `prompt: "builtin:code-review"`) is **not** resolved against providers at parse time; provider resolution — and the binding/compatibility checks that depend on it — happen at invocation time. This is intentional: configured prompt providers (and a referenced prompt's argument schema) may change between save and run.

### Invocation-time (when the user invokes the workflow)

- All required inputs are supplied
- All `agent`-typed input values reference agents that exist in the project
- Each hardcoded `send.prompt` id resolves through configured providers, and each of its computed `template_vars` keys is a real argument of the resolved prompt — a non-empty `template_vars \ prompt-args` set (or an unresolvable id) is an invocation-blocking incompatibility naming the offending prompt and argument (see §"Binding classification")
- Every required user-fillable prompt argument (auto-derived, `prompt-args \ template_vars`) is supplied
- All template variable references resolve in the available scope (per "Available template variables") at the time the template is about to render
- Built-in functions (`responses_from`, etc.) get arguments of the right shape
- Any `[agent]` list resolved for use as a step target (`to`), synchronization argument (`agents`), forwarding source (`forward_from`), or helper-function argument (`responses_from`, `aggregated_responses`, `agent_names`) contains unique agents — after hyphen→underscore normalization (per system-design §3 Primitive 1). Duplicate references fail invocation pre-flight; rationale: double-dispatch to a busy agent, ambiguous waits, and mapping-key collisions in fan-in helpers all silently produce wrong results.
- Any `[agent]` list resolved for use as above is non-empty. `[agent]`-typed inputs that resolve to an empty list (e.g., the user supplied no agents in the invocation form's multi-select) fail invocation pre-flight. Rationale: an empty fan-in list silently produces a vacuous "success" with no actual fan-in, which is almost certainly an authoring or invocation error rather than intent. (Empty `[text]` lists used by `for_each` remain valid — see §`for_each`.)

Pre-dispatch failures fail the relevant step per "Failure handling" above.

## Worked examples

### 1. Sequential handoff (planner → implementer)

```yaml
name: plan-then-implement
description: Planner produces a plan; implementer executes it.

inputs:
  planner: agent
  implementer: agent
  goal: text

steps:
  - send:
      to: "{{ planner }}"
      text: "Produce a step-by-step plan to: {{ goal }}"
  - wait_for:
      agent: "{{ planner }}"
  - send:
      to: "{{ implementer }}"
      forward_from: "{{ planner }}"
      text: |
        Execute the plan above. Ask me if you encounter ambiguity
        rather than guessing.
  - wait_for:
      agent: "{{ implementer }}"
```

### 2. Fan-in review (review-and-aggregate, the canonical)

```yaml
name: review-and-aggregate
description: Send to multiple reviewers in parallel, aggregate, send to primary.

inputs:
  primary_agent: agent
  reviewer_agents: [agent]

steps:
  - send:
      to: "{{ reviewer_agents }}"
      prompt: "builtin:code-review"
      # No template_vars: code-review's only argument, `context`, is optional and
      # user-fillable — it is auto-derived and shown as a form field at invocation.
  - wait_for_all:
      agents: "{{ reviewer_agents }}"
  - send:
      to: "{{ primary_agent }}"
      prompt: "builtin:ai-review-feedback"
      template_vars:
        review: "{{ aggregated_responses(reviewer_agents) }}"
```

Both prompts are hardcoded literals, not inputs. The invocation form shows `primary_agent`, `reviewer_agents`, and one auto-derived field — `code-review`'s optional `context` — and nothing else. `ai-review-feedback`'s required `review` argument is a **computed binding** (filled by `aggregated_responses`), so it is hidden from the user; if `ai-review-feedback` ever renamed `review`, the `template_vars: { review: … }` binding would become a `T \ A` invalid binding and invocation would be blocked with an error naming the prompt and argument.

This passes the aggregated reviews as a single text blob to whatever argument the analysis prompt declares — here `review`. A cross-platform prompt that takes a single text argument wraps the blob directly:

```jinja
Here is feedback from AI coding agents:

"""
{{ review }}
"""

Summarize agreement and disagreement.
```

If the workflow author wants full control over formatting (e.g., per-agent XML tags, conditional sections, custom delimiters), they can use `responses_from(reviewer_agents)` instead and iterate over the returned mapping in the prompt template:

```jinja
{% for name, response in responses.items() %}
## {{ name }}
{{ response }}

{% endfor %}
```

This authoring path is for Switchboard-aware prompts only — the iteration won't make sense in any other context.

### 3. Milestone iteration (the per-milestone plan-implement-review loop)

```yaml
name: implement-milestones
description: For each milestone, plan, get user approval, implement, review, and pause for revision.

inputs:
  coder: agent
  reviewer: agent
  milestones: [text]

steps:
  - for_each:
      item: milestone
      in: "{{ milestones }}"
      steps:
        - send:
            to: "{{ coder }}"
            text: "Plan milestone: {{ milestone }}. Output the plan only; don't implement yet."
        - wait_for:
            agent: "{{ coder }}"
        - pause_for_user:
            context: "Plan for {{ milestone }} ready. Approve, redirect, or add context."
            recipient: "{{ coder }}"
        # No wait_for here — pause_for_user with `recipient` implicitly waits.
        - send:
            to: "{{ reviewer }}"
            forward_from: "{{ coder }}"
            text: "Review the implementation above for the milestone: {{ milestone }}"
        - wait_for:
            agent: "{{ reviewer }}"
        - send:
            to: "{{ coder }}"
            forward_from: "{{ reviewer }}"
            text: "Address the review feedback above for: {{ milestone }}"
        - wait_for:
            agent: "{{ coder }}"
        - pause_for_user:
            context: "Milestone {{ milestone }} done. Commit and continue, or revise?"
            recipient: "{{ coder }}"
```

(In v1 the user explicitly types "commit" or revision instructions in the final pause. Conditional / loop-until-approved is deferred to v2.)

## Forward-compat reservations

The following top-level workflow-file keys and step-type keys are **reserved** in v1 and must not be used. They are earmarked for v2+ features so that v1 workflow files remain forward-compatible without a schema-breaking migration:

| Reserved key | Earmarked for |
|---|---|
| `if:` (top-level step-type) | Conditional steps (`if reviewer flagged a bug, halt`) |
| `branch:` (top-level step-type) | Branching workflows |
| `wait_for_first:` (step-type) | Race semantics (first-of-N completes wins) |
| `until:` (top-level workflow key) | Iterate-until-condition workflows |
| `output_var:` (field on `pause_for_user`) | Named pause outputs (multiple pauses with distinct variables) |
| `outputs:` (top-level workflow key) | Workflow output declarations (return values) |
| `metadata:` (top-level workflow key) | Workflow metadata for v2 catalog/library views |

Using any reserved key in a v1 workflow file is a parse-time validation error.

## Out-of-scope decisions deferred to M6 expansion

The following are *implementation* details, not language-spec details, and live in M6's per-milestone expansion (see `docs/implementation_plans/2026-05-12-v1.md`):

- On-disk encoding of workflow runs (`<directory>/.switchboard/projects/<project-id>/runs/<run-id>.jsonl` schema)
- Concurrency model for parallel `send` dispatches within a `wait_for_all`
- Tauri command shapes for invoking workflows from the Svelte frontend
- Workflow-progress event payload format for the frontend ring buffer
- Built-in workflow files shipped with v1 (`review-and-aggregate.yaml`, etc.) — content TBD
