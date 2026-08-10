# Switchboard

![Switchboard](docs/images/banner.png)

Switchboard is a human-directed orchestrator for AI coding agents — a desktop app you run alongside your existing CLIs: **Claude Code, Codex, Gemini, and Antigravity**.

Spawn multiple agent sessions in a single project, route messages between them, and define reusable workflows for common multi-agent operations like second-opinion code review, plan-and-implement, and parallel-solution adjudication.

It's built for anyone who wants explicit, human-in-the-loop control over multi-agent workflows — not an opinionated SDLC engine, not a full agent replacement, just the coordination layer between agents you're already using.

![Project view](docs/images/project-view.png)

**Projects** are where the work happens. A project holds any number of agents — here one agent implementing while two review its work — and you split the transcript into side-by-side panes to keep each group's conversation readable. You can hide/show and toggle through panes. The compose bar sends the message to whichever agents (or panes) you target, so fanning a message out to every reviewer is one send rather than multiple copy-pastes.

![Git view](docs/images/git-view.png)

Switchboard also has a **Git view** for reviewing repositories, changed files, diffs, and commit history without leaving the app. It is deliberately read-only: Switchboard does not stage files, create commits, or replace a full Git client.

## Features

- **Work with multiple CLI agents in one project.** Run any combination of the supported Claude Code, Codex, Gemini, and Antigravity sessions while keeping each agent's conversation and status visible.
- **Fan-out and fan-in.** Send one request to several agents in parallel, then forward one or more responses into the next message — or let a workflow wait for them, combine them, and send the result onward — without copying and pasting between terminals.
- **Group agents into transcript panes.** Keep implementers, reviewers, or other roles together, target the group as one recipient, and hide or solo agents when you need a quieter view.
- **Use one prompt library across your agent CLIs.** Choose parameterized prompts from the built-in library, local files, or HTTP MCP prompt servers; Switchboard centrally resolves and renders them for whichever agents you select.
- **Run reusable multi-agent workflows.** Built-in and user-authored workflows can route prompts, run agents in parallel, wait for their responses, combine the results, and pass them to the next agent.
- **Keep important messages beside the transcript.** Pin complete messages in the sidebar so you can refer to them while working elsewhere, then jump back to their original location when needed.
- **Search and navigate long conversations.** Search message text across agents, filter by role or pinned status, preview results, and jump back to a result when its agent is visible in a pane.
- **Keep track of work across projects.** The project list shows which projects are still running, marks background work when it finishes, and lets you jump directly to newly completed work you haven't viewed.

## Install

macOS only (v1, early development). Switchboard is currently installed by building from source — a one-time setup, after which it lives in `/Applications` and updates with a single command. A signed Homebrew install is planned.

**1. Clone the repo:**

```sh
git clone https://github.com/shane-kercheval/switchboard
cd switchboard
```

**2. Install the prerequisites** (one-time, run from the repo root). Each is needed to build from source:

- **Xcode Command Line Tools** — `xcode-select --install`
- **Rust** — install [rustup](https://rustup.rs); the pinned toolchain auto-installs on the first build. **After installing, restart your terminal** (or run `source "$HOME/.cargo/env"`) so `cargo` is on your `PATH` — otherwise the build fails with `cargo metadata ... No such file or directory`.
- **Node** — version **22 or newer**. Use whatever you already have, or install one via [nvm](https://github.com/nvm-sh/nvm), [fnm](https://github.com/Schniz/fnm), Homebrew, etc. (You don't need a specific patch version — `make install` checks for the minimum and stops with a clear message if it's too old. Contributors: [`.nvmrc`](./.nvmrc) pins the exact version CI runs, picked up with `nvm use`; that exact pin isn't required just to build the app.)
- **pnpm** — run `corepack enable`. Corepack ships with Node and provides the pnpm version pinned in [`package.json`](./package.json); you do **not** install pnpm separately.

Confirm the toolchain resolves before continuing:

```sh
node --version   # 22 or newer
pnpm --version   # Corepack provides the pinned version on first pnpm call
```

**3. Build, install, and launch:**

```sh
make install   # install JS dependencies (pnpm install --frozen-lockfile)
make deploy    # build, install to /Applications, and launch
```

Update with `git pull && make deploy`; remove with `make uninstall-app`.

## Why

Running multiple AI coding agents in parallel — one to plan, others to review, one to implement — produces meaningfully better results than running a single agent, but the manual coordination overhead (copy-paste between terminals, tracking which agent has which context, applying prompt templates by hand) is busywork that should be automated so you can spend that time on the parts that need judgment.

Switchboard removes the coordination overhead while keeping the human in the loop where judgment matters: deciding what to route, when to revise, when to proceed.

The goal isn't to give the AI a task and review what it produced. It's to stay in the decisions that matter — is this plan good enough to implement? which review feedback is worth acting on? — while automating the mechanical routing in between. Switchboard is the coordination layer; you're still the one making the calls.

## Coordinating agents

A fan-out sends one instruction to several agents so they can work independently — for example, asking multiple reviewers to assess the same plan or diff. A fan-in brings their work back together: select one or more agent responses as sources for a new message, or use a workflow to wait for every reviewer, combine their responses, and pass the result to another agent.

Forwarding uses the agents' actual responses and clearly labels each source for the recipient. If a selected agent is still working, Switchboard holds the dependent message until the response is ready. You can add your own instructions or apply a prompt while forwarding, so the handoff can say what the next agent should do with the supplied material rather than merely pasting it into a new terminal.

Panes make that routing easy to see. You can keep agents with related roles together and address the pane as a group, while every agent remains an independent session that can also be targeted directly.

## Prompts and workflows

Prompts are reusable, optionally parameterized text templates. They can come from Switchboard's built-in library, local files, or an HTTP MCP prompt server. The compose bar presents the prompt's arguments as fields, lets you preview the rendered result, and centrally resolves the same template for every supported agent CLI. Your prompt library therefore needs to be configured only once, including for CLIs with limited native MCP prompt support.

Add an HTTP MCP prompt server under Settings → **Add MCP server**, choosing one of two authentication modes. **OAuth sign-in** opens your browser to sign in with the server's own account system — nothing to paste; this requires the server to support OAuth dynamic client registration, and it needs a browser, so it isn't for headless machines. [Tiddly](https://tiddly.me) works this way: add `https://prompts-mcp.tiddly.me/mcp`, then click Sign in on its row. **Bearer token** is the path for servers without OAuth support and for headless or scripted use — paste a token minted by the server (for Tiddly, a Personal Access Token); tokens are stored in your OS keychain. Stdio MCP servers are planned but not currently supported.

Workflows record routing patterns you otherwise repeat by hand: send a prompt to several agents, wait for them, aggregate their responses, and hand the result to the next agent. Switchboard includes built-in workflows, and you can copy or write personal YAML workflows that are available in every project on this Mac. The invocation form shows what a workflow will do and lets you bind its agent roles; while it runs, the compose area shows the current step and what remains.

## Transcripts, search, and pins

Switchboard combines each project's agent sessions into one chronological transcript while preserving agent attribution, tool calls, reasoning, failures, cancellations, and context-compaction boundaries. Compact mode keeps routine detail out of the way without removing it from the history.

Press ⌘F to search message text across the project, filter by role or pinned status, and preview a result before jumping to it. Selecting a result whose agent is visible in a pane reveals that pane, scrolls to the message, and expands it when necessary. Messages from eye-hidden or unassigned agents remain searchable but cannot be jumped to until the agent is visible in a pane.

Pins are for messages you need to keep reading, not just bookmarks. A pinned message appears in full in the sidebar, where it can remain visible while you work elsewhere in the transcript. Jumping from a pin scrolls the transcript to the original message. Pins can also be added or filtered from search.

## Transcript panes

By default a project shows one unified transcript of all its agents. You can split it into side-by-side panes — for example reviewers on the left, the implementer on the right — where each pane shows only its agents' conversation.

- **Create and populate a pane**: click the **+** button in the project header to add an empty pane, then use an agent's **⋯ actions menu** to move it into that pane. **Move to new pane** creates and populates one in a single action. On the first split from the default unified view, if the compose bar targets only a subset of agents, the original pane keeps that subset and the other agents become unassigned; otherwise its membership stays unchanged.
- **Send to a pane**: click a pane's header (or ⌘-click anywhere in it, type `@panename` in the composer, or press ⌘⌥1–9) to make that pane's agents the message recipients. The targeted pane shows a green ring — your draft goes exactly to the agents inside it. Hold ⌘ to preview which pane a click would target.
- **Rename / resize / close**: panes rename from the pencil icon in their header and resize by dragging the divider between them. Closing a pane leaves its agents unassigned and stops targeting them, but the agents keep working. Choose **Return to unified view** from a pane's menu to show every agent together again.
- **Hide agents**: the eye icon on an agent's sidebar card hides its messages without removing it (⌥-click to solo it — show only that agent in its pane). Hidden agents still receive messages you send them; the recipient chip shows a warning when you're about to message a hidden agent.

The layout (panes, names, widths, hidden agents) is remembered per project on this machine.

## Non-goals

- Replacing the agent CLIs. Switchboard drives them; it doesn't reimplement them.
- Prescribing a software development lifecycle. Workflows are user-defined; Switchboard ships defaults but doesn't impose process.
- Managing git, CI, or PR workflows. Out of scope.
- Cross-session persistent agent memory. Possibly a future addition; not in scope for v1.
- A hosted / SaaS service. Switchboard runs locally on your machine. A future hosted service may exist for cross-machine sync of workflows and prompts; that is not v1.

## Agent CLI support and limitations

Switchboard drives each agent through its own CLI, so it inherits that CLI's capabilities — and a few CLI-specific limitations are worth knowing up front:

- **Model selection.** Claude Code, Codex, and Gemini let Switchboard choose the model per agent — pick it when you create the agent, or change it later from the agent's actions menu; the transcript records the model each past turn actually ran on. **Antigravity does not** — its CLI exposes no model option, so Antigravity agents run on whatever model you've selected inside Antigravity itself, and Switchboard can't change it per agent (the sidebar shows the model it observes Antigravity using).
- **Reasoning effort.** Claude Code and Codex let Switchboard set the reasoning-effort level per agent (alongside the model). **Gemini does not** — Gemini exposes reasoning effort only through its own config, not a per-run option, so Switchboard can't set it; Gemini agents use whatever Gemini's config specifies. For **Antigravity**, effort is part of the model name you pick inside Antigravity, so it follows the same limitation as model selection above.
- **Codex models depend on your plan.** When you sign in to Codex with a ChatGPT subscription, only the models your plan includes are available; choosing one your plan doesn't cover fails the turn with Codex's own error.
- **The highest Codex effort levels need a GPT-5.6 model.** The `Max` and `Ultra` reasoning-effort levels work on the GPT-5.6 family (Sol, Terra, Luna); older models such as GPT-5.5 top out at `XHigh`. Selecting a higher level on an older model fails the turn with Codex's own message listing the levels that model supports — switch the model or lower the effort and resend.
- **Gemini isn't added to new projects by default.** Gemini is no longer available on individual plans, so a new project starts with a Claude Code, Codex, and Antigravity agent but not a Gemini one. Gemini is still fully supported — if you have access, add a Gemini agent yourself from the "Add agent" dialog.
- **Antigravity and hidden folders.** Antigravity can't work in a project whose path contains a hidden (dot-prefixed) folder — for example anything under `~/.config/…`. The agent still runs but can't see your files. Keep projects under normal paths like `~/repos/…`.
- **Some messages can't be pinned.** Switchboard disables Pin when a message has no identity that survives reopening rather than risk attaching the pin to a different message. This includes some imported history and every Antigravity reply. A newly completed Gemini reply becomes pinnable after you reopen Switchboard and its session history is loaded. User messages sent through Switchboard can still be pinned.
- **Picking up terminal-continued sessions.** If you continue a session in the agent CLI's own terminal, **Claude Code** picks up the new turns when you switch back to the project. **Codex, Gemini, and Antigravity don't yet** — reopen Switchboard to load their updated history.
- **Slash-leading prompts can retain CLI command behavior.** Switchboard centrally resolves and renders the same prompt for each selected agent, but Gemini still processes recognized slash-leading text as native commands. Avoid slash-leading prompt bodies when they must behave the same across agent CLIs.

## Design and discussion

The architectural decisions, functional requirements, and open questions are being worked through in [`docs/`](./docs), starting with [`docs/system-design.md`](./docs/system-design.md). Comments and pushback welcome via issues.

## Local development

macOS only for v1. The build prerequisites are the same as [Install](#install) above — Xcode Command Line Tools, Rust (rustup), Node (pinned in [`.nvmrc`](./.nvmrc)), and pnpm (`corepack enable`). If you've installed the app, you already have everything.

Common commands (run from the repo root):

```sh
make install     # one-time: pnpm install --frozen-lockfile
make dev         # run the Tauri dev shell
make test         # run all Rust + frontend tests (fast, offline jsdom suite)
make test-browser # real-WebKit frontend suite (Vitest browser mode); installs WebKit if needed
make lint         # clippy, eslint, svelte-check
make check        # everything CI runs (incl. the browser suite) — run before opening a PR
make test-live    # live-harness suite against the real agent CLIs (developer-local)
```

`make test-live` exercises the adapters against the real `claude` / `codex` / `gemini` / `antigravity` CLIs to catch upstream drift. See [`crates/harness/tests/README.md`](./crates/harness/tests/README.md) for what it covers and how to set it up.

`make test-browser` (and `make check`) run the frontend suite in a real WebKit engine via Vitest browser mode. The target installs a Playwright-managed WebKit build on demand — the first run downloads ~100 MB (cached afterward), so it needs network access once; no extra system packages are required on macOS. The default `make test` stays jsdom-only and needs none of this.

See [`AGENTS.md`](./AGENTS.md) for project orientation and conventions, and [`docs/implementation_plans/`](./docs/implementation_plans/) for the roadmap and per-phase implementation plans.

### Developing without an agent CLI installed

If no agent CLI is on your `PATH` (or you don't want to burn quota during UI iteration), launch with the mock harness:

```sh
SWITCHBOARD_HARNESS=mock make dev
```

The mock emits canned streaming responses (`Mock response to: <prompt> — replied by mock harness.`) — identical event-stream shape to a real harness, so the UI exercises every code path, and the startup binary-not-found banner stays hidden.

## License

Apache 2.0. See [LICENSE](./LICENSE).
