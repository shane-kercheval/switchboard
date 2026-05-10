# Switchboard — High-Level Plan

> Status: Draft. This document captures the goals, functional requirements, and key design decisions for Switchboard. It is intentionally high-level: enough to confidently spin off implementation plans for individual phases, not so detailed that it pretends decisions have been made that haven't.
>
> Open questions are flagged inline and aggregated at the end.

## 1. What Switchboard is

Switchboard is a **human-directed orchestrator for AI coding agents** — an interactive tool you run alongside your existing Claude Code and Codex setup. It lets a developer spawn multiple agent sessions within a project, route messages between them, and define reusable patterns for common multi-agent workflows like second-opinion code review, plan-and-implement, and parallel-solution adjudication.

More precisely, it is a **workflow engine for primitives, not for processes**. It codifies the *shape* of common multi-agent operations (fan-out, fan-in with template wrapping, sequential handoff) so they can be invoked with one command instead of manually copy-pasted. It does not impose any larger structure on top of those primitives — there is no built-in concept of "plan phase" or "review phase," no SDLC walkthrough, no opinionated process. The user composes patterns ad hoc and saves the ones they reuse.

The human stays in the loop where judgment matters (deciding what to route, when to revise, when to proceed) and is removed from the loop where mechanics waste time (copy-paste, template application, babysitting parallel agents).

A second consequence of this design: **because Switchboard resolves prompts itself and sends agents plain text, prompt-provider configuration lives in *one place* (Switchboard) and works across every agent backend**. A user's prompt library — whether in Tiddly, another MCP server, or Switchboard's local store — works identically with both Claude Code and Codex agents, without configuring the prompt source in either. This is especially useful for Codex, where MCP prompt support is limited or absent depending on version. The same does **not** hold for MCP tools or for Claude Code skills, which are invoked by the model mid-turn and must still be configured in the underlying agent (see section 6).

## 2. Goals and non-goals

### Goals

- **Multi-agent spawn and management.** Multiple Claude Code and Codex agent instances run in a single project with user-assigned names.
- **Routing primitives.** Explicit fan-out (one source → many agents, where the source is either a human-composed message or another agent's output), fan-in (many agents → one recipient), and sequential handoff, with optional prompt-template wrapping.
- **Reusable, parameterized patterns.** Patterns are files that compose primitives — invoked by name, parameterized at invocation time.
- **Agent-friendly authoring.** Pattern files and other authorable artifacts (local prompts, project setup) are documented in instruction docs under `docs/agent-instructions/` designed for AI coding agents to consume. The intended authoring path is to point your existing Claude Code or Codex agent at the relevant instruction file and ask it to generate the artifact from a description, rather than learning the DSL by hand.
- **Autonomous pattern execution.** A pattern continues to run after launch so the user can switch focus to other work without babysitting it (within the lifetime of the Switchboard host process; see §7).
- **Configurable prompt providers.** Apply prompt templates from one or more configured prompt providers during routing, with parameterized substitution. Providers include a built-in local file store (prompts authored as files inside the project or the user's Switchboard config directory) and any MCP server that exposes prompts (for example [Tiddly](https://tiddly.me)). Provider configuration is centralized in Switchboard, so a user's prompt library works identically across Claude Code, Codex, and future agent backends without per-agent MCP setup. Does not extend to MCP *tools* or to model-discovered *skills*, which remain per-agent concerns.
- **Zero-setup onboarding.** Switchboard ships with example prompts and example patterns in the local store so a new user can invoke a useful pattern within minutes of installation, without configuring an MCP server.
- **Full access to the underlying harness.** Switchboard drives Claude Code and Codex; it doesn't replace them.
- **Shareable, versioned configuration.** Patterns, local prompts, and project configuration are file-based and live inside the project's `.switchboard/` directory, so they version, diff, review, and share via the user's normal git workflow.

### Non-goals

- **Replacing the Claude Code or Codex harness.** Compaction, tool rendering, permission policy, plan mode, hooks, and skills all live in the harnesses. Switchboard drives them via their non-interactive modes (`claude -p`, `codex exec`).
- **Prescribing a software development lifecycle.** Switchboard does not know about "planner" or "reviewer" as roles with semantics. Roles are labels the user assigns; the tool is agnostic.
- **Managing git, CI, or PR workflows.** Out of scope. Patterns can read agent outputs and route them; they don't run `git commit`, open PRs, or integrate with CI.
- **Cross-session persistent agent memory** (vector DBs, RAG over prior sessions). Considered as a future feature; the v1 architecture should not preclude it but does not implement it.
- **Visual / GUI pattern editor.** Authoring is file-based, supported by agent-consumable instruction docs (see goals — "Agent-friendly authoring"). A visual or form-based pattern editor is not planned.
- **Multi-user collaboration.** Single-developer tool. Sharing patterns and configurations via git is supported as a side effect of file-based config, but there is no real-time collaboration model.
- **Hosted / SaaS service.** Switchboard runs locally on the developer's machine. There is no managed cloud version, no shared backend, no remote agent execution. A future hosted service may eventually provide cross-machine sync of patterns, prompts, and project configuration; that is out of scope for v1.

## 3. Core concepts

| Concept | Definition |
|---|---|
| **Project** | A workspace containing a group of agents working toward a shared goal. Persistent, named, and bound to a working directory (typically a git repo). Project-specific config, patterns, and local prompts live under `<project>/.switchboard/` (see directory layout below). |
| **Agent** | A Claude Code or Codex session within a project, with a user-assigned name. Each agent has a persistent harness session ID under the hood. |
| **Primitive** | An atomic operation Switchboard provides for a pattern to compose: spawn agent, send message, auto-forward output, fan-in with template, save/invoke pattern. Five exist in v1; see §4. |
| **Pattern** | A named, parameterized composition of primitives — for example "fan-out review and aggregate." Defined as a YAML file under `<project>/.switchboard/patterns/`. Invoked by name with arguments. |
| **Prompt template** | A named prompt definition resolved by ID at routing time. Used as message content (sent to an agent) or as a wrapper applied around aggregated outputs before forwarding (used in fan-in; see §4 Primitive 4). |
| **Prompt provider** | A source of prompts Switchboard resolves IDs against. Two implementations ship in v1: `local` (file store) and any registered MCP-server provider. Addressed by prefix (e.g. `local:code-review`, `tiddly:code-review`). See §6. |
| **Routing** | Message passing between agents. Includes fan-out (one source, many recipients), fan-in (many sources, one recipient, with template wrapping), and sequential handoff. |
| **Harness session** | The underlying Claude Code or Codex session that backs an agent. Persisted on disk by the harness; resumed via `--resume`. |

A note on terminology: "session" in the agent ecosystem is overloaded. Switchboard uses **project** for its top-level workspace concept and reserves **session** to mean the underlying harness session backing a single agent.

### Project directory layout

Each project's Switchboard-managed state lives in a `.switchboard/` directory at the project root. The shape (illustrative; exact contents TBD):

```
<project>/
└── .switchboard/
    ├── config.yaml         # project config (registered MCP providers, harness flags, etc.)
    ├── patterns/           # pattern definitions (YAML)
    ├── prompts/            # local prompts (markdown body + YAML frontmatter)
    └── state/              # runtime state (agent registry, harness session IDs)
```

`config.yaml`, `patterns/`, and `prompts/` are intended to be checked into git and shared. `state/` is local-machine runtime data and should be `.gitignore`d.

## 4. Functional primitives

These five primitives cover everything Switchboard needs to do at the functional level. Patterns compose them.

### Primitive 1 — Spawn an agent

Create a new agent within a project. User specifies:

- Agent type (Claude Code or Codex).
- Name (free-form label).
- Optional initial prompt (sent as the first message after spawn to prime the agent with role context, project background, or any other instructions the user wants in place before the first real turn). Authored like any other prompt — free-form text or a fully-qualified prompt ID resolved through the prompt-provider system (§6).
- Optional working directory override (defaults to project working directory).

The harness session ID is captured and persisted. The agent is now part of the project and can receive messages and participate in patterns.

### Primitive 2 — Send a message to one or more agents

User specifies:

- Recipients (one or more agents).
- Prompt template (a fully-qualified prompt ID, e.g. `local:code-review` or `tiddly:code-review`) and/or free-form text.
- Optional parameters for the prompt template.

The composed message is sent to each recipient. If recipients are multiple, this is a fan-out: each agent receives the same message and runs independently.

This primitive is **synchronous from the human's perspective** — the human sends the message, the agents start working. The human can then watch any agent's output, switch between them, or walk away.

### Primitive 3 — Auto-forward an agent's output

Configure: when agent A finishes its current turn, forward that output to one or more recipient agents, optionally wrapped in a prompt template.

Used for sequential handoff (planner → implementer with the plan as input) and for agent-driven fan-out (planner → multiple implementers in parallel, one reviewer → multiple follow-up reviewers, etc.). Configured before agent A is launched on its turn; fires automatically when A completes.

### Primitive 4 — Fan-in with template wrapping

Configure: when all of agents A, B, ..., N finish their current turns, combine their outputs into a single message using a wrapping prompt template, then send to agent X.

The wrapping template has access to each agent's response by name (or by position): `{{ responses.reviewer_claude }}`, `{{ responses.reviewer_codex }}`, etc. Agent names containing hyphens are normalized to underscores in template contexts (so an agent named `reviewer-claude` is accessed as `responses.reviewer_claude`). Templates may use Jinja-style for-loops to handle variable numbers of sources.

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
  review_prompt: prompt_id           # invocation supplies e.g. local:code-review
  aggregation_prompt: prompt_id      # invocation supplies e.g. tiddly:ai-review-feedback
  user_context: text                 # optional
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

Switchboard interacts with Claude Code and Codex through their non-interactive modes (`claude -p` and `codex exec`). The underlying sessions are real Claude Code / Codex sessions backed by the harnesses' own session files — they survive Switchboard, can be resumed later, and could in principle be opened in the harness's interactive TUI by the user if they wanted. Switchboard does not lock the user out of the harness; it just drives it.

### Process model

Per-message process spawn for v1: each turn invokes `claude -p --resume <session-id>` or `codex exec resume <session-id>`, captures the structured output stream, and exits. State persists in the harness's session files between invocations. Long-lived agent processes can be considered later if latency matters.

Switchboard consumes the harness stream by spawning the process, reading stdout line-by-line as JSONL, and dispatching each event into the normalized event stream described below. Standard pipe-and-readline; no file-watching for the basic case. Full streaming details in [docs/research/harness-comparison.md](research/harness-comparison.md).

Switchboard runs `claude -p` in its **default** mode (no `--bare`) so the agent inherits the user's full environment: skills, hooks, plugins, MCP servers, CLAUDE.md, and auto-memory all load exactly as they would in an interactive session. The Codex equivalent (we do not pass `--ignore-user-config` or `--ephemeral`) gives the same outcome: the user's `~/.codex/config.toml` and session persistence are honored. This is deliberate — Switchboard's value is to orchestrate normal Claude Code / Codex sessions, not to amputate them. Anthropic has stated that `--bare` will become the `-p` default in a future release; when that happens, Switchboard will need to pass equivalent context-loading flags (`--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, `--append-system-prompt`) to preserve current behavior. To make that change a one-place edit, harness command-line construction is centralized in a single "harness invoker" helper from day one. Tracked under open question 10.9; full background in [docs/research/claude-code-headless.md](research/claude-code-headless.md).

### Permissions and sandboxing

For MVP, Switchboard exposes a single user-facing toggle: **skip permissions (default: on)**. When on, agents run with maximum autonomy:

- Claude Code: `--dangerously-skip-permissions`
- Codex: `--dangerously-bypass-approvals-and-sandbox` (also accepts `--yolo` as an undocumented alias in 0.128.0; relying on the long form is safer)

Internally, the configuration layer maps this single toggle to the actual flags per harness, so granular control (separate sandbox modes, per-tool allowlists) can be added later without breaking the user-facing model.

**Known issues to track:**

- Codex has open bugs around `--dangerously-bypass-approvals-and-sandbox` not fully bypassing in all sub-modes (e.g., a recent regression where the directory-trust prompt fires anyway). Switchboard should pin tested Codex versions and surface any unexpected prompts as errors.
- Codex separates approval policy from sandbox mode. The MVP collapses these; v2 may expose them separately.

### Required harness commands for MVP

What Switchboard needs from each harness, with notes on what is exposed natively, derived, or unavailable. Hands-on probe results are documented in [docs/research/claude-code-cli-observed.md](research/claude-code-cli-observed.md), [docs/research/codex-cli-observed.md](research/codex-cli-observed.md), and [docs/research/harness-comparison.md](research/harness-comparison.md).

- **Spawn** a session with explicit flags. *(native, both)*
- **Send** a message and capture the structured stream. *(native, both — `claude -p --output-format stream-json` and `codex exec --json`)*
- **Detect turn completion.** *(native, both — single terminal event per turn.)* Claude Code emits `result`; Codex emits `turn.completed` on success and `turn.failed` on error. Switchboard's adapter waits for either.
- **Detect errors.** *(native, both, but asymmetric.)* Claude Code: `result.is_error` and/or `result.api_error_status` (do not rely on `subtype` — it stays `"success"` even on error). Codex: a `turn.failed` event terminates the turn, payload carries the API error. Both harnesses also exit non-zero on error.
- **Resume** a session by UUID. *(native, both — Claude Code: `--resume <uuid>`; Codex: `codex exec resume <uuid>`.)*
- **Assign a session ID at spawn.** *(Claude Code only — `--session-id <uuid>`. Codex assigns its own; Switchboard captures it from the first stream event.)*
- **Fork** a session from a checkpoint. *(Native in Claude Code via `--fork-session` with `--resume`. **Gap in Codex** — no non-interactive `codex exec fork`. Workarounds: copy the session JSONL to a new file, or start a fresh session and re-feed summarized prior context. Tracked under open question 10.14.)*
- **Read session metadata** (model, session ID, cost, tokens) from the stream. *(Asymmetric.)* Claude Code: `result.total_cost_usd`, `result.modelUsage.<model>.{contextWindow, maxOutputTokens, costUSD}`. Codex: only token counts in `turn.completed.usage`; cost must be derived via a Switchboard-maintained per-model pricing table; `model_context_window` is in the **session file** (not the stream).
- **Derive context utilization.** *(Native for Claude Code, asymmetric for Codex.)* Claude Code exposes `contextWindow` per turn in the result event — Switchboard reads it directly. Codex's stream omits it; Switchboard either reads the session file or maintains a model→max-context map. Open question 10.12 captures the choice.
- **Surface compaction state.** *(No programmatic `/compact` in either harness; both do auto-compact at high utilization.)* Switchboard's role is to monitor utilization and surface warnings as the threshold approaches, not to drive compaction itself. Reimplementing summarization in Switchboard would underperform the harnesses' tuned compaction and is not planned (see open question 10.11).
- **Read tool calls and tool results** from the stream. *(Asymmetric.)* Claude Code emits typed `tool_use` and `tool_result` content blocks (with named tools, including MCP tools). Codex routes everything through `command_execution` items (raw shell commands with `aggregated_output` and `exit_code`). Switchboard renders them differently per harness — there is no single unified rendering.
- **Capture permission denials.** *(Confirmed for Claude Code via `result.permission_denials`.)* Denials do **not** error the turn — the model receives them as feedback and adapts. Switchboard treats denials as a distinct UX category (informational, not failure). Codex behavior presumed similar; verification deferred.
- **Run agents concurrently.** *(Confirmed for Claude Code: three parallel `claude -p` from the same cwd produced three independent session files with no contention.)* This is what makes fan-out feasible. Codex concurrent runs not directly probed; presumed similar.

### Per-harness adapter and normalized event stream

The two harness streams are structurally different (event-name vocabularies, content shapes, where cost / context-window / rate-limit info appears). To keep the rest of Switchboard harness-agnostic, the harness layer is organized around **per-harness adapters** that translate native events into a normalized internal event stream the rest of the system consumes:

```
TurnStart       { agent, session_id }
ContentChunk    { agent, kind: thinking | text | tool_use, data }
ToolResult      { agent, tool_use_id, output, is_error }
TurnEnd         { agent, status: success | error,
                  stop_reason, usage: { input, output, cached, reasoning, context_window? },
                  cost_usd?, permission_denials, raw_event }
RateLimitEvent  { agent, info }
```

Each adapter (Claude Code, Codex) is responsible for: building the harness command line, spawning the process, parsing its native stream, normalizing into the events above, and surfacing harness-specific metadata in `raw_event` so callers that need to dig in can. The pattern engine, UI, and persistence layer consume only the normalized stream.

Reading Codex session files (in addition to the stream) is an implementation choice the Codex adapter may make to fill in gaps the stream doesn't expose (rate limits, context window, full reasoning). Tracked under open question 10.15.

### Passthrough mechanism

For harness commands Switchboard does not need to coordinate, the design *intent* is a passthrough: the user types a harness slash command (e.g., `/model`, `/clear`, `/cost`) when interacting with an agent, and Switchboard forwards it to the harness verbatim. This avoids reimplementing every harness feature.

**Important caveat — `claude -p` limitation today:** Headless mode does not currently accept slash commands as input — that includes built-in commands (`/cost`, `/model`, `/clear`) and user-invoked skills (`/skill-name`). This is a known upstream gap ([anthropics/claude-code#837](https://github.com/anthropics/claude-code/issues/837), [#38505](https://github.com/anthropics/claude-code/issues/38505)). Until it is resolved, Switchboard's passthrough is constrained to commands we can implement out-of-band: `/cost` derived from `--output-format json` metadata, `/model` implemented by re-spawning with a different `--model` flag, and so on. A blanket "type any slash command and forward it" is not achievable in pure headless mode today. Tracked under open question 10.10; see [docs/research/claude-code-headless.md](research/claude-code-headless.md) for sources. (The auto-invoked side of skills is unaffected — see §6.)

**Open question 5.2:** Exact mechanism for passthrough — does it require a prefix to disambiguate from Switchboard's own slash commands, or do Switchboard's commands live in a separate namespace? Partially blocked on the upstream limitation above.

### What we lose by going non-interactive

The interactive Claude Code and Codex TUIs are not used. Switchboard renders the structured output stream itself. This means rendering tool calls, diffs, todo lists, and thinking blocks is Switchboard's responsibility.

What is **preserved** because the harness still runs in default mode: hooks fire, MCP servers connect and tools work, auto-invoked skills trigger normally, sub-agents (Claude Code's `Task` tool) spawn as expected, auto-compaction runs when context climbs.

What is **lost** in headless mode:

- **Plan mode** (Claude Code's interactive plan/approve cycle) — REPL-only; no headless equivalent.
- **User-invoked slash commands** — `/cost`, `/model`, `/clear`, `/compact`, and `/skill-name` are not accepted as input in `claude -p`. See §5 passthrough and open question 10.10.
- **Programmatic compaction** — both harnesses do auto-compact; neither exposes a triggerable `/compact` from headless. See open question 10.11.
- **The harness's own TUI rendering** — Switchboard renders everything itself from the stream.

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

**Resolution rules.**

- **Pattern files require explicit prefix.** Every prompt reference in a pattern is fully qualified (e.g. `local:code-review`, `tiddly:code-review`). This keeps pattern files portable: a pattern shared between projects always resolves to the same prompt source, regardless of how the receiving user has their providers configured. There is no concept of a "default provider" for unprefixed lookup in pattern files.
- **Prefixed lookup is strict.** A prefixed ID resolves only against the named provider; if not found, it errors. No cross-provider fallback.
- **Local-store scopes.** Local-store lookup checks the project scope (`<project>/.switchboard/prompts/`) first, then the user-global scope (`~/.config/switchboard/prompts/`). A project-scoped prompt with the same name shadows the user-global one — intentional, so a project can override a personal prompt.
- **Interactive UI ergonomics.** When the user types a slash command in the message bar, the UI may provide autocomplete across all configured providers and may accept a bare name if it matches exactly one provider's prompt. This is a UI-layer affordance only — it does not affect how patterns or other persisted artifacts reference prompts.

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

The user sees the project's agents. One is currently selected; the others are accessible. The selected agent's conversation is the main view; switching to a different agent is a single action.

### Sending a message

The user composes a message via:

- A slash command (resolves to a prompt by ID; the UI may accept a bare name if it matches exactly one configured provider — see §6 resolution rules).
- Free-form text.
- Optionally both: slash command for the structured part, free-form for context.

The user picks recipient(s): the currently selected agent by default, or any combination of agents in the project. Send.

### Invoking a pattern

A pattern is invoked by name. Switchboard prompts for the pattern's inputs (which agents to use, which prompts, any free-form context). The user confirms; the pattern launches and runs autonomously.

### Watching a pattern run

The user can switch focus among agents to watch any of their outputs. The pattern continues running in the background regardless of which agent the user is currently viewing. When the pattern completes (the final step has dispatched its output), the user is notified.

### Failure handling

If a step in a pattern fails (an agent errors, a harness call fails, a template substitution fails), the pattern halts. Partial results are retained. The user sees the error, can inspect the state of each agent involved, and decides whether to retry the pattern, retry from a specific step, or abandon.

A turn that ends with a tool **permission denial** is *not* a failure. The harness reports the denial (Claude Code's `result.permission_denials`, similar in Codex), the model receives the denial as feedback and adapts its response, and the turn completes normally. Switchboard surfaces denials as informational ("the model attempted X, was blocked") rather than as pattern-halting errors. Failures are reserved for harness-level errors (`is_error: true` / `turn.failed` / non-zero exit), template substitution errors, and pattern-orchestration errors.

### Walking away

A pattern continues to run as long as the Switchboard host process is alive. Closing the UI window does not stop a pattern. Putting the machine to sleep stops a pattern (because the host process is paused). When the user returns, Switchboard shows the state of any in-progress or completed patterns.

## 8. Worked example: review-and-aggregate

To anchor the abstractions above, here is what a code-review workflow looks like end to end.

**Setup:**

The user has a project `feature-event-logs` open in Switchboard. They have three agents:

- `planner` (Claude Code, currently selected)
- `reviewer-claude` (Claude Code)
- `reviewer-codex` (Codex)

The user has previously authored a pattern in `.switchboard/patterns/review-and-aggregate.yaml`. The review prompt ships as a built-in local prompt (`local:code-review`); the aggregation wrapper is one the user keeps in Tiddly (`tiddly:ai-review-feedback`). Both work because Switchboard resolves each ID against the named provider.

**Invocation:**

1. The user invokes the pattern: "Run review-and-aggregate."
2. Switchboard prompts for each pattern input:
   - `primary_agent`: `planner` (autofilled with the currently selected agent; user can change)
   - `reviewer_agents`: `reviewer-claude`, `reviewer-codex` (multi-select)
   - `review_prompt`: `local:code-review` (bundled with Switchboard)
   - `aggregation_prompt`: `tiddly:ai-review-feedback` (the user's own, stored in Tiddly)
   - `user_context`: "Review milestone 1, focus on the event-emission API."
3. The user confirms. The pattern launches.

**Execution:**

1. Switchboard sends the review-prompt message (with user context appended) to both reviewers in parallel. Each reviewer runs.
2. Switchboard waits for both reviewers to complete their turns.
3. Switchboard collects both reviewers' final assistant messages.
4. Switchboard renders the aggregation-prompt template, substituting in the two reviews under their respective variable names.
5. Switchboard sends the rendered message to `planner` (the agent supplied as the pattern's `primary_agent` input).
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
- **Visual pattern editor.** v1 is file-based, with agent-consumable authoring docs as the supported authoring path.
- **Granular permission/sandbox config.** v1 collapses to a single toggle.
- **Cross-session persistent agent memory.** Architecture should not preclude; not implemented in v1.
- **Multi-project workflows.** Each project is independent in v1.
- **Pattern conditionals and branching.** v1 patterns are linear.

## 10. Open questions

Aggregated from inline flags above, plus a few additional:

- **5.1** Exact pattern DSL keywords and structure. Needs a separate spec.
- **5.2** Passthrough mechanism for harness commands — namespacing.
- **6.1** Templating syntax (Jinja2 vs simpler) and template-available variables beyond `responses`.
- ~~**10.1** What does Switchboard do when an agent's "next assistant response" is a tool call rather than text?~~ **Resolved by hands-on probe:** both harnesses run the model → tool_use → tool_result → model loop internally and emit a single terminal event per user-initiated turn (Claude Code: `result`; Codex: `turn.completed` / `turn.failed`). Switchboard always sees a complete turn — there is no "tool-call-only response" to handle. See [docs/research/harness-comparison.md](research/harness-comparison.md).
- **10.2** When two patterns reference the same agent, what happens? Disallow concurrent use? Queue? Refuse?
- **10.3** How are agents preserved across Switchboard restarts? Harness session IDs persist on disk; Switchboard's project/agent registry needs its own persistence model.
- **10.4** Pattern versioning. If a pattern file changes mid-execution (unlikely but possible), what happens to the in-flight pattern?
- **10.5** Notifications when a pattern completes — terminal bell? OS notification? Just visible state in the UI?
- **10.6** Multi-machine workflows (running Switchboard on a remote dev machine over SSH). Out of scope for v1, but the architecture should not fight it.
- **10.7** Local prompt file format. Markdown body with YAML frontmatter is the working assumption; alternatives (pure YAML, plain `.txt` with separate manifest) should be evaluated against authoring ergonomics and round-tripping with editors.
- **10.8** Whether the local store and the MCP-server provider need to expose the same template-arguments contract (variable names, types, defaults) so a prompt can move between them without breaking pattern files. Working assumption: yes; the local file's frontmatter mirrors what an MCP `prompts/get` response would carry.
- **10.9 (monitoring)** `--bare` will become the `claude -p` default in a future Anthropic release ([source](https://code.claude.com/docs/en/headless)). When it lands, default `-p` no longer auto-loads skills, hooks, plugins, MCP servers, or CLAUDE.md, and Switchboard must explicitly pass `--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, `--append-system-prompt`, etc. to preserve current behavior. Mitigation: harness command-line construction is centralized from day one (§5 "Process model"). Action: monitor Anthropic release notes; flip the helper when announced. Background in [docs/research/claude-code-headless.md](research/claude-code-headless.md).
- **10.10 (monitoring)** Headless slash-command support. `claude -p` does not accept slash commands today, blocking §5's full passthrough vision. Tracked upstream at [anthropics/claude-code#837](https://github.com/anthropics/claude-code/issues/837) and [#38505](https://github.com/anthropics/claude-code/issues/38505). Workarounds described in §5; full passthrough lights up automatically when upstream lands.
- **10.11** Compaction strategy. Programmatic `/compact` is unavailable in both harnesses today; both do auto-compact at high utilization. Working assumption: Switchboard monitors token usage, warns the user as the auto-compact threshold approaches, and defers actual compaction to the harness. We do not implement Switchboard-side summarization (would underperform the harnesses' tuned compaction). Alternative to consider: surface a "fork from checkpoint with summary" action that uses the existing fork primitive plus an explicit summarize-and-restart prompt, as a coarse user-driven alternative when the user wants to reclaim context outside auto-compact. Background in [docs/research/claude-code-headless.md](research/claude-code-headless.md) and [docs/research/codex-noninteractive.md](research/codex-noninteractive.md).
- **10.12** Model→max-context map maintenance. **Partially resolved by hands-on probe:** Claude Code v2.1.138 *does* expose `contextWindow` per turn in `result.modelUsage.<model>` — Switchboard reads it directly, no map needed for Claude Code. Codex's stream omits it; the value lives in the session file's `task_started` event. Working assumption for Codex: ship a bundled model→max-context map, but also let the Codex adapter read the session file and prefer that as authoritative when present. Still open: do we ship the map, read the session file, or both? Open: where is the canonical map source for new models we sync from?
- **10.13 (monitoring)** Programmatic `/compact` exposure in either harness. Multiple Anthropic feature requests open ([anthropics/claude-code#5643](https://github.com/anthropics/claude-code/issues/5643), [#39275](https://github.com/anthropics/claude-code/issues/39275), [#39574](https://github.com/anthropics/claude-code/issues/39574), [#26488](https://github.com/anthropics/claude-code/issues/26488)); Codex equivalent not documented. When upstream lands, Switchboard can offer first-class compaction control inside patterns.
- **10.14** Codex non-interactive fork. Claude Code has native `--fork-session`; Codex has no `codex exec fork`. Three options: (a) drop fork from v1's Codex agent capability and document the asymmetry; (b) implement fork by copying the session JSONL to a new file and passing the new ID to `codex exec resume` (untested — file format may not support this cleanly); (c) implement fork by spawning a fresh Codex session and re-feeding a summarized version of the prior context as the initial prompt. Decision deferred to implementation; (a) is the safe v1 default.
- **10.15** Should the Codex adapter read the session file (`~/.codex/sessions/...jsonl`) in addition to the `--json` stream? The session file carries information the stream doesn't (rate limits, `model_context_window`, full reasoning blocks). Tradeoff: more complete information vs more file-watching plumbing and the question of whether to tail-read live or read on completion. Working assumption: read on turn completion (after `turn.completed`/`turn.failed`) to enrich the normalized event stream with the missing fields.
- **10.16** Disk usage of harness session files. Both harnesses persist transcripts indefinitely (Claude Code at `~/.claude/projects/<encoded-cwd>/*.jsonl`, Codex at `~/.codex/sessions/YYYY/MM/DD/...`). A long-lived project with many agents and many turns will accumulate. Should Switchboard offer pruning, surface totals, or otherwise manage this? Out of scope for v1, but the architecture should not preclude it.
- **10.17** Network failure and retry policy. What does Switchboard do when a turn fails mid-pattern because of a transient API error or network blip? Working assumption: a single configurable retry on transient errors (rate-limit, 5xx) before marking the step as failed. Permanent errors (auth, invalid model, denied content) fail immediately. To be detailed in §7 once we have an implementation footprint.
- **10.18** Cost budgeting at the pattern and project level. Both harnesses support `--max-budget-usd` (Claude Code) per invocation. A pattern that fans out × N multiplies cost per step; a long-running project running unattended could rack up real money. Should Switchboard offer per-pattern and per-project budget caps in addition to per-invocation? Working assumption: yes for both, with clear pre-launch cost estimates for fan-out patterns. Detailed design deferred.
- **10.19** Switchboard as MCP client. The plan says "Switchboard resolves prompt IDs via the configured MCP server" — meaning Switchboard itself runs an MCP client to fetch prompts (independent of the harnesses' own MCP clients). This is an implementation responsibility worth noting explicitly: Switchboard ships an MCP client implementation for the prompt-provider feature, not just the harness wrappers.

---

*Last updated: drafted from design conversation. Subject to revision as implementation reveals gaps. Phasing and per-release plans live separately in `docs/implementation-plans/`.*
