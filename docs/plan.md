# Switchboard — High-Level Plan

> Status: Draft. This document captures the goals, functional requirements, and key design decisions for Switchboard. It is intentionally high-level: enough to confidently spin off implementation plans for individual phases, not so detailed that it pretends decisions have been made that haven't.
>
> Open questions are flagged inline and aggregated at the end.

## 1. What Switchboard is

Switchboard is a **human-directed orchestrator for AI coding agents** — an interactive tool you run alongside your existing Claude Code and Codex setup. It lets a developer spawn multiple agent sessions within a project, route messages between them, and define reusable patterns for common multi-agent workflows like second-opinion code review, plan-and-implement, and parallel-solution adjudication.

More precisely, it is a **workflow engine for primitives, not for processes**. It codifies the *shape* of common multi-agent operations (fan-out, fan-in with template wrapping, sequential handoff) so they can be invoked with one command instead of manually copy-pasted. It does not impose any larger structure on top of those primitives — there is no built-in concept of "plan phase" or "review phase," no SDLC walkthrough, no opinionated process. The user composes patterns ad hoc and saves the ones they reuse.

The human stays in the loop where judgment matters (deciding what to route, when to revise, when to proceed) and is removed from the loop where mechanics waste time (copy-paste, template application, babysitting parallel agents).

A second consequence of this design: because Switchboard resolves prompts itself and sends agents plain text, prompt-provider configuration lives in *one place* (Switchboard) and works across every agent backend. A user's prompt library — whether in Tiddly, another MCP server, or Switchboard's local store — works identically with both Claude Code and Codex agents, without configuring the prompt source in either. This is especially useful for Codex, where MCP prompt support is limited or absent depending on version. The same does **not** hold for MCP tools or for Claude Code skills, which are invoked by the model mid-turn and must still be configured in the underlying agent (see section 6).

## 2. Goals and non-goals

### Goals

- Spawn and manage multiple Claude Code and Codex agent instances in a single project with named roles.
- Route messages between agents with explicit fan-out (one human → many agents), fan-in (many agents → one agent), and sequential handoff, with optional prompt-template wrapping.
- Apply prompt templates from one or more configured **prompt providers** during routing, with parameterized substitution. Providers include a built-in local file store (prompts authored as files inside the project or the user's Switchboard config directory) and any MCP server that exposes prompts (for example [Tiddly](https://tiddly.me)).
- Centralize prompt-provider configuration in Switchboard so a user's prompt library works identically across all supported agent backends (Claude Code, Codex, future additions) without per-agent MCP setup. Does not extend to MCP *tools* or to model-discovered *skills*, which remain per-agent concerns.
- Ship example prompts and example patterns in the local store so a new user is productive immediately, with no MCP setup required.
- Define reusable patterns as files that compose primitives — invoked by name, parameterized at invocation time.
- Run patterns autonomously after launch so the user can walk away and return to completed work.
- Preserve full access to the underlying Claude Code and Codex experience — Switchboard drives the harnesses, doesn't replace them.

### Non-goals

- **Replacing the Claude Code or Codex harness.** Compaction, tool rendering, permission policy, plan mode, hooks, and skills all live in the harnesses. Switchboard drives them via their non-interactive modes (`claude -p`, `codex exec`).
- **Prescribing a software development lifecycle.** Switchboard does not know about "planner" or "reviewer" as roles with semantics. Roles are labels the user assigns; the tool is agnostic.
- **Managing git, CI, or PR workflows.** Out of scope. Patterns can read agent outputs and route them; they don't run `git commit`, open PRs, or integrate with CI.
- **Cross-session persistent agent memory** (vector DBs, RAG over prior sessions). Considered as a future feature; the v1 architecture should not preclude it but does not implement it.
- **In-UI pattern authoring** in v1. Patterns are authored as files; UI authoring may come later.
- **Multi-user collaboration.** Single-developer tool. Sharing patterns and configurations via git is supported as a side effect of file-based config, but there is no real-time collaboration model.

## 3. Core concepts

| Concept | Definition |
|---|---|
| **Project** | A workspace containing a group of agents working toward a shared goal. Projects are persistent, named, and have a working directory (typically a git repo). |
| **Agent** | A Claude Code or Codex session within a project, with a user-assigned name and optional initial-prompt configuration. Each agent has a persistent harness session ID under the hood. |
| **Primary agent** | The agent the user is currently focused on (foregrounded in the UI). Background agents exist but are not the focus. |
| **Pattern** | A named, parameterized composition of primitives — for example "fan-out review and aggregate." Defined as a YAML file in the project directory. Invoked by name with arguments. |
| **Prompt template** | A named prompt definition resolved by ID at routing time. Used as message content (sent to an agent) or as a wrapper (applied around aggregated outputs before forwarding). |
| **Prompt provider** | A source of prompts that Switchboard can resolve IDs against. Two implementations ship in v1: a local file store (markdown/YAML files on disk) and an MCP-server provider (any MCP server that exposes prompts; Tiddly is the canonical example). Providers are addressed by a short prefix (e.g. `local:code-review`, `tiddly:code-review`). |
| **Routing** | Message passing between agents. Includes fan-out (one source, many recipients), fan-in (many sources, one recipient, with template wrapping), and sequential handoff. |
| **Harness session** | The underlying Claude Code or Codex session that backs an agent. Persisted on disk by the harness; resumed via `--resume`. |

A note on terminology: "session" in the agent ecosystem is overloaded. Switchboard uses **project** for its top-level workspace concept and reserves **session** to mean the underlying harness session backing a single agent.

## 4. Functional primitives

These five primitives cover everything Switchboard needs to do at the functional level. Patterns compose them.

### Primitive 1 — Spawn an agent

Create a new agent within a project. User specifies:

- Agent type (Claude Code or Codex).
- Name (free-form label).
- Optional initial prompt (sent as the first message after spawn; substitutes for a system prompt where harness-level system prompts aren't accessible).
- Optional working directory override (defaults to project working directory).

The harness session ID is captured and persisted. The agent is now part of the project and can receive messages and participate in patterns.

### Primitive 2 — Send a message to one or more agents

User specifies:

- Recipients (one or more agents).
- Prompt template (an MCP prompt ID — e.g. from Tiddly) and/or free-form text.
- Optional parameters for the prompt template.

The composed message is sent to each recipient. If recipients are multiple, this is a fan-out: each agent receives the same message and runs independently.

This primitive is **synchronous from the human's perspective** — the human sends the message, the agents start working. The human can then watch any agent's output, switch between them, or walk away.

### Primitive 3 — Auto-forward an agent's output

Configure: when agent A finishes its current turn (next assistant text response), forward that output to agent B, optionally wrapped in a prompt template.

Used for sequential handoff (planner → implementer with the plan as input, for example). Configured before agent A is launched on its turn; fires automatically when A completes.

### Primitive 4 — Fan-in with template wrapping

Configure: when all of agents A, B, ..., N finish their current turns, combine their outputs into a single message using a wrapping prompt template, then send to agent X.

The wrapping template has access to each agent's response by name (or by position): `{{ responses.reviewer_a }}`, `{{ responses.reviewer_b }}`, etc. Templates may use Jinja-style for-loops to handle variable numbers of sources.

This is the most behaviorally-rich primitive. It implies waiting on multiple agents, accumulating their final responses, applying a template, and dispatching. Failure handling (one agent crashes mid-pattern) is covered in section 7.

### Primitive 5 — Save and invoke a reusable pattern

A pattern is a named, parameterized composition of the other primitives, defined as a YAML file in the project directory. Invoking a pattern fills in its parameters and runs it.

Pattern definition format (illustrative; exact schema TBD):

```yaml
name: review-and-aggregate
description: Send a message to multiple reviewers, aggregate, send to primary.
inputs:
  primary_agent: agent
  reviewer_agents: [agent]
  review_prompt: prompt_id
  aggregation_prompt: prompt_id
  user_context: text  # optional
steps:
  - send:
      to: "{{ reviewer_agents }}"
      prompt: "{{ review_prompt }}"
      context: "{{ user_context }}"
  - wait_for_all: "{{ reviewer_agents }}"
  - send:
      to: "{{ primary_agent }}"
      prompt: "{{ aggregation_prompt }}"
      template_vars:
        responses: "{{ responses_from(reviewer_agents) }}"
```

When invoked, Switchboard prompts the user for each input and then executes the steps.

**Open question 5.1:** The exact pattern DSL — keywords, structure, escape hatches, error handling — is not finalized. The example above is a sketch. A separate doc (`docs/patterns-spec.md`) should formalize this before implementation begins.

## 5. Harness integration

Switchboard interacts with Claude Code and Codex through their non-interactive modes. The user retains the ability to interact with each agent as if it were a normal Claude Code or Codex session.

### Process model

Per-message process spawn for v1: each turn invokes `claude -p --resume <session-id>` or `codex exec --resume <session-id>`, captures the structured output stream, and exits. State persists in the harness's session files between invocations. Long-lived agent processes can be considered later if latency matters.

Switchboard runs `claude -p` in its **default** mode (no `--bare`) so the agent inherits the user's full environment: skills, hooks, plugins, MCP servers, CLAUDE.md, and auto-memory all load exactly as they would in an interactive session. This is deliberate — Switchboard's value is to orchestrate normal Claude Code / Codex sessions, not to amputate them. Anthropic has stated that `--bare` will become the `-p` default in a future release; when that happens, Switchboard will need to pass equivalent context-loading flags (`--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, `--append-system-prompt`) to preserve current behavior. To make that change a one-place edit, harness command-line construction is centralized in a single "harness invoker" helper from day one. Tracked under open question 10.9; full background in [docs/research/claude-code-headless.md](research/claude-code-headless.md).

### Permissions and sandboxing

For MVP, Switchboard exposes a single user-facing toggle: **skip permissions (default: on)**. When on, agents run with maximum autonomy:

- Claude Code: `--dangerously-skip-permissions`
- Codex: `--dangerously-bypass-approvals-and-sandbox` (alias `--yolo`)

Internally, the configuration layer maps this single toggle to the actual flags per harness, so granular control (separate sandbox modes, per-tool allowlists) can be added later without breaking the user-facing model.

**Known issues to track:**

- Codex has open bugs around `--dangerously-bypass-approvals-and-sandbox` not fully bypassing in all sub-modes (e.g., a recent regression where the directory-trust prompt fires anyway). Switchboard should pin tested Codex versions and surface any unexpected prompts as errors.
- Codex separates approval policy from sandbox mode. The MVP collapses these; v2 may expose them separately.

### Required harness commands for MVP

Switchboard must be able to:

- **Spawn** a session with explicit flags.
- **Send** a message and capture the structured stream.
- **Detect turn completion** (the harness emits a stop event).
- **Trigger compaction** (`/compact` equivalent invoked programmatically).
- **Read context utilization** (percent of context consumed).
- **Read session metadata** (model, session ID, cost/tokens).
- **Resume** a session by ID.
- **Fork** a session from a checkpoint.

### Passthrough mechanism

For harness commands Switchboard does not need to coordinate, the design *intent* is a passthrough: the user types a harness slash command (e.g., `/model`, `/clear`, `/cost`) when interacting with an agent, and Switchboard forwards it to the harness verbatim. This avoids reimplementing every harness feature.

**Important caveat — `claude -p` limitation today:** Headless mode does not currently accept slash commands as input — that includes built-in commands (`/cost`, `/model`, `/clear`) and user-invoked skills (`/skill-name`). This is a known upstream gap ([anthropics/claude-code#837](https://github.com/anthropics/claude-code/issues/837), [#38505](https://github.com/anthropics/claude-code/issues/38505)). Until it is resolved, Switchboard's passthrough is constrained to commands we can implement out-of-band: `/cost` derived from `--output-format json` metadata, `/model` implemented by re-spawning with a different `--model` flag, and so on. A blanket "type any slash command and forward it" is not achievable in pure headless mode today. Tracked under open question 10.10; see [docs/research/claude-code-headless.md](research/claude-code-headless.md) for sources. (The auto-invoked side of skills is unaffected — see §6.)

**Open question 5.2:** Exact mechanism for passthrough — does it require a prefix to disambiguate from Switchboard's own slash commands, or do Switchboard's commands live in a separate namespace? Partially blocked on the upstream limitation above.

### What we lose by going non-interactive

The interactive Claude Code and Codex TUIs are not used. Switchboard renders the structured output stream itself. This means rendering tool calls, diffs, todo lists, and thinking blocks is Switchboard's responsibility. Behavior (compaction, hooks, skills, plan mode, sub-agents) is unchanged because the harness still runs.

## 6. Patterns

### Authoring

Patterns are authored as YAML files in `<project>/.switchboard/patterns/`. Authored externally (in the user's editor), versioned in git, sharable across projects by copying or symlinking.

In-UI authoring is **deferred**. v1 ships with a small library of built-in patterns (review-and-aggregate, sequential handoff with template) as starting points; users author their own by editing files.

### Prompt providers

Pattern files and slash commands reference prompts by ID. The *prompt text* lives in a **prompt provider**; the *workflow* lives in the pattern file. Switchboard reads pattern files, resolves prompt IDs to prompt content via the configured providers, and applies templates with substitution.

Two providers ship in v1:

- **Local file store.** Prompts authored as files (markdown body with YAML frontmatter for metadata: id, description, arguments). Lives at two scopes: a project-scoped directory at `<project>/.switchboard/prompts/` (versioned with the project) and a user-global directory at `~/.config/switchboard/prompts/` (shared across projects). The local store is the lowest-friction way to author a prompt and the mechanism Switchboard uses to ship example prompts and patterns.
- **MCP-server provider.** Resolves IDs against any MCP server the user has configured that exposes prompts. [Tiddly](https://tiddly.me) is the canonical example and the development reference, but the integration is generic: pointing Switchboard at a different MCP prompt server is a configuration change, not a code change.

Providers are addressed by a short prefix in prompt IDs:

- `local:code-review` — resolves against the local file store.
- `tiddly:code-review` — resolves against the MCP server registered under the name `tiddly`.

The prefix is the user-chosen registration name for an MCP-server provider, so a user with two MCP prompt servers configured can address both unambiguously. The `local` prefix is reserved for the built-in local store.

**Default provider.** Each project has a configured default provider (set in project config; defaults to `local` for new projects). An unprefixed prompt ID resolves against the default. This keeps simple cases terse: a project that uses only Tiddly can set the default to `tiddly` and write `code-review` everywhere.

**Resolution rules.**

- A prefixed ID resolves only against the named provider; if not found there, it errors. No cross-provider fallback.
- An unprefixed ID resolves against the project's default provider only. Same rule.
- Local-store lookup checks the project scope first, then the user-global scope. A project-scoped prompt with the same name shadows the user-global one — intentional, so a project can override a personal prompt.

This separation between provider and workflow is intentional: a prompt store is a prompt store, not a workflow engine. Encoding control flow ("run agent A, then fan out to B and C, then aggregate via template D") in a stored prompt would stretch the store out of shape. Patterns are programs; prompts are data.

#### Cross-agent normalization

Switchboard resolves prompt IDs itself and sends agents the rendered text as a plain message — the agent never sees the MCP call, the provider, or the arguments. The useful side effect: prompt-provider configuration lives in *one place* (Switchboard) and works the same across every agent backend. A user's prompts (Tiddly, another MCP server, or the local store) work identically with both Claude Code and Codex agents, without configuring the prompt source in either harness. This is especially useful for Codex, whose MCP prompt support is limited or absent depending on version — Switchboard gives Codex users a Claude-Code-style prompt library experience without requiring Codex to support it.

What this does **not** cover:

- **MCP tools.** Tools are invoked by the model mid-turn, not by the user pre-turn. Switchboard cannot proxy them; tools (e.g. an Atlassian MCP server, Google Drive integration) must still be configured in the underlying agent.
- **Claude Code skills.** Configured in Claude Code itself (`~/.claude/skills/`, project `.claude/skills/`); Switchboard does not mediate them. **Auto-invoked skills do work normally in Switchboard-spawned sessions** because default `claude -p` loads the user's full environment — the model can discover and invoke skills mid-turn just as it would interactively. The *user-invoked* side of skills (`/skill-name` as an explicit command) is currently unavailable due to a `claude -p` limitation; see §5 passthrough and [docs/research/claude-code-headless.md](research/claude-code-headless.md).
- **Per-agent setup in general.** Authentication, permission flags, hooks, and MCP tool registration remain the underlying harness's concern.

Switchboard normalizes the *user-invoked prompt* surface across agents. Model-invoked capabilities (tools, skills) and harness-level configuration are still per-agent.

#### Authoring a local prompt

A local prompt is a single file. Example (`<project>/.switchboard/prompts/code-review.md`):

```markdown
---
id: code-review
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

A user can promote a local prompt to an MCP server later (paste it into Tiddly, change the prefix in pattern files), but is never required to.

### Wrapping templates

Wrapping templates (used for fan-in) are prompts — from any provider — that take agent responses as variables. The pattern definition declares which agent maps to which template variable. The template uses Jinja-style substitution:

```jinja
The following are reviews from multiple agents:

{% for name, response in responses.items() %}
## {{ name }}

{{ response }}

{% endfor %}

Summarize the recommendations and identify points of agreement and
disagreement.
```

**Open question 6.1:** Exact templating syntax (Jinja2 vs simpler substitution) and what variables are available in templates beyond `responses` (e.g., `user_context`, `agent_metadata`, `project_info`).

## 7. User-facing model

This section describes the conceptual user experience. The specific UI form factor (TUI vs desktop app) is **deferred** until functional requirements are locked. See section 9.

### Project list

The user opens Switchboard and sees a list of their projects. They open one, or create a new one. A project is bound to a working directory.

### Inside a project

The user sees the project's agents. One is foregrounded as primary; others are accessible (background). The primary agent's conversation is the main view; switching primary is a single action.

### Sending a message

The user composes a message via:

- A slash command (resolves to a prompt by ID, against the project's default provider unless prefixed).
- Free-form text.
- Optionally both: slash command for the structured part, free-form for context.

The user picks recipient(s): the primary agent by default, or any combination of agents in the project. Send.

### Invoking a pattern

A pattern is invoked by name. Switchboard prompts for the pattern's inputs (which agents to use, which prompts, any free-form context). The user confirms; the pattern launches and runs autonomously.

### Watching a pattern run

The user can switch focus among agents to watch any of their outputs. The pattern continues running in the background regardless of which agent is foregrounded. When the pattern completes (the final step has dispatched its output), the user is notified.

### Failure handling

If a step in a pattern fails (an agent errors, a harness call fails, a template substitution fails), the pattern halts. Partial results are retained. The user sees the error, can inspect the state of each agent involved, and decides whether to retry the pattern, retry from a specific step, or abandon.

### Walking away

A pattern continues to run as long as the Switchboard host process is alive. Closing the UI window does not stop a pattern. Putting the machine to sleep stops a pattern (because the host process is paused). When the user returns, Switchboard shows the state of any in-progress or completed patterns.

## 8. Worked example: review-and-aggregate

To anchor the abstractions above, here is what a code-review workflow looks like end to end.

**Setup:**

The user has a project `feature-event-logs` open in Switchboard. They have three agents:

- `planner` (Claude Code, primary)
- `reviewer-claude` (Claude Code, background)
- `reviewer-codex` (Codex, background)

The user has previously authored a pattern in `.switchboard/patterns/review-and-aggregate.yaml`. The review prompt ships as a built-in local prompt (`local:code-review`); the aggregation wrapper is one the user keeps in Tiddly (`tiddly:ai-review-feedback`). Both work because Switchboard resolves each ID against the named provider.

**Invocation:**

1. The user invokes the pattern: "Run review-and-aggregate."
2. Switchboard prompts:
   - Primary: `planner` (default)
   - Reviewers: `reviewer-claude`, `reviewer-codex` (multi-select)
   - Review prompt: `local:code-review` (bundled with Switchboard)
   - Aggregation prompt: `tiddly:ai-review-feedback` (the user's own, stored in Tiddly)
   - User context: "Review milestone 1, focus on the event-emission API."
3. The user confirms. The pattern launches.

**Execution:**

1. Switchboard sends the review-prompt message (with user context appended) to both reviewers in parallel. Each reviewer runs.
2. Switchboard waits for both reviewers to complete their turns.
3. Switchboard collects both reviewers' final assistant messages.
4. Switchboard renders the aggregation-prompt template, substituting in the two reviews under their respective variable names.
5. Switchboard sends the rendered message to the primary agent (`planner`).
6. The planner runs and produces its response.
7. Pattern complete. The user is notified.

**During execution:**

The user can watch any agent's output. When both reviews are in flight, the user might split attention between the two. When the planner is processing, the user watches the planner. None of this affects pattern execution.

**Afterwards:**

The user reads the planner's response, decides whether to revise, route to the implementer, or do something else. The pattern is done; the next action is the user's.

## 9. Deferred decisions

Decisions explicitly **not made** in this document, to be addressed in later docs or after early implementation:

- **UI form factor.** TUI vs desktop app. Will be decided once functional requirements above are validated by an early prototype.
- **Language and stack.** Depends on UI form factor.
- **Long-lived agent processes.** Per-message spawn for v1; may revisit if latency dominates.
- **In-UI pattern authoring.** v1 is file-based.
- **Granular permission/sandbox config.** v1 collapses to a single toggle.
- **Cross-session persistent agent memory.** Architecture should not preclude; not implemented in v1.
- **Multi-project workflows.** Each project is independent in v1.
- **Pattern conditionals and branching.** v1 patterns are linear.

## 10. Open questions

Aggregated from inline flags above, plus a few additional:

- **5.1** Exact pattern DSL keywords and structure. Needs a separate spec.
- **5.2** Passthrough mechanism for harness commands — namespacing.
- **6.1** Templating syntax (Jinja2 vs simpler) and template-available variables beyond `responses`.
- **10.1** What does Switchboard do when an agent's "next assistant response" is a tool call rather than text? Default proposed: wait for the next text response. Override?
- **10.2** When two patterns reference the same agent, what happens? Disallow concurrent use? Queue? Refuse?
- **10.3** How are agents preserved across Switchboard restarts? Harness session IDs persist on disk; Switchboard's project/agent registry needs its own persistence model.
- **10.4** Pattern versioning. If a pattern file changes mid-execution (unlikely but possible), what happens to the in-flight pattern?
- **10.5** Notifications when a pattern completes — terminal bell? OS notification? Just visible state in the UI?
- **10.6** Multi-machine workflows (running Switchboard on a remote dev machine over SSH). Out of scope for v1, but the architecture should not fight it.
- **10.7** Local prompt file format. Markdown body with YAML frontmatter is the working assumption; alternatives (pure YAML, plain `.txt` with separate manifest) should be evaluated against authoring ergonomics and round-tripping with editors.
- **10.8** Whether the local store and the MCP-server provider need to expose the same template-arguments contract (variable names, types, defaults) so a prompt can move between them without breaking pattern files. Working assumption: yes; the local file's frontmatter mirrors what an MCP `prompts/get` response would carry.
- **10.9 (monitoring)** `--bare` will become the `claude -p` default in a future Anthropic release ([source](https://code.claude.com/docs/en/headless)). When it lands, default `-p` no longer auto-loads skills, hooks, plugins, MCP servers, or CLAUDE.md, and Switchboard must explicitly pass `--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, `--append-system-prompt`, etc. to preserve current behavior. Mitigation: harness command-line construction is centralized from day one (§5 "Process model"). Action: monitor Anthropic release notes; flip the helper when announced. Background in [docs/research/claude-code-headless.md](research/claude-code-headless.md).
- **10.10 (monitoring)** Headless slash-command support. `claude -p` does not accept slash commands today, blocking §5's full passthrough vision. Tracked upstream at [anthropics/claude-code#837](https://github.com/anthropics/claude-code/issues/837) and [#38505](https://github.com/anthropics/claude-code/issues/38505). Workarounds described in §5; full passthrough lights up automatically when upstream lands.

## 11. Phasing

Sketch only. Each phase will get its own implementation plan in `docs/implementation-plans/`.

### v0.1 — Walking skeleton

The minimum thing that demonstrates the model end to end:

- Project create / open.
- Spawn one agent (Claude Code only).
- Send a message; render the streamed response.
- Persist project + agent registry across restarts.
- Local prompt provider (file-based), with a small bundled set of example prompts so the slash-command path is exercised end to end.

Not yet: patterns, fan-out, fan-in, Codex, multi-agent, MCP-server provider.

### v0.2 — Multi-agent and basic fan-out

- Spawn multiple agents in a project.
- Send a message to multiple agents (primitive 2 with N>1 recipients).
- Watch any agent's output, switch focus.
- Codex agents alongside Claude Code agents.
- MCP-server prompt provider (Tiddly used as the development reference), with the prefix-based addressing scheme from section 6.

### v0.3 — Patterns

- Pattern file format and parser.
- Pattern invocation UI (collect inputs, confirm).
- Auto-forward (primitive 3).
- Fan-in with template wrapping (primitive 4).
- Built-in pattern: review-and-aggregate.

### v0.4 — Polish and second-order features

- Failure handling and retry UI.
- Notifications.
- Built-in pattern library expanded.
- Documentation for authoring custom patterns.

### Beyond v0.4

- In-UI pattern authoring.
- Granular permission config.
- Long-lived agent processes (if latency demands).
- Cross-session memory primitives.
- Multi-machine workflows.

---

*Last updated: drafted from design conversation. Subject to revision as implementation reveals gaps.*
