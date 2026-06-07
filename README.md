# Switchboard

![Switchboard](docs/images/banner.png)

Switchboard is a human-directed orchestrator for AI coding agents — a desktop app you run alongside your existing CLIs: **Claude Code, Codex, Gemini, and Antigravity**.

Spawn multiple agent sessions in a single project, route messages between them, and define reusable workflows for common multi-agent operations like second-opinion code review, plan-and-implement, and parallel-solution adjudication.

It's built for anyone who wants explicit, human-in-the-loop control over multi-agent workflows — not an opinionated SDLC engine, not a full agent replacement, just the coordination layer between agents you're already using.

## Install

macOS only (v1, early development). Switchboard is currently installed by building from source — a one-time setup, after which it lives in `/Applications` and updates with a single command. A signed Homebrew install is planned.

```sh
git clone https://github.com/shane-kercheval/switchboard
cd switchboard
make install   # install dependencies (prerequisites: see Local development)
make deploy    # build, install to /Applications, and launch
```

Update with `git pull && make deploy`; remove with `make uninstall-app`.

## Why

Running multiple AI coding agents in parallel — one to plan, others to review, one to implement — produces meaningfully better results than running a single agent, but the manual coordination overhead (copy-paste between terminals, tracking which agent has which context, applying prompt templates by hand) is busywork that should be automated so you can spend that time on the parts that need judgment.

Switchboard removes the coordination overhead while keeping the human in the loop where judgment matters: deciding what to route, when to revise, when to proceed.

The goal isn't to give the AI a task and review what it produced. It's to stay in the decisions that matter — is this plan good enough to implement? which review feedback is worth acting on? — while automating the mechanical routing in between. Switchboard is the coordination layer; you're still the one making the calls.

There is also a quieter benefit: because Switchboard resolves prompts itself and sends agents plain text, your prompt library lives in one place and works identically across all your agents — without configuring the prompt source in each harness. Especially useful for CLIs with limited MCP prompt support, like Codex.

## Core ideas

- **Project**: a workspace containing related agents working toward a shared goal (a feature, a refactor, a document).
- **Agent**: a named agent session within a project, backed by one of the supported CLIs.
- **Workflow**: a reusable, parameterized routing template — for example "fan-out review and aggregate" — defined as a YAML file and invoked by name.
- **Routing**: message passing between agents, optionally wrapped in a prompt template, with support for fan-out (one to many) and fan-in (many to one).

## Non-goals

- Replacing the agent CLIs. Switchboard drives them; it doesn't reimplement them.
- Prescribing a software development lifecycle. Workflows are user-defined; Switchboard ships defaults but doesn't impose process.
- Managing git, CI, or PR workflows. Out of scope.
- Cross-session persistent agent memory. Possibly a future addition; not in scope for v1.
- A hosted / SaaS service. Switchboard runs locally on your machine. A future hosted service may exist for cross-machine sync of workflows and prompts; that is not v1.

## Harness support and limitations

Switchboard drives each agent through its own CLI, so it inherits that CLI's capabilities — and a few per-harness limitations are worth knowing up front:

- **Model selection.** Claude Code, Codex, and Gemini let Switchboard choose the model per agent — pick it when you create the agent, or change it later from the agent's actions menu; the transcript records the model each past turn actually ran on. **Antigravity does not** — its CLI exposes no model option, so Antigravity agents run on whatever model you've selected inside Antigravity itself, and Switchboard can't change it per agent (the sidebar shows the model it observes Antigravity using).
- **Reasoning effort.** Claude Code and Codex let Switchboard set the reasoning-effort level per agent (alongside the model). **Gemini does not** — Gemini exposes reasoning effort only through its own config, not a per-run option, so Switchboard can't set it; Gemini agents use whatever Gemini's config specifies. For **Antigravity**, effort is part of the model name you pick inside Antigravity, so it follows the same limitation as model selection above.
- **Codex models depend on your plan.** When you sign in to Codex with a ChatGPT subscription, only the models your plan includes are available; choosing one your plan doesn't cover fails the turn with Codex's own error.
- **Antigravity and hidden folders.** Antigravity can't work in a project whose path contains a hidden (dot-prefixed) folder — for example anything under `~/.config/…`. The agent still runs but can't see your files. Keep projects under normal paths like `~/repos/…`.
- **Picking up terminal-continued sessions.** If you continue a session in the harness's own terminal (outside Switchboard) and then switch back to that project, **Claude Code** agents pick up the new turns automatically. **Codex, Gemini, and Antigravity don't yet** — their history updates only on the next full reload. (This refresh happens on project switch-back, not while you stay inside a project.)
- **Prompts work across every harness.** Switchboard resolves prompts itself and sends each agent plain text, so a prompt library — local files or any MCP prompt server you add in Settings — works identically with Claude Code, Codex, Gemini, and Antigravity, with no per-CLI setup. This is especially useful for Codex and Gemini, whose native MCP-prompt support is limited. Add an MCP prompt server (e.g. [Tiddly](https://tiddly.me) — paste a Tiddly access token as the bearer) under Settings → "Add MCP server"; tokens are stored in your OS keychain. (HTTP MCP servers only for now; stdio servers and a one-click Tiddly login are planned.)

## Design and discussion

The architectural decisions, functional requirements, and open questions are being worked through in [`docs/`](./docs), starting with [`docs/system-design.md`](./docs/system-design.md). Comments and pushback welcome via issues.

## Local development

macOS only for v1. Prerequisites:

- **Rust** — pinned via [`rust-toolchain.toml`](./rust-toolchain.toml). Install [rustup](https://rustup.rs); the toolchain will be auto-installed on first build.
- **Node** — version pinned in [`.nvmrc`](./.nvmrc). Install via [nvm](https://github.com/nvm-sh/nvm) (`nvm use`) or any Node version manager.
- **pnpm** — pinned via the `packageManager` field in `package.json`. Enable via `corepack enable` (Corepack ships with Node).
- **Xcode Command Line Tools** — `xcode-select --install` (required for native macOS builds).

Common commands (run from the repo root):

```sh
make install     # one-time: pnpm install --frozen-lockfile
make dev         # run the Tauri dev shell
make test        # run all Rust + frontend tests
make lint        # clippy, eslint, svelte-check
make check       # everything CI runs — run before opening a PR
make test-live   # live-harness suite against the real agent CLIs (developer-local)
```

`make test-live` exercises the adapters against the real `claude` / `codex` / `gemini` / `antigravity` CLIs to catch upstream drift. See [`crates/harness/tests/README.md`](./crates/harness/tests/README.md) for what it covers and how to set it up.

See [`AGENTS.md`](./AGENTS.md) for project orientation and conventions, and [`docs/implementation_plans/`](./docs/implementation_plans/) for the roadmap and per-phase implementation plans.

### Developing without an agent CLI installed

If no agent CLI is on your `PATH` (or you don't want to burn quota during UI iteration), launch with the mock harness:

```sh
SWITCHBOARD_HARNESS=mock make dev
```

The mock emits canned streaming responses (`Mock response to: <prompt> — replied by mock harness.`) — identical event-stream shape to a real harness, so the UI exercises every code path, and the startup binary-not-found banner stays hidden.

## License

Apache 2.0. See [LICENSE](./LICENSE).
