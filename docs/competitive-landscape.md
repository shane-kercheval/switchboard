# Switchboard — Competitive Landscape

*Researched May 2026. Covers the multi-agent coding orchestration space as it exists now.*

## Summary

The multi-agent coding tool space is crowded at both ends of a spectrum, with a genuine gap in the middle where Switchboard sits:

- **Fully autonomous** — give a goal, the system decomposes and runs with it. Human-in-the-loop is minimal or post-hoc.
- **Passive session managers** — dashboards and TUIs that help you watch and manually control multiple agent terminals. No workflow automation.

Switchboard occupies the middle: **human-directed, workflow-file-driven, with explicit pause-and-interjection primitives**. Nothing currently in the landscape clearly occupies that space.

---

## Passive session managers

These tools help you run and watch multiple agent sessions. They do not pass messages between agents or automate any routing. The user is still the message bus.

### [Claude Squad](https://github.com/smtg-ai/claude-squad)

Terminal-native TUI (Go). Uses tmux for session isolation and git worktrees for branch separation. Supports Claude Code, Codex, Aider, OpenCode, Amp, Gemini via configurable launch commands. Single window with keyboard navigation across sessions, status display, attach/detach, and optional auto-yes (yolo) mode for unattended runs.

**What it does not do:** No workflow automation, no fan-out/fan-in, no prompt library, no YAML DSL, no inter-agent message routing of any kind.

### [Nimbalyst](https://nimbalyst.com/)

Desktop app (macOS/Windows/Linux, MIT-licensed, open source). Visual workspace for Claude Code and Codex (OpenCode and GitHub Copilot in alpha). Features: session kanban board, 7+ visual editors (WYSIWYG markdown, Excalidraw, Mermaid, ERD, CSV, mockups, Monaco code), inline red/green diff review, git worktree support, MCP server integration, iOS companion app, SOC 2 certified.

**On inter-agent communication specifically:** Nimbalyst does not pass messages between agent instances programmatically. The "Agent Orchestration" feature is a kanban board for organizing sessions. One of their own comparison articles describes the problem as "the user becomes the message bus between agents, copy-pasting context and re-explaining decisions" — Nimbalyst makes that manual process less painful (visual diff review, easy session switching) but does not eliminate it. Routing one agent's output to another as a workflow primitive does not exist.

### [Conductor](https://amux.io/blog/best-multi-agent-orchestrators-2026/) (Melty Labs)

Free macOS desktop app. Visual dashboard, diff-first review UI, Claude and Codex side-by-side. Light and fast. Comparable to Nimbalyst in tier (visual management of isolated worktrees), lighter on editors, stronger on diff review UX.

**What it does not do:** No workflow files, no inter-agent routing.

### [Agent Deck](https://github.com/asheshgoplani/agent-deck)

TUI session manager for Claude, Gemini, OpenCode, Codex. Similar tier to Claude Squad, less mature.

### [amux](https://github.com/mixpeek/amux)

tmux-based agent multiplexer for running dozens of parallel Claude Code sessions unattended. Web dashboard and kanban, self-healing (auto-compact, restart on corruption). Agents can discover peers and delegate work via a REST API + shared global memory — the closest thing in this tier to inter-agent communication, but it is infrastructure for headless unattended runs, not a human-in-the-loop workflow tool.

---

## Fully autonomous / AI-directed systems

These tools take a goal and run with it. Human checkpoints are minimal or post-hoc. They solve a different problem from Switchboard.

### [Augment Code Intent](https://docs.augmentcode.com/intent/overview)

macOS desktop app (public beta; Windows coming). The most sophisticated product in this category. Three-agent architecture: Coordinator breaks down a spec into tasks → Specialist agents (6 personas: Implementation, Architecture, Testing, etc.) execute in parallel → Verifier validates against spec before human review. The "living spec" — a self-updating document all agents share — is the core innovation.

**Key differences from Switchboard:**
- Fully AI-directed: you write a spec and the system runs. Human checkpoints are post-hoc (review after, not mid-workflow).
- No explicit pause-for-user-input primitive that encodes a mid-workflow human decision point.
- Augment-model-only (their Context Engine); not a harness-agnostic layer over Claude Code + Codex.
- No user-authored workflow files; no YAML DSL.

### [oh-my-claudecode](https://ohmyclaudecode.com/)

32 specialized agents (Planner, Architect, Critic, Explorer, Executor, etc.), 40+ skills. Pipeline: team-plan → team-prd → team-exec → team-verify → team-fix, loops until verify passes. Smart model routing (Haiku for simple, Opus for complex). Runs up to 5 Claude instances in parallel. Zero-config.

**Key difference:** Fully autonomous swarm; you describe intent and it runs. Not designed for workflows where the human interjects at defined points with their own judgment.

### [Claude Code Agent Teams](https://code.claude.com/docs/en/agent-teams) (Anthropic, experimental)

One Claude Code session acts as team lead, coordinates teammates via a shared task list; teammates run in their own context windows. Still experimental and disabled by default. AI-directed (the lead agent decides what to delegate), not human-directed.

### [ComposioHQ/agent-orchestrator](https://github.com/ComposioHQ/agent-orchestrator)

Plans tasks, spawns agents, handles CI fixes, merge conflicts, and code reviews. Autonomous pipeline for unattended parallel feature work.

---

## Scripting / framework tier

### [wshobson/agents](https://github.com/wshobson/agents)

Intelligent automation and multi-agent orchestration scripts for Claude Code. Script/hook-level rather than a DSL + desktop app.

### [claude-code-by-agents](https://github.com/baryhuang/claude-code-by-agents)

Desktop app and API for multi-agent Claude Code orchestration via @mentions. Coordinates local and remote agents. More developer-API-focused than user-workflow-focused.

### DIY shell scripts

There is a whole genre of blog posts and GitHub repos that approximate Switchboard's fan-out/fan-in by scripting multiple `claude -p` invocations, collecting outputs, and routing them manually. This works but is brittle, not portable across projects, and requires rewriting per workflow shape.

---

## Where Switchboard's gap is real

Mapping the features of the tools above against Switchboard's core capabilities:

| Capability | Claude Squad | Nimbalyst | Conductor | amux | Intent | oh-my-claude |
|---|---|---|---|---|---|---|
| Multi-agent session management | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Fan-out (dispatch same message to N agents) | manual | manual | manual | manual | AI-directed | AI-directed |
| Fan-in (aggregate N outputs → one agent) | manual | manual | manual | partial | AI-directed | AI-directed |
| Reusable YAML workflow files | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Named, parameterized, version-controlled workflows | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Explicit mid-workflow human pause primitive | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Prompt library (local + MCP) across harnesses | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Works across Claude Code + Codex uniformly | ✓ | ✓ | ✓ | partial | ✗ | ✗ |
| Human-directed (not AI-directed) orchestration | ✓ | ✓ | ✓ | ✓ | ✗ | ✗ |

**The specific features no existing tool has:**

1. **Reusable YAML workflow DSL** that encodes a multi-step, multi-agent operation (fan-out → wait → fan-in → pause → dispatch) as a named, parameterized, version-controlled file you invoke by name.

2. **Explicit `pause_for_user` as a workflow primitive** — encoding "run these autonomous steps, then stop and get my input, then continue" is the shape of real human-in-the-loop workflows (e.g.: run review agents autonomously, aggregate their output, pause and show me the summary before I decide what to tell the implementer). No tool formalizes this.

3. **Cross-harness prompt library management** — resolving `tiddly:ai-review-feedback` and `local:code-review` as a unified prompt provider surface that works identically across Claude Code and Codex sessions. Prompt-provider configuration in one place, not per-agent.

4. **Fan-in with template wrapping** — taking N agents' outputs, composing them with a wrapping prompt (e.g., the Tiddly `ai-review-feedback` prompt), and dispatching the result to another agent as a spec-level primitive, not a copy-paste operation.

---

## Closest competitors by dimension

| What you care about | Closest existing option | Gap |
|---|---|---|
| Desktop app for managing multiple Claude Code / Codex sessions | Nimbalyst | No inter-agent routing; no workflow files |
| Terminal-based multi-agent session manager | Claude Squad | TUI only; no automation |
| Autonomous multi-agent coding loop | Intent (Augment) | AI-directed, not human-directed |
| Scripted fan-out/fan-in | DIY shell scripts | Not reusable, not portable, no UI |
| Prompt library across agents | — | Nothing |
| Workflow files + human pause points | — | Nothing |

---

## References

- [Claude Squad](https://github.com/smtg-ai/claude-squad)
- [Nimbalyst](https://nimbalyst.com/) / [Features](https://nimbalyst.com/features/)
- [amux](https://github.com/mixpeek/amux)
- [Conductor — Best Multi-Agent Orchestrators 2026 (amux blog)](https://amux.io/blog/best-multi-agent-orchestrators-2026/)
- [Augment Code Intent](https://docs.augmentcode.com/intent/overview)
- [oh-my-claudecode](https://github.com/yeachan-heo/oh-my-claudecode)
- [Claude Code Agent Teams](https://code.claude.com/docs/en/agent-teams)
- [ComposioHQ/agent-orchestrator](https://github.com/ComposioHQ/agent-orchestrator)
- [claude-code-by-agents](https://github.com/baryhuang/claude-code-by-agents)
- [Best Multi-Agent Desktop Apps for Claude Code, Codex (Nimbalyst blog)](https://nimbalyst.com/blog/best-multi-agent-desktop-apps-claude-code-codex-2026/)
- [Best Multi-Agent Coding Tools in 2026 (Nimbalyst blog)](https://nimbalyst.com/blog/best-multi-agent-coding-tools-2026/)
