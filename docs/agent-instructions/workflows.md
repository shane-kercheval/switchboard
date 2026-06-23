# Authoring a workflow for Switchboard

> **Audience:** an AI coding agent (Claude Code or Codex) being asked to generate a starter workflow file for a Switchboard working directory. If you are a human author, this doc works for you too — you are just not the primary audience.
>
> **Companion docs:** the formal DSL spec is at `docs/workflow-spec.md`. Read this doc first for the authoring path; consult the spec for edge cases, validation rules, and the full reserved-keys list.

## What a workflow is

A **workflow** is a YAML file that defines a reusable, parameterized sequence of agent operations — for example "fan out to three reviewers, aggregate their feedback, send to the implementer." Workflows are how the user automates multi-agent coordination they would otherwise do by hand.

Workflows are file-based. There is no in-app editor. You are generating a file the user will save into their **user-global** workflows folder (the OS config dir's `workflows/` — e.g. `~/Library/Application Support/switchboard/workflows/` on macOS; see `docs/system-design.md` §3). Settings → Workflows shows the exact path with an "Open" button.

## Where workflows live

Workflows are **user-global** — one folder, shared across every project in every working directory:

- `<config-dir>/workflows/<workflow-name>.yaml`

A workflow authored once is invocable from every project without redefinition — its `agent`/`[agent]` inputs are bound to whichever project's agents it's run against, so the definition isn't tied to any one repo. Workflows describe *how to do work* (a reusable, portable template, like a prompt); projects scope *the work in progress*. See `docs/system-design.md` §3 for the model.

## Top-level structure

Every workflow file is a YAML mapping with these top-level keys:

```yaml
name: review-and-aggregate
description: Send to multiple reviewers, aggregate, send to primary.

inputs:
  primary_agent: agent
  reviewer_agents: [agent]

steps:
  - label: Send the review to reviewers
    send: { ... }
  - label: Wait for all reviews
    wait_for_all: { ... }
  - label: Aggregate feedback for the primary
    send: { ... }
```

Every step carries a required `label` — a short, human-readable name for the step
(see "Steps" below). It is a reserved sibling key of the step-type key.

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
| Free-form text | `text` / `text?` | Plain text field | `text?` is optional (user can leave blank). |
| List of text | `[text]` | Repeatable text field | Used by `for_each` for iteration lists (e.g., a milestone list). |

There is **no `prompt_id` input type**. A step's prompt is a hardcoded literal, not something the user picks — see "How a `send` step produces its message" below. A prompt's own user-fillable arguments appear as form fields automatically, without being declared here.

### Shorthand vs long form

Most inputs use the shorthand:

```yaml
inputs:
  primary_agent: agent
  reviewer_agents: [agent]
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

`steps` is a YAML sequence. Each entry is a mapping with a required **`label`** plus **exactly one** key naming the step type. The five v1 step types are:

- `send` — dispatch a message to one or more agents
- `wait_for` — block until one named agent's in-flight turn completes
- `wait_for_all` — block until all named agents complete (used as the wait phase of fan-in)
- `pause_for_user` — suspend the workflow and wait for the user to type a response
- `for_each` — repeat a sub-sequence of steps once per item in a list

**Every step requires a `label`** — a short, human-readable name (e.g. `Send the review to reviewers`) shown in the workflow's progress and preview views. It is a reserved sibling key of the step-type key and applies to *every* step type, including those inside a `for_each` body. A missing, blank, or non-string `label` is a parse error.

### `send` — dispatch a message

```yaml
- label: Ask the primary to plan
  send:
    to: "{{ primary_agent }}"
    text: "Plan the next milestone."
```

| Field | Required | Notes |
|---|---|---|
| `to` | yes | One agent or a list of agents. Templated. |
| `prompt` | yes (or `text` or `forward_from`) | A **hardcoded prompt id literal** like `"builtin:code-review"`. Not templated. The prompt is resolved; its user-fillable arguments are auto-derived and its `template_vars` (computed bindings) are wired in. See "How a `send` step produces its message" below. |
| `text` | yes (or `prompt` or `forward_from`) | Literal text to send. Templated (`{{ }}` resolved from workflow scope). Mutually exclusive with `prompt`. |
| `template_vars` | optional | **Computed bindings only** — a workflow expression wired to a prompt argument (e.g., `review: "{{ aggregated_responses(reviewers) }}"`). Each key must be a real argument of the hardcoded prompt. User-fillable arguments do **not** go here; they are auto-derived. |
| `forward_from` | optional | One agent or a list. The latest output(s) of those agents are appended to the message body in a canonical shape (see "Forwarding" below). |

`send` is **fire-and-forget**: it dispatches and returns immediately. To wait for the recipient(s) to finish, follow with `wait_for` or `wait_for_all`. (The exception is `pause_for_user` with `recipient` set, which bundles dispatch and wait — see below.)

#### How a `send` step produces its message

A `send` step builds its message body one of **two ways** — pick one per step:

**(a) Named prompt — `prompt: "builtin:…"`.** The step runs a **fixed, hardcoded prompt** named by literal id (`provider:name`, e.g. `builtin:code-review`, `tiddly:ai-review-feedback`). The id is *not* templated — you cannot write `prompt: "{{ x }}"` (that's a parse error), and there is no `prompt_id` input. The prompt's arguments are filled from two sources:

- **Auto-derived user-fillable arguments.** Every argument the prompt declares that you do *not* wire in `template_vars` becomes a **form field automatically**, with the prompt's own label/description, required iff the prompt marks it required. You write nothing for these — `builtin:code-review`'s optional `context`, for example, just appears as a fillable field at invocation. Do **not** declare them in `inputs`.
- **Computed bindings — `template_vars`.** For an argument you want filled by a workflow expression the user can't type (a fan-in handoff like `aggregated_responses(reviewers)`), wire it in `template_vars`. These are hidden from the user. Every `template_vars` key must be a real argument of the prompt; if it isn't (e.g. the prompt renamed the argument), invocation is **blocked** with an error naming the prompt and argument.

**(b) Inline text — `text: "…"`.** The step sends a **literal body** you write directly. Its `{{ }}` variables resolve from workflow scope (inputs, the iteration variable, `user_input`, and the built-in helper functions). To let the user fill a value used inside the text, **declare it in `inputs`** — that's what surfaces it as a form field. `template_vars` does **not** apply to `text` (it only feeds a `prompt`).

Worked **inline-text-with-arguments** example — a declared input feeds a `{{ }}` slot in the body:

```yaml
inputs:
  implementer: agent
  focus: text?          # declaring it here is what makes it a fillable form field
steps:
  - label: Send the milestone to the implementer
    send:
      to: "{{ implementer }}"
      text: |
        Implement the next milestone.
        {% if focus %}Focus especially on: {{ focus }}{% endif %}
```

The contrast: in the **named-prompt** path the user-fillable fields come from the *prompt's* schema (you declare nothing); in the **inline-text** path you make a value fillable by **declaring it in `inputs`** and referencing it with `{{ }}`.

**When `to` is a list of agents**: dispatches are issued in declared order; agents run in parallel; the step returns once all dispatches have been issued (not when any has completed). Always follow with `wait_for_all` to synchronize before consuming their outputs. If any dispatch in the list fails (e.g., one agent is busy), the remaining dispatches are not attempted and the step is marked `failed`, but dispatches already issued in the same step are **not** cancelled — they run to their natural terminal state and their output stays visible. Retry re-runs the whole step, re-issuing every dispatch.

### `wait_for` and `wait_for_all` — synchronization

```yaml
- label: Wait for the planner
  wait_for:
    agent: "{{ planner }}"

- label: Wait for all reviewers
  wait_for_all:
    agents: "{{ reviewer_agents }}"
```

`wait_for_all` is the wait phase of fan-in. After it succeeds, you can use `responses_from(agents)` or `aggregated_responses(agents)` (see "Built-in template functions") in the next `send` step to reference the agents' completed outputs.

### `pause_for_user` — wait for the human

```yaml
- label: Ask the user for direction
  pause_for_user:
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
- label: Iterate over milestones
  for_each:
    item: milestone
    in: "{{ milestones }}"
    steps:
      - label: Plan the milestone
        send: { ... }
      - label: Wait for the plan
        wait_for: { ... }
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

### Forward-from on a workflow form field (completed-only)

A workflow's user-fillable **text fields** — both genuine `text` inputs and the auto-derived prompt arguments — can be filled by **forwarding an agent's or pane's latest output** at invocation, the same way a prompt's arguments are forwarded in the compose bar: instead of typing a value, the user attaches a source agent/pane and the field's value becomes that source's forwarded output (composed after any text typed into the field).

**This is completed-only.** The source's latest **completed** turn is captured at invoke. If a chosen source still has an **in-flight (streaming) turn**, invocation is **rejected** with a clear message ("agent X is still responding — wait for it to finish, then run the workflow") — the workflow launch is never held open waiting on a streaming agent. So a forward-from source must already be done responding when you invoke. This differs from manual compose-bar forwarding, which *holds* the send until a streaming source finishes; workflow invocation does not hold.

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
  - label: Ask the planner for a plan
    send:
      to: "{{ planner }}"
      text: "Produce a step-by-step plan to: {{ goal }}"
  - label: Wait for the plan
    wait_for:
      agent: "{{ planner }}"
  - label: Hand the plan to the implementer
    send:
      to: "{{ implementer }}"
      forward_from: "{{ planner }}"
      text: |
        Execute the plan above. Ask me if you encounter ambiguity
        rather than guessing.
  - label: Wait for the implementation
    wait_for:
      agent: "{{ implementer }}"
```

### 2. Fan-in review

```yaml
name: review-and-aggregate
description: Send to multiple reviewers in parallel, aggregate, send to primary.

inputs:
  primary_agent: agent
  reviewer_agents: [agent]

steps:
  - label: Send the review to reviewers
    send:
      to: "{{ reviewer_agents }}"
      prompt: "builtin:code-review"
      # code-review's optional `context` argument is auto-derived as a form field —
      # not declared here, not wired in template_vars.
  - label: Wait for all reviews
    wait_for_all:
      agents: "{{ reviewer_agents }}"
  - label: Aggregate feedback for the primary
    send:
      to: "{{ primary_agent }}"
      prompt: "builtin:ai-review-feedback"
      template_vars:
        review: "{{ aggregated_responses(reviewer_agents) }}"   # computed binding, hidden from user
```

The invocation form here shows three fields: `primary_agent`, `reviewer_agents`, and the auto-derived `context` (from `builtin:code-review`). `ai-review-feedback`'s `review` argument is a computed binding, so it never appears.

### 3. Per-milestone iteration with user approval

```yaml
name: implement-milestones
description: For each milestone, plan, get approval, implement, review, pause.

inputs:
  coder: agent
  reviewer: agent
  milestones: [text]

steps:
  - label: Iterate over milestones
    for_each:
      item: milestone
      in: "{{ milestones }}"
      steps:
        - label: Ask the coder to plan the milestone
          send:
            to: "{{ coder }}"
            text: "Plan milestone: {{ milestone }}. Output the plan only."
        - label: Wait for the plan
          wait_for:
            agent: "{{ coder }}"
        - label: Pause for plan approval
          pause_for_user:
            context: "Plan for {{ milestone }} ready. Approve, redirect, or add context."
            recipient: "{{ coder }}"
        # No wait_for here — pause_for_user with `recipient` (Mode 2) implicitly waits.
        - label: Send the implementation to the reviewer
          send:
            to: "{{ reviewer }}"
            forward_from: "{{ coder }}"
            text: "Review the implementation above for the milestone: {{ milestone }}"
        - label: Wait for the review
          wait_for:
            agent: "{{ reviewer }}"
        - label: Send review feedback to the coder
          send:
            to: "{{ coder }}"
            forward_from: "{{ reviewer }}"
            text: "Address the review feedback above for: {{ milestone }}"
        - label: Wait for the revision
          wait_for:
            agent: "{{ coder }}"
        - label: Pause to commit or revise
          pause_for_user:
            context: "Milestone {{ milestone }} done. Commit and continue, or revise?"
            recipient: "{{ coder }}"
```

## Shipped built-in workflows

Two workflows ship with the app as a read-only built-in library (alongside the built-in prompts they consume, `code-review` and `ai-review-feedback`). They appear in the `+ Workflow` menu tagged **built-in / read-only**; "Copy to my workflows" writes an editable copy into your user-global workflows folder if you want to customize one. They are the canonical examples to model your own on. Both standardize on `reviewers` (the fan-out list) and `worker` (the single agent that synthesizes).

### `review-and-aggregate` (generic fan-in)

Review in parallel with several agents, then hand the combined feedback to a worker agent. The reviewers run the hardcoded `builtin:code-review` prompt; the aggregation is an **inline `text`** step (no second prompt), so the workflow is self-contained. `code-review`'s optional `context` argument is auto-derived as a user-fillable field — it is not declared as an input.

```yaml
name: review-and-aggregate
description: Fan a code review out to several reviewers in parallel, then hand the combined feedback to a worker agent for recommendations.
inputs:
  reviewers:
    type: [agent]
    description: The agents that review in parallel. Each receives the review prompt and works independently.
  worker:
    type: agent
    description: The agent that receives every reviewer's combined feedback and produces the recommendations.
steps:
  - label: Send code review to reviewers
    send:
      to: "{{ reviewers }}"
      prompt: "builtin:code-review"
  - label: Wait for all reviews
    wait_for_all:
      agents: "{{ reviewers }}"
  - label: Send combined feedback to worker
    send:
      to: "{{ worker }}"
      text: |
        Here's feedback from several reviewers:

        {{ aggregated_responses(reviewers) }}

        Let me know what your recommendations are.
```

The invocation form shows `reviewers`, `worker`, and the auto-derived optional `context` (from `builtin:code-review`). `context` is the background handed to each reviewer; the worker never sees it directly — only the reviewers' responses.

### `review-analyze-discuss` (flagship)

Reviewers review in parallel with `builtin:code-review`; a worker distills their feedback into a decision-ready verdict using `builtin:ai-review-feedback` (its required `review` argument filled with the reviewers' aggregated responses via `template_vars`); the reviewers respond to the analysis; the worker gives a final recommendation. It demonstrates a computed binding into a hardcoded prompt's argument, plus round-trip discussion via the `last_output` / `aggregated_responses` helpers in inline `text`.

```yaml
name: review-analyze-discuss
description: Reviewers review in parallel, a worker distills their feedback into a decision-ready verdict, the reviewers respond to it, and the worker gives a final recommendation.
inputs:
  reviewers:
    type: [agent]
    description: The agents that review in parallel, then respond to the worker's analysis.
  worker:
    type: agent
    description: The agent that distills the reviewers' feedback into a verdict, weighs their pushback, and gives the final recommendation.
steps:
  - label: Send code review to reviewers
    send:
      to: "{{ reviewers }}"
      prompt: "builtin:code-review"
  - label: Wait for all reviews
    wait_for_all:
      agents: "{{ reviewers }}"
  - label: Distill feedback into a verdict
    send:
      to: "{{ worker }}"
      prompt: "builtin:ai-review-feedback"
      template_vars:
        review: "{{ aggregated_responses(reviewers) }}"
  - label: Wait for the verdict
    wait_for:
      agent: "{{ worker }}"
  - label: Ask reviewers to weigh in on the verdict
    send:
      to: "{{ reviewers }}"
      text: |
        An analyst reviewed all of the feedback and responded:

        {{ last_output(worker) }}

        Do you agree with this analysis? Push back where you think it's wrong, and confirm where it's right.
  - label: Wait for reviewer responses
    wait_for_all:
      agents: "{{ reviewers }}"
  - label: Send responses to worker for final call
    send:
      to: "{{ worker }}"
      text: |
        Here's how the reviewers responded to your analysis:

        {{ aggregated_responses(reviewers) }}

        Weigh their responses and give your final recommendation.
  - label: Wait for final recommendation
    wait_for:
      agent: "{{ worker }}"
```

Both prompts are hardcoded. The invocation form shows `reviewers`, `worker`, and `code-review`'s auto-derived optional `context`. `ai-review-feedback`'s `review` argument is a computed binding, so it is hidden; if that prompt ever renamed `review`, invocation would be blocked with an error naming the prompt and argument rather than failing mid-run.

## Conventions

- **Filename = name field**. `review-and-aggregate.yaml` has `name: review-and-aggregate`.
- **Slug-style names**. Lowercase, hyphens, descriptive. Verb-first if the workflow is action-shaped (`plan-then-implement`, `review-and-aggregate`).
- **Prefer agent-typed inputs over hardcoded names**. Workflows are reusable; hardcoding `reviewer-claude` makes the workflow unusable for someone whose agents are named differently.
- **Explicit waits**. Even though `pause_for_user` with `recipient` waits implicitly, every other `send` is fire-and-forget. Pair `send` steps with `wait_for` / `wait_for_all` deliberately.
- **One workflow per task**. If a workflow is starting to feel like it's doing two things, split it.

## Failure handling

- A step failure (harness error, template render error, contention refusal, missing/incompatible prompt) halts the workflow with status `failed`.
- The user cancelling the workflow (or any participating agent's turn during a workflow) marks the workflow `cancelled`.
- A crash, OS reboot, or force-kill mid-workflow marks it `interrupted`.

**v1 does not resume or retry a run.** A `failed`, `interrupted`, or `cancelled` run is terminal. The user **abandons** it (which clears its run record) and, if the work still needs doing, **re-invokes the workflow from the start** — there is no resume-from-step and no per-iteration checkpoint. This applies to `for_each` too: a failure inside iteration K does not leave a resumable checkpoint; re-invoking re-runs the whole workflow from the first step. Because of that, if you write iteration bodies with side-effecting steps (commits, file writes), keep them idempotent or guard them, since a re-invoke after a partial run will replay earlier iterations.

You don't need to write failure-handling logic in the workflow file; the runtime handles the status transitions. Just write the happy path.

## After authoring

1. Save the file as `<config-dir>/workflows/<name>.yaml` (filename matches `name`). Workflows are user-global — available in every project. (Settings → Workflows → "Open" jumps to the folder.)
2. The user invokes it from Switchboard's workflow picker. The invocation form auto-generates from the `inputs` declaration **plus the user-fillable arguments auto-derived from each hardcoded prompt** (see "How a `send` step produces its message").
3. The workflow runs autonomously; the user watches via the workflow-progress surface and per-agent panes.

## When to point at the formal spec

This doc covers the common authoring path. For edge cases, full validation rules, the MiniJinja subset details, the v2+-reserved-keys list, and the failure-status taxonomy, see `docs/workflow-spec.md`.
