# Authoring a workflow for Switchboard

> **Audience:** an AI coding agent (Claude Code or Codex) being asked to generate a starter workflow file for a Switchboard working directory. If you are a human author, this doc works for you too — you are just not the primary audience.
>
> **Companion docs:** the formal DSL spec is at `docs/workflow-spec.md`. Read this doc first for the authoring path; consult the spec for edge cases, validation rules, and the full reserved-keys list.

## What a workflow is

A **workflow** is a YAML file that defines a reusable, parameterized sequence of agent operations — for example "fan out to three reviewers, aggregate their feedback, send to the implementer." Workflows are how the user automates multi-agent coordination they would otherwise do by hand.

Workflows are file-based. There is no in-app editor. You are generating a file the user will save into the working directory's workflows directory (`<directory>/.switchboard/workflows/` — shared across all projects in that working directory; see `docs/system-design.md` §3).

## Where workflows live

Workflows are directory-scoped (shared across all projects in the same working directory):

- `<directory>/.switchboard/workflows/<workflow-name>.yaml`

There is no user-global workflow directory. To share a workflow across working directories (different repos), the user copies or symlinks the file. Within a working directory, all projects can invoke the same workflow definitions — workflows describe *how to do work*; projects scope *the work in progress*. See `docs/system-design.md` §3 for the directory/project model.

## Top-level structure

Every workflow file is a YAML mapping with these top-level keys:

```yaml
name: review-and-aggregate
description: Send to multiple reviewers, aggregate, send to primary.

inputs:
  primary_agent: agent
  reviewer_agents: [agent]
  review_prompt: prompt_id
  aggregation_prompt: prompt_id

steps:
  - send: { ... }
  - wait_for_all: { ... }
  - send: { ... }
```

| Key | Required | Notes |
|---|---|---|
| `name` | yes | Slug. Lowercase, hyphens allowed. Must equal the filename without extension. |
| `description` | yes | One-line human description. Surfaced in invocation UI. |
| `inputs` | optional | Declared parameters the user supplies at invocation time. Omit if the workflow takes no inputs. |
| `steps` | yes | Ordered list of steps. Must be non-empty. |

Top-level keys not in this table are an error. (Reserved keys for v2+ are listed in the spec under "Forward-compat reservations" — do not use them.)

## Inputs — parameter slots, bound at invocation time

Inputs are **parameter slots**, not hardcoded values. The workflow file declares the *shape*; the user supplies actual values via a UI form when they invoke the workflow.

### Input types

| Type | Shorthand | UI form field | Notes |
|---|---|---|---|
| Single agent | `agent` | Dropdown of agents in the project | The user picks one of their existing agents. |
| List of agents | `[agent]` | Multi-select of agents | For fan-out / fan-in steps. |
| Prompt ID | `prompt_id` | Autocomplete from configured prompt providers | E.g., `local:code-review` or `tiddly:foo`. |
| Free-form text | `text` / `text?` | Plain text field | `text?` is optional (user can leave blank). |
| List of text | `[text]` | Repeatable text field | Used by `for_each` for iteration lists (e.g., a milestone list). |

### Shorthand vs long form

Most inputs use the shorthand:

```yaml
inputs:
  primary_agent: agent
  reviewer_agents: [agent]
  review_prompt: prompt_id
  user_context: text?
```

Long form lets you add a description and a default. Use it when the description meaningfully improves the invocation form:

```yaml
inputs:
  user_context:
    type: text?
    description: Optional context the reviewers should focus on.
    default: ""
```

Long-form keys: `type` (required), `description` (optional), `default` (optional — providing a default implicitly makes the input optional; the `?` suffix is shorthand for "optional with default `''`").

### Validation rules (summary)

- Input names: lowercase with underscores, e.g., `reviewer_agents`.
- Reserved built-in name: `user_input` cannot be used as an input name.
- Required inputs (no `?` suffix and no `default`) must be supplied at invocation; missing values fail invocation pre-flight.
- `[agent]` lists used as step targets, sync arguments, forwarding sources, or helper-function arguments must contain unique agents — after hyphen→underscore normalization. Duplicates fail invocation pre-flight (e.g., `[reviewer-a, reviewer_a]` collides; `[reviewer-a, reviewer-a]` is a literal duplicate).
- `[agent]` lists used as above must also be non-empty; empty lists fail invocation pre-flight (an empty fan-in is almost certainly an authoring error rather than intent).
- A `for_each` `item:` name that collides with a workflow input name or with `user_input` is a parse-time error.

## Steps

`steps` is a YAML sequence. Each entry is a mapping with **exactly one** top-level key naming the step type. The five v1 step types are:

- `send` — dispatch a message to one or more agents
- `wait_for` — block until one named agent's in-flight turn completes
- `wait_for_all` — block until all named agents complete (used as the wait phase of fan-in)
- `pause_for_user` — suspend the workflow and wait for the user to type a response
- `for_each` — repeat a sub-sequence of steps once per item in a list

### `send` — dispatch a message

```yaml
- send:
    to: "{{ primary_agent }}"
    text: "Plan the next milestone."
```

| Field | Required | Notes |
|---|---|---|
| `to` | yes | One agent or a list of agents. Templated. |
| `prompt` | yes (or `text` or `forward_from`) | A prompt ID like `"{{ review_prompt }}"`. The prompt is resolved and rendered with workflow scope + `template_vars`. |
| `text` | yes (or `prompt` or `forward_from`) | Literal text to send. Templated. Mutually exclusive with `prompt`. |
| `template_vars` | optional | Variables passed to the prompt template at render time. |
| `forward_from` | optional | One agent or a list. The latest output(s) of those agents are appended to the message body in a canonical shape (see "Forwarding" below). |

`send` is **fire-and-forget**: it dispatches and returns immediately. To wait for the recipient(s) to finish, follow with `wait_for` or `wait_for_all`. (The exception is `pause_for_user` with `recipient` set, which bundles dispatch and wait — see below.)

**When `to` is a list of agents**: dispatches are issued in declared order; agents run in parallel; the step returns once all dispatches have been issued (not when any has completed). Always follow with `wait_for_all` to synchronize before consuming their outputs. If any dispatch in the list fails (e.g., one agent is busy), the remaining dispatches are not attempted and the step is marked `failed`, but dispatches already issued in the same step are **not** cancelled — they run to their natural terminal state and their output stays visible. Retry re-runs the whole step, re-issuing every dispatch.

### `wait_for` and `wait_for_all` — synchronization

```yaml
- wait_for:
    agent: "{{ planner }}"

- wait_for_all:
    agents: "{{ reviewer_agents }}"
```

`wait_for_all` is the wait phase of fan-in. After it succeeds, you can use `responses_from(agents)` or `aggregated_responses(agents)` (see "Built-in template functions") in the next `send` step to reference the agents' completed outputs.

### `pause_for_user` — wait for the human

```yaml
- pause_for_user:
    context: "Reviews are in. What direction do you want to take?"
    recipient: "{{ primary_agent }}"
```

Suspends the workflow, fires an OS notification, and surfaces the compose bar to the user. The user's typed text becomes available as `user_input` in subsequent steps.

| Field | Required | Notes |
|---|---|---|
| `context` | optional | Templated message shown to the user explaining what they're being asked. |
| `recipient` | optional | If set, the user's input is also dispatched to this agent and the step blocks until the agent's turn completes (Mode 2 — see below). |
| `required` | optional | Default `true`. If `true` and the user skips, the workflow is `cancelled`. If `false` and the user skips, `user_input` is bound to the empty string and the step proceeds — in Mode 2, no dispatch happens on skip. |

**Two modes**, picked by whether `recipient` is set:

- **Mode 1 (no `recipient`) — capture only.** Workflow suspends, user submits or skips, `user_input` is bound, next step runs immediately. No dispatch happens. Use this when you want the user's input as data for subsequent steps but don't want to send it to an agent yet.
- **Mode 2 (with `recipient`) — capture, dispatch, and implicitly wait.** Workflow suspends, user submits, the input is dispatched to `recipient`, and the step blocks until the recipient's turn reaches terminal state. Use this when you want the user to drive an agent directly. This is the only step type that bundles dispatch with an implicit wait — the rationale is ergonomic (the user just answered a question; the natural expectation is to see the agent's response before the workflow moves on).

If you want fire-and-forget after a pause, drop `recipient` (use Mode 1) and write a separate `send` step that uses `{{ user_input }}`.

### `for_each` — iterate over a list

```yaml
- for_each:
    item: milestone
    in: "{{ milestones }}"
    steps:
      - send: { ... }
      - wait_for: { ... }
```

| Field | Required | Notes |
|---|---|---|
| `item` | yes | The iteration variable name. Lowercase with underscores. |
| `in` | yes | The list to iterate over. Must resolve to a list. |
| `steps` | yes | Sub-steps to execute per iteration. Same structure as the top-level `steps`. |

The iteration variable is bound for each iteration's body and accessible as `{{ milestone }}` (or whatever `item` is named). Iterations are **sequential** — they do not run in parallel. Iterating over an empty list is a no-op.

**Constraints in v1**: nested `for_each` is not allowed; iterations cannot share state with each other (`user_input` from iteration N is not visible in iteration N+1); the list is supplied at invocation time, not computed from a prior step's output. The `item:` name must not collide with any workflow input name or with the reserved built-in name `user_input` — collisions are a parse-time error.

## Templating

All string values are rendered through MiniJinja before use. This is the same engine used for local prompts (see `docs/agent-instructions/prompts.md`).

### Available template variables

Variables are resolved innermost first:

1. **Step-local** `template_vars` (visible only inside that one `send` step's prompt render)
2. **Iteration scope** — the iteration variable inside a `for_each` body
3. **Pause scope** — `user_input` after a `pause_for_user`
4. **Workflow inputs** — names declared in the top-level `inputs`

### Built-in template functions

| Function | Returns | Use when |
|---|---|---|
| `aggregated_responses(agents)` | text (canonical shape) | The receiving prompt takes a single text-blob argument. **Default for cross-platform prompts** (Tiddly, MCP servers, hand-authored prompts not aware of Switchboard's data shape). |
| `responses_from(agents)` | mapping name → text | You're authoring a Switchboard-aware prompt and want full control over formatting (per-agent XML tags, conditional sections, custom delimiters). The prompt iterates over the mapping. |
| `last_output(agent)` | text | Single agent's latest completed output. |
| `agent_names(agents)` | [text] | List of agent name strings — useful when iterating in a template. |

**Picking between `aggregated_responses` and `responses_from`:**

- If the user already has an aggregation prompt (e.g., a Tiddly prompt that wraps `{{ feedback }}` in some framing): use `aggregated_responses` and bind it to the prompt's argument name.
- If you (or the user) is authoring a fresh aggregation prompt that wants per-agent formatting: use `responses_from` and iterate in the prompt's template.

**Output scope (important).** These helpers — and `forward_from` on `send` steps — see only turns that the **current workflow run** dispatched and observed reach terminal state via `wait_for`, `wait_for_all`, or a Mode-2 `pause_for_user` implicit wait. They do *not* see manual compose-bar dispatches the user made between workflow steps, turns from prior workflow runs, or turns from concurrent workflow runs against the same agent. If you call `last_output(agent)` and the workflow itself never dispatched to that agent, you get a runtime error — even if the agent has perfectly good output from elsewhere. Always pair a `send` with a `wait_for` (or use Mode-2 pause) before calling helpers on that agent. (Cross-iteration note: turns from earlier iterations of the same `for_each` *are* visible to helpers in later iterations — only `user_input` is per-iteration.)

## Forwarding

`forward_from` on a `send` step appends each forwarded agent's latest completed output to the message body, separated by sentinel lines:

```
<rendered text or prompt body>

=== START forwarded from <agent_name> ===
<agent's latest completed output>
=== END forwarded from <agent_name> ===
```

If only `forward_from` is set (no `text`, no `prompt`), the body is the forwarded composition alone with no leading content.

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

### 2. Fan-in review

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

### 3. Per-milestone iteration with user approval

```yaml
name: implement-milestones
description: For each milestone, plan, get approval, implement, review, pause.

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
            text: "Plan milestone: {{ milestone }}. Output the plan only."
        - wait_for:
            agent: "{{ coder }}"
        - pause_for_user:
            context: "Plan for {{ milestone }} ready. Approve, redirect, or add context."
            recipient: "{{ coder }}"
        # No wait_for here — pause_for_user with `recipient` (Mode 2) implicitly waits.
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

## Conventions

- **Filename = name field**. `review-and-aggregate.yaml` has `name: review-and-aggregate`.
- **Slug-style names**. Lowercase, hyphens, descriptive. Verb-first if the workflow is action-shaped (`plan-then-implement`, `review-and-aggregate`).
- **Prefer agent-typed inputs over hardcoded names**. Workflows are reusable; hardcoding `reviewer-claude` makes the workflow unusable for someone whose agents are named differently.
- **Explicit waits**. Even though `pause_for_user` with `recipient` waits implicitly, every other `send` is fire-and-forget. Pair `send` steps with `wait_for` / `wait_for_all` deliberately.
- **One workflow per task**. If a workflow is starting to feel like it's doing two things, split it.

## Failure handling

- A step failure (harness error, template render error, contention refusal, missing prompt) halts the workflow with status `failed`.
- The user cancelling the workflow (or any participating agent's turn during a workflow) marks the workflow `cancelled`.
- A crash, OS reboot, or force-kill mid-workflow marks it `interrupted` — the user can retry from the failed step or abandon.

**Retry semantics inside `for_each`**: when a workflow is retried after a failure inside an iteration, the runtime restores the iteration variable and the per-run output state from the checkpoint and resumes at the *failed step within that iteration*. Earlier steps in the same iteration are **not** re-executed. If you write iteration bodies with side-effecting steps (commits, file writes), keep this in mind: on retry of step N within iteration K, steps 1..N-1 of iteration K do not run again — design so the failing step is the side-effecting one (so its effects are not double-applied) or so earlier-step effects are idempotent.

You don't need to write failure-handling logic in the workflow file; the runtime handles it. Just write the happy path.

## After authoring

1. Save the file as `<directory>/.switchboard/workflows/<name>.yaml` (filename matches `name`). Workflows are directory-scoped — shared across all projects in that working directory.
2. The user invokes it from Switchboard's workflow picker. The invocation form auto-generates from the `inputs` declaration.
3. The workflow runs autonomously; the user watches via the workflow-progress surface and per-agent panes.

## When to point at the formal spec

This doc covers the common authoring path. For edge cases, full validation rules, the MiniJinja subset details, the v2+-reserved-keys list, and the failure-status taxonomy, see `docs/workflow-spec.md`.
