# Switchboard

## 1. What Switchboard is

Switchboard is a **human-directed orchestrator for AI coding agents** — a desktop application you run alongside your existing Claude Code and Codex setup. It lets you spawn multiple agent sessions within a project, route messages between them, and define reusable patterns for common multi-agent workflows like second-opinion code review, plan-and-implement, and parallel-solution adjudication.

More precisely, it is a **workflow engine for primitives, not for processes**. It codifies the *shape* of common multi-agent operations (fan-out, fan-in with template wrapping, sequential handoff) so they can be invoked with one command instead of manually copy-pasted. It does not impose any larger structure on top of those primitives — there is no built-in concept of "plan phase" or "review phase," no SDLC walkthrough, no opinionated process. The user composes patterns ad hoc and saves the ones they reuse.

The human stays in the loop where judgment matters (deciding what to route, when to revise, when to proceed) and is removed from the loop where mechanics waste time (copy-paste, template application, babysitting parallel agents).

This orchestration model has a useful side effect for prompt management. Because Switchboard resolves prompts itself and sends agents plain text, **prompt-provider configuration lives in *one place* (Switchboard) and works across every agent backend**. A user's prompt library — whether in Tiddly, another MCP server, or Switchboard's local store — works identically with both Claude Code and Codex agents, without configuring the prompt source in either. This is especially useful for Codex, where MCP prompt support is limited or absent depending on version. (The same does **not** hold for MCP tools or Claude Code skills, which are invoked by the model mid-turn and must still be configured in the underlying agent; see section 6.)

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
- **Arbitrary harness slash-command passthrough.** v1 does not support invoking arbitrary harness slash commands as input (`/cost`, `/model`, `/skill-name`, etc.). This is an upstream `claude -p` limitation; specific commands are worked around individually (see §9). Tracked in §12 (10.10).

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

State is kept project-local rather than in a user-global registry so that opening the project from a different machine surfaces "no state yet" explicitly rather than silently dereferencing stale registry entries.

## 4. Functional primitives

These five primitives cover everything Switchboard needs to do at the functional level. Patterns compose them.

### Primitive 1 — Spawn an agent

Create a new agent within a project. User specifies:

- Agent type (Claude Code or Codex).
- Name (free-form label).
- Optional initial prompt (sent as the first message after spawn to prime the agent with role context, project background, or any other instructions the user wants in place before the first real turn). Authored like any other prompt — free-form text or a fully-qualified prompt ID resolved through the prompt-provider system (§6).
- Optional working directory override (defaults to project working directory).

Agent names are restricted to `[A-Za-z0-9_-]+` at creation; this avoids ambiguity when names appear as template variable identifiers (Primitive 4) or in file paths.

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

## 5. Patterns

### Authoring

Patterns are authored as YAML files at `<project>/.switchboard/patterns/`. Because they live inside the project directory, they are naturally version-controlled along with the project itself — diffed, reviewed, and shared via the user's normal git workflow. There is no directory-picker step in Switchboard's UI; the location is conventional.

Authoring is intentionally file-based. The user edits patterns in whichever editor they prefer (Vim, VS Code, etc.); Switchboard's UI reads the files but does not include an editor of its own. The supported authoring path for new users is to point an existing Claude Code or Codex agent at `docs/agent-instructions/patterns.md` and have it generate a starter pattern from a description (per §2 "Agent-friendly authoring"). Hand-authoring against the DSL spec works too for power users.

v1 ships with a small library of built-in patterns (review-and-aggregate, sequential handoff with template) as starting points; users can copy or fork these to author their own.

Users without an existing Claude Code or Codex installation outside Switchboard can use a Switchboard-spawned agent itself to author a pattern from the instruction docs — agents Switchboard manages are full Claude Code / Codex sessions and can read project files normally. Hand-authoring against the DSL spec also works for power users.

Pattern files are project-scoped only — there is no user-global pattern directory parallel to user-global prompts. Reuse across projects happens via copy or symlink. (Asymmetric with prompts on purpose: patterns tend to be project-shaped — they reference specific agent names and workflows — whereas prompts are more reusable as personal templates.)

## 6. Prompts and prompt providers

> *Scope note: this section covers prompts only. Model-invoked MCP tools and Claude Code skills remain configured per-agent, not per-Switchboard. See "Cross-agent normalization" below for the boundary.*

A **prompt** is a reusable, optionally parameterized text template — for example, "Review the diff focusing on `{{ focus }}`." Pattern files and slash commands reference prompts by ID. The *prompt text* lives in a **prompt provider**; the *workflow* lives in the pattern file. Switchboard reads pattern files, resolves prompt IDs to prompt content via the configured providers, and applies templates with substitution.

### Providers

Two providers ship in v1:

- **Local file store.** Prompts authored as files (markdown body with YAML frontmatter for metadata: id, description, arguments). Resolved across one or more directories: a fixed project scope at `<project>/.switchboard/prompts/`, plus an ordered list of user-configured directories (`local_prompt_dirs` in config — see "Configuring local prompt directories" below). This lets a power user keep their personal prompt library in their own git repo (e.g. `~/repos/my-prompts/`) instead of being limited to the OS-conventional app data directory. The local store is the lowest-friction way to author a prompt and the mechanism Switchboard uses to ship example prompts.
- **MCP-server provider.** Resolves IDs against any MCP server the user has configured that exposes prompts. [Tiddly](https://tiddly.me) is the canonical example and the development reference, but the integration is generic: pointing Switchboard at a different MCP prompt server is a configuration change, not a code change.

### Authoring a local prompt

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

Local prompts are authored file-first, the same as patterns: edit them in whichever editor you prefer (Vim, VS Code, etc.), optionally have your existing Claude Code or Codex agent generate a starter from `docs/agent-instructions/prompts.md`. Switchboard reads the files and surfaces them via slash-command autocomplete; in v1 it does not provide an in-app editor for prompts (see "Future direction: prompt library view" below for what changes in v2+).

**Frontmatter spec for v1:**

| Field | Required | Notes |
|---|---|---|
| `id` | yes | Slug; matches MCP's `name` field. Used as the suffix in `local:<id>` references. |
| `description` | yes | Short human description. Matches MCP standard. |
| `arguments` | optional | Array of `{name, description, required}`. Matches MCP standard. All arguments are treated as strings in template substitution; typed arguments may be added later if needed. No `default` field in v1 — local frontmatter intentionally mirrors MCP's surface so prompts move cleanly between providers (open question 10.8). |
| `tags` | optional | Array of strings. Matches Tiddly's tag extension. Reserved for future library/browse views (see "Future direction" below); v1 does not use them in the slash-command UI. |

This minimal set mirrors the MCP `prompts/list` standard, plus Tiddly's tag extension as a superset, so prompts move cleanly between local and MCP storage. Other metadata fields (title, owner, etc.) are explicitly out of scope for v1.

### Configuring local prompt directories and MCP-server providers

Both local prompt directories (`local_prompt_dirs`) and MCP-server providers (`mcp_providers`) are declared in YAML config at one of two scopes:

- **User-global**: `~/.config/switchboard/config.yaml` (path resolved per OS via the Rust `directories` crate). For personal preferences — your prompt library location, your Tiddly account, etc.
- **Project-scoped**: `<project>/.switchboard/config.yaml`. Adds or replaces user-global config. Useful when a team pattern needs a specific MCP provider (e.g., a team Tiddly URL distinct from the user's personal one) or when a project ships its own curated set of prompt directories.

Resolution rules differ slightly between the two keys:

- **`local_prompt_dirs`**: project's list, if set, *replaces* the user-global list (project intent is explicit).
- **`mcp_providers`**: project providers shadow user-global providers with the same `name` (entry-level merge); the user's other providers stay available.

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

**Tiddly is a first-class preset.** The Switchboard UI offers a one-click "Connect Tiddly" action: the user pastes a Personal Access Token, and the app writes the corresponding `preset: tiddly` config entry automatically. Tiddly's URL and auth pattern are baked in. Other MCP servers require manual config (or a generic "Add MCP server" form in the UI). The presets list is open — additional first-class integrations (e.g., a future popular prompt-store MCP) can be added the same way.

### Addressing prompts

Providers are addressed by a short prefix in prompt IDs:

- `local:code-review` — resolves against the local file store.
- `tiddly:code-review` — resolves against the MCP server registered under the name `tiddly`.

The prefix is the user-chosen registration name for an MCP-server provider, so a user with two MCP prompt servers configured can address both unambiguously. The `local` prefix is reserved for the built-in local store.

### Resolution rules

- **Pattern files require explicit prefix.** Every prompt reference in a pattern is fully qualified (e.g. `local:code-review`, `tiddly:code-review`). This keeps pattern files portable: a pattern shared between projects always resolves to the same prompt source, regardless of how the receiving user has their providers configured. There is no concept of a "default provider" for unprefixed lookup in pattern files.
- **Prefixed lookup is strict.** A prefixed ID resolves only against the named provider; if not found, it errors. No cross-provider fallback.
- **Local-store resolution.** Project scope (`<project>/.switchboard/prompts/`) is checked first, then each directory in `local_prompt_dirs` (from project config if present, otherwise from user config) in declared order. Default value if not configured: `[<OS-conventional path>]` (e.g. `~/.config/switchboard/prompts/` on Linux, resolved via the Rust [`directories`](https://crates.io/crates/directories) crate). A prompt with the same name in an earlier-checked directory shadows later ones — intentional, lets a project override a personal library, lets a personal library override a team library, etc. Project config's `local_prompt_dirs` (if set) replaces the user-config list rather than merging, so the project's intent is explicit.
- **Interactive UI ergonomics.** When the user types a slash command in the message bar, the UI may provide autocomplete across all configured providers and may accept a bare name if it matches exactly one provider's prompt. This is a UI-layer affordance only — it does not affect how patterns or other persisted artifacts reference prompts.
- **Prompt versioning is out of scope for v1.** Pattern references resolve to whatever the provider returns at invocation time; if a Tiddly prompt is edited, every pattern referencing it picks up the new version on the next invocation. Recovery and history are deferred to the upstream tool — Tiddly's own version history for hosted prompts, git for local prompts.

### Prompt arguments

Prompts can declare arguments (per the frontmatter spec for local prompts, or per the MCP `prompts/list` response for MCP-served prompts). At routing time:

1. Switchboard discovers a prompt's arguments from its provider (frontmatter for local; `prompts/list` for MCP).
2. The user (or the invoking pattern) supplies values for each argument.
3. **For MCP-served prompts**, Switchboard calls `prompts/get` with the supplied values; the MCP server renders the template and returns the rendered text.
4. **For local prompts**, Switchboard renders the template itself with MiniJinja (see "Wrapping templates" below for the Rust templating choice) and produces the rendered text.

The agent only ever sees the final rendered text — neither the template nor the arguments. This is what makes the "prompt-provider configuration lives in one place" property work across harnesses (see "Cross-agent normalization" below).

This separation between provider and workflow is intentional: a prompt store is a prompt store, not a workflow engine. Encoding control flow ("run agent A, then fan out to B and C, then aggregate via template D") in a stored prompt would stretch the store out of shape. Patterns are programs; prompts are data.

### Cross-agent normalization

Switchboard resolves prompt IDs itself and sends agents the rendered text as a plain message — the agent never sees the MCP call, the provider, or the arguments. The useful side effect: prompt-provider configuration lives in *one place* (Switchboard) and works the same across every agent backend. A user's prompts (Tiddly, another MCP server, or the local store) work identically with both Claude Code and Codex agents, without configuring the prompt source in either harness. This is especially useful for Codex, whose MCP prompt support is limited or absent depending on version — Switchboard gives Codex users a Claude-Code-style prompt library experience without requiring Codex to support it.

What this does **not** cover:

- **MCP tools.** Tools are invoked by the model mid-turn, not by the user pre-turn. Switchboard cannot proxy them; tools (e.g. an Atlassian MCP server, Google Drive integration) must still be configured in the underlying agent.
- **Claude Code skills.** Configured in Claude Code itself (`~/.claude/skills/`, project `.claude/skills/`); Switchboard does not mediate them. **Auto-invoked skills do work normally in Switchboard-spawned sessions** because default `claude -p` loads the user's full environment — the model can discover and invoke skills mid-turn just as it would interactively. The *user-invoked* side of skills (`/skill-name` as an explicit command) is currently unavailable due to a `claude -p` limitation; see §9 passthrough and [docs/research/claude-code-headless.md](research/claude-code-headless.md).
- **Per-agent setup in general.** Authentication, permission flags, hooks, and MCP tool registration remain the underlying harness's concern.

Switchboard normalizes the *user-invoked prompt* surface across agents. Model-invoked capabilities (tools, skills) and harness-level configuration are still per-agent.

### Wrapping templates

Wrapping templates (used for fan-in) are prompts — from any provider — that take agent responses as variables. The pattern definition declares which agent maps to which template variable. The template uses **Jinja2-compatible syntax**, rendered via [MiniJinja](https://github.com/mitsuhiko/minijinja) (a native Rust templating engine by the author of Jinja2, designed for Jinja2 compatibility — chosen so prompts move cleanly between Tiddly's Jinja2 and Switchboard's local rendering without surprises). Open question 6.1 captures whether v1 commits to MiniJinja's full surface or a restricted subset:

```jinja
The following are reviews from multiple agents:

{% for name, response in responses.items() %}
## {{ name }}

{{ response }}

{% endfor %}

Summarize the recommendations and identify points of agreement and
disagreement.
```

**Open question 6.1:** Exact templating subset (full MiniJinja vs a smaller restricted subset) and what variables are available in templates beyond `responses` (e.g., `user_context`, `agent_metadata`, `project_info`).

### Future direction: prompt library view

v1's prompt UX is slash-command-driven — the user types a slash command in the message bar, autocomplete suggests prompts across configured providers, and the prompt is sent to the agent. This is the minimum surface to make prompts useful.

A richer **prompt library view** is plausible for v2+: a "Prompts" panel that lists all prompts from all configured providers, lets the user filter by provider or tag, search by name/description/content, and edit local prompts in their editor (or open Tiddly-hosted prompts in Tiddly via deep link). This is what makes the optional `tags` field in the local prompt frontmatter useful — v1's slash command picker doesn't need them, but a library view does.

The schema and provider model already accommodate this; v1 just doesn't ship the UI. Keeping the data shape compatible (mirroring MCP's prompt schema + Tiddly's tag extension) is what makes it cheap to add later.

## 7. User-facing model

This section describes the conceptual user experience. The desktop form factor and frontend stack are documented in §10 (Form factor and distribution).

### Project list

The user opens Switchboard and sees a list of their projects. They open one, or create a new one. A project is bound to a working directory.

### Inside a project

The user sees the project's agents in a multi-pane layout — every agent's most recent output is visible at a glance, with one designated as the primary view (active focus for typing input, larger pane). Background agents (those not in the primary view) can be collapsed to a status row to reclaim space, or expanded to see their full output. Panes can be rearranged and resized within the main window. A persistent overview panel lists all agents with their real-time status (idle, processing, waiting on tool, errored) for quick triage.

Agents in v1 are **project-scoped** — they're created within a project and stay there. Cross-project / global agent templates are a planned future direction (tracked in §11) — for example, a personal "writing editor" persona that knows your voice and applies across blog posts, docs, and emails, or a "domain expert" persona carrying institutional knowledge (a regulatory framework, your team's architecture conventions, a research methodology) reusable in any project that touches the area. Optionally these could be surfaced via semantic search over the project context, suggesting which template fits.

### Per-agent status and actions

Each agent in the project surfaces real-time state alongside its conversation:

- **Status**: idle, processing, waiting on tool, errored.
- **Context utilization**: % of model context used, derived from the harness JSON (see §9 "Required harness commands"). Surfaced as a progress bar so the user can see when an agent approaches the auto-compact threshold.
- **Cost / token usage**: per turn and cumulative. Native for Claude Code (`total_cost_usd` from the harness); derived for Codex via a per-model pricing table (see §9).

Each agent also exposes a context menu of user actions:

- **Fork session** — create a new agent branched from the current state (per §9 "Fork a session from a checkpoint"; native in Claude Code, workaround for Codex per open question 10.14).
- **Open session file** — open the underlying harness JSONL session file in the user's default editor for inspection or external tooling.
- **Reset / remove** — clean up the agent (CRUD-y; not enumerated in §4 primitives, just a UI affordance).
- **Switch to interactive mode** — open the underlying session in the harness's own TUI for actions Switchboard's headless surface can't reach (manual `/compact`, plan mode, etc.).

### Composing and dispatching messages

The user's core action — whether typing a fresh message, forwarding an agent's output, or invoking a saved pattern — is one primitive: **compose a message and dispatch it to one or more agents.** The composition has three components:

- **Source.** What is being sent — any combination of: free-form text the user types, and/or the output from one or more agents (latest turn by default; the user can pick a specific earlier turn). When multiple sources are combined, the optional wrapping prompt is the natural way to control how they're stitched together via template variables.
- **Optional wrapping.** A prompt template from any provider (e.g. `local:code-review`, `tiddly:ai-review-feedback`) that the source(s) are rendered into. May be invoked via slash command in the message bar; the UI may accept a bare name if it matches exactly one configured provider (see §6 resolution rules).
- **Recipients.** One or more agents to receive the (possibly wrapped) message. Currently focused agent is the default; multi-select picks any combination of agents in the project.

These three components compose freely. Typing a fresh message is just user text + no wrapping + one recipient. A fan-out is user text + optional wrapping + many recipients. Forwarding an agent's output is the agent's turn + optional wrapping + other agents. A **pattern** is the saved (and possibly sequenced) version of one or more of these compositions — see "Invoking a pattern" below.

### Invoking a pattern

A pattern is the **saved, named, optionally sequenced and autonomous version of compose-and-dispatch.** A single-step pattern is functionally identical to a manual send — just persisted under a name for reuse. A multi-step pattern (e.g. fan-out → wait → fan-in → dispatch) adds sequencing and autonomous execution: the user invokes once, the pattern runs through multiple compose-and-dispatch steps automatically.

A pattern is invoked by name. Switchboard prompts for the pattern's inputs (which agents to use, which prompts, any free-form context). The user confirms; the pattern launches and runs autonomously.

### Watching a pattern run

All participating agents stay simultaneously visible in their panes throughout pattern execution; status indicators show which are still running, waiting, or completed. The user can collapse background agents to focus on a specific one, or expand them all to watch the work in parallel. Pattern execution is independent of which pane has focus — agents continue running in the background regardless. When the pattern completes (the final step has dispatched its output), the user is notified via OS-native notification (per §10 Form factor).

### Agent contention

Switchboard enforces **one in-flight turn per agent** at the application layer. A dispatch (whether from a pattern step or a manual user send) against an agent that is already mid-turn is refused with a clear error ("agent X is busy"); the user can switch focus to inspect the busy agent. Queueing is not implemented in v1.

This rule lives in Switchboard, not the harnesses, because **neither harness rejects same-session parallel invocation** — both accept it, both succeed, and the on-disk effects diverge unhelpfully (Claude Code grows an orphan branch in its session tree; Codex silently interleaves both turns into one flat transcript and a future resume cannot tell them apart). See [docs/research/same-session-parallel-invocation.md](research/same-session-parallel-invocation.md) for the probe and the empirical findings. Since the harnesses don't protect us, the dispatcher must.

Two patterns invoked simultaneously that target *disjoint* agents both run normally. The constraint is per-agent, not per-pattern.

### Failure handling

If a step in a pattern fails (an agent errors, a harness call fails, a template substitution fails), the pattern halts. Partial results are retained. The user sees the error, can inspect the state of each agent involved, and decides whether to retry the pattern, retry from a specific step, or abandon.

A turn that ends with a tool **permission denial** is *not* a failure. The harness reports the denial (Claude Code's `result.permission_denials`, similar in Codex), the model receives the denial as feedback and adapts its response, and the turn completes normally. Switchboard surfaces denials as informational ("the model attempted X, was blocked") rather than as pattern-halting errors. Failures are reserved for harness-level errors (`is_error: true` / `turn.failed` / non-zero exit), template substitution errors, and pattern-orchestration errors.

### Walking away

Patterns run inside the Switchboard backend (the Rust core; see §10 Form factor). They keep running as long as the backend process is alive — independent of whether the UI window is visible. Specifically:

- **Minimize / hide the window**: backend keeps running normally; pattern continues.
- **Close the window** (X button): hides the app to the system tray (or dock on macOS). The backend stays up; the pattern continues. The user can reopen the window from the tray icon to check on progress.
- **Quit the app explicitly** (cmd-Q, tray-menu Quit): stops the backend. If any patterns are in progress, Switchboard prompts the user to confirm and then cancels them cleanly (see "Cancelling a pattern or turn" below).
- **Machine sleep**: backend is suspended with the OS. In-flight harness calls may time out across long sleeps; on wake Switchboard surfaces any failed turns and lets the user retry. Patterns themselves don't auto-resume mid-turn.

When the user returns, Switchboard shows the state of any patterns that completed, are in progress, or were cancelled.

### Cancelling a pattern or turn

The user can cancel at two granularities:

- **Cancel a pattern.** Stops the pattern's orchestration. Switchboard sends `SIGTERM` to the in-flight harness subprocess (using the process group it spawned in, so both single-process Claude Code and Codex's parent+child tree are cleaned up uniformly — see §9 and the harness-cancellation research note). Partial results stay: the agent's harness session file persists on disk and can be inspected or sent further messages. The pattern is marked **cancelled** and cannot be auto-resumed — re-invoking starts from the beginning.
- **Cancel an agent's turn.** Kills the spawned harness subprocess for a single agent's in-flight turn (useful if the agent is going off the rails). Same `SIGTERM`-to-process-group mechanism. The agent stays around and can be re-prompted; the harness session is in a usable state for the next message, with the cancelled turn simply absent.

If Switchboard buffered streaming output from the cancelled turn (whatever the agent had produced before the kill), the user can review that partial content in-app — it's available from the buffered stream, not from the harness session file (see the harness-cancellation research note for why). The buffered partial content is in-memory only; restarting Switchboard discards it. The harness session file remains the durable record (which by design omits incomplete turns).

## 8. Worked example: review-and-aggregate

To anchor the abstractions above, here is what a code-review workflow looks like end to end.

**Setup:**

The user has a project `feature-event-logs` open in Switchboard. They have three agents:

- `implementer` (Claude Code, currently selected)
- `reviewer-claude` (Claude Code)
- `reviewer-codex` (Codex)

The user has previously authored a pattern in `.switchboard/patterns/review-and-aggregate.yaml`. The review prompt ships as a built-in local prompt (`local:code-review`); the aggregation wrapper is one the user keeps in Tiddly (`tiddly:ai-review-feedback`). Both work because Switchboard resolves each ID against the named provider.

**Invocation:**

1. The user invokes the pattern: "Run review-and-aggregate."
2. Switchboard pops up an invocation form with one field per input the pattern declared in its YAML (`primary_agent`, `reviewer_agents`, `review_prompt`, etc. — see §4 for the schema). The user fills in:
   - **`primary_agent`** → `implementer` (autofilled with the currently selected agent; user can change)
   - **`reviewer_agents`** → `reviewer-claude` and `reviewer-codex` (multi-select)
   - **`review_prompt`** → `local:code-review` (bundled with Switchboard)
   - **`aggregation_prompt`** → `tiddly:ai-review-feedback` (the user's own, stored in Tiddly)
   - **`user_context`** → "Review milestone 1, focus on the event-emission API."
3. The user confirms. The pattern launches.

**Execution:**

1. Switchboard sends the review-prompt message (with user context appended) to both reviewers in parallel. Each reviewer runs.
2. Switchboard waits for both reviewers to complete their turns.
3. Switchboard collects both reviewers' final responses.
4. Switchboard renders the aggregation-prompt template, substituting in the two reviews under their respective variable names.
5. Switchboard sends the rendered message to `implementer` (the agent supplied as the pattern's `primary_agent` input).
6. The implementer runs and produces its response.
7. Pattern complete. The user is notified.

**During execution:**

All three agents stay simultaneously visible in their panes throughout. While both reviewers are running, the user can watch both streams in parallel — or collapse the reviewer panes down to just their status indicators (running / completed) and let them work in the background. When the implementer kicks in, its pane comes alive with the aggregation. The user doesn't have to switch around to know what's happening.

**Afterwards:**

The user reads the implementer's response and decides what's next. Common follow-ups: compose-and-dispatch the response onward (e.g., forward to a follow-up agent with a wrapping prompt — see §7 "Composing and dispatching messages"), invoke another pattern, or just stop. The pattern is done; the next move is the user's.

## 9. Harness integration

Switchboard interacts with Claude Code and Codex through their non-interactive modes (`claude -p` and `codex exec`). The underlying sessions are real Claude Code / Codex sessions backed by the harnesses' own session files — they survive Switchboard, can be resumed later, and could in principle be opened in the harness's interactive TUI by the user if they wanted. Switchboard does not lock the user out of the harness; it just drives it.

The architectural backbone of this section is the **per-harness adapter** pattern: one adapter per harness translates that harness's native event stream into a normalized internal stream the rest of Switchboard consumes. This keeps the pattern engine, UI, and persistence layer harness-agnostic, while letting each adapter handle its own quirks (event vocabularies, exit-code semantics, session-file richness, etc.). See "Per-harness adapter and normalized event stream" below for the shape; see [docs/research/harness-comparison.md](research/harness-comparison.md) for the cross-harness comparison that drove the design.

### Process model

Per-message process spawn for v1: each turn invokes `claude -p --resume <session-id>` or `codex exec resume <session-id>`, captures the structured output stream, and exits. State persists in the harness's session files between invocations. Long-lived agent processes can be considered later if latency matters.

Switchboard runs `claude -p` in its **default** mode (no `--bare`) so the agent inherits the user's full environment: skills, hooks, plugins, MCP servers, CLAUDE.md, and auto-memory all load exactly as they would in an interactive session. The Codex equivalent (we do not pass `--ignore-user-config` or `--ephemeral`) gives the same outcome: the user's `~/.codex/config.toml` and session persistence are honored. This is deliberate — Switchboard's value is to orchestrate normal Claude Code / Codex sessions, not to amputate them. Anthropic has stated that `--bare` will become the `-p` default in a future release; when that happens, Switchboard will need to pass equivalent context-loading flags (`--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, `--append-system-prompt`) to preserve current behavior. To make that change a one-place edit, harness command-line construction is centralized in a single "harness invoker" helper from day one. Tracked under open question 10.9; full background in [docs/research/claude-code-headless.md](research/claude-code-headless.md).

Switchboard consumes the harness stream by spawning the process, reading stdout line-by-line as JSONL, and dispatching each event into the normalized event stream described below. Standard pipe-and-readline; no file-watching for the basic case. Full streaming details in [docs/research/harness-comparison.md](research/harness-comparison.md).

**Process group**: the harness is spawned in its own process group (Rust: `Command::process_group(0)`) so cancellation can `killpg` the entire group with one signal. This handles both Claude Code (single process) and Codex (Node parent + Rust child) uniformly — verified empirically; see the cancellation sections of the per-harness research notes. **Note**: Codex's parent process catches `SIGTERM` and exits with code `0`, so Switchboard cannot detect cancellation from the exit code alone — it relies on the absence of a terminal event in the stream (`turn.completed` or `turn.failed`).

### Permissions and sandboxing

For v1, Switchboard runs both harnesses with maximum autonomy — skip-permissions is effectively required, not optional:

- Claude Code: `--dangerously-skip-permissions`
- Codex: `--dangerously-bypass-approvals-and-sandbox` (also accepts `--yolo` as an undocumented alias in 0.128.0; relying on the long form is safer)

This is a deliberate v1 simplification. Headless mode has no native interactive permission prompt UX, and building one inside Switchboard (intercept denials at runtime, modal-prompt the user, re-issue the turn) is non-trivial work that we don't want to gate v1 on. Granular permission control is a deferred design decision — see §11.

**Known issues to track:**

- Codex has open bugs around `--dangerously-bypass-approvals-and-sandbox` not fully bypassing in all sub-modes (e.g., a recent regression where the directory-trust prompt fires anyway). Switchboard should pin tested Codex versions and surface any unexpected prompts as errors.
- Codex separates approval policy from sandbox mode. The MVP collapses these; v2 may expose them separately.

### Safety guidance

Switchboard's v1 posture (autonomous agents with full filesystem and shell access, patterns that can run unattended) makes git-based projects strongly recommended: uncommitted local damage is recoverable; surprise rewrites of untracked files are not. Concrete guidance:

- Run patterns inside a project that's checked into git.
- Commit work in progress before invoking long-running patterns.
- Treat unattended runs as risky-by-default.

A one-time first-launch acknowledgement dialog (with a checkbox the user must tick to proceed) surfaces this posture explicitly so users opt in knowingly rather than discovering the autonomy posture after damage. This is a v1 acceptance item.

### Harness capabilities Switchboard depends on

The capabilities and behaviors Switchboard needs from each harness, with notes on what is exposed natively, derived, or unavailable. Hands-on probe results are documented in [docs/research/claude-code-cli-observed.md](research/claude-code-cli-observed.md), [docs/research/codex-cli-observed.md](research/codex-cli-observed.md), and [docs/research/harness-comparison.md](research/harness-comparison.md).

**Summary table** (details in the prose below):

| Capability | Claude Code | Codex |
|---|---|---|
| Spawn with explicit flags | native | native |
| Send + capture structured stream | native | native |
| Detect turn completion | native | native |
| Detect errors | native | native (asymmetric payload) |
| Resume by UUID | native | native |
| Assign session ID at spawn | native | unavailable (captured from stream) |
| Fork from checkpoint | native | unavailable (workarounds — see 10.14) |
| Read session metadata (cost, tokens, context window) | native | partial (cost derived; context-window in session file) |
| Derive context utilization | native | derived (session file or pricing map) |
| Programmatic compaction | unavailable (auto only) | unavailable (auto only) |
| Tool calls + results in stream | native (typed blocks) | native (`command_execution` only) |
| Capture permission denials | native | presumed (verification deferred) |
| Run agents concurrently | confirmed | presumed |

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
- **Run agents concurrently.** *(Confirmed for Claude Code: three parallel `claude -p` from the same cwd produced three independent session files with no contention.)* This is what makes fan-out feasible. Codex's process model (parent + child) and per-session-file isolation suggest the same property holds; explicit verification deferred to implementation.

### Per-harness adapter and normalized event stream

The two harness streams are structurally different (event-name vocabularies, content shapes, where cost / context-window / rate-limit info appears). To keep the rest of Switchboard harness-agnostic, the harness layer is organized around **per-harness adapters** that translate native events into a normalized internal event stream the rest of the system consumes:

```
TurnStart       { agent, session_id }
ContentChunk    { agent, kind: thinking | text | tool_use, data }
ToolResult      { agent, tool_use_id, output, is_error }
TurnEnd         { agent, status: success | error,
                  stop_reason, usage: { input, output, cached, reasoning, context_window? },
                  cost_usd?, permission_denials, raw_event }
RateLimitEvent  { agent, info }    // info: harness-specific shape, surfaced for UI display, not interpreted by the pattern engine
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
- **User-invoked slash commands** — `/cost`, `/model`, `/clear`, `/compact`, and `/skill-name` are not accepted as input in `claude -p`. See the passthrough section above and open question 10.10.
- **Programmatic compaction** — both harnesses do auto-compact; neither exposes a triggerable `/compact` from headless. See open question 10.11.
- **The harness's own TUI rendering** — Switchboard renders everything itself from the stream.

### Integration testing

Switchboard's per-harness adapters are exercised by an integration test suite that runs against the **real, installed Claude Code and Codex CLIs** — not mocks. This is critical: adapter correctness depends on harness behavior we don't control (event vocabularies, exit codes, stream timing, session-file format), and mocked tests would silently lock in our current understanding while upstream releases drift. Real-harness tests catch those regressions on the next CI run. Switchboard's own logic (pattern parser, prompt resolver, MCP client, normalized event dispatcher) is covered separately by ordinary unit tests with the harness mocked at the adapter boundary.

To keep the integration suite affordable in time and API cost, every test prompt is constrained to a small response (e.g., "reply with the single word 'ack'", not "write me a poem"). Modern Claude / Codex usage limits are generous enough that even a thorough suite runs in minutes-to-tens-of-minutes for a small dollar amount per CI run. The constraint isn't test count — it's per-test response size.


## 10. Form factor and distribution

### Form factor: single-binary desktop app

Switchboard ships as a **single-binary desktop application** rather than a TUI or browser-based tool. Reasoning:

- The UX vision (multi-pane agent dashboards, real-time per-agent status, expand/collapse outputs, native context menus, slick aesthetics) is desktop-shaped — TUIs can approximate it but always feel cramped at the high end.
- Single-binary distribution: download an installer or run a package-manager command, double-click. No language runtime prereq, no browser tab to manage, no separate server to start.
- Native OS integration: dock icon, system notifications, native file dialogs, proper window management, system tray.
- The "anyone who wants" audience benefits more from a polished desktop app than from either a TUI or a browser-tab UX.

### Framework: Tauri (Rust core + WebView frontend)

[Tauri](https://tauri.app/) is the chosen framework. Reasons:

- **Single small binary** (~3 MB Hello World, vs Electron's ~150 MB). Sub-half-second startup, low memory footprint.
- **OS-native WebView** for rendering — WebKit on macOS, WebView2 on Windows, WebKitGTK on Linux. ~99% of modern web tech works identically across platforms. (Linux's WebKitGTK lags WebKit-on-macOS in version and bug surface; sticking to widely-supported CSS/JS features avoids cross-platform rendering surprises.)
- **Rust core** handles backend logic: filesystem, harness adapters, MCP client, IPC handlers. Single-process app — no Python subprocess, no separate server.
- **Web frontend** (HTML/CSS/JS) renders the UI in the WebView and talks to the Rust core via Tauri's typed command system: the WebView calls Rust functions via `invoke()` (typed inputs and return values), and the Rust core streams events back via Tauri's event API (the harness streams Switchboard receives are republished as events the frontend subscribes to). Standard web tech, any framework.
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

- **Long-lived agent processes.** Per-message spawn for v1; may revisit if latency dominates.
- **Visual pattern editor.** v1 is file-based, with agent-consumable authoring docs as the supported authoring path.
- **Granular permission / sandbox config.** v1 runs both harnesses with maximum autonomy (skip-permissions on); the off / restricted-mode user experience is deferred. Plausible future directions: **config-driven tool allowlists** (YAML per project / per agent, passed through as `--allowedTools` / `--permission-mode`), **interactive permission prompts** (Switchboard intercepts denials at runtime and pops a modal asking the user to allow / deny / always-allow, then re-issues the turn — pending a probe of harness resume-after-denial mechanics), and **per-pattern permission scoping** (a pattern step declares its required tools, Switchboard restricts the harness for the duration). Codex's separate approval-policy vs sandbox-mode distinction (currently collapsed into the single max-autonomy posture) is part of this same design space.
- **Cross-session persistent agent memory.** Architecture should not preclude; not implemented in v1.
- **Global / cross-project agent templates.** Agents in v1 are project-scoped. A future direction lets users define reusable agent templates (personas) that can be invoked from any project — for example, a personal "writing editor" persona that knows your voice and applies across blog posts, docs, and emails, or a "domain expert" persona carrying institutional knowledge (a regulatory framework, your team's architecture conventions, a research methodology) reusable in any project that touches the area. Optionally surfaced via semantic search over the project context to suggest which template fits. Distinct from "Cross-session persistent agent memory" above (memory is what an agent remembers across sessions; global templates are which agents are available to spawn).
- **Multi-project workflows.** Each project is independent in v1. (Related to "Global / cross-project agent templates" above — both concern workflows that span more than one project.)
- **Pattern conditionals and branching.** v1 patterns are linear.

## 12. Open questions

Aggregated from inline flags above, plus a few additional:

- **5.1** Exact pattern DSL keywords and structure. Needs a separate spec.
- **5.2** Passthrough mechanism for harness commands — namespacing. Partially blocked on 10.10; namespacing only matters once upstream allows arbitrary slash-command passthrough.
- **6.1** MiniJinja subset (full MiniJinja vs a restricted subset) and template-available variables beyond `responses`.
- ~~**10.1** What does Switchboard do when an agent's "next assistant response" is a tool call rather than text?~~ **Resolved by hands-on probe:** both harnesses run the model → tool_use → tool_result → model loop internally and emit a single terminal event per user-initiated turn (Claude Code: `result`; Codex: `turn.completed` / `turn.failed`). Switchboard always sees a complete turn — there is no "tool-call-only response" to handle. See [docs/research/harness-comparison.md](research/harness-comparison.md).
- ~~**10.2** When two patterns reference the same agent, what happens? Disallow concurrent use? Queue? Refuse?~~ **Resolved by hands-on probe:** Switchboard enforces one in-flight turn per agent at the application layer; collisions are refused with a clear error. Queueing is deferred. See §7 "Agent contention" and [docs/research/same-session-parallel-invocation.md](research/same-session-parallel-invocation.md). The harnesses themselves do not error on same-session parallel invocation — they silently corrupt (Claude Code: orphan branch in session tree) or conflate (Codex: interleaved transcript) — so this enforcement must live in Switchboard.
- **10.3** How are agents preserved across Switchboard restarts? Harness session IDs persist on disk; Switchboard's project/agent registry needs its own persistence model.
- **10.4** Pattern versioning. If a pattern file changes mid-execution (unlikely but possible), what happens to the in-flight pattern?
- ~~**10.5** Notifications when a pattern completes — terminal bell? OS notification? Just visible state in the UI?~~ **Resolved by §10 form factor commitment:** OS-native notifications via Tauri's notification plugin. See §7 "Watching a pattern run" for when notifications fire. Remaining UX details (which events notify, user opt-out controls) are implementation choices, not plan-level questions.
- **10.6** Multi-machine workflows (running Switchboard on a remote dev machine over SSH). Out of scope for v1, but the architecture should not fight it.
- ~~**10.7** Local prompt file format. Markdown body with YAML frontmatter is the working assumption; alternatives (pure YAML, plain `.txt` with separate manifest) should be evaluated against authoring ergonomics and round-tripping with editors.~~ **Resolved:** committed to markdown body with YAML frontmatter for v1. Schema documented in §6 "Authoring a local prompt".
- **10.8** Whether the local store and the MCP-server provider need to expose the same template-arguments contract (variable names, types, defaults) so a prompt can move between them without breaking pattern files. Working assumption: yes; the local file's frontmatter mirrors what an MCP `prompts/get` response would carry.
- **10.9 (monitoring)** `--bare` will become the `claude -p` default in a future Anthropic release ([source](https://code.claude.com/docs/en/headless)). When it lands, default `-p` no longer auto-loads skills, hooks, plugins, MCP servers, or CLAUDE.md, and Switchboard must explicitly pass `--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, `--append-system-prompt`, etc. to preserve current behavior. Mitigation: harness command-line construction is centralized from day one (§9 "Process model"). Action: monitor Anthropic release notes; flip the helper when announced. Background in [docs/research/claude-code-headless.md](research/claude-code-headless.md).
- **10.10 (monitoring)** Headless slash-command support. `claude -p` does not accept slash commands today, blocking §9's full passthrough vision. Tracked upstream at [anthropics/claude-code#837](https://github.com/anthropics/claude-code/issues/837) and [#38505](https://github.com/anthropics/claude-code/issues/38505). Workarounds described in §9; full passthrough lights up automatically when upstream lands.
- **10.11** Compaction strategy. Programmatic `/compact` is unavailable in both harnesses today; both do auto-compact at high utilization. Working assumption: Switchboard monitors token usage, warns the user as the auto-compact threshold approaches, and defers actual compaction to the harness. We do not implement Switchboard-side summarization (would underperform the harnesses' tuned compaction). Alternative to consider: surface a "fork from checkpoint with summary" action that uses the existing fork primitive plus an explicit summarize-and-restart prompt, as a coarse user-driven alternative when the user wants to reclaim context outside auto-compact. Background in [docs/research/claude-code-headless.md](research/claude-code-headless.md) and [docs/research/codex-noninteractive.md](research/codex-noninteractive.md).
- **10.12** Model→max-context map maintenance. **Partially resolved by hands-on probe:** Claude Code v2.1.138 *does* expose `contextWindow` per turn in `result.modelUsage.<model>` — Switchboard reads it directly, no map needed for Claude Code. Codex's stream omits it; the value lives in the session file's `task_started` event. Working assumption for Codex: ship a bundled model→max-context map, but also let the Codex adapter read the session file and prefer that as authoritative when present. Still open: do we ship the map, read the session file, or both? Open: where is the canonical map source for new models we sync from?
- **10.13 (monitoring)** Programmatic `/compact` exposure in either harness. Multiple Anthropic feature requests open ([anthropics/claude-code#5643](https://github.com/anthropics/claude-code/issues/5643), [#39275](https://github.com/anthropics/claude-code/issues/39275), [#39574](https://github.com/anthropics/claude-code/issues/39574), [#26488](https://github.com/anthropics/claude-code/issues/26488)); Codex equivalent not documented. When upstream lands, Switchboard can offer first-class compaction control inside patterns.
- **10.14** Codex non-interactive fork. Claude Code has native `--fork-session`; Codex has no `codex exec fork`. Three options: (a) drop fork from v1's Codex agent capability and document the asymmetry; (b) implement fork by copying the session JSONL to a new file and passing the new ID to `codex exec resume` (untested — file format may not support this cleanly); (c) implement fork by spawning a fresh Codex session and re-feeding a summarized version of the prior context as the initial prompt. Decision deferred to implementation; (a) is the safe v1 default.
- **10.15** Should the Codex adapter read the session file (`~/.codex/sessions/...jsonl`) in addition to the `--json` stream? The session file carries information the stream doesn't (rate limits, `model_context_window`, full reasoning blocks). Tradeoff: more complete information vs more file-watching plumbing and the question of whether to tail-read live or read on completion. Working assumption: read on turn completion (after `turn.completed`/`turn.failed`) to enrich the normalized event stream with the missing fields.
- **10.16** Disk usage of harness session files. Both harnesses persist transcripts indefinitely (Claude Code at `~/.claude/projects/<encoded-cwd>/*.jsonl`, Codex at `~/.codex/sessions/YYYY/MM/DD/...`). A long-lived project with many agents and many turns will accumulate. Should Switchboard offer pruning, surface totals, or otherwise manage this? Out of scope for v1, but the architecture should not preclude it.
- **10.17** Network failure and retry policy. What does Switchboard do when a turn fails mid-pattern because of a transient API error or network blip? Working assumption: a single configurable retry on transient errors (rate-limit, 5xx) before marking the step as failed. Permanent errors (auth, invalid model, denied content) fail immediately. To be detailed in §7 once we have an implementation footprint.
