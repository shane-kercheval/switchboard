# Switchboard workflow DSL spec (v1)

## Purpose

This document defines the workflow file format Switchboard parses and executes. It is the formal version of the illustrative YAML in [`system-design.md`](system-design.md) §5 and resolves open question 5.1.

This spec is **implementation-grade**: it pins down field names, types, scoping rules, error conditions, and built-in template functions concretely enough that a parser and runtime can be implemented from it.

It is **not** a user tutorial. Tutorial-style authoring guidance lives in `docs/agent-instructions/workflows.md` (separate artifact, written for AI coding agents to consume per system-design §2 "Agent-friendly authoring").

## Scope

- File structure: top-level keys, inputs declaration, steps array
- Step types: one per primitive (system-design §4 Primitives 1–6) plus the `wait_for_all` helper
- Templating: MiniJinja subset, available variables, built-in template functions
- Variable scoping rules
- Error and validation conventions
- Three worked examples
- Reserved keys for v2+ (forward-compat)

What is **out of scope** for this doc:

- Parser internals (recursive descent vs YAML library choice — M5 implementation)
- Persistence-layer encoding of workflow runs (`state/runs/<run-id>.jsonl` schema — M5 expansion)
- UI rendering of workflow progress (system-design §7)

## File location and naming

- Workflow files live under `<project>/.switchboard/workflows/`
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
| Prompt ID | `prompt_id` | A fully-qualified prompt reference (e.g., `local:code-review`, `tiddly:foo`) |
| Text | `text` | Free-form string |
| Text (optional) | `text?` | Free-form string; user may leave blank |
| List of text | `[text]` | List of strings (used by Primitive 6 iteration) |

In v1 the only optional type variant is `text?`. `agent?` and `prompt_id?` are deferred to v2+.

### Shorthand form

```yaml
inputs:
  primary_agent: agent
  reviewer_agents: [agent]
  review_prompt: prompt_id
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
| `default` | optional | Default value if the user leaves the field blank. Providing `default` implicitly makes the input optional; the `?` suffix on a type is shorthand for an optional input with a default of `""`. |

Long-form `type` is required when long form is used; mixing shorthand and long form across different inputs is allowed.

### Validation rules

- Input names must match `[a-z][a-z0-9_]*`
- An input named with a reserved built-in name (`user_input`) is an error
- Required inputs (no `?` suffix, no `default`) must be supplied at invocation; missing values fail invocation pre-flight
- Agent-typed inputs are validated at invocation time — referenced agents must exist in the project
- `prompt_id`-typed inputs are validated at invocation time — the prompt must resolve through configured providers

## Steps

`steps` is a YAML sequence. Each entry is a mapping with exactly one top-level key naming the step type. The value of that key is the step's parameters.

```yaml
steps:
  - send:
      to: "{{ reviewer_agents }}"
      prompt: "{{ review_prompt }}"
  - wait_for_all:
      agents: "{{ reviewer_agents }}"
  - send:
      to: "{{ primary_agent }}"
      prompt: "{{ aggregation_prompt }}"
      template_vars:
        responses: "{{ responses_from(reviewer_agents) }}"
```

Each step type is documented below. Unknown step types are a validation error.

### `send` (Primitive 2)

Dispatch a message to one or more agents. Returns immediately; does not wait for the agents to complete (see `wait_for` / `wait_for_all` for synchronization).

| Field | Required | Type | Notes |
|---|---|---|---|
| `to` | yes | agent or [agent] | Recipient(s). Single agent or a list. Templated. |
| `prompt` | yes (or `text`) | prompt_id | Prompt to resolve and render. Templated. |
| `text` | yes (or `prompt`) | text | Literal text to send (no prompt resolution). Templated. Mutually exclusive with `prompt`. |
| `template_vars` | optional | mapping | Variables passed to the prompt template at render time. Mapping of name → templated value. |
| `forward_from` | optional | agent or [agent] | Auto-forward source(s). When set, the latest output(s) of the named agent(s) are composed into the message body per the canonical shape below. Equivalent to Primitive 3 (auto-forward). |

If `prompt` is set, the prompt's template is rendered with workflow-scope variables plus any `template_vars` and dispatched. If `text` is set, the literal text is dispatched (after templating). At least one of `prompt`, `text`, or `forward_from` is required.

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

Suspend workflow execution and wait for the user to dispatch a response from the compose bar. Fires an OS-native notification when entered.

| Field | Required | Type | Notes |
|---|---|---|---|
| `context` | optional | text | Templated message shown to the user (e.g., "Reviews are in. What direction do you want to take?"). |
| `recipient` | optional | agent | If set, the compose bar is pre-targeted at this agent — the user just types their response, and the input is dispatched to this recipient as part of the same step. Templated. |
| `required` | optional | bool | Default `true`. If `false`, the user may "skip" without supplying input (the workflow proceeds with `user_input` set to empty string). If `true`, "skip" cancels the workflow. |

After the user dispatches, the variable `user_input` (built-in, scoped per "Variable scoping" below) holds the user's typed text. If the user combined a prior agent's output or applied a wrapping prompt in the compose bar, those affect what is dispatched to `recipient` — they do not change `user_input`. This keeps `user_input` predictable for subsequent template references.

**Implicit wait when `recipient` is set:** `pause_for_user` with `recipient` resolves only when the recipient's turn (dispatched from the user's input) reaches terminal state. The pause + dispatch + wait are bundled into one step. This is the only step in the spec that bundles wait with dispatch — the rationale is ergonomic: the user has just answered a question and the natural expectation is to see what the agent says before the workflow proceeds, and pause-with-recipient targets exactly one agent (no fan-out parallelism to preserve). Authors who want true fire-and-forget after a pause should omit `recipient` and write a separate `send` step using `user_input`.

### `for_each` (Primitive 6)

Repeat a sub-sequence of steps once for each item in a list.

| Field | Required | Type | Notes |
|---|---|---|---|
| `item` | yes | string | The iteration variable name. Must match `[a-z][a-z0-9_]*`. |
| `in` | yes | [text] or [agent] | The list to iterate over. Templated. Must resolve to a list. |
| `steps` | yes | sequence | Sub-steps to execute per iteration. Same structure as the top-level `steps`. |

The iteration variable is bound for each iteration's body and accessible in template substitution (e.g., `{{ milestone }}`). Iterations are sequential, not parallel. Iterating over an empty list is a no-op (the body executes zero times); not an error. A failure inside iteration N halts the whole workflow per system-design §4 (no per-iteration error handling in v1). Nested `for_each` is an error in v1.

## Templating

All string values in a workflow file are passed through the templating engine before use. The engine is a **MiniJinja subset**.

### Supported MiniJinja features

- Variable substitution: `{{ var }}`
- Member access: `{{ obj.field }}`, `{{ list[0] }}`
- For loops: `{% for x in list %}...{% endfor %}` (including `loop.index`, `loop.first`, `loop.last`)
- If conditions: `{% if expr %}...{% elif %}...{% else %}...{% endif %}` (limited to truthiness checks and equality; arithmetic deferred to v2)
- Whitespace control: `{%-`, `-%}`, `{{-`, `-}}`
- Comments: `{# ... #}`
- Built-in filters: `length`, `lower`, `upper`, `default`, `join`, `trim`

### Unsupported in v1 (use produces a render error)

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
| `responses_from(agents)` | mapping name → text | Maps each agent's name (with hyphens normalized to underscores) to that agent's latest completed turn output. Errors if any agent has no completed output yet. Agent-name uniqueness after hyphen→underscore normalization is enforced at agent-creation time (per system-design §3 Primitive 1), so collisions cannot occur here. Use this when authoring a Switchboard-aware aggregation prompt that wants to iterate over responses with custom formatting. |
| `aggregated_responses(agents)` | text | Returns the same data as `responses_from(agents)` pre-formatted into a single string in the canonical aggregation shape (defined below). Use this when the receiving prompt takes a single text-blob argument — typical of cross-platform prompts (Tiddly, MCP servers, hand-authored prompts not aware of Switchboard's data shape). Errors if any agent has no completed output yet. |
| `last_output(agent)` | text | Single agent's latest completed output. Errors if the agent has no completed output yet. |
| `agent_names(agents)` | [text] | Maps a list of agent references to their string names. Useful when iterating in a template. |

These functions are callable inside `{{ ... }}` expressions and `{% ... %}` control structures.

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

A receiving prompt that simply wraps the aggregation as `{{ feedback }}` (e.g., `tiddly:ai-review-feedback`) gets this canonical shape with no Switchboard-specific authoring required.

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

A workflow run terminates in one of these statuses:

| Status | Meaning |
|---|---|
| `complete` | All steps executed; all turns reached terminal state |
| `cancelled` | User cancelled (via cancel-workflow OR by cancelling an agent's turn while the agent was in a workflow step) |
| `failed` | A step failed (harness error, template render error, pre-dispatch resolution error, contention refusal, fan-in per-agent failure) |
| `interrupted` | Switchboard exited uncontrollably (crash / OS reboot / force-kill) mid-workflow with an in-flight step. Explicit quit cleanly cancels in-flight workflows → `cancelled`, not `interrupted` (per system-design §7 "Walking away"). |

Per system-design §7:
- A pre-dispatch failure (prompt ID not found, MCP server unreachable, agent deleted, contention refusal, template render error) → `failed`
- A harness-level error during a turn (`is_error: true`, `turn.failed`, non-zero exit) → `failed`
- A user manually cancelling an agent's turn that is part of a workflow step → `cancelled`. Applies uniformly — cancelling any one participating agent in a fan-in step also marks the whole workflow `cancelled`.
- A user clicking cancel-workflow → `cancelled`
- Any agent failure within a `wait_for_all` → step `failed`
- Switchboard process death mid-step → `interrupted` (recovery: surface "interrupted at step N" with retry/abandon options)

A `failed` or `interrupted` workflow can be retried from the failed step or abandoned. A `cancelled` workflow cannot be auto-resumed (user must re-invoke from the start).

## Validation

A workflow file is validated at two times:

### Parse-time (file save / load)

- YAML well-formed
- Top-level keys are exactly `name`, `description`, optionally `inputs`, `steps`
- `name` matches the slug regex and equals the filename
- `inputs` declarations use valid types
- Each `steps` entry has exactly one step-type key with a known type
- Each step's required fields are present
- All template strings parse as valid templates (referenced variable names need not be declared yet — that's an invocation-time check)
- No nested `for_each`
- No reserved built-in names used as input names

Hardcoded `prompt_id` literals (e.g., `prompt: "local:code-review"` with no template substitution) are **not** resolved against providers at parse time; provider resolution happens at invocation time. This is intentional — configured prompt providers may change between save and run.

### Invocation-time (when the user invokes the workflow)

- All required inputs are supplied
- All `agent`-typed input values reference agents that exist in the project
- All `prompt_id`-typed input values resolve through configured providers
- All template variable references resolve in the available scope (per "Available template variables") at the time the template is about to render
- Built-in functions (`responses_from`, etc.) get arguments of the right shape

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
  review_prompt: prompt_id
  aggregation_prompt: prompt_id
  user_context: text?

steps:
  - send:
      to: "{{ reviewer_agents }}"
      prompt: "{{ review_prompt }}"
      template_vars:
        context: "{{ user_context }}"
  - wait_for_all:
      agents: "{{ reviewer_agents }}"
  - send:
      to: "{{ primary_agent }}"
      prompt: "{{ aggregation_prompt }}"
      template_vars:
        feedback: "{{ aggregated_responses(reviewer_agents) }}"
```

This passes the aggregated reviews as a single text blob to whatever variable the aggregation prompt declares (commonly `feedback`). A typical cross-platform aggregation prompt — including ones authored outside Switchboard, such as `tiddly:ai-review-feedback` — wraps the blob directly:

```jinja
Here is feedback from AI coding agents:

"""
{{ feedback }}
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
        - wait_for:
            agent: "{{ coder }}"
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

## Out-of-scope decisions deferred to M5 expansion

The following are *implementation* details, not language-spec details, and live in M5's per-milestone expansion (see `docs/v1-plan.md`):

- On-disk encoding of workflow runs (`<project>/.switchboard/state/runs/<run-id>.jsonl` schema)
- Concurrency model for parallel `send` dispatches within a `wait_for_all`
- Tauri command shapes for invoking workflows from the Svelte frontend
- Workflow-progress event payload format for the frontend ring buffer
- Built-in workflow files shipped with v1 (`review-and-aggregate.yaml`, etc.) — content TBD
