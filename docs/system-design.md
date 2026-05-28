# Switchboard

## 1. What Switchboard is

Switchboard is a **human-directed orchestrator for AI coding agents** — a desktop application you run alongside your existing Claude Code, Codex, Gemini, and Antigravity setup. It lets you spawn multiple agent sessions within a project, route messages between them, and define reusable workflows for common multi-agent operations like second-opinion code review, plan-and-implement, and parallel-solution adjudication.

More precisely, it is a **workflow engine for primitives, not for processes**. It codifies the *shape* of common multi-agent operations (fan-out, fan-in with template wrapping, sequential handoff, pause-for-user-input, iteration) so they can be invoked with one command instead of manually copy-pasted. It does not impose any larger structure on top of those primitives — there is no built-in concept of "plan phase" or "review phase," no SDLC walkthrough, no opinionated process. The user composes workflows ad hoc and saves the ones they reuse.

The human stays in the loop where judgment matters (deciding what to route, when to revise, when to proceed) and is removed from the loop where mechanics waste time (copy-paste, template application, babysitting parallel agents).

This orchestration model has a useful side effect for prompt management. Because Switchboard resolves prompts itself and sends agents plain text, **prompt-provider configuration lives in *one place* (Switchboard) and works the same way across every agent backend**. A user's prompt library — whether in Tiddly, another MCP server, or Switchboard's local store — uses the same lookup and invocation surface across all three harnesses (Claude Code, Codex, Gemini), without configuring the prompt source in any of them. This is especially useful for Codex and Gemini, whose native MCP prompt support is limited or absent depending on version. (The same does **not** hold for MCP tools or per-harness skills, which are invoked by the model mid-turn and must still be configured in the underlying agent; see section 6.)

## 2. Goals and non-goals

### Goals

- **Multi-agent spawn and management.** Multiple Claude Code, Codex, Gemini, and Antigravity agent instances run in a single project with user-assigned names.
- **Routing primitives.** Explicit fan-out (one source → many agents, where the source is either a human-composed message or another agent's output), fan-in (many agents → one recipient), and sequential handoff, with optional prompt-template wrapping.
- **Reusable, parameterized workflows.** Workflows are files that compose primitives — invoked by name, parameterized at invocation time.
- **Agent-friendly authoring.** Workflow files and other authorable artifacts (local prompts, project setup) are documented in instruction docs under `docs/agent-instructions/` designed for AI coding agents to consume. The intended authoring path is to point an existing Claude Code, Codex, or Gemini agent at the relevant instruction file and ask it to generate the artifact from a description, rather than learning the DSL by hand.
- **Autonomous workflow execution.** A workflow continues to run after launch so the user can switch focus to other work without babysitting it (within the lifetime of the Switchboard host process; see §7).
- **Configurable prompt providers.** Apply prompt templates from one or more configured prompt providers during routing, with parameterized substitution. Providers include a built-in local file store (prompts authored as files inside the project or the user's Switchboard config directory) and any MCP server that exposes prompts (for example [Tiddly](https://tiddly.me)). Provider configuration is centralized in Switchboard, so a user's prompt library works identically across Claude Code, Codex, and future agent backends without per-agent MCP setup. Does not extend to MCP *tools* or to model-discovered *skills*, which remain per-agent concerns.
- **Zero-setup onboarding.** Switchboard ships with example prompts and example workflows in the local store so a new user can invoke a useful workflow within minutes of installation, without configuring an MCP server.
- **Full access to the underlying harness.** Switchboard drives Claude Code, Codex, Gemini, and Antigravity; it doesn't replace them.
- **Shareable, versioned configuration.** Workflows, local prompts, and project configuration are file-based and live inside the project's `.switchboard/` directory, so they version, diff, review, and share via the user's normal git workflow.

### Non-goals

- **Replacing the Claude Code, Codex, Gemini, or Antigravity harness.** Compaction, tool rendering, permission policy, plan mode, hooks, and skills all live in the harnesses. Switchboard drives them via their non-interactive modes (`claude -p`, `codex exec`, `gemini -p`, `agy -p`).
- **Prescribing a software development lifecycle.** Switchboard does not know about "planner" or "reviewer" as roles with semantics. Roles are labels the user assigns; the tool is agnostic.
- **Managing git, CI, or PR workflows.** Out of scope. Workflows can read agent outputs and route them; they don't run `git commit`, open PRs, or integrate with CI.
- **Cross-session persistent agent memory** (vector DBs, RAG over prior sessions). Considered as a future feature; the v1 architecture should not preclude it but does not implement it.
- **Visual / GUI workflow editor.** Authoring is file-based for V1, supported by agent-consumable instruction docs (see goals — "Agent-friendly authoring"). A visual or form-based workflow editor is not planned until later versions.
- **Multi-user collaboration.** Single-developer tool. Sharing workflows and configurations via git is supported as a side effect of file-based config, but there is no real-time collaboration model.
- **Hosted / SaaS service.** Switchboard runs locally on the developer's machine. There is no managed cloud version, no shared backend, no remote agent execution. A future hosted service may eventually provide cross-machine sync of workflows, prompts, and project configuration; that is out of scope for v1.
- **Arbitrary harness slash-command passthrough.** v1 does not support invoking arbitrary harness slash commands as input (`/model`, `/skill-name`, etc.). This is an upstream `claude -p` limitation; specific commands are worked around individually (see §9). Tracked in §12 (10.10).
- **API-key authentication for the harnesses.** v1 is built around **subscription / tier auth only**: Claude Code via `claude login` (Pro / Max / Team subscription) and Codex via `codex login` (ChatGPT Plus / Pro). Switchboard does **not** support `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` flows in v1, and does **not** ship per-model pricing tables to derive dollars from token counts for pay-as-you-go users. This is a load-bearing product decision: it shapes the cost / quota surface (see §7 "Per-agent status and actions") and removes the maintenance burden of keeping pricing tables current with vendor releases. Users who have only API-key auth available are surfaced a clear error at agent-creation directing them to install and authenticate the harness's interactive CLI first. API-key support may be revisited in v2+ if user demand surfaces.
- **Raw token counts in the UI.** Token usage is plumbed through the normalized event stream (and used to compute context utilization) but is not surfaced as a UI affordance for users in v1. The user-facing cost / quota surface is dollars (Claude Code, via `total_cost_usd`) or rate-limit / quota signals (Codex, via session-file `token_count.rate_limits`). See §7.
- **Cross-harness dollar normalization.** Switchboard does not synthesize a single "total spend" number across a mixed-harness project. Claude Code's dollars and Codex's rate-limit signals live in different billing realities (separate vendors, separate auth tiers, different metering models per §2 below); aggregating them into one number would mislead. Per-agent and per-project aggregates within a harness are fine; cross-harness aggregation is not.
- **Multi-agent parallel writes to project files.** A general property of any multi-agent setup; out of Switchboard's ownership. Users running multiple agents that write to the same files at the same time accept the conflict risk; resolving such conflicts is the user's responsibility (typically via git, file scoping, or workflow design that serializes writes).

## 3. Core concepts

| Concept | Definition |
|---|---|
| **Working directory** | An on-disk directory (typically a git repo) where Switchboard does its work. Identified by canonicalized path. One Switchboard-managed `.switchboard/` lives at the directory root and contains zero or more **projects**. |
| **Project** | A named, task-scoped grouping of agents + workflow runs + runtime state, hosted within a working directory. (Workflow *definitions* are directory-scoped — shared across projects; projects own workflow *runs* — the in-flight invocations against their agents.) Each project has a UUID (`ProjectId`) and a user-supplied name (unique within its directory; can collide across directories). Multiple projects can coexist in the same working directory, allowing the user to run separate workstreams (backend / frontend / planning / etc.) on the same repo simultaneously. Project-specific state lives at `<directory>/.switchboard/projects/<project-id>/` (see directory layout below). |
| **Agent** | A Claude Code, Codex, Gemini, or Antigravity session within a project, with a user-assigned name. Each agent has a persistent harness session under the hood (a session ID, or a server-assigned conversation UUID carried in a per-agent sidecar for Codex / Antigravity). Agents are bound to their project, not directly to the directory. |
| **Primitive** | An atomic operation Switchboard provides for a workflow to compose: spawn agent, send message, auto-forward output, fan-in with template, pause for user input, iterate over a list. Six exist in v1; see §4. (Saving and invoking a reusable workflow is the composition layer over these primitives, not itself a primitive — see §5.) |
| **Workflow** | A named, parameterized composition of primitives — for example "fan-out review and aggregate." Defined as a YAML file under `<directory>/.switchboard/workflows/` (workflows are directory-scoped, shared across projects in that directory; rationale below). Invoked by name with arguments against a specific project. |
| **Prompt template** | A named prompt definition resolved by ID at routing time. Used as message content (sent to an agent) or as a wrapper applied around aggregated outputs before forwarding (used in fan-in; see §4 Primitive 4). |
| **Prompt provider** | A source of prompts Switchboard resolves IDs against. Two implementations ship in v1: `local` (file store) and any registered MCP-server provider. Addressed by prefix (e.g. `local:code-review`, `tiddly:code-review`). See §6. |
| **Routing** | Message passing between agents. Includes fan-out (one source, many recipients), fan-in (many sources, one recipient, with template wrapping), and sequential handoff. |
| **Harness session** | The underlying Claude Code, Codex, Gemini, or Antigravity session that backs an agent. Persisted on disk by the harness; resumed via `--resume` (or `--conversation <uuid>` for Antigravity). |

A note on terminology: "session" in the agent ecosystem is overloaded. Switchboard uses **project** for its task-scoped workspace concept (multiple projects per working directory) and reserves **session** to mean the underlying harness session backing a single agent.

### Why multiple projects per working directory

A single working directory (a git repo) often hosts multiple in-flight workstreams: planning a feature while implementing another, backend changes alongside frontend changes, several `plan-*.md` documents being drafted in parallel. Switchboard accommodates this by letting the user create multiple **projects** under the same working directory — each with its own agents, workflows-in-flight, and state. Switching the displayed project in the UI is a display change only; the other projects keep running in the background (workflows don't pause, agents don't unsubscribe), whether they live in the same working directory or another one the user has added. v1 is single-window; multi-window is not in scope.

### Directory layout

Switchboard-managed state lives in a `.switchboard/` directory at the working directory's root. The shape (illustrative):

```
<directory>/
└── .switchboard/
    ├── config.yaml             # directory-level config (placeholder in v1; mostly empty)
    ├── workflows/              # workflow definitions (YAML), shared across projects in this directory
    ├── prompts/                # local prompts (markdown body + YAML frontmatter), shared across projects
    ├── projects.jsonl          # append-only index of projects: { id, name, created_at }
    └── projects/
        └── <project-id>/       # per-project state (one subdirectory per project)
            ├── config.yaml     # per-project config
            ├── instance.lock   # M4+ flock — one Switchboard process per project at a time
            ├── registry.jsonl  # agent registry for this project (append-only)
            ├── sessions/       # M2+ — Codex session-link sidecar files (per agent)
            └── runs/           # M6+ — workflow-run checkpoints

~/.config/switchboard/          # user-global config (illustrative Linux/XDG path; resolved via the `directories` crate —
│                               #   on macOS this is ~/Library/Application Support/switchboard/)
├── config.yaml                 # personal preferences (prompt library location, accounts)
├── prompts/                    # optional personal prompt library (see §6)
└── workspace.yaml              # app-managed: the working directories the user works across,
                                #   each with a cached snapshot of its projects
```

Switchboard is **single-instance**: launching it again focuses the running window rather than starting a second process, so there is exactly one writer of `workspace.yaml`. The per-project `instance.lock` (below) remains as defense-in-depth for the pathological case of two processes (e.g. a dev build alongside the bundled app); multi-window is out of scope for v1.

**What's directory-scoped vs project-scoped:** workflows and local prompts are directory-scoped — defined once per repo, reusable across projects. Agents, runtime state, workflow runs, and harness session links are project-scoped — each project has its own. Rationale: workflow definitions and prompt libraries are about *how to do the work*; they belong to the repo. Agents and runtime state are about *the work in progress*; they belong to the project (the task).

The directory-level `config.yaml`, `workflows/`, and `prompts/` are intended to be checked into git and shared. Everything else — `projects.jsonl`, the entire `projects/` tree (including per-project `config.yaml`, `registry.jsonl`, `journal.jsonl`, `sessions/`, `runs/`), and lock files — is local-machine runtime data and should be `.gitignore`d.

### Working directories and the workspace

Each working directory owns its projects: the source of truth for a directory's projects and their state is that directory's own `.switchboard/`, so a directory is self-contained and travels with its git repo. Switchboard tracks the set of working directories the user works across in a user-global `workspace.yaml` and presents the projects from all of them as a single flat list — the user opens Switchboard and sees every project across every directory at once, each labelled with its directory, without choosing a directory first.

`workspace.yaml` records, per directory, its path and a cached snapshot of its projects (each the project's `{ id, name, created_at }`, so an unavailable directory's rows keep their identity and ordering). The cache lets the flat list render even when a directory is temporarily unavailable (unmounted, moved, or deleted): its projects appear marked unavailable with a remove action, rather than silently vanishing. The cache is refreshed from a directory's `.switchboard/` whenever that directory is read successfully **and** after any project create/rename in it — the directory's own state always wins, and the cache is consulted only when the directory can't be read. Projects are addressed by their globally-unique `ProjectId`; each carries its owning directory for routing (the agent spawn cwd) and labelling.

Adding a directory appends it to `workspace.yaml`; removing one drops its entry and leaves the directory's on-disk `.switchboard/` untouched (re-adding the directory restores its projects — removing from the workspace is not deletion). Re-pointing a moved directory to a new path is a planned affordance.

**Conversation source of truth — a split.** Switchboard keeps no full transcript store, but the harness session files do not hold *everything* either: they cannot faithfully represent the user's side of a multi-agent conversation — a fan-out replicates the user's prompt across every recipient's file, and an instantly-cancelled send is written to none. So the model is a split:

- **Switchboard owns the user's side** — a per-project append-only **conversation journal** (`projects/<project-id>/journal.jsonl`) recording each *send* (prompt + recipient + a `send_id` that groups a fan-out's recipients; written **when the turn starts**, one record per recipient) and an *outcome marker* for every **non-completed** turn — failed or cancelled — (outcome metadata only, including the failure reason; never agent content). The send record is written **fail-closed, immediately before the harness subprocess is spawned**: if it can't be persisted, the turn does not start (a silently-lost send would surface as an assistant reply with no user message above it). A turn that *fails to start at all* — the harness errored before any stream — gets a `Failed` outcome marker too, so it shows as a failed turn rather than an orphaned send. **Durable history begins when a turn starts, not when the user hits Send:** a message still queued behind a busy agent is live-UI-only and lost on restart (the queue is in-memory), and a queued message removed before it starts is never journaled.
- **The harness session files own the agents' side** — agent-produced content (responses, tool calls) for **completed** turns, read on hydration.

The two sources **partition** cleanly (no correlation or de-dup between them): a completed turn's content comes from the harness file; a failed or cancelled turn comes from the journal's outcome marker (the harness file is not consulted for it). The unified transcript is the merge: user messages from the journal (grouped by `send_id`, rendered once — a fan-out is one "User → B|C" entry, not one per recipient), completed-turn content from the harness files, and failed/cancelled markers from the journal. What shows after restart for a non-completed (failed or cancelled) turn is therefore exactly what the *harness* persisted: Switchboard adds no agent content of its own, and the merge renders whatever assistant content the harness session file holds, with the journal's outcome marker overlaid by timestamp. Claude Code and Codex persist **nothing** for an aborted turn (their session files stop at the last completed turn), so those show the marker only; a harness that *does* persist partial content would show it automatically, above its marker, with no special handling (Gemini and Antigravity cancellation persistence is **unverified** — see the per-harness research notes). The live session always shows partial output from Switchboard's in-memory stream buffer, which is discarded on restart; "open session file" exposes the raw harness content. (Worked examples: §7 "Unified history after restart".) After restart a partially-queued fan-out shows only the recipients whose turns actually started (e.g. "User → A" if B was still queued) — intended, since B never received the message. A per-agent "what this agent saw" view can still read a single harness file verbatim (including the user message as that agent received it).

**Default project on first init.** When a working directory is initialized for the first time, Switchboard auto-suggests creating one project named after the directory's basename (e.g., directory `switchboard` → suggested project name `switchboard`). The user can override the name or skip and pick later. Rationale: removes friction on first use; the implicit assumption is that users who don't multi-project still get a usable default.

**Project name uniqueness within a directory** uses the same canonicalization as agent names (per §4 Primitive 1): hyphens normalized to underscores, lowercased, for the uniqueness check only — `feature-a` and `feature_a` collide; `feature-A` and `feature-a` collide. Stored verbatim as the user typed.

**Deletion in v1.** Project and agent deletion are deliberately out of scope for v1. No UI affordance, no command-level deletion API, no canonical deletion semantics are defined. Users may manually edit `.switchboard/` state at their own risk (the file formats are documented), but this is not a supported workflow — first-class deletion semantics (cascade behavior, in-flight flock handling, append-only-vs-tombstone, history retention) will be specified when the feature is added. Tracked: M4 may add project deletion via the project switcher; agent deletion is unscheduled.

## 4. Functional primitives

These six primitives cover everything Switchboard needs to do at the functional level. Workflows compose them. (Saving and invoking a reusable workflow is the composition layer over these primitives, covered in §5.)

### Primitive 1 — Spawn an agent

Create a new agent within a project. User specifies:

- Agent type (Claude Code, Codex, Gemini, or Antigravity).
- Name (free-form label).
- Optional initial prompt (sent as the first message after spawn to prime the agent with role context, project background, or any other instructions the user wants in place before the first real turn). Authored like any other prompt — free-form text or a fully-qualified prompt ID resolved through the prompt-provider system (§6).
- Optional working directory override (defaults to project working directory).

Agent names are restricted to `[A-Za-z0-9_-]+` at creation; this avoids ambiguity when names appear as template variable identifiers (Primitive 4) or in file paths. Two agents in the same project whose names differ only in hyphen vs. underscore (e.g., `reviewer-a` vs `reviewer_a`) are not allowed — agent names must be unique after normalizing hyphens to underscores, since both forms collapse to the same template-variable identifier in fan-in contexts (Primitive 4).

The harness session ID is captured and persisted. The agent is now part of the project and can receive messages and participate in workflows.

### Primitive 2 — Send a message to one or more agents

User specifies:

- Recipients (one or more agents).
- Prompt template (a fully-qualified prompt ID, e.g. `local:code-review` or `tiddly:code-review`) and/or free-form text.
- Optional parameters for the prompt template.

The composed message is sent to each recipient. If recipients are multiple, this is a fan-out: each agent receives the same message and runs independently.

Typical uses include multiple reviewers on the same diff, second opinions on a plan, and parallel solution exploration.

This primitive is **synchronous from the human's perspective** — the human sends the message, the agents start working. The human can then watch any agent's output, switch between them, or walk away.

### Primitive 3 — Auto-forward an agent's output

A subsequent `send` step references the latest completed output of one or more upstream agents and composes them into the new message body. The composition follows a canonical text shape (see `docs/workflow-spec.md` §`send` `forward_from`) so the receiving agent sees clearly delimited source content.

Used for sequential handoff (planner → implementer with the plan as input) and for agent-driven fan-out (planner → multiple implementers in parallel, one reviewer → multiple follow-up reviewers, etc.). The forwarding is resolved when the upstream agents' referenced turns reach a terminal state — if a referenced agent is still mid-turn, the dependent `send` waits for it (the dependency-resolution mechanism of §7 "Agent contention"). (Implementation note: this is a step-time composition by the workflow interpreter, not an event-driven hook configured on the upstream agent.)

**This is the same primitive the manual compose bar exposes** (§7 "Composing and dispatching messages" Source) — auto-forward is not workflow-only. The workflow interpreter and the manual compose-and-dispatch surface drive one dependency-resolution mechanism; a workflow `send` with `forward_from` is the recorded form of a user manually forwarding one agent's output (including its not-yet-finished output) to another. See the binding principle in §7.

Output is scoped to the current workflow run — see `docs/workflow-spec.md` §"Output scope" for the rule (helpers see only turns dispatched by this workflow run and observed reaching terminal state via a synchronization step).

### Primitive 4 — Fan-in with template wrapping

Configure: when all of agents A, B, ..., N finish their current turns, combine their outputs into a single message using a wrapping prompt template, then send to agent X.

Wrapping templates receive agent responses through two helper functions exposed by the workflow DSL — `aggregated_responses(agents)` returning the canonical text shape (the default for cross-platform prompts that take a single text-blob argument), and `responses_from(agents)` returning a name → text mapping for Switchboard-aware prompts that want to iterate with custom formatting. The author binds whichever helper they need into a step-local `template_vars` slot at workflow-authoring time. Agent names containing hyphens are normalized to underscores in mapping keys. See `docs/workflow-spec.md` for the exact surface, including the canonical text shape and the name-collision rules.

This is the most behaviorally-rich primitive. It implies waiting on multiple agents, accumulating their final responses, applying a template, and dispatching. Failure handling (one agent crashes mid-workflow) is covered in section 7. Output is scoped to the current workflow run — see `docs/workflow-spec.md` §"Output scope".

### Primitive 5 — Pause for user input

Pause workflow execution and wait for the user to respond. The workflow step specifies:

- Optional context message shown to the user (often a wrapping prompt referencing prior agent outputs — "here's what the reviewers said; what direction do you want to take?").
- Optional pre-configured recipient — an agent that the user's response will be dispatched to. The user just types; they don't have to remember which agent the message goes to.
- Whether the input is required (default: yes — skipping cancels the workflow; if `false`, skipping proceeds with empty input bound to `{{ user_input }}`).

When the workflow reaches this step, Switchboard fires the OS-native notification (per §10), suspends workflow execution, and surfaces the compose bar pre-targeted at the configured recipient. The user composes their response using the same compose-and-dispatch semantics as §7 (free-form text, optionally combining a prior agent's output, optionally wrapped in a prompt). Their typed text becomes available to subsequent steps as `{{ user_input }}`.

When `recipient` is set, the pause step also implicitly waits for the recipient's turn (dispatched from the user's input) to reach terminal state before the workflow proceeds. Rationale: pause-with-recipient targets exactly one agent, so there is no fan-out parallelism to preserve, and the natural user expectation is to see the agent's response before continuing. Authors wanting fire-and-forget after a pause should drop `recipient` and write a separate `send` step that uses `{{ user_input }}`. See `docs/workflow-spec.md` §`pause_for_user` for the full lifecycle including the no-recipient and skip cases.

This makes the human-in-the-loop framing explicit at the workflow level, not just implicit at the workflow's end: a workflow author can encode "do these autonomous steps, then pause for me to weigh in, then continue" without forcing the user to remember to manually trigger the next phase.

### Primitive 6 — Iterate over a list

Repeat a sub-sequence of workflow steps once for each item in a list supplied at invocation time. The workflow step specifies:

- The iteration variable name (e.g., `milestone`, `task`, `target`).
- The input list to iterate over (a workflow input, e.g., `{{ milestones }}`, supplied by the user at invocation).
- The sub-sequence of steps to run per iteration. Steps inside the loop body use the existing primitives (send, auto-forward, fan-in, pause for user input).

The iteration variable is bound for each iteration's body and available in template substitution (e.g., `prompt: "{{ execute_plan_prompt }}", context: "milestone {{ milestone }}"`).

Used for milestone-based work, per-target processing, or any "do this whole sub-workflow once per item in a list" shape — for example, a plan-then-implement-and-review workflow that should run once per milestone in an implementation plan.

**Deliberate v1 scope:**

- **Bounded over a list only.** No "iterate until condition X" — that requires conditionals (deferred to v2; see §11).
- **Failure halts the whole workflow.** A step failure inside iteration N halts the workflow; the user resolves it (retry, abandon) just like any other step failure. No "skip to next iteration on failure."
- **No cross-iteration state.** Each iteration is independent. `{{ user_input }}` from a pause-for-user-input step in iteration 2 doesn't see iteration 1's input. This keeps scoping rules simple.
- **No nested loops in v1.** Outer-only iteration. Nested iteration is a v2+ consideration.
- **Lists are static, supplied at invocation time.** The list isn't computed from a prior step's agent output — that opens dynamic-allocation questions out of scope for v1.

### Execution model: implicit DAGs

The primitives compose into DAGs (directed acyclic graphs): fan-out runs nodes in parallel, fan-in synchronizes them, sequential steps sequence in order, and iteration replays a sub-DAG bounded by a static list. The runtime must therefore handle parallel dispatch within fan-out and proper synchronization within fan-in — not just walk the YAML top-to-bottom and serialize all dispatches.

The DAG is conceptual, not declared. The runtime never sees explicit edges like "task A depends on task B" the way an Airflow DSL would express them. Instead, the graph emerges implicitly from which agents each step references — recipients of a `send` step, agents listed in a `wait_for_all`, agents whose outputs are template variables in a fan-in. The runtime can read off the parallelism and synchronization it needs from those references; it doesn't need a separate dependency declaration.

A general-purpose DAG scheduler (topological sort over arbitrary node dependencies, dynamic scheduling) is **not** required for v1. A step-based interpreter that supports parallel dispatch within fan-out and proper synchronization within fan-in is sufficient for the six primitives above. A general DAG model is a possible v2+ direction if workflows ever grow arbitrary inter-step dependencies.

## 5. Workflows

A **workflow** is a named, parameterized composition of the primitives in §4, defined as a directory-scoped YAML file under `<directory>/.switchboard/workflows/` (shared across the projects in that working directory). Invoking a workflow fills in its parameters and runs it. Directory-scoped resources (workflows, local prompts) resolve relative to the owning directory of the project the workflow runs against — there is no single "current" directory in the flat workspace model.

Workflow definition format (illustrative; the authoritative schema is `docs/workflow-spec.md`):

```yaml
name: review-and-aggregate
description: Send a message to multiple reviewers, aggregate, send to primary.
inputs:
  primary_agent: agent
  reviewer_agents: [agent]
  review_prompt: prompt_id           # invocation supplies e.g. local:code-review
  aggregation_prompt: prompt_id      # invocation supplies e.g. tiddly:ai-review-feedback
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

When invoked, Switchboard prompts the user for each input and then executes the steps.

The example above uses `aggregated_responses(...)` — the helper that returns the reviewers' outputs pre-formatted in a canonical text shape — because typical aggregation prompts (especially cross-platform ones like `tiddly:ai-review-feedback`) take a single text-blob argument. A sibling helper `responses_from(...)` returns the same data as a name → text mapping for Switchboard-aware prompts that want to iterate with custom formatting. See `docs/workflow-spec.md` for both.

The DSL exposes both `wait_for` (single agent) and `wait_for_all` (multiple agents) as synchronization steps. See `docs/workflow-spec.md` for their exact semantics, including behavior when no in-flight turn exists at the synchronization point.

The DSL exposes fan-in as `wait_for_all` + a `send` step (using either helper above) — the wait and dispatch phases are separate steps for readability and flexibility, not because fan-in is itself a composition of other primitives.

### Authoring

Workflows are authored as YAML files at `<directory>/.switchboard/workflows/` (directory-scoped — shared across all projects in that working directory; see §3 for the rationale). Because they live inside the working directory, they are naturally version-controlled along with the repo — diffed, reviewed, and shared via the user's normal git workflow. There is no directory-picker step in Switchboard's UI; the location is conventional.

Authoring is intentionally file-based. The user edits workflows in whichever editor they prefer (Vim, VS Code, etc.); Switchboard's UI reads the files but does not include an editor of its own. The supported authoring path for new users is to point an existing Claude Code, Codex, or Gemini agent at `docs/agent-instructions/workflows.md` and have it generate a starter workflow from a description (per §2 "Agent-friendly authoring"). Hand-authoring against the DSL spec works too for power users.

v1 ships with a small library of built-in workflows (review-and-aggregate, sequential handoff with template) as starting points; users can copy or fork these to author their own.

Users without an existing harness installation outside Switchboard can use a Switchboard-spawned agent itself to author a workflow from the instruction docs — agents Switchboard manages are full Claude Code / Codex / Gemini sessions and can read project files normally. Hand-authoring against the DSL spec also works for power users.

Workflow files are **directory-scoped** — there is no user-global workflow directory parallel to user-global prompts. Workflows live under `<directory>/.switchboard/workflows/` and are shared across all projects in that working directory (per §3 directory layout). Reuse across *different* working directories (different repos) happens via copy or symlink. (Asymmetric with prompts on purpose: prompts are user-portable templates while workflows are repo-shaped — they reference the conventions of a specific codebase. But within a single repo, workflows are shared infrastructure, not per-task — multiple projects in the same directory invoke the same workflow definitions against their own agents.)

## 6. Prompts and prompt providers

> *Scope note: this section covers prompts only. Model-invoked MCP tools and Claude Code skills remain configured per-agent, not per-Switchboard. See "Cross-agent normalization" below for the boundary.*

A **prompt** is a reusable, optionally parameterized text template — for example, "Review the diff focusing on `{{ focus }}`." Workflow files and slash commands reference prompts by ID. The *prompt text* lives in a **prompt provider**; the *workflow* lives in the workflow file. Switchboard reads workflow files, resolves prompt IDs to prompt content via the configured providers, and applies templates with substitution.

### Providers

Two providers ship in v1:

- **Local file store.** Prompts authored as files (markdown body with YAML frontmatter for metadata: name, description, arguments). Resolved across one or more directories: a fixed directory scope at `<directory>/.switchboard/prompts/` (shared across all projects in that working directory; see §3), plus an ordered list of user-configured directories (`local_prompt_dirs` in config — see "Configuring local prompt directories" below). This lets a power user keep their personal prompt library in their own git repo (e.g. `~/repos/my-prompts/`) instead of being limited to the OS-conventional app data directory. The local store is the lowest-friction way to author a prompt and the mechanism Switchboard uses to ship example prompts.
- **MCP-server provider.** Resolves IDs against any MCP server the user has configured that exposes prompts. [Tiddly](https://tiddly.me) is the canonical example and the development reference, but the integration is generic: pointing Switchboard at a different MCP prompt server is a configuration change, not a code change.

### Authoring a local prompt

A local prompt is a single file. Example (`<directory>/.switchboard/prompts/code-review.md`):

```markdown
---
name: code-review
description: Ask an agent to review the current diff against a checklist.
arguments:
  - name: focus
    description: Optional focus area for the review.
    required: false
---
Please review the current uncommitted changes in this repository.

{% if focus %}Focus area: {{ focus }}{% endif %}

For each issue, identify the file, the concern, and a suggested fix.
```

Local prompts are authored file-first, the same as workflows: edit them in whichever editor you prefer (Vim, VS Code, etc.), optionally have an existing Claude Code, Codex, or Gemini agent generate a starter from `docs/agent-instructions/prompts.md`. Switchboard reads the files and surfaces them via slash-command autocomplete; in v1 it does not provide an in-app editor for prompts (see "Future direction: prompt library view" below for what changes in v2+).

**Frontmatter spec for v1:**

| Field | Required | Notes |
|---|---|---|
| `name` | yes | Slug. Matches MCP's `prompts/list` `name` field and Claude Code's skill `name` field. Used as the suffix in `local:<name>` references. |
| `description` | yes | Short human description. Matches MCP standard. |
| `arguments` | optional | Array of `{name, description, required}`. Matches MCP standard. All arguments are treated as strings in template substitution; typed arguments may be added later if needed. No `default` field in v1 — local frontmatter intentionally mirrors MCP's surface so prompts move cleanly between providers (open question 10.8). |
| `tags` | optional | Array of strings. Matches Tiddly's tag extension. Reserved for future library/browse views (see "Future direction" below); v1 does not use them in the slash-command UI. |

This minimal set mirrors the MCP `prompts/list` standard, plus Tiddly's tag extension as a superset, so prompts move cleanly between local and MCP storage. Other metadata fields (title, owner, etc.) are explicitly out of scope for v1.

**Skill-file compatibility.** Because the frontmatter shape (`name` + `description` + body) is identical to a Claude Code skill's frontmatter, a skill `.md` file can be dropped into a Switchboard prompts directory and used as a local prompt as-is. The semantics differ — a skill is invoked by the model mid-turn within a Claude Code session, whereas a Switchboard prompt is dispatched by the user via slash command — but the file format is the same, and skill bodies typically read as instructions that work fine when sent as a user message. (The reverse — a Switchboard prompt with `arguments` working as a Claude Code skill — does not hold; skills aren't parameterized.)

### Configuring local prompt directories and MCP-server providers

Both local prompt directories (`local_prompt_dirs`) and MCP-server providers (`mcp_providers`) are declared in YAML config at one of two scopes:

- **User-global**: `~/.config/switchboard/config.yaml` (path resolved per OS via the Rust `directories` crate). For personal preferences — your prompt library location, your Tiddly account, etc.
- **Directory-scoped**: `<directory>/.switchboard/config.yaml`. Adds or replaces user-global config. Useful when a team workflow needs a specific MCP provider (e.g., a team Tiddly URL distinct from the user's personal one) or when a repo ships its own curated set of prompt directories. Shared across all projects in that working directory.

Resolution rules differ slightly between the two keys:

- **`local_prompt_dirs`**: directory's list, if set, *replaces* the user-global list (directory intent is explicit).
- **`mcp_providers`**: directory providers shadow user-global providers with the same `name` (entry-level merge); the user's other providers stay available.

**Example config:**

```yaml
# A list of directories Switchboard scans for local prompts, in declared
# order. If unset, defaults to [<OS-conventional path>]. Power users can
# point at a personal git-managed prompt library here.
local_prompt_dirs:
  - ~/repos/my-prompts-library      # personal git-managed library
  - ~/.config/switchboard/prompts   # OS-conventional default; explicitly include if you want it

mcp_providers:
  - name: tiddly                    # used as the prefix (tiddly:my-prompt)
    preset: tiddly                  # first-class preset; only requires a token
    token: ${TIDDLY_PAT}            # env var reference

  - name: my-team-mcp               # generic HTTP MCP server
    transport:
      type: http
      url: https://mcp.example.com
    auth:
      type: bearer
      token: ${TEAM_MCP_TOKEN}

  - name: my-local-stdio            # generic stdio MCP server (e.g. an npm-packaged server)
    transport:
      type: stdio
      command: ["npx", "-y", "@example/mcp-server"]
```

**Tiddly is a first-class preset.** The Switchboard UI offers a one-click "Connect Tiddly" action: the user pastes a Personal Access Token, and the app writes the corresponding `preset: tiddly` config entry automatically. Tiddly's URL and auth workflow are baked in. Other MCP servers require manual config (or a generic "Add MCP server" form in the UI). The presets list is open — additional first-class integrations (e.g., a future popular prompt-store MCP) can be added the same way.

### Addressing prompts

Providers are addressed by a short prefix in prompt IDs:

- `local:code-review` — resolves against the local file store.
- `tiddly:code-review` — resolves against the MCP server registered under the name `tiddly`.

The prefix is the user-chosen registration name for an MCP-server provider, so a user with two MCP prompt servers configured can address both unambiguously. The `local` prefix is reserved for the built-in local store.

### Resolution rules

- **Workflow files require explicit prefix.** Every prompt reference in a workflow is fully qualified (e.g. `local:code-review`, `tiddly:code-review`). This keeps workflow files portable: a workflow shared between projects always resolves to the same prompt source, regardless of how the receiving user has their providers configured. There is no concept of a "default provider" for unprefixed lookup in workflow files.
- **Prefixed lookup is strict.** A prefixed ID resolves only against the named provider; if not found, it errors. No cross-provider fallback.
- **Local-store resolution.** Directory scope (`<directory>/.switchboard/prompts/`) is checked first, then each directory in `local_prompt_dirs` (from directory config if present, otherwise from user config) in declared order. Default value if not configured: `[<OS-conventional path>]` (e.g. `~/.config/switchboard/prompts/` on Linux, resolved via the Rust [`directories`](https://crates.io/crates/directories) crate). A prompt with the same name in an earlier-checked directory shadows later ones — intentional, lets a working directory override a personal library, lets a personal library override a team library, etc. Directory config's `local_prompt_dirs` (if set) replaces the user-config list rather than merging, so the directory's intent is explicit.
- **Interactive UI ergonomics.** When the user types a slash command in the message bar, the UI may provide autocomplete across all configured providers and may accept a bare name if it matches exactly one provider's prompt. This is a UI-layer affordance only — it does not affect how workflows or other persisted artifacts reference prompts.
- **Prompt versioning is out of scope for v1.** Workflow references resolve to whatever the provider returns at invocation time; if a Tiddly prompt is edited, every workflow referencing it picks up the new version on the next invocation. Recovery and history are deferred to the upstream tool — Tiddly's own version history for hosted prompts, git for local prompts.

### Prompt arguments

Prompts can declare arguments (per the frontmatter spec for local prompts, or per the MCP `prompts/list` response for MCP-served prompts). At routing time:

1. Switchboard discovers a prompt's arguments from its provider (frontmatter for local; `prompts/list` for MCP).
2. The user (or the invoking workflow) supplies values for each argument.
3. **For MCP-served prompts**, Switchboard calls `prompts/get` with the supplied values; the MCP server renders the template and returns the rendered text.
4. **For local prompts**, Switchboard renders the template itself with MiniJinja (see "Wrapping templates" below for the Rust templating choice) and produces the rendered text.

The agent only ever sees the final rendered text — neither the template nor the arguments. This is what makes the "prompt-provider configuration lives in one place" property work across harnesses (see "Cross-agent normalization" below).

This separation between provider and workflow is intentional: a prompt store is a prompt store, not a workflow engine. Encoding control flow ("run agent A, then fan out to B and C, then aggregate via template D") in a stored prompt would stretch the store out of shape. Workflows are programs; prompts are data.

### Cross-agent normalization

Switchboard resolves prompt IDs itself and sends agents the rendered text as a plain message — the agent never sees the MCP call, the provider, or the arguments. The useful side effect: prompt-provider configuration lives in *one place* (Switchboard) and uses the same lookup and invocation surface across every agent backend. A user's prompts (Tiddly, another MCP server, or the local store) work the same way with Claude Code, Codex, and Gemini agents, without configuring the prompt source in any harness. (The actual rendered text can differ — local prompts render via MiniJinja, MCP-served prompts render server-side; portability across providers is bounded by the shared template-feature subset captured in 6.1.) This is especially useful for Codex and Gemini, whose native MCP prompt support is limited or absent depending on version — Switchboard gives them a Claude-Code-style prompt library experience without requiring native support.

What this does **not** cover:

- **MCP tools.** Tools are invoked by the model mid-turn, not by the user pre-turn. Switchboard cannot proxy them; tools (e.g. an Atlassian MCP server, Google Drive integration) must still be configured in the underlying agent.
- **Claude Code skills.** Configured in Claude Code itself (`~/.claude/skills/`, project `.claude/skills/`); Switchboard does not mediate them. **Auto-invoked skills do work normally in Switchboard-spawned sessions** because default `claude -p` loads the user's full environment — the model can discover and invoke skills mid-turn just as it would interactively. The *user-invoked* side of skills (`/skill-name` as an explicit command) is currently unavailable due to a `claude -p` limitation; see §9 passthrough and [docs/research/archive/claude-code-headless.md](research/archive/claude-code-headless.md).
- **Per-agent setup in general.** Authentication, permission flags, hooks, and MCP tool registration remain the underlying harness's concern.

Switchboard normalizes the *user-invoked prompt* surface across agents. Model-invoked capabilities (tools, skills) and harness-level configuration are still per-agent.

### Wrapping templates

Wrapping templates (used for fan-in) are prompts — from any provider — that take agent responses as a template argument. The workflow author binds the responses into a template variable using one of the DSL helpers (`aggregated_responses(agents)` for a canonical text blob, or `responses_from(agents)` for a name → text mapping). The template uses **Jinja2-compatible syntax**, rendered via [MiniJinja](https://github.com/mitsuhiko/minijinja) (a native Rust templating engine by the author of Jinja2, designed for Jinja2 compatibility — chosen so prompts move cleanly between Tiddly's Jinja2 and Switchboard's local rendering without surprises).

The most common shape uses `aggregated_responses` and a single text argument — works with any cross-platform prompt that takes a string:

```jinja
The following are reviews from multiple agents:

{{ feedback }}

Summarize the recommendations and identify points of agreement and
disagreement.
```

For Switchboard-aware prompts that want full formatting control, `responses_from` returns a mapping the template iterates explicitly:

```jinja
{% for name, response in responses.items() %}
## {{ name }}

{{ response }}

{% endfor %}
```

In both cases the variable name (`feedback`, `responses`) is whatever the workflow author bound into `template_vars`. Neither is an implicit ambient variable — the workflow's `template_vars` declaration carries the binding.

**Resolved in `docs/workflow-spec.md` §Templating** (MiniJinja subset, available variable scopes, built-in template functions).

### Future direction: prompt library view

v1's prompt UX is slash-command-driven — the user types a slash command in the message bar, autocomplete suggests prompts across configured providers, and the prompt is sent to the agent. This is the minimum surface to make prompts useful.

A richer **prompt library view** is plausible for v2+: a "Prompts" panel that lists all prompts from all configured providers, lets the user filter by provider or tag, search by name/description/content, and edit local prompts in their editor (or open Tiddly-hosted prompts in Tiddly via deep link). This is what makes the optional `tags` field in the local prompt frontmatter useful — v1's slash command picker doesn't need them, but a library view does.

The schema and provider model already accommodate this; v1 just doesn't ship the UI. Keeping the data shape compatible (mirroring MCP's prompt schema + Tiddly's tag extension) is what makes it cheap to add later.

## 7. User-facing model

This section describes the conceptual user experience. The desktop form factor and frontend stack are documented in §10 (Form factor and distribution).

### Project list

The user opens Switchboard and sees a single flat list of all their projects, drawn from every working directory they've added, each project labelled with its directory. They open one, create a new one in any of their directories, or add another working directory. Projects from a currently-unavailable directory still appear, marked unavailable, with the option to remove the directory from the list. A project is bound to a working directory; switching which project is displayed is a display change only — projects in other directories keep running in the background (see §3).

### Inside a project

The user sees a **single unified transcript view** for the project — every turn from every agent appears in one chronologically-ordered stream, with each turn attributed to the agent that produced it (name + harness badge). Tool calls, errors, and per-turn metadata (cost / context utilization) nest under the turn they belong to. The user reads the project's conversation flow as one timeline; "Agent X responded at 10:02, then I forwarded to Agent Y at 10:03, then Y responded at 10:04" is legible at a glance without manually correlating across panes. The user's **own** messages appear **once** in this timeline regardless of how many agents received them — a single fan-out send to several agents is one "User → B|C" entry, not one per recipient (the user's side of the conversation is Switchboard-owned; see §3 and "Cancelling a workflow or turn" below). A fan-out's N responses render as **one group** at the send's point in the timeline, laid out **side-by-side** (one card per recipient, collapsing to a vertical stack on narrow viewports) — a horizontal layout *within* a single timeline entry, which preserves the single-stream model rather than departing from it.

A **per-agent overview sidebar** lists every agent in the project with its real-time operational state — name, harness badge, status (idle, processing, waiting on tool, errored), context utilization, last-turn cost (Claude Code) or quota signal (Codex). Clicking an agent in the sidebar surfaces its context menu (fork session, open session file, reset/remove, cancel in-flight turn). The sidebar is the agent-management surface; the unified transcript is the conversation surface.

**No singleton "active" or "focused" agent.** All agents in a loaded project are equally first-class. The compose bar picks recipient(s) explicitly per send (single agent or multi-select) — no agent is the default recipient by virtue of being "viewed." Users may colloquially think of one agent as primary (e.g., the implementer in a review workflow) and others as secondary (the reviewers), but the architecture does not encode this — it's a label, not a structural concept.

Agents in v1 are **project-scoped** — they're created within a project and stay there. Cross-project / global agent templates are a planned future direction (tracked in §11) — for example, a personal "writing editor" persona that knows your voice and applies across blog posts, docs, and emails, or a "domain expert" persona carrying institutional knowledge (a regulatory framework, your team's architecture conventions, a research methodology) reusable in any project that touches the area. Optionally these could be surfaced via semantic search over the project context, suggesting which template fits.

### Per-agent status and actions

Each agent's operational state is surfaced in the **overview sidebar** (per "Inside a project" above). State includes:

- **Status**: idle, processing, waiting on tool, errored.
- **Model**: the model identifier the harness reported on this dispatch (e.g. `claude-sonnet-4-5`, `gpt-5-codex`, `gemini-3-flash-preview`).
- **MCP servers / skills**: counts of configured MCP servers and discovered skills for the agent's harness, loaded from per-harness config files and skills directories on each dispatch (see §9 for the per-harness paths). Display-only; failures degrade to empty counts.
- **Context utilization**: % of model context used, derived from the harness's reported context window and the most recent turn's input/output tokens. Surfaced as a progress bar so the user can see when an agent approaches the auto-compact threshold. Per-harness availability — see "Per-harness sidebar surface" below.
- **Cost / quota signal**: harness-asymmetric per the v1 cost surface (§2). Per-harness availability — see "Per-harness sidebar surface" below. **No raw token counts are surfaced in the UI for any harness** — the underlying tokens are plumbed through the normalized event stream and used to compute context utilization, but the user-facing surface is dollars (Claude) or quota (Codex), not "X input tokens." See §9 for the normalized event vocabulary.

#### Per-harness sidebar surface

Different harnesses expose different telemetry. The sidebar fields below are intentionally asymmetric — empty cells reflect what the harness itself emits (or doesn't), not Switchboard scope:

| Field | Claude Code | Codex | Gemini | Antigravity |
|---|---|---|---|---|
| Status | ✓ | ✓ | ✓ | ✓ |
| Model | ✓ | ✓ | ✓ | ✓ |
| MCP servers (count) | ✓ | ✓ | ✓ | ✓ |
| Skills (count) | ✓ | ✓ | ✓ | ✓ |
| Cost $ (per turn + session aggregate) | ✓ | — | — | — |
| Quota % (window used) | — | ✓ | — | — |
| Context after last turn (%) | ✓ | ✓ | — | — |

**Per-asymmetry rationale**:

- **Cost $** — only Anthropic exposes `total_cost_usd` on completed turns (drawn from the Agent SDK credit pool post-2026-06-15). Codex (subscription auth) does not surface a dollar number; OpenAI's billing model under subscription is quota-based. Gemini's free OAuth tier has no per-turn dollar cost to display. Antigravity surfaces no usage/cost/token data at all (it is a thin client to a server-side agent — all accounting is server-side and never written to disk).
- **Quota %** — only Codex emits a `RateLimitEvent` with `primary.used_percent`, reflecting OpenAI's sliding rate-limit window. Anthropic's tier is closer to a hard limit without a visible counter; Gemini's free OAuth tier and Antigravity likewise expose none.
- **Context after last turn (%)** — Claude reads its window from the stream's `result.modelUsage.<model>.contextWindow`; Codex enriches from the session file's `task_started.model_context_window`. Gemini's session file carries no analogous context-window field, and Antigravity's transcript carries no token/context data either, so neither can compute the ratio. If upstream telemetry adds a context-window field, the asymmetry closes.

Antigravity's model / MCP / skills cells populate from its `SessionMeta` (model parsed from the user-settings envelope; MCP / skills from `~/.gemini/config/`); its cost / quota / context cells render "—".

Empty rows are not Switchboard-side roadmap items — they reflect what the underlying harness emits. If a harness adds a telemetry field upstream, the sidebar surface follows.

Each agent also exposes a context menu of user actions (accessed from the sidebar entry, or from any of its turns in the unified transcript):

- **Fork session** — create a new agent branched from the current state. Native in Claude Code via `--fork-session`. Unavailable for Codex agents in v1 (per resolved 10.14) — the menu item is shown only on Claude Code agents; Codex agents see an explanatory tooltip.
- **Open session file** — open the underlying harness JSONL session file in the user's default editor for inspection or external tooling.
- **Reset / remove** — clean up the agent (CRUD-y; not enumerated in §4 primitives, just a UI affordance).

### Composing and dispatching messages

The user's core action — whether typing a fresh message, forwarding an agent's output, or invoking a saved workflow — is one primitive: **compose a message and dispatch it to one or more agents.** The composition has three components:

- **Source.** What is being sent — any combination of: free-form text the user types, and/or the output from one or more agents (latest turn by default; the user can pick a specific earlier turn). When multiple sources are combined, the optional wrapping prompt is the natural way to control how they're stitched together via template variables. When a source agent is *mid-turn* at send time, the intended behavior is to wait for that turn to complete and forward its finished output — a cross-agent dependency handled by the dependency-resolution layer described under "Agent contention" (shared with workflows; lands in M6). Until that layer exists, forwarding resolves to the source agent's already-completed latest turn at send time.
- **Optional wrapping.** A prompt template from any provider (e.g. `local:code-review`, `tiddly:ai-review-feedback`) that the source(s) are rendered into. May be invoked via slash command in the message bar; the UI may accept a bare name if it matches exactly one configured provider (see §6 resolution rules).
- **Recipients.** One or more agents to receive the (possibly wrapped) message — picked explicitly per send via a recipient picker on the compose bar. No agent is the implicit default; the picker preselects whichever agent the user last sent to (a UI ergonomic, not a semantic privilege). Multi-select picks any combination of agents in the project. A send to N recipients creates N independent **turns** (see "Sends and turns" below). If a recipient is busy with an in-flight turn, the message is **queued** for that agent rather than refused — it dispatches automatically when that agent next goes idle (see "Agent contention" below), and the queued state is visible inline. Recipients that are idle at send time start immediately, so a single send can have some turns running and others queued.

**Sends and turns.** A *send* is one dispatch action — one compose-bar submit (or, later, one workflow step) — and may target one or more recipients. Each recipient's resulting request→response cycle is a *turn*. A multi-recipient send therefore produces multiple **independent** turns, one per agent, that succeed, fail, or are cancelled independently of one another. There is no aggregation across the turns of a manual send; combining several agents' outputs into one downstream message is a workflow concern (§4 Primitive 4 — fan-in).

**Switchboard never silently discards user-authored text.** If the user removes a queued message before it dispatches, its text returns to the compose bar for editing or re-send rather than being lost.

These three components compose freely. Typing a fresh message is just user text + no wrapping + one recipient. A fan-out is user text + optional wrapping + many recipients. Forwarding an agent's output is the agent's turn + optional wrapping + other agents. A **workflow** is the saved (and possibly sequenced) version of one or more of these compositions — see "Invoking a workflow" below.

This compose-and-dispatch surface is also where truly ad-hoc aggregation happens. "Aggregate whatever agents are running right now" is not a workflow — workflows declare their participants up front, even when those participants are supplied dynamically as an invocation-time list. For one-off cases (the user manually kicked off three agents, wants to gather their outputs once they finish), the compose bar is the right surface: pick the source agents, pick a wrapping prompt, dispatch to the recipient.

### Invoking a workflow

A workflow is the **saved, named, optionally sequenced and autonomous version of compose-and-dispatch.** A single-step workflow is functionally identical to a manual send — just persisted under a name for reuse. A multi-step workflow (e.g. fan-out → wait → fan-in → dispatch) adds sequencing and autonomous execution: the user invokes once, the workflow runs through multiple compose-and-dispatch steps automatically.

A workflow is invoked by name. Switchboard prompts for the workflow's inputs (which agents to use, which prompts, any free-form context). The user confirms; the workflow launches and runs autonomously.

### Watching a workflow run

Workflow turns appear in the unified project transcript as they happen — each turn attributed to its producing agent — so the user reads the workflow's progress as a single timeline. The overview sidebar shows real-time per-agent status (which agents are still running, waiting, or have completed their step) alongside cost / quota state. Workflow execution proceeds independently of the user's scroll position or interaction — agents keep running in the background regardless of where the user is reading. When a workflow completes (all turns it initiated have reached a terminal state) or pauses on Primitive 5 (waiting for user input), the user is notified via OS-native notification (per §10 Form factor).

A **workflow-progress surface** (shape TBD — status row in the project header, side panel, or modal) shows each active workflow's name, current step, total steps, and per-step status. Multiple workflows can be in flight simultaneously (when they target disjoint agents — see "Agent contention" below); the surface lists each. When a workflow is paused on Primitive 5 (waiting for user input), the surface shows "step N of M — waiting for your input." For workflows using Primitive 6 (iterate over a list), the surface shows the iteration dimension as well, in the user's own vocabulary — e.g., "iteration 2 of 3 (milestone = "implement-handlers"), step 3 of 8" — using the loop variable name and value the workflow declared. On return after walk-away, this is the first thing the user sees: any workflow that was interrupted is surfaced with the same step (and iteration) detail and options to retry or abandon (see "Walking away" below).

### Agent contention

Switchboard enforces **one in-flight turn per agent** at the application layer. The contention check is a single rule applied at the dispatcher, but it surfaces differently depending on the source:

- **UI compose-bar dispatch:** a send to a busy agent is **queued**, not refused. Switchboard keeps a per-agent FIFO queue; when the agent's in-flight turn reaches a terminal state, the next queued message dispatches automatically. The queued message is visible inline ("queued — agent X is busy") and the user can remove it before it dispatches (its text returns to the compose bar per "Composing and dispatching messages"). The queue is **per-agent and in-memory** — queued-but-undispatched messages do not survive an app restart, consistent with in-flight turns (which also don't).
- **Workflow-step dispatch:** there's no compose-bar UX to gate; the dispatcher refuses the step with a clear error ("agent X is busy, currently running step N of workflow P") and the workflow halts on it as a step failure. This applies uniformly to `send` steps and to `pause_for_user` Mode-2 dispatches — see `docs/workflow-spec.md` §`pause_for_user` for the Mode-2 retry rule (re-enter the pause UI with prior `user_input` pre-filled; require explicit re-submit). The per-agent queue is a *manual-send* convenience; a workflow step fails fast on contention rather than queuing, so an autonomous collision surfaces to the user instead of silently serializing.

The per-agent FIFO queue (manual sends) orders *independent* messages to a single agent; it never makes one message wait on another agent's response. Cross-agent **dependency chaining** — e.g. "send agent A a message built from agent B's not-yet-produced output" — is a distinct, higher layer: a **dependency-resolution mechanism** that holds a message *outside* any agent's queue, waits for the referenced agent's turn to reach a terminal state, then resolves the reference with that turn's real output and dispatches the (now fully-resolved) message into the target agent's lane. The two layers are complementary, not competing: the per-agent FIFO is the contention substrate that gives cross-agent parallelism (independent sends to A and B run concurrently, each in its own lane); the dependency layer sits on top and decides *when* a message is allowed to enter a lane.

**Binding principle — the dependency layer is shared between manual compose-and-dispatch and workflows; manual sends are first-class users of it, not a workflow-only capability.** A workflow is "recorded manual steps" (see "Invoking a workflow"), so any dependency a workflow's auto-forward / fan-in primitives (§4 Primitives 3–4) can express must also be expressible interactively from the compose bar — otherwise workflows would be strictly more powerful than the manual surface they claim to record, which breaks the model. Concretely: if a user, in the conversation, asks to forward agent B's response to agent A while B is still streaming, the system holds A's send, waits for B's current turn to finish, then forwards B's completed output to A (the overwhelmingly-expected behavior — the user wants the *finished* response, not a mid-stream snapshot). This is the same auto-forward edge a workflow `send` step expresses; the manual compose bar and the workflow interpreter must drive one mechanism, not two.

**Sequencing.** v1 builds these in layers: the per-agent FIFO contention queue lands first (M4.4 — manual sends to a busy agent wait for *that* agent), and is the necessary substrate regardless of what sits above it. The dependency-resolution layer lands with the workflow engine (M6), and M6 must expose it to manual compose-and-dispatch, not only to authored workflows (see v1 plan M6). Until M6, the compose bar's "forward another agent's output" source resolves to that agent's *already-completed* latest turn at send time (a snapshot); waiting on an in-flight turn is the M6 capability.

This rule lives in Switchboard, not the harnesses, because **neither harness rejects same-session parallel invocation** — both accept it, both succeed, and the on-disk effects diverge unhelpfully (Claude Code grows an orphan branch in its session tree; Codex silently interleaves both turns into one flat transcript and a future resume cannot tell them apart). See [docs/research/same-session-parallel-invocation.md](research/same-session-parallel-invocation.md) for the probe and the empirical findings. Since the harnesses don't protect us, the dispatcher must.

Two workflows invoked simultaneously that target *disjoint* agents both run normally. The constraint is per-agent, not per-workflow.

**Out-of-band harness use is outside the enforcement layer.** If the user manually invokes `claude --resume <session-id>` or `codex exec resume <thread-id>` in a separate terminal against a session Switchboard is also tracking, the same-session-parallel-invocation hazards apply — Switchboard can't see the external process and won't gate against it. This is the trade-off for not locking users out of their own harness sessions; users who do this are taking on the risk knowingly.

### Failure handling

If a step in a workflow fails, the workflow halts. Partial results are retained. The user sees the error, can inspect the state of each agent involved, and decides whether to retry the workflow, retry from a specific step, or abandon.

A step is considered failed in any of these cases:

- An agent's turn errors (harness `is_error` / `turn.failed`).
- A pre-dispatch resolution fails: prompt ID not found in its provider, MCP server unreachable, agent referenced by name has been deleted.
- A template substitution fails (missing variable, render error).
- An agent contention refusal: the step's target is mid-turn (per §7 "Agent contention"). This counts as a step failure rather than a transient retry condition — it indicates a genuine collision with other in-flight work.
- A user manually cancels an agent's turn while the agent is participating in a workflow step. The workflow is marked **cancelled** (not failed) — the user's cancellation is intent-bearing, identical to clicking cancel-workflow directly. This rule applies uniformly: cancelling any participating agent's turn during a workflow (including just one of N agents in a fan-in step) marks the whole workflow `cancelled`.
- Within a fan-in step (Primitive 4), any participating agent failing fails the whole step. **This rule is under revision for the workflow engine** toward a human-in-the-loop pause when ≥1 sibling is still alive or has produced useful output (rather than auto-cancelling survivors). How a cancelled or failed participant's output is represented in an aggregated result — omitted, marked as cancelled/failed, or surfaced for the user to decide — is an open design question resolved when fan-in failure handling is built (tracked in §11 and the workflow engine plan).

A turn that ends with a tool **permission denial** is *not* a failure. The harness reports the denial (Claude Code's `result.permission_denials`; presumed similar in Codex but not yet verified — see §9), the model receives the denial as feedback and adapts its response, and the turn completes normally. Switchboard surfaces denials as informational ("the model attempted X, was blocked") rather than as workflow-halting errors. Failures are reserved for harness-level errors (`is_error: true` / `turn.failed` / non-zero exit), template substitution errors, and workflow-orchestration errors.

### Walking away

Workflows run inside the Switchboard backend (the Rust core; see §10 Form factor). They keep running as long as the backend process is alive — independent of whether the UI window is visible. Specifically:

- **Minimize / hide the window**: backend keeps running normally; workflow continues.
- **Close the window** (X button): hides the app to the system tray (or dock on macOS). The backend stays up; the workflow continues. The user can reopen the window from the tray icon to check on progress. On Linux desktops without tray support (per §10), Close-the-window quits the app instead of hiding to tray; user is prompted to confirm cancellation of any in-flight workflows first.
- **Quit the app explicitly** (cmd-Q, tray-menu Quit): stops the backend. If any workflows are in progress, Switchboard prompts the user to confirm and then cancels them cleanly (see "Cancelling a workflow or turn" below).
- **Machine sleep**: backend is suspended with the OS. In-flight harness calls may time out across long sleeps; on wake Switchboard surfaces any failed turns and lets the user retry. Workflows themselves don't auto-resume mid-turn.
- **Switchboard crash or OS reboot**: harness subprocesses die with Switchboard's process tree. On next start, any workflow that was in flight is surfaced (via the workflow-progress surface) as "interrupted at step N" — the last step boundary that was successfully checkpointed. For iterated workflows (Primitive 6), the checkpoint also captures the iteration index and value (e.g., "interrupted at iteration 2, step 3"). The user chooses retry-from-step-N or abandon. Mid-step recovery (resuming an interrupted in-flight turn) is out of scope; see 10.3.

When the user returns, Switchboard shows the state of any workflows that completed, are in progress, paused for user input, were cancelled, or were interrupted by a crash.

### Cancelling a workflow or turn

The user can cancel at three granularities:

- **Cancel a workflow.** Stops the workflow's orchestration. Switchboard sends `SIGTERM` to the in-flight harness subprocess (using the process group it spawned in, so single-process Claude Code, Codex's parent+child tree, and Gemini's process tree are all cleaned up uniformly — see §9 and the harness-cancellation research note). Partial results stay: the agent's harness session file persists on disk and can be inspected or sent further messages. The workflow is marked **cancelled** and cannot be auto-resumed — re-invoking starts from the beginning.
- **Cancel a turn.** Kills the spawned harness subprocess for a single agent's in-flight turn (useful if the agent is going off the rails). Same `SIGTERM`-to-process-group mechanism, escalating to `SIGKILL` if the process group doesn't exit promptly. The agent stays around and can be re-prompted; the harness session is in a usable state for the next message; what remains in the harness session file is whatever the harness persisted (Claude Code and Codex persist nothing for the aborted turn — it is simply absent; see §3). Because a multi-recipient send produces independent turns, cancelling one agent's turn does **not** affect its siblings — the others continue and produce their responses (outside a workflow there is no aggregation, so a cancelled turn just means that agent has no response for this send). Cancelling a turn does not clear that agent's queued messages; the next queued message dispatches when the agent goes idle. To stop everything for an agent, the user cancels the in-flight turn and removes its queued messages — two deliberate actions.
- **Cancel a send (fan-out).** A send to N recipients is presented in the unified transcript as one grouped unit (the single user message + its N responses); while any of its turns are still live, the group offers a single **cancel-send** control. Cancelling the send is **scoped to that send**, not to the agents: each recipient's turn is stopped only if the agent's *currently in-flight* turn belongs to this send (`send_id`), and any of this send's messages still *queued* behind a busy agent are removed (their text is not restored — this is an explicit stop, unlike a single queued-message removal). A recipient that already finished this send's turn and moved on to a later, unrelated turn is left untouched. Each stopped recipient becomes an independent `cancelled` outcome (identical to cancelling that turn directly); there is no aggregate "send" outcome — the grouping is a UI affordance over N independent turns. (The model has no notion of a send-level state; see "Sends and turns" above.)

Cancellation is a **distinct outcome from failure.** A cancelled turn is intent-bearing — a human (or, later, the workflow engine, or app shutdown) stopped it deliberately — not a harness error. It is surfaced and recorded as cancelled, internally tagged with its source (user / workflow / shutdown).

While the session is live, Switchboard's in-memory stream buffer holds whatever the agent produced before the kill, so the user can review that partial content in-app. That buffer is **in-memory only**; restarting Switchboard discards it. The *fact* that the turn was cancelled persists: cancelled (and failed) turns are recorded as **outcome markers** in Switchboard's conversation journal (outcome + source/reason + timestamp + agent/send, no content — see §3). On restart, history correctly shows the turn as cancelled (or failed) rather than silently omitting it, and — per §3 — any partial *content* shown after restart comes solely from the harness session file: Switchboard persists none of its own, so the post-restart partial is whatever that particular harness chose to keep (nothing, for Claude Code and Codex). This is the §3 split — Switchboard persists the user's sends and every non-completed turn's *outcome*; agent-produced *content* always comes from the harness session files, and the merge renders whatever they hold.

### Unified history after restart — worked examples

These pin how the §3 split renders concretely after an app restart, when live UI state is gone and the unified transcript is rebuilt by merging the conversation journal (user sends grouped by `send_id`; outcome markers for non-completed turns) with the harness session files (agent-produced content), ordered by timestamp. The three rendered kinds are disjoint — **user messages** come only from the journal, **agent content** only from harness files, **failed/cancelled markers** only from the journal — so the merge needs no correlation or de-dup between sources. Agents below: **B** = Claude, **C** = Codex.

1. **Single completed turn.** You send "hello" to B; it completes. Journal: one `Send`, no outcome (completed turns get none). Harness file: a user-role "hello" and an assistant-role reply. **Renders:** `User → B: "hello"` (from the journal) · `B: <reply>` (harness, assistant-role only). The harness file's user-role copy is dropped — the journal is the canonical record of the user's words.

2. **Fan-out, both complete.** One send to B and C ("status?"), both finish. Journal: two `Send` records sharing one `send_id`. Harness files: each has its own user-role "status?" + assistant reply. **Renders:** `User → B|C: "status?"` **once** (grouped by `send_id`) · `B: <reply>` · `C: <reply>`. The user message is never shown once-per-recipient.

3. **Partially-started fan-out.** You fan out "do X" to B (idle, starts) and C (busy, queued); C's queued turn is removed or the app restarts before it starts. The `Send` is written *at turn-start*, which never happened for C, so the journal has only B's record. **Renders:** `User → B: "do X"` · B's reply. **C does not appear at all** — a never-started turn has no durable existence. (You intended B *and* C, but history shows "→ B"; intended, since C never received the message.)

4. **Failed to start (no `TurnStart` ever).** You send "run build" to B; the journal write succeeds but the harness fails to launch. Journal: a `Send` plus a `Failed` outcome against the minted turn id. Harness file: nothing (the turn never ran). **Renders:** `User → B: "run build"` · `⚠ B: failed — <reason>`. Not an orphan user message with silence after it.

5. **Cancelled mid-stream.** You send "write a long essay" to B; it streams "Once upon a time…", you hit stop. *Live:* the partial text is visible from Switchboard's in-memory buffer. *After restart:* the buffer is gone, and what's shown is whatever the **harness** persisted plus the journal's `cancelled` marker. Claude Code and Codex persist nothing for an aborted turn, so it renders `User → B: "write a long essay"` · `⚠ B: cancelled` — **marker only, no partial**. A harness that *does* persist partial content (Gemini/Antigravity — unverified) would render that content above its marker automatically, with no special handling. Switchboard never persists agent content of its own; the post-restart partial is purely a function of what each harness keeps.

In a fan-out where outcomes differ per recipient (e.g. B completes, C is cancelled — whether individually or via cancel-send), these compose: the user message renders once (`User → B|C`), then B's completed content, then C's `cancelled` marker (with C's partial above it only if C's harness persisted any), interleaved by timestamp. Cancellation/error is always per-turn; there is no aggregate send-level outcome record.

## 8. Worked example: review-and-aggregate

To anchor the abstractions above, here is what a code-review workflow looks like end to end.

**Setup:**

The user has a project `feature-event-logs` open in Switchboard. They have three agents:

- `implementer` (Claude Code)
- `reviewer-claude` (Claude Code)
- `reviewer-codex` (Codex)

All three are listed in the overview sidebar. The unified project transcript shows their conversation history so far in chronological order.

The user has previously authored a workflow in `.switchboard/workflows/review-and-aggregate.yaml`. The review prompt ships as a built-in local prompt (`local:code-review`); the aggregation wrapper is one the user keeps in Tiddly (`tiddly:ai-review-feedback`). Both work because Switchboard resolves each ID against the named provider.

**Invocation:**

1. The user invokes the workflow: "Run review-and-aggregate."
2. Switchboard pops up an invocation form with one field per input the workflow declared in its YAML (`primary_agent`, `reviewer_agents`, `review_prompt`, etc. — see §4 for the schema). The user fills in:
   - **`primary_agent`** → `implementer`
   - **`reviewer_agents`** → `reviewer-claude` and `reviewer-codex` (multi-select)
   - **`review_prompt`** → `local:code-review` (bundled with Switchboard)
   - **`aggregation_prompt`** → `tiddly:ai-review-feedback` (the user's own, stored in Tiddly)
   - **`user_context`** → "Review milestone 1, focus on the event-emission API."
3. The user confirms. The workflow launches.

**Execution:**

1. Switchboard sends the review-prompt message (with user context appended) to both reviewers in parallel. Each reviewer runs.
2. Switchboard waits for both reviewers to complete their turns.
3. Switchboard collects both reviewers' final responses.
4. Switchboard composes the two reviews into a single aggregated text blob (canonical shape per `docs/workflow-spec.md`), then renders the aggregation-prompt template with that blob bound to the prompt's text argument. Because the aggregation prompt is a generic Tiddly prompt that takes a single `{{ feedback }}` argument, no Switchboard-specific authoring is needed — the helper does the formatting.
5. Switchboard sends the rendered message to `implementer` (the agent supplied as the workflow's `primary_agent` input).
6. The implementer runs and produces its response.
7. Workflow complete. The user is notified.

**During execution:**

Each reviewer's turn appears in the unified project transcript as it streams — attributed to `reviewer-claude` and `reviewer-codex` respectively — so the user reads both reviews interleaved chronologically as they land. The overview sidebar shows real-time status (running → completed) and per-agent cost / quota for each reviewer. When the implementer kicks in, its aggregated turn appears next in the same transcript, attributed to `implementer`. The workflow-progress surface (per §7) shows where the workflow is overall — e.g., "review-and-aggregate: step 2 of 3 (waiting on reviewers)" — alongside the transcript and sidebar.

**Afterwards:**

The user reads the implementer's response and decides what's next. Common follow-ups: compose-and-dispatch the response onward (e.g., forward to a follow-up agent with a wrapping prompt — see §7 "Composing and dispatching messages"), invoke another workflow, or just stop. The workflow is done; the next move is the user's.

**Variation worth noting:** the same workflow could insert a Primitive 5 (Pause for user input) step between the aggregation and the implementer dispatch — letting the user read the aggregated reviews and weigh in (approve, redirect, add context) before the implementer runs. The workflow then dispatches the user's input together with the aggregation to the implementer. This is the natural shape when you want the autonomous fan-out-and-aggregate work to happen without you, but reserve the judgment moment for yourself.

## 9. Harness integration

Switchboard interacts with Claude Code, Codex, and Gemini through their non-interactive modes (`claude -p`, `codex exec`, `gemini -p`). The underlying sessions are real harness sessions backed by each harness's own session files — they survive Switchboard, can be resumed later, and could in principle be opened in the harness's interactive TUI by the user if they wanted. Switchboard does not lock the user out of the harness; it just drives it.

The architectural backbone of this section is the **per-harness adapter** pattern (design-pattern sense): one adapter per harness translates that harness's native event stream into a normalized internal stream the rest of Switchboard consumes. This keeps the workflow engine, UI, and persistence layer harness-agnostic, while letting each adapter handle its own quirks (event vocabularies, exit-code semantics, session-file richness, etc.). See "Per-harness adapter and normalized event stream" below for the shape. Per-harness ground truth lives in [harness-behavior.md](research/harness-behavior.md) (raw probes archived under `research/archive/`).

### Process model

Per-message process spawn for v1: each turn invokes `claude -p --resume <session-id>`, `codex exec resume <session-id>`, `gemini -p --session-id <uuid>` (first turn) / `gemini -p --resume <uuid>` (subsequent turns), or `agy -p <prompt> [--conversation <uuid>] --dangerously-skip-permissions` (Antigravity), captures the output, and exits. State persists in the harness's session files between invocations. Long-lived agent processes can be considered later if latency matters.

**Antigravity is the structural outlier.** `agy` is a thin client to a server-side agent with a contract unlike the other three: no structured stream (plain markdown drips to stdout, no `--output-format` flag — and stdout can't be used for content because it replays prior answers on resume, so the adapter tails `transcript.jsonl` for all content instead); the conversation UUID is **server-assigned**, captured post-spawn by watching for a new `~/.gemini/antigravity-cli/brain/<uuid>/` directory and persisted to a per-agent sidecar (the `AgentRecord.session_id` stays `None`, like Codex); resume passes the captured UUID via `--conversation`; and the process exit — not a stream/transcript record — is the authoritative turn terminator (`agy` exits 0 on essentially every condition, so the exit code is useless for outcome detection). Auth is macOS-Keychain-only, and a stale token triggers an interactive-OAuth fallback the adapter detects on stdout and force-kills. Full ground truth in [harness-behavior.md](research/harness-behavior.md).

**Why both Gemini and Antigravity exist.** Both root under `~/.gemini/`, which can look redundant. Gemini remains supported for users whose Gemini CLI path still works (e.g. paid Workspace); Antigravity (`agy`) is added because Google directed free / Pro / Ultra users to it as the Gemini CLI's replacement for those tiers (observed 2026-05; see [`archive/antigravity-cli-observed.md`](research/archive/antigravity-cli-observed.md)). They are separate adapters with separate config paths and contracts — see that doc for the contract diff.

Switchboard runs `claude -p` in its **default** mode (no `--bare`) so the agent inherits the user's full environment: skills, hooks, plugins, MCP servers, CLAUDE.md, and auto-memory all load exactly as they would in an interactive session. The Codex equivalent (we do not pass `--ignore-user-config` or `--ephemeral`) gives the same outcome: the user's `~/.codex/config.toml` and session persistence are honored. This is deliberate — Switchboard's value is to orchestrate normal Claude Code / Codex sessions, not to amputate them. Anthropic has stated that `--bare` will become the `-p` default in a future release; when that happens, Switchboard will need to pass equivalent context-loading flags (`--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, `--append-system-prompt`) to preserve current behavior. To make that change a one-place edit, harness command-line construction is centralized in a single "harness invoker" helper from day one. Tracked under open question 10.9; full background in [docs/research/archive/claude-code-headless.md](research/archive/claude-code-headless.md).

Switchboard consumes the harness stream by spawning the process, reading stdout line-by-line as JSONL, and dispatching each event into the normalized event stream described below. Standard pipe-and-readline; no file-watching for the basic case. Per-harness stream details live in the per-harness observed-behavior docs under `docs/research/`.

**Process group**: the harness is spawned in its own process group (Rust: `Command::process_group(0)`) so cancellation can `killpg` the entire group with one signal. This handles both Claude Code (single process) and Codex (Node parent + Rust child) uniformly — verified empirically; see the cancellation sections of the per-harness research notes. **Note**: Codex's parent process catches `SIGTERM` and exits with code `0`, so Switchboard cannot detect cancellation from the exit code alone — it relies on the absence of a terminal event in the stream (`turn.completed` or `turn.failed`).

**Per-harness flag baselines** for scope-gatekeeping checks that the harness imposes by default:

- **Codex** passes `--skip-git-repo-check` unconditionally. Switchboard's safety guidance about git-tracked projects (see §9 "Safety guidance") sits at a higher layer than Codex's own check; we don't want Codex refusing to spawn in a non-git directory just because the user hasn't initialized a repo yet.
- **Gemini** passes `--skip-trust` unconditionally. Gemini's workspace-trust gate otherwise blocks headless dispatches by default. Switchboard's bound cwd is by definition the user's working directory — the gate's question is already answered — so the adapter asserts this every spawn.
- **Antigravity** passes `--dangerously-skip-permissions` unconditionally — the analog of Gemini's `--skip-trust` / `--yolo`. Without it, tool calls would block on interactive permission prompts that a headless dispatch can't answer. The bound cwd is the user's own workspace, so per-tool approval is auto-granted, consistent with the max-autonomy posture (§11).

### Permissions and sandboxing

For v1, Switchboard runs all four harnesses with maximum autonomy — skip-permissions is effectively required, not optional:

- Claude Code: `--dangerously-skip-permissions`
- Codex: `--dangerously-bypass-approvals-and-sandbox` (also accepts `--yolo` as an undocumented alias in 0.128.0; relying on the long form is safer)
- Gemini: `--yolo`
- Antigravity: `--dangerously-skip-permissions`

This is a deliberate v1 simplification. Headless mode has no native interactive permission prompt UX, and building one inside Switchboard (intercept denials at runtime, modal-prompt the user, re-issue the turn) is non-trivial work that we don't want to gate v1 on. Granular permission control is a deferred design decision — see §11.

**Known issues to track:**

- Codex has open bugs around `--dangerously-bypass-approvals-and-sandbox` not fully bypassing in all sub-modes (e.g., a recent regression where the directory-trust prompt fires anyway). Switchboard should pin tested Codex versions and surface any unexpected prompts as errors.
- Codex separates approval policy from sandbox mode. The MVP collapses these; v2 may expose them separately.

### Safety guidance

Switchboard's v1 posture (autonomous agents with full filesystem and shell access, workflows that can run unattended) makes git-based projects strongly recommended: uncommitted local damage is recoverable; surprise rewrites of untracked files are not. Concrete guidance:

- Run workflows inside a project that's checked into git.
- Commit work in progress before invoking long-running workflows.
- Treat unattended runs as risky-by-default.

A one-time first-launch acknowledgement dialog (with a checkbox the user must tick to proceed) surfaces this posture explicitly so users opt in knowingly rather than discovering the autonomy posture after damage. This is a v1 acceptance item.

**Why not a more constrained default?** We considered shipping with `--sandbox workspace-write` (Codex) and an equivalent constrained `--permission-mode` for Claude Code. Both block network access and out-of-cwd shell writes, which would silently break the agent capabilities Switchboard exists to coordinate: a reviewer using `gh pr view`, an implementer running `cargo build` against a new dependency, any agent calling an external API or running a tool that fetches data. The choice would make the safe default the *broken* default, training users to bypass the safety guard reflexively — worst of both worlds. Full autonomy is the default that makes the headline workflows actually run. Granular permission control (allowlists, per-agent constraints, per-workflow scoping) is tracked in §11 for users who want a more restrictive posture later.

### Harness capabilities Switchboard depends on

The capabilities and behaviors Switchboard needs from each harness, with notes on what is exposed natively, derived, or unavailable. Hands-on probe results are distilled in [harness-behavior.md](research/harness-behavior.md) (raw captures under `research/archive/`).

| Capability | Claude Code | Codex | Gemini | Antigravity |
|---|---|---|---|---|
| Spawn with explicit flags | native | native | native | native |
| Send + capture structured stream | native | native | native | **unavailable** (no structured stream; all content tailed from `transcript.jsonl`, stdout is a control channel only) |
| Detect turn completion | native | native | native | derived (process exit; no terminal stream/transcript record) |
| Detect errors | native | native (asymmetric payload) | native | derived (stdout scan; `agy` exits 0 on essentially everything) |
| Resume by UUID | native | native | native | native (`--conversation <uuid>`) |
| Assign session ID at spawn | native (`--session-id`) | unavailable (captured from stream → sidecar) | native (`--session-id`) | unavailable (server-assigned; captured from `brain/<uuid>/` → sidecar) |
| Fork from checkpoint | native (`--fork-session`) | unavailable in v1 (per resolved 10.14) | unavailable in v1 | unavailable in v1 |
| Read context window in stream | native (`result.modelUsage`) | unavailable (session-file only) | unavailable (no analog field) | unavailable (no token/context data anywhere — server-side) |
| Read cost in stream | native (`total_cost_usd`) | unavailable (subscription quota model) | unavailable (free OAuth tier) | unavailable (server-side) |
| Read rate-limit / quota | unavailable (no public stream signal) | native (`token_count.rate_limits` in session file) | unavailable | unavailable |
| Tool calls + results in stream | native (typed blocks) | native (`command_execution` only) | native (filtered `update_topic`; output empty for read-like tools) | derived (from `transcript.jsonl` tail, not a stream) |
| Programmatic compaction | unavailable (auto only) | unavailable (auto only) | unavailable (auto only) | unavailable (server-side; auto behavior unverified) |
| Capture permission denials | native (`result.permission_denials`) | presumed (verification deferred) | not yet probed | n/a (auto-approved via `--dangerously-skip-permissions`) |
| Run agents concurrently | confirmed | presumed | confirmed | confirmed (distinct server-assigned UUIDs; no prefix-collision hazard) |

**Per-row notes** (only where the matrix needs elaboration):

- **Send + capture structured stream**: Claude `claude -p --output-format stream-json`; Codex `codex exec --json`; Gemini `gemini -p --output-format stream-json`. **Antigravity has no structured-stream mode** — `agy -p` drips plain markdown to stdout, but the adapter ignores stdout for content (it replays prior answers on resume) and tails `transcript.jsonl` for all displayed content; see "Antigravity is the first adapter that builds its live stream from a file tail" above.
- **Detect turn completion**: single terminal event per turn for the three structured-stream harnesses. Claude: `result`. Codex: `turn.completed` (success) or `turn.failed` (error). Gemini: `result.status:"success"` or `result.status:"error"`. The adapter waits for whichever terminal shape the harness emits. **Antigravity is the exception** — it has no terminal stream/transcript record, so the adapter uses **process exit** as the turn terminator (see Process model).
- **Detect errors**: Claude uses `result.is_error` and/or `result.api_error_status` (do **not** rely on `result.subtype` — stays `"success"` even on error). Codex: `turn.failed` event terminates the turn with the API error in the payload. Gemini: `result.status:"error"` with `error.message` — auth failures detected via substring match per `is_gemini_auth_failure_message`. Those three exit non-zero on error. **Antigravity is the exception** — `agy` exits 0 on essentially every condition, so the adapter scans stdout for `Error:` / `Authentication required` lines instead of trusting the exit code.
- **Assign session ID at spawn**: Claude and Gemini both let the caller pass `--session-id <uuid>`. Codex and Antigravity assign their own server-side and Switchboard captures it (Codex from the first stream event; Antigravity from the new `brain/<uuid>/` directory) and persists to a per-agent sidecar. Note: Gemini uses UUID **v4** (not v7) because Gemini's session-file filename uses only the first 8 hex chars and v7s minted in the same millisecond share that prefix — see `gemini/mod.rs` for the rationale.
- **Fork from checkpoint**: Claude `--fork-session` with `--resume`. Codex has no non-interactive `codex exec fork`; Gemini and Antigravity have no equivalent. The Fork affordance surfaces only on Claude Code agents; the others show a tooltip explaining the gap.
- **Read context window in stream**: Claude is the only harness with this stream-side. Codex enriches from the session file's `task_started.model_context_window` per resolved 10.15. Gemini has neither a stream field nor a session-file analog, and Antigravity exposes no token/context data anywhere (all server-side) — the context-utilization bar is hidden for both (per the per-harness sidebar matrix in §7).
- **Read cost in stream**: Claude only (Agent SDK credit pool post-2026-06-15). Codex doesn't expose dollar costs under subscription auth; Gemini's free OAuth tier doesn't either; Antigravity surfaces no cost data at all.
- **Read rate-limit / quota**: Codex only — emitted as a `RateLimitEvent` enriched from the session file's `token_count.rate_limits` post-terminal. The other three harnesses don't surface a comparable signal.
- **Tool calls + results in stream**: structurally divergent across harnesses. Claude emits typed `tool_use` / `tool_result` content blocks with named tools (including MCP). Codex routes everything through `command_execution` items (raw shell commands with `aggregated_output` and `exit_code`). Gemini emits `tool_use` / `tool_result` events; the adapter filters the `update_topic` internal tool, and `tool_result.output` is empty for read-like tools like `read_file` (the real content lives only in the session file — surfaces on transcript hydration). Antigravity has no tool events in any stream — they're recovered from the `transcript.jsonl` tail (`PLANNER_RESPONSE.tool_calls` → `ToolStarted`; the following tool-result record → `ToolCompleted`, paired FIFO). Switchboard renders these differently per harness; no single unified rendering. **No raw token counts are surfaced in the UI for any harness** — tokens are plumbed through the event stream and used internally (context utilization, debugging), but the user-facing surface is dollars (Claude) or quota (Codex) or empty (Gemini / Antigravity).
- **Permission denials**: Claude's `result.permission_denials` is informational (not a turn-error); the model receives them as feedback and adapts. Codex and Gemini behaviors are presumed similar but unverified — §7 wording is hedged accordingly. Antigravity runs with `--dangerously-skip-permissions`, so tool calls are auto-approved and no denial signal is produced.
- **Run agents concurrently**: Claude confirmed via three parallel `claude -p` invocations producing three independent session files. Gemini confirmed via the M3.1 collision probe (two concurrent processes with deliberately-colliding 8-char-prefix UUIDs surfaced the documented prefix-collision hazard, but both invocations completed independently). Codex's process model and per-session-file isolation suggest the property holds; explicit verification deferred. Antigravity gets a distinct server-assigned UUID per conversation (no filename-prefix collision hazard), and the adapter correlates each dispatch to its conversation by exact prompt match — so byte-identical concurrent prompts in one cwd fail loud rather than mis-bind.

### Per-harness adapter and normalized event stream

The four harness streams (Claude Code, Codex, Gemini, Antigravity) are structurally different (event-name vocabularies, content shapes, where context-window / rate-limit / cost info appears, what's in the session file vs. stream-only). To keep the rest of Switchboard harness-agnostic, the harness layer is organized around **per-harness adapters** that translate native events into a normalized internal event stream the rest of the system consumes.

Each adapter is responsible for: building the harness command line, spawning the process, parsing its native stream, normalizing into the event vocabulary below, and surfacing harness-specific metadata in `raw` so callers that need to dig in can. The workflow engine, UI, and persistence layer consume only the normalized stream.

**Antigravity is the first adapter that builds its live stream from a file tail, not stdout.** Claude / Codex / Gemini parse a single structured JSONL stream from stdout. Antigravity has no structured stream, so its adapter tails the conversation's `transcript.jsonl` for **all** displayed content — assistant text (`ContentChunk { Text }`), `thinking` (`ContentChunk { Thinking }`), and tool lifecycle (`ToolStarted` / `ToolCompleted`) — which is the same source hydration reconstructs from on project reopen. **stdout is a control channel, not a content source**: `agy` replays the whole conversation's prior answers to stdout on a resume turn (so emitting it would make each turn accumulate every earlier answer), and the adapter reads it only for the auth-failure fast-fail, `Error:` lines, and a "produced output" liveness signal (used to distinguish output-without-a-readable-answer from no-output — not a success signal; a turn completes only on a transcript terminal answer). The cost of transcript-sourcing is that the answer lands when its record is written (turn completion) rather than char-streaming; thinking and tools still stream live as their records arrive. See [harness-behavior.md](research/harness-behavior.md).

#### Event vocabulary

The authoritative struct definitions live in [`crates/harness/src/events.rs`](../crates/harness/src/events.rs). The summary here is for design-doc readers who need a one-line orientation per event without opening the code.

| Event | Purpose |
|---|---|
| `SessionMeta { agent, model, harness_version, tools, mcp_servers, skills, raw }` | Emitted once at first turn. Carries session-scope metadata for the sidebar. Per-field sources differ by harness — see table below. |
| `TurnStart { agent, session_id }` | A turn has begun. The UI uses this to lock the compose bar for the affected agent and start the busy indicator. |
| `ContentChunk { agent, kind: thinking \| text, data }` | A piece of model output. The `kind` discriminator lets the UI render reasoning blocks separately from final response text. |
| `ToolStarted { agent, tool_use_id, kind, input }` | A tool call was dispatched. Fires *before* the result lands so the UI can show "running tool... (3.2s elapsed)" rather than appearing frozen. |
| `ToolCompleted { agent, tool_use_id, output, is_error }` | The tool returned. Paired with its `ToolStarted` via `tool_use_id`. |
| `TurnEnd { agent, outcome, ended_at, usage? }` | Terminal event. Exactly one per turn. `usage` carries token counts and (where available) `context_window` / `total_cost_usd`. See *Terminal outcome variants* below. |
| `RateLimitEvent { agent, info }` | Harness-specific quota signal, surfaced for UI display. The workflow engine doesn't interpret it. Codex emits this post-terminal from the session file; Claude, Gemini, and Antigravity have no comparable signal. |

#### Terminal outcome variants

`TurnEnd.outcome` is the discriminator that distinguishes success from each failure class:

- **`Completed`** — harness reported a clean terminal event.
- **`Failed { kind: HarnessError, message }`** — harness's terminal event reported `is_error`. Causes: bad model name, rate limit, transient API error, invalid prompt content. The harness gave us a clean error signal.
- **`Failed { kind: AdapterFailure, message }`** — synthesized by the adapter. Causes: subprocess died, parser hit malformed JSON, or stdout EOF arrived without a terminal event (e.g., Codex parent silently exits 0 on SIGTERM). Infrastructure-level; not the user's fault.
- **`Failed { kind: AuthFailure, message }`** — subscription / tier auth is missing or expired. Detected per-harness via stream substring match: Claude `assistant.error == "authentication_failed"`; Codex `turn.failed.error.message` contains `"401 Unauthorized"`; Gemini per `is_gemini_auth_failure_message` (see `gemini/parser.rs`). Distinct from `HarnessError` so the UI can render an auth-specific banner rather than a generic error.
- **`Cancelled { source: User | Workflow }`** — reserved for M4 when per-turn cancellation lands.

**Why a single terminal event type.** Terminal status lives in `outcome` — there is no separate `TurnAborted` / `TurnTimeout` / `TurnCancelled` wire event. The `kind` field on `Failed` lets consumers (UI, partial-failure rules) distinguish causes without proliferating variants at the event level. Exactly one `TurnEnd` per turn, always.

#### SessionMeta per-harness sources

For each `SessionMeta` field, where each harness sources the value live (from the stream) and on rehydration (parsing the session file plus any Switchboard-side fallback). Empty cells mean Switchboard returns the field's default (empty string or empty vec) for that harness on that path.

| Field | Claude (live) | Claude (rehydration) | Codex (live + rehydration) | Gemini (live) | Gemini (rehydration) | Antigravity (live + rehydration) |
|---|---|---|---|---|---|---|
| `model` | `system/init` stream event | session-file `system/init` record | session-file first `turn_context` record | stream-init event | last gemini-record's `model` field | parsed from the `USER_INPUT` record's `<USER_SETTINGS_CHANGE>` envelope (only present when the model changed — empty otherwise; the reducer keeps the prior model on empty) |
| `harness_version` | `system/init.claude_code_version` | session-file `system/init` | session-file `session_meta.cli_version` | lazy `gemini --version` (cached `OnceLock`; `""` on probe failure) | empty (session file lacks the field) | live: lazy `agy --version` (cached `OnceLock`); rehydration: empty |
| `mcp_servers` | `system/init.mcp_servers` (Claude pre-merges scopes) | Switchboard reads `~/.claude.json` (user + local) + `<cwd>/.mcp.json` (project) | Switchboard reads `~/.codex/config.toml` + `<cwd>/.codex/config.toml` | adapter loader injects post-parse | Switchboard reads `~/.gemini/settings.json` + `<cwd>/.gemini/settings.json` | Switchboard reads `~/.gemini/config/mcp_config.json` (user scope only) |
| `skills` | `system/init.skills` (Claude pre-merges scopes) | Switchboard directory-scans `~/.claude/skills/` + `<cwd>/.claude/skills/` | Switchboard directory-scans `~/.agents/skills/` + `<cwd>/.agents/skills/` | adapter loader injects post-parse | Switchboard directory-scans `~/.agents/skills/` + `<cwd>/.gemini/skills/` | Switchboard scans `~/.gemini/config/plugins/<plugin>/skills/<skill>/SKILL.md`, displayed as `<plugin>/<skill>` (user scope only) |
| `tools` | `system/init.tools` (merged builtin + MCP + dynamic list) | empty | empty | empty | empty | empty |

**Cross-harness invariants** that apply to both `mcp_servers` and `skills`:

- **Project / workspace scope wins on name collision.** Consistent with `git`, `npm`, every other config-stacking system, and the more-specific-scope-wins convention used by every registry-style loader in Switchboard. Tests pin direction (not just dedup) in each loader.
- **`~/.agents/skills/` is a shared multi-harness user-scope path** used by both Codex and Gemini. Changes to its layout affect both loaders — audit together if path semantics ever shift.
- **Display-only.** Failures to read config files / scan directories emit empty lists with a warning. Loader errors never propagate as `Result::Err`; dispatch is unaffected.

The `tools` field is preserved across the wire for the populated Claude-live path; it's reserved for a future symmetric registry surface across harnesses (Codex and Gemini have no `init`-event tools list to draw from).

#### Notes

- **Codex session-file dependency.** Reading Codex session files in addition to the `--json` stream is a committed v1 dependency for the Codex adapter (per resolved 10.15) — needed to fill in gaps the stream doesn't expose (`rate_limits`, `model_context_window`, full reasoning blocks, `session_meta`). Multiple §7 and §9 commitments depend on this enrichment.
- **Forward-compat — §7 workflow status taxonomy.** §7 describes workflow-level status as "complete, cancelled, failed, interrupted." After M4 lands `Cancelled` as a top-level `TurnOutcome` variant, the per-turn vocabulary will be `Completed | Failed | Cancelled` — three statuses, not four. Where does "interrupted" map? Two reasonable resolutions for when M4 expansion picks this up: (a) fold into `Cancelled { source: User | Workflow | Signal }`, with `Signal` covering SIGINT/SIGTERM (cleanest — one variant, source field discriminates); (b) keep `Interrupted` as its own top-level `TurnOutcome` variant for OS-signal-driven shutdowns specifically. Decision deferred to M4, but flagged here so §7 and §9 don't silently drift in the meantime.

### Passthrough mechanism

For harness commands Switchboard does not need to coordinate, the design *intent* is a passthrough: the user types a harness slash command (e.g., `/model`, `/clear`) when interacting with an agent, and Switchboard forwards it to the harness verbatim. This avoids reimplementing every harness feature.

**Important caveat — CLI-path limitation:** The `claude -p` CLI does not accept slash commands as input — that includes built-in commands (`/model`, `/clear`) and user-invoked skills (`/skill-name`). The CLI gap is tracked upstream at [anthropics/claude-code#837](https://github.com/anthropics/claude-code/issues/837) and [#38505](https://github.com/anthropics/claude-code/issues/38505). Note this is a CLI-path limitation specifically: the Claude Agent SDK does support headless slash-command dispatch, so a future SDK-based adapter could close this gap. v1 stays on the CLI path because Codex's and Gemini's integrations are also CLI-only; an SDK-vs-CLI split across the harness adapters would create avoidable asymmetry. Until then, Switchboard's passthrough is constrained to commands we can implement out-of-band: `/model` implemented by re-spawning with a different `--model` flag, and so on. Tracked under open question 10.10; see [docs/research/archive/claude-code-headless.md](research/archive/claude-code-headless.md) for sources. (The auto-invoked side of skills is unaffected — see §6.)

**Open question 5.2:** Exact mechanism for passthrough — does it require a prefix to disambiguate from Switchboard's own slash commands, or do Switchboard's commands live in a separate namespace? Partially blocked on the upstream limitation above.

### What we lose by going non-interactive

The interactive Claude Code, Codex, Gemini, and Antigravity TUIs are not used. Switchboard renders the output itself. This means rendering tool calls, diffs, todo lists, and thinking blocks is Switchboard's responsibility.

What is **preserved** because the harness still runs in default mode: hooks fire, MCP servers connect and tools work, auto-invoked skills trigger normally, sub-agents (Claude Code's `Task` tool) spawn as expected, auto-compaction runs when context climbs.

What is **lost** in headless mode:

- **Plan mode** (Claude Code's interactive plan/approve cycle) — REPL-only; no headless equivalent.
- **User-invoked slash commands** — `/model`, `/clear`, `/compact`, and `/skill-name` are not accepted as input in `claude -p`. See the passthrough section above and open question 10.10.
- **Programmatic compaction** — Claude / Codex / Gemini all auto-compact; none expose a triggerable `/compact` from headless. (Antigravity's compaction is server-side and unverified.) See open question 10.11.
- **The harness's own TUI rendering** — Switchboard renders everything itself from the stream.

### Integration testing

Switchboard's per-harness adapters are exercised by a **live-harness test suite** that runs against the real, installed Claude Code, Codex, Gemini, and Antigravity CLIs — not mocks. This is critical: adapter correctness depends on harness behavior we don't control (event vocabularies, exit codes, stream timing, session-file format), and fixture-only tests would silently lock in our current understanding while upstream releases drift. Live tests catch those regressions when a developer runs them locally.

**Live tests are developer-local, not CI.** Subscription auth tokens (the only supported auth in v1) tend to rotate on use and can be device-bound, which makes them brittle as CI secrets and creates a non-trivial blast radius if leaked. The trade-off: upstream CLI changes are detected reactively (when a developer runs `make test-live`) rather than proactively via scheduled CI. The fixture-driven adapter tests are the default suite, run in CI; live tests run on demand. Full policy in [AGENTS.md](../AGENTS.md) "Live testing against real harnesses."

To keep the live suite affordable in time and subscription cost, every test prompt is constrained to a small response (e.g., "reply with the single word 'ack'", not "write me a poem"). Modern Claude / Codex / Gemini usage limits are generous enough that even a thorough suite runs in minutes for a negligible cost per run. The constraint is per-test response size, not test count.

Switchboard's own logic (workflow parser, prompt resolver, MCP client, normalized event dispatcher) is covered separately by ordinary unit tests with the harness mocked at the adapter boundary.


## 10. Form factor and distribution

### Form factor: single-binary desktop app

Switchboard ships as a **single-binary desktop application** rather than a TUI or browser-based tool. Reasoning:

- The UX vision (unified per-project transcript with per-agent attribution, real-time per-agent status sidebar, native context menus, slick aesthetics) is desktop-shaped — TUIs can approximate it but always feel cramped at the high end.
- Single-binary distribution: download an installer or run a package-manager command, double-click. No language runtime prereq, no browser tab to manage, no separate server to start.
- Native OS integration: dock icon, system notifications, native file dialogs, proper window management, system tray.
- The "anyone who wants" audience benefits more from a polished desktop app than from either a TUI or a browser-tab UX.

### Framework: Tauri (Rust core + WebView frontend)

[Tauri](https://tauri.app/) is the chosen framework. Reasons:

- **Single small binary** (~3 MB Hello World, vs Electron's ~150 MB). Sub-half-second startup, low memory footprint.
- **OS-native WebView** for rendering — WebKit on macOS, WebView2 on Windows, WebKitGTK on Linux. ~99% of modern web tech works identically across platforms. (Linux's WebKitGTK lags WebKit-on-macOS in version and bug surface; sticking to widely-supported CSS/JS features avoids cross-platform rendering surprises.)
- **Rust core** handles backend logic: filesystem, harness adapters, MCP client, IPC handlers. Single-process app — no Python subprocess, no separate server.
- **Web frontend** (HTML/CSS/JS) renders the UI in the WebView and talks to the Rust core via Tauri's typed command system: the WebView calls Rust functions via `invoke()` (typed inputs and return values), and the Rust core streams events back via Tauri's event API (the harness streams Switchboard receives are republished as events the frontend subscribes to). Standard web tech, any framework. Event emission from the Rust core to the WebView is fire-and-forget; the core owns a bounded per-agent ring buffer that the frontend renders the latest state from. UI lag or a collapsed pane never blocks the harness adapter or the workflow engine — buffer sizing and per-event coalescing rules are implementation choices.
- **Native OS integration** via Tauri plugins (notifications, file dialogs, system tray, auto-updater). On Linux, system tray availability depends on the desktop environment (GNOME requires the AppIndicator extension); Switchboard falls back to a windowed-only mode where tray is unavailable.
- **Tauri 2.x** (released 2024) is mature with cross-platform desktop support and a growing plugin ecosystem.

Other options were considered (Electron, local web UI, TUI). Comparison and reasoning captured in [docs/research/desktop-framework-evaluation.md](research/desktop-framework-evaluation.md).

**Architecturally: one process.** Rust core + WebView are bundled together inside the Tauri app. Harness subprocesses (Claude Code, Codex) are spawned by the Rust core but live within Switchboard's process tree. There is no separate backend server, no sidecar process, no Python or Node runtime to install.

### Backend language: Rust

Follows from Tauri. The Rust core handles filesystem access, harness adapters (spawning and stream-parsing `claude -p` / `codex exec`), the MCP client (for prompts), and the Tauri command handlers the frontend invokes. Single-process app: no separate backend server, no language-runtime prereq for the user.

### MCP client

The MCP client uses the [official Rust MCP SDK (`rmcp`)](https://github.com/modelcontextprotocol/rust-sdk), currently **Tier 2** in the [MCP SDK Tiering System](https://modelcontextprotocol.io/community/sdk-tiers).

What Tier 2 means concretely vs Tier 1 (Python, TypeScript, Go, C#):

- Slower SLAs: issue triage within a month vs 2 days; critical bug fixes within 2 weeks vs 7 days; new protocol features within 6 months vs same-release.
- Smaller community for examples and prior art.
- Up to 20% of conformance tests may fail (vs 100% pass for Tier 1).

Functionally adequate for Switchboard: `prompts/list` and `prompts/get` are supported and stable, which is what we need. The Tier 2 risks (slow upstream evolution, slower bug response) are acceptable given the architectural wins of staying in Rust. Full discussion in the research note.

If MCP evolves faster than `rmcp` can track, fallback paths include (a) wrapping a Tier 1 SDK via FFI / sidecar, or (b) moving the MCP layer to a TypeScript companion process invoked over IPC. Both preserve the Rust core's other architectural wins; both are larger changes than we'd want to make casually but are not architectural rewrites.

### Frontend stack: Svelte + Tailwind CSS

The UI is built with **Svelte 5** and **Tailwind CSS**, with components from **shadcn-svelte** as needed. Reasons:

- Svelte produces smaller bundles than React with less boilerplate.
- Reactive model and minimal ceremony are well-suited to AI-agent-written code.
- Tailwind for styling; no separate component framework required.
- shadcn-svelte for design-system primitives (modal, tabs, accordion) when we want them.

React + Tailwind + shadcn/ui is a viable alternative if a future contributor or rewrite prefers the bigger ecosystem. The architectural decisions (Tauri shell, Rust core) don't depend on the frontend framework.

### Distribution: signed native binaries

Primary install paths per platform:

- **macOS**: `brew install switchboard` (via a Homebrew tap) or direct `.dmg` download.
- **Linux**: `.deb` / `.rpm` packages, or direct binary download for other distros.
- **Windows**: `.msi` installer or direct `.exe` download.

Cross-platform builds via Tauri's bundling pipeline. Code signing (Apple Developer ID for macOS, Authenticode for Windows) is required for friction-free installs and is part of the v1 release infrastructure work.

Tauri's built-in updater is wired in from day one so users get version updates inside the app.

For developers running from source: standard `cargo tauri dev` workflow.

## 11. Deferred decisions

Decisions explicitly **not made** in this document, to be addressed in later docs or after early implementation:

- **Long-lived agent processes.** Per-message spawn for v1; may revisit if latency dominates. Cold-start latency should be benchmarked once early in implementation (default-mode `claude -p` loads skills, hooks, MCP servers, plugins, CLAUDE.md, and auto-memory each turn); revisit if consistently >2s.
- **Visual workflow editor.** v1 is file-based, with agent-consumable authoring docs as the supported authoring path.
- **Granular permission / sandbox config.** v1 runs all four harnesses with maximum autonomy (skip-permissions on); the off / restricted-mode user experience is deferred. Plausible future directions: **config-driven tool allowlists** (YAML per project / per agent, passed through as `--allowedTools` / `--permission-mode`), **interactive permission prompts** (Switchboard intercepts denials at runtime and pops a modal asking the user to allow / deny / always-allow, then re-issues the turn — pending a probe of harness resume-after-denial mechanics), and **per-workflow permission scoping** (a workflow step declares its required tools, Switchboard restricts the harness for the duration). Codex's separate approval-policy vs sandbox-mode distinction and Gemini's workspace-trust gate (currently bypassed unconditionally via `--skip-trust`; see §9 "Process model") are both part of this same design space — collapsed into the single max-autonomy posture in v1.
- **Cross-session persistent agent memory.** Architecture should not preclude; not implemented in v1.
- **Global / cross-project agent templates.** Agents in v1 are project-scoped. A future direction lets users define reusable agent templates (personas) that can be invoked from any project — for example, a personal "writing editor" persona that knows your voice and applies across blog posts, docs, and emails, or a "domain expert" persona carrying institutional knowledge (a regulatory framework, your team's architecture conventions, a research methodology) reusable in any project that touches the area. Optionally surfaced via semantic search over the project context to suggest which template fits. Distinct from "Cross-session persistent agent memory" above (memory is what an agent remembers across sessions; global templates are which agents are available to spawn).
- **Multi-project workflows.** Each project is independent in v1. (Related to "Global / cross-project agent templates" above — both concern workflows that span more than one project.)
- **Workflow conditionals and branching.** v1 workflows are linear except for bounded iteration over a static list (Primitive 6). Conditional steps (`if reviewer flagged a bug, halt`), iterative-until-condition workflows (`iterate until tests pass`), race semantics (`wait_for_first` / first-of-N completion), nested loops, and dynamic iteration lists (computed from a prior step's output) are all deferred to v2+.
- **Per-workflow MCP tool selection / allowlists.** The v1 prompt/tool boundary (prompts normalized at Switchboard, tools per-agent) is a v1 simplification, not a permanent commitment. A future direction: workflow steps could declare their required MCP tools, and Switchboard could constrain the harness for that step's duration.
- **Compaction event normalization.** Whether either harness emits a structured event when auto-compaction fires is unprobed. If they do, `Compacted { agent, before_tokens, after_tokens? }` joins the normalized vocabulary; if not, the §9 vocabulary accepts that gap and the UI works from the context-utilization signal alone.
- **DAG visualization of in-flight workflows.** v1's workflow-progress surface (§7) shows current step, total steps, status — a linear view. Workflows are conceptually DAG-shaped (per §4 "Execution model"); a graph view that shows the actual execution shape with current/complete/pending nodes highlighted is a natural v2+ extension, especially as workflows grow with iteration and pause-for-user-input nodes.
- **In-app launch of the harness's interactive TUI** ("Switch to interactive mode" on an agent for actions Switchboard's headless surface can't reach — manual `/compact`, plan mode, etc.). Spawning the harness's terminal TUI from a desktop app requires OS-specific terminal-launching and lock-tracking complexity that doesn't earn its keep against the niche use cases. Users who need this today can quit Switchboard and run `claude --resume <session-id>` (or `codex exec resume`) themselves — sessions are real Claude/Codex sessions and survive Switchboard. Deferred to v2 if user demand surfaces.

## 12. Open questions

Aggregated from inline flags above, plus a few additional:

- **5.1** Exact workflow DSL keywords and structure. Resolved in `docs/workflow-spec.md`. The spec pins down the template-function surface for dynamic agent sets (`responses_from(...)` returning a name → text mapping for Switchboard-aware prompts; `aggregated_responses(...)` returning the same data pre-formatted as a single text blob for cross-platform prompts; `last_output(agent)` and `agent_names(agents)` helpers); how invocation-time list inputs (`reviewer_agents: [agent]`) flow into template variables; the iteration variable scoping rules from Primitive 6.
- **5.2** Passthrough mechanism for harness commands — namespacing. Partially blocked on 10.10; namespacing only matters once upstream allows arbitrary slash-command passthrough.
- ~~**6.1** MiniJinja subset and template-available variables.~~ **Resolved in `docs/workflow-spec.md` §Templating** (supported / unsupported MiniJinja features, available variable scopes, built-in template functions).
- ~~**10.1** What does Switchboard do when an agent's "next assistant response" is a tool call rather than text?~~ **Resolved by hands-on probe:** all three structured-stream harnesses run the model → tool_use → tool_result → model loop internally and emit a single terminal event per user-initiated turn (Claude Code: `result`; Codex: `turn.completed` / `turn.failed`; Gemini: `result`). Switchboard always sees a complete turn — there is no "tool-call-only response" to handle. (Antigravity, added later, emits no terminal stream event at all — it uses process exit as the turn terminator; see §9 "Process model".)
- ~~**10.2** When two workflows reference the same agent, what happens? Disallow concurrent use? Queue? Refuse?~~ **Resolved by hands-on probe:** Switchboard enforces one in-flight turn per agent at the application layer. Manual compose-bar sends to a busy agent are queued (per-agent FIFO, in-memory; revised in M4); workflow-step collisions are refused with a clear error. Cross-agent dependency chaining stays out of v1. See §7 "Agent contention" and [docs/research/same-session-parallel-invocation.md](research/same-session-parallel-invocation.md). The harnesses themselves do not error on same-session parallel invocation — they silently corrupt (Claude Code: orphan branch in session tree) or conflate (Codex: interleaved transcript) — so this enforcement must live in Switchboard.
- **10.3 (partially resolved)** Persistence schema. **Resolved for what's shipped:** the project/agent registry and the Codex per-agent session-link sidecar are append-only JSONL under `<directory>/.switchboard/{projects.jsonl, projects/<project-id>/{registry.jsonl, sessions/<agent-id>.jsonl}}`. Switchboard-owned JSONL fails loud on corruption (see AGENTS.md). **Still open:** workflow-run checkpoints (M6+ work) — the on-disk shape for in-flight workflow state, atomicity guarantees on concurrent agent-spawn-during-write under that load, and the eventual pruning story. Mid-step recovery (resuming an interrupted in-flight turn) remains out of scope.
- ~~**10.4** Workflow versioning.~~ **Resolved (commitment).** Workflow runs execute against an immutable snapshot of the workflow file and its bound inputs, captured at invocation. Prompt resolution still happens at each step's dispatch (per §6 prompt resolution rules) — edits to a referenced prompt take effect on the next workflow invocation, not the in-flight run. Edits to the workflow file on disk after invocation do not affect the in-flight run or retries; the snapshot is what executes. Rationale: deterministic execution and deterministic retry; reload-on-retry would create incoherent "same run, different program" behavior given step-index checkpointing.
- ~~**10.5** Notifications when a workflow completes — terminal bell? OS notification? Just visible state in the UI?~~ **Resolved by §10 form factor commitment:** OS-native notifications via Tauri's notification plugin. See §7 "Watching a workflow run" for when notifications fire. Remaining UX details (which events notify, user opt-out controls) are implementation choices, not plan-level questions.
- **10.6** Multi-machine workflows (running Switchboard on a remote dev machine over SSH). Out of scope for v1, but the architecture should not fight it.
- ~~**10.7** Local prompt file format. Markdown body with YAML frontmatter is the working assumption; alternatives (pure YAML, plain `.txt` with separate manifest) should be evaluated against authoring ergonomics and round-tripping with editors.~~ **Resolved:** committed to markdown body with YAML frontmatter for v1. Schema documented in §6 "Authoring a local prompt".
- **10.8** Whether the local store and the MCP-server provider need to expose the same template-arguments contract (variable names, types, defaults) so a prompt can move between them without breaking workflow files. Working assumption: yes; the local file's frontmatter mirrors what an MCP `prompts/get` response would carry.
- **10.9 (monitoring)** `--bare` will become the `claude -p` default in a future Anthropic release ([source](https://code.claude.com/docs/en/headless)). When it lands, default `-p` no longer auto-loads skills, hooks, plugins, MCP servers, or CLAUDE.md, and Switchboard must explicitly pass `--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, `--append-system-prompt`, etc. to preserve current behavior. Mitigation: harness command-line construction is centralized from day one (§9 "Process model"). Action: monitor Anthropic release notes; flip the helper when announced. Background in [docs/research/archive/claude-code-headless.md](research/archive/claude-code-headless.md).
- **10.10 (monitoring)** Headless slash-command support. `claude -p` (CLI path) does not accept slash commands today, blocking §9's full passthrough vision. Tracked upstream at [anthropics/claude-code#837](https://github.com/anthropics/claude-code/issues/837) and [#38505](https://github.com/anthropics/claude-code/issues/38505). The Claude Agent SDK does support this, so a future SDK-based adapter is an alternative path; v1 stays on CLI for Claude/Codex consistency (see §9 passthrough).
- **10.11** Compaction strategy. Programmatic `/compact` is unavailable in Claude / Codex / Gemini today; all three do auto-compact at high utilization (Antigravity's compaction is server-side and unverified). Working assumption: Switchboard monitors token usage, warns the user as the auto-compact threshold approaches, and defers actual compaction to the harness. We do not implement Switchboard-side summarization (would underperform the harnesses' tuned compaction). Alternative to consider: surface a "fork from checkpoint with summary" action that uses the existing fork primitive plus an explicit summarize-and-restart prompt, as a coarse user-driven alternative when the user wants to reclaim context outside auto-compact. Background in [docs/research/archive/claude-code-headless.md](research/archive/claude-code-headless.md) and [docs/research/archive/codex-noninteractive.md](research/archive/codex-noninteractive.md).
- ~~**10.12** Model→max-context map maintenance.~~ **Resolved (commitment).** No bundled model→max-context map ships in v1. Each harness sources the value from whatever it makes available: Claude reads `contextWindow` per turn from the stream's `result.modelUsage.<model>`; Codex enriches `task_started.model_context_window` from the session file post-terminal (per resolved 10.15); Gemini has neither a stream field nor a session-file analog, so `usage.context_window` stays `None` and the per-agent context-utilization bar is hidden for Gemini agents (per the §7 sidebar matrix). The harness-asymmetric sourcing is documented in the §9 SessionMeta source table; the "what the user sees" implications are documented in the §7 sidebar matrix.
- **10.13 (monitoring)** Programmatic `/compact` exposure in either harness. Multiple Anthropic feature requests open ([anthropics/claude-code#5643](https://github.com/anthropics/claude-code/issues/5643), [#39275](https://github.com/anthropics/claude-code/issues/39275), [#39574](https://github.com/anthropics/claude-code/issues/39574), [#26488](https://github.com/anthropics/claude-code/issues/26488)); Codex equivalent not documented. When upstream lands, Switchboard can offer first-class compaction control inside workflows.
- ~~**10.14** Codex non-interactive fork.~~ **Resolved for v1: option (a).** Fork is unavailable for Codex agents in v1 — the agent context-menu Fork action is shown only on Claude Code agents; Codex agents show an explanatory tooltip ("Fork is not available for Codex sessions in v1; see the docs for workarounds"). Workarounds (b) copy session JSONL and (c) re-feed summarized prior context are deferred to v2+ if user demand surfaces. The asymmetry is documented in §9 and the M4 deliverables.
- ~~**10.15** Should the Codex adapter read the session file (`~/.codex/sessions/...jsonl`) in addition to the `--json` stream?~~ **Resolved (commitment).** The Codex adapter reads the session file on turn completion to enrich the normalized event stream with fields the `--json` stream omits (rate limits, `model_context_window`, full reasoning blocks, `session_meta` for the SessionMeta event). Multiple §7 and §9 commitments now depend on this (per-turn context-utilization for Codex, RateLimitEvent timing, SessionMeta). What remains as implementation detail: tail-vs-completion timing for any future live-stream enrichment, and the lookup-strategy mechanics for the date-partitioned session path (see the archived `research/archive/codex-cli-observed.md` research note).
- **10.16** Disk usage of harness session files. Both harnesses persist transcripts indefinitely (Claude Code at `~/.claude/projects/<encoded-cwd>/*.jsonl`, Codex at `~/.codex/sessions/YYYY/MM/DD/...`). A long-lived project with many agents and many turns will accumulate. Should Switchboard offer pruning, surface totals, or otherwise manage this? Out of scope for v1, but the architecture should not preclude it.
- **10.17** Network failure and retry policy. What does Switchboard do when a turn fails mid-workflow because of a transient API error or network blip? Working assumption: a single configurable retry on transient errors (rate-limit, 5xx) before marking the step as failed. Permanent errors (auth, invalid model, denied content) fail immediately. To be detailed in §7 once we have an implementation footprint.
- **10.18** Stall detection. A turn with no stream events for T seconds is ambiguous (genuinely hung vs slow tool). UX (passive surface, prompt-to-cancel after threshold, or some other affordance) deferred to implementation, including a probe of whether either harness emits reliable heartbeats during long operations.
- **10.19** Hard per-turn timeout. Distinct from 10.18 (passive stall detection — observation-only). Should Switchboard impose its own active per-turn timeout that hard-kills the subprocess after T seconds regardless of activity? Open sub-questions: default value (none / 5min / configurable?), per-workflow override, user-configurability, surfacing as `TurnEnd { outcome: Failed { kind: Timeout, ... } }` (the `FailureKind` enum already accommodates this — see system-design §9 / `docs/implementation_plans/2026-05-12-v1-m1-scaffolding.md` M1.3). Defer until there's concrete demand; runaway costs in long workflows could surface this as a real need.
