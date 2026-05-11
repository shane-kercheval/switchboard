# Switchboard v1 implementation plan

## How to read this doc

This is the v1 implementation plan, organized as seven sequenced milestones. Each milestone is independently demoable — by the end of M*N*, there's something concrete a user (or the maintainer) can use end-to-end at the level of capability M*N* added.

The canonical "what is Switchboard and why" lives in [`docs/system-design.md`](system-design.md); this plan answers "in what order do we build it." The system-design is intentionally intent-level (architectural commitments without implementation specifics); this plan is implementation-level (specific files, types, dependencies, acceptance criteria).

**Each milestone below is currently outlined only.** Per-milestone expansion to implementation-grade detail (file layout, Rust types and function signatures, Tauri command shapes, schemas, crate-with-version dependencies, testable acceptance criteria, gotcha notes) happens in dedicated passes once the prerequisites land — see "Prerequisites" below.

## Prerequisites

Two artifacts are needed before per-milestone implementation specs can be expanded:

- **`docs/workflow-spec.md`** — the formal workflow DSL spec (open question 5.1 in system-design). Keywords, schema, escape hatches, error handling, template-function surface (`responses_from(...)` mapping form for Switchboard-aware prompts; `aggregated_responses(...)` text-blob form for cross-platform prompts), iteration variable scoping (Primitive 6), step-checkpoint semantics. Blocks M3+ specs from being concretely expandable.
- **A pricing table for Codex cost derivation** — small data file listing per-model token prices. Needed in M2 for normalized cost reporting; can be drafted any time before then.

## v1 scope (consolidated from system-design §11 / §12)

### In v1

- Multi-agent project workspace, file-based config under `<project>/.switchboard/`
- Per-harness adapters for Claude Code (`claude -p`) and Codex (`codex exec`)
- Normalized event stream (full vocabulary by M2: TurnStart, ContentChunk, ToolStarted, ToolCompleted, TurnEnd, TurnAborted, RateLimitEvent, SessionMeta — M1 lands the minimal subset for streaming text)
- Six functional primitives composable into workflows (spawn, send, auto-forward, fan-in with template wrapping, pause for user input, iterate over a list)
- Workflow YAML execution engine with step-boundary checkpointing
- Prompt providers: local file store + MCP-served (via `rmcp`); Tiddly preset
- Single-dispatcher chokepoint with one-in-flight-turn-per-agent enforcement; project-level `flock` for multi-instance protection
- Multi-pane desktop UI (Tauri + Rust + Svelte) with workflow-progress surface
- First-launch safety acknowledgement dialog
- Cancellation (workflow + per-turn) via SIGTERM-to-process-group
- Walk-away semantics (close-to-tray, machine-sleep handling, crash recovery to step-boundary checkpoint)
- Cross-platform distribution (Homebrew + .dmg on macOS; .deb/.rpm on Linux; .msi on Windows) with Tauri auto-updater

### Deferred to v2+

(Comprehensive list in system-design §11; abbreviated here.)

- Long-lived agent processes (revisit if cold-start benchmark is >2s)
- Visual workflow editor
- Granular permission / sandbox config (config-driven allowlists, interactive denial prompts, per-workflow scoping)
- Cross-session persistent agent memory
- Global / cross-project agent templates
- Multi-project workflows
- Workflow conditionals, branching, race semantics, iterate-until-condition, nested loops, dynamic iteration lists
- Per-workflow MCP tool selection / allowlists
- Compaction event normalization (probe + decide)
- DAG visualization of in-flight workflows
- In-app launch of harness's interactive TUI

## Critical path

```
M1 → M2 → M3 → M4 → M5 → M6
                              ↘
                                 M7 (parallel-able, late M5/M6)
```

- **M1 → M2** validates the per-harness adapter abstraction before more layers depend on it.
- **M3** introduces the dispatcher and contention enforcement — load-bearing for everything that follows.
- **M4** lands prompt providers; M5 (workflow engine) depends on these for templates to be resolvable.
- **M5 → M6** sequence the workflow-engine work (basic primitives first, then pause + iterate).
- **M7** can begin in parallel toward the end of M5/M6 — distribution infrastructure (signing, packaging, auto-updater) is largely orthogonal to the engine.

## Milestones

### M1 — Walking skeleton (Claude Code only)

**Goal:** Open Switchboard, create a project bound to a working directory, spawn one Claude Code agent, send a message, see streaming text in a single pane. macOS only.

**Scope:**

- Tauri 2.x app shell (Rust core + Svelte 5 frontend, shadcn-svelte components, Tailwind)
- Single project on disk: `<project>/.switchboard/{config.yaml,state/registry.jsonl}`
- Single per-message process spawn for `claude -p --resume <session-id>` with `--dangerously-skip-permissions`
- Normalized event stream wired up minimally (TurnStart, ContentChunk(text), TurnEnd) — enough to render streaming text
- Single-pane agent UI with compose bar and streaming output
- Agent registry persistence (project ↔ agent ↔ harness session ID)

**Deliverables:** runnable `cargo tauri dev` workflow; macOS-only build; one project, one agent, one streaming send-and-receive.

**Dependencies:** none (foundational).

**Acceptance:** maintainer can `cargo tauri dev`, create a project, spawn an agent named `assistant`, send "What's 2+2?", see "4" stream into the pane.

---

### M2 — Both harnesses through the same abstraction

**Goal:** Spawn a Codex agent in the same project, switch between agents via a minimal selector, get the same UX as Claude Code.

**Scope:**

- Codex adapter: `codex exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox`
- Codex session-file enrichment (per resolved 10.15) — read `~/.codex/sessions/...jsonl` on `turn.completed`/`turn.failed` for rate limits, `model_context_window`, `session_meta`
- Normalized event vocabulary fully populated for both harnesses (TurnStart, ContentChunk, ToolStarted, ToolCompleted, TurnEnd, TurnAborted, RateLimitEvent, SessionMeta)
- Codex pricing table for cost derivation (since Codex stream omits cost)
- Process-group spawn for both harnesses (`Command::process_group(0)`)
- Stall-mitigation: close stdin after dispatch (per codex-cli-observed.md research note)
- **Probe and document Codex `turn.failed` payload shape and permission-denial behavior.** Update `docs/research/codex-cli-observed.md` with findings; reflect any adjustments in the normalized error taxonomy / `permission_denials` field.
- **Minimal agent selector UI** — single-pane view at a time, with a selector to switch between agents in the project. Multi-pane layout deferred to M3.
- **Integration test suite scaffolding** — small-prompt tests against real `claude -p` and `codex exec`. Initial coverage: terminal-event detection (TurnEnd vs TurnAborted), error reporting, basic vocabulary normalization. Suite extends with each subsequent milestone.

**Deliverables:** Codex adapter; pricing table; per-harness adapter abstraction validated end-to-end; minimal agent selector; integration test scaffolding.

**Dependencies:** M1 (Tauri shell, normalized stream skeleton, registry).

**Acceptance:** spawn one Claude Code agent and one Codex agent in the same project; switch between them via the agent selector; each streams correctly through the normalized event pipeline; per-agent metadata (cost, tokens, context utilization) populates correctly for both. Integration test suite runs locally against installed harnesses with at least one test per normalized event type.

---

### M3 — Multi-agent UI + dispatcher + contention + per-turn cancel

**Goal:** Multiple agents per project, multi-pane visible, manual fan-out from the compose bar works without corrupting sessions, and individual agent turns can be cancelled.

**Scope:**

- Multi-pane layout: every agent's most recent output visible; one focused agent (active for compose-bar input); collapse/expand background agents
- Persistent overview panel: all agents with real-time status (idle, processing, waiting on tool, errored)
- **Single-dispatcher chokepoint** (per system-design §7 Agent contention) — only entry point that talks to harness adapters
- Per-agent in-memory state (`HashMap<AgentId, AgentState>` behind a Tokio Mutex)
- Project-level `flock` on `<project>/.switchboard/state/instance.lock` (multi-window-same-project guard)
- Compose-and-dispatch UI: source (free-form text + optional latest-output-of-agent) + recipients (multi-select); Send gated when recipient busy
- Agent context menu: fork session (Claude Code only — `--fork-session`), open session file, reset/remove
- **Per-turn cancellation:** SIGTERM to the in-flight harness subprocess's process group; partial output buffered in-memory and stays accessible in the agent's pane until the next turn or restart (per system-design §7 Cancelling). Detection accounts for Codex's SIGTERM-exits-0 quirk (absence of terminal event).
- Integration test suite extended for contention enforcement and cancellation paths.

**Deliverables:** multi-agent UI; dispatcher; contention enforcement (UI gating + dispatch error per §7); per-turn cancel.

**Dependencies:** M2 (both adapters working through normalized stream).

**Acceptance:** spawn 3 agents; manually fan-out a single message to 2 of them via compose bar; both stream in parallel; sending to a busy agent shows the gating UX inline; opening the same project in a second Switchboard window is refused with a clear error; cancelling an in-flight turn cleanly terminates the harness subprocess and partial buffered output remains visible.

---

### M4 — Prompt providers

**Goal:** Slash-command in the compose bar resolves prompts from the local file store and from a Tiddly MCP server. Templates render correctly.

**Scope:**

- Local file store: `<project>/.switchboard/prompts/` plus configurable `local_prompt_dirs` (per system-design §6)
- Frontmatter parsing: `name`, `description`, `arguments`, `tags`
- MiniJinja rendering for local prompts
- MCP client via `rmcp` — `prompts/list` and `prompts/get` only
- Tiddly preset: one-click PAT setup writes a `preset: tiddly` config entry
- Generic MCP server config (HTTP transport, stdio transport)
- Slash-command UI: autocomplete across all configured providers; bare-name resolution allowed when the match is unambiguous; ambiguous bare names show all candidates in autocomplete with their full prefix (per system-design §6)
- Skill-file compatibility: a Claude Code skill `.md` dropped into a Switchboard prompts directory works as-is
- Integration test suite extended for prompt resolution (local + MCP via a stub MCP server)

**Deliverables:** local file store + MCP client; rendering pipeline; slash-command compose-bar UI; Tiddly preset; ships with example local prompts.

**Dependencies:** M3 (compose bar exists).

**Acceptance:** drop a local prompt file with arguments into `<project>/.switchboard/prompts/`, invoke via `/promptname`, fill arguments, see rendered text dispatched. Configure Tiddly preset, invoke a Tiddly prompt the same way. Drop a Claude Code skill `.md` file into the prompts directory, confirm it works as a local prompt.

---

### M5 — Workflow engine + workflow-level cancel

**Goal:** Invoke a YAML workflow file, watch it run, see the workflow-progress surface, and stop a workflow mid-flight.

**Scope:**

- Workflow YAML parser per `docs/workflow-spec.md`
- Step-based execution interpreter (parallel dispatch within fan-out, synchronization within fan-in — per system-design §4 Execution model)
- Primitives 1–4: spawn, send, auto-forward, fan-in with template wrapping
- Workflow-progress surface (per §7) — each active workflow's name, current step, total steps, per-step status; supports multiple concurrent workflows
- Step-boundary checkpointing to `<project>/.switchboard/state/runs/<run-id>.jsonl`
- Failure handling per system-design §7 (pre-dispatch failures, contention refusals, fan-in per-agent failures)
- **Workflow-level cancellation:** "Cancel workflow" action stops orchestration; SIGTERM-to-process-group on whichever harness subprocesses are in-flight; workflow marked `cancelled`. User cancelling an agent's turn while it's part of a workflow step → workflow also marked `cancelled` (per system-design §7 — user intent-bearing).
- Built-in workflows shipped: `review-and-aggregate`, `sequential handoff with template`
- Workflow invocation form (one field per declared input)
- Integration test suite extended for workflow execution paths (sequential handoff, fan-in completion, failure modes, cancellation).

**Deliverables:** workflow engine; built-in workflows; progress surface; checkpoint persistence; workflow-level cancellation.

**Dependencies:** M4 (prompts must be resolvable for workflow templates to work); `docs/workflow-spec.md` must be finalized.

**Acceptance:** invoke `review-and-aggregate` against three agents (one implementer, two reviewers); workflow runs end-to-end; workflow-progress surface shows correct step transitions; cancelling the workflow mid-flight cleanly terminates the in-flight harness subprocess and marks the workflow `cancelled`; force-killing Switchboard mid-workflow (e.g., via Activity Monitor) and restarting surfaces the workflow as "interrupted at step N" with retry/abandon options. (Polished walk-away surfaces — close-to-tray, quit-with-confirmation, sleep-resume — land in M7.)

---

### M6 — Pause for user input + iteration

**Goal:** A workflow can pause mid-flight for user input, and a workflow can iterate over a static list.

**Scope:**

- Primitive 5 (Pause for user input) — workflow suspends; OS-native notification fires; compose bar pre-targeted at the configured recipient; user's response becomes a template variable for subsequent steps
- Primitive 6 (Iterate over a list) — bounded iteration over a static invocation-time list; iteration variable available in template substitution; loop body uses existing primitives
- Iteration-aware checkpointing — checkpoint captures iteration index and value
- Workflow-progress surface gains the iteration dimension ("iteration 2 of 3 (milestone = 'X'), step 3 of 8")
- Crash recovery surfaces interrupted iterations with full context
- Integration test suite extended for pause-resume cycles and iteration (including mid-iteration interrupt + recovery).

**Deliverables:** Primitives 5 and 6; iteration-aware progress surface and checkpoints.

**Dependencies:** M5 (workflow engine, progress surface, checkpointing).

**Acceptance:** invoke a workflow with a pause-for-user-input step — workflow suspends, notification fires, user types a response, workflow continues with the response dispatched to the configured recipient. Invoke a workflow that iterates over a 3-item list — pattern runs three times, progress surface shows iteration index, restart mid-iteration recovers correctly.

---

### M7 — Polish, safety, distribution

**Goal:** Switchboard ships as signed binaries on all three platforms, with auto-updater, safety dialog, polished walk-away semantics, and the integration test suite gated in CI.

**Scope:**

- First-launch acknowledgement dialog (autonomy posture per §9 Safety guidance)
- Window close-to-tray (with Linux no-tray windowed-only fallback per §10)
- Walk-away semantics: minimize, close-to-tray, quit-with-confirmation (prompts when workflows are in-flight), machine sleep handling
- **Cancellation polish** — partial-output buffer surfaced in a dedicated review UI; notifications fire on cancellation. (Cancel mechanism itself is delivered in M3/M5; M7 is the polish edge.)
- Notifications: workflow completion, pause, error, cancellation
- Out-of-band-harness-use note in onboarding (per system-design §7 Agent contention)
- Tauri event-emission ring buffer (bounded per-agent, per §10) — UI lag never blocks core
- Cross-platform builds: macOS via Homebrew tap + signed `.dmg`; Linux `.deb`/`.rpm`; Windows signed `.msi`
- Code signing (Apple Developer ID, Authenticode) — release infrastructure
- Tauri auto-updater wired
- **CI plumbing for the integration test suite** — wire the suite (built up incrementally across M2–M6) into push-to-main and PR-from-collaborators triggers; forks get the unit suite. API key management for CI.

**Deliverables:** signed cross-platform binaries; auto-updater; safety dialog; polished walk-away UX; partial-output review UI; CI integration suite gated on push.

**Dependencies:** M6 (engine + primitives complete) for the polished walk-away path to be meaningful; distribution infrastructure can begin in parallel toward the end of M5/M6.

**Acceptance:** maintainer can install Switchboard via `brew install switchboard` on macOS; first launch presents acknowledgement dialog; closing the window hides to tray (or windowed-only on Linux without AppIndicator); quitting prompts for confirmation if workflows are in flight and walk-away path resumes correctly on next launch; cancelling a fan-out workflow surfaces partial-output review; auto-updater detects and installs a new release end-to-end; CI integration suite runs on every PR from a collaborator and on push to main.

## What this plan does not commit to

- **Per-milestone time estimates.** Speculative pre-implementation; will be added inline when each milestone is expanded if useful.
- **Implementation-grade detail per milestone.** This is the outline; expansion happens in dedicated passes.
- **The exact workflow DSL.** Lives in `docs/workflow-spec.md` (prerequisite).
- **Persistence schema details (10.3).** On-disk format choices for `state/registry.jsonl` and `state/runs/<run-id>.jsonl` are deferred until M3 / M5 expansion.
- **Stall detection threshold and UX (10.18).** Deferred to M2/M3 expansion when the live-stream behavior of both harnesses is being implemented.
