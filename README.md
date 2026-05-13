# Switchboard

![Switchboard](docs/images/banner.png)

Switchboard is a human-directed orchestrator for AI coding agents — a desktop application you run alongside your existing Claude Code and Codex setup. 

Switchboard lets you spawn multiple Claude Code and Codex sessions in a single project, route messages between them, and define reusable workflows for common multi-agent operations like second-opinion code review, plan-and-implement, and parallel-solution adjudication.

It's built for anyone who wants explicit, human-in-the-loop control over multi-agent workflows — not an opinionated SDLC engine, not a full agent replacement, just the coordination layer between agents you're already using.

## Why

Running multiple AI coding agents in parallel — one to plan, others to review, one to implement — produces meaningfully better results than running a single agent, but the manual coordination overhead (copy-paste between terminals, tracking which agent has which context, applying prompt templates by hand) is busywork that should be automated so you can spend that time on the parts that need judgment.

Switchboard removes the coordination overhead while keeping the human in the loop where judgment matters: deciding what to route, when to revise, when to proceed.

The goal isn't to give the AI a task and review what it produced. It's to stay in the decisions that matter — is this plan good enough to implement? which review feedback is worth acting on? — while automating the mechanical routing in between. Switchboard is the coordination layer; you're still the one making the calls.

There is also a quieter benefit: because Switchboard resolves prompts itself and sends agents plain text, your prompt library lives in one place and works identically with both Claude Code and Codex agents — without configuring the prompt source in either harness. Especially useful for Codex, where MCP prompt support is limited.

## Core ideas

- **Project**: a workspace containing related agents working toward a shared goal (a feature, a refactor, a document).
- **Agent**: a Claude Code or Codex session, named within a project.
- **Workflow**: a reusable, parameterized routing template — for example "fan-out review and aggregate" — defined as a YAML file and invoked by name.
- **Routing**: message passing between agents, optionally wrapped in a prompt template, with support for fan-out (one to many) and fan-in (many to one).

## Non-goals

- Replacing the Claude Code or Codex harness. Switchboard drives them; it doesn't reimplement them.
- Prescribing a software development lifecycle. Workflows are user-defined; Switchboard ships defaults but doesn't impose process.
- Managing git, CI, or PR workflows. Out of scope.
- Cross-session persistent agent memory. Possibly a future addition; not in scope for v1.
- A hosted / SaaS service. Switchboard runs locally on your machine. A future hosted service may exist for cross-machine sync of workflows and prompts; that is not v1.

## Status

Early development. Design is being captured in [`docs/system-design.md`](./docs/system-design.md).

Switchboard will ship as a single-binary desktop app (built with Tauri) — installable via Homebrew on macOS, `.deb`/`.rpm` on Linux, and a signed `.msi` installer on Windows once available.

This is not yet usable software. Star or watch the repo if you want to follow along.

## Design and discussion

The architectural decisions, functional requirements, and open questions are being worked through in [`docs/`](./docs). Comments and pushback welcome via issues.

## Local development

macOS only for v1. Prerequisites:

- **Rust** — pinned via [`rust-toolchain.toml`](./rust-toolchain.toml). Install [rustup](https://rustup.rs); the toolchain will be auto-installed on first build.
- **Node** — version pinned in [`.nvmrc`](./.nvmrc). Install via [nvm](https://github.com/nvm-sh/nvm) (`nvm use`) or any Node version manager.
- **pnpm** — pinned via the `packageManager` field in `package.json`. Enable via `corepack enable` (Corepack ships with Node).
- **Xcode Command Line Tools** — `xcode-select --install` (required for native macOS builds).

Common commands (run from the repo root):

```sh
make install   # one-time: pnpm install --frozen-lockfile
make dev       # run the Tauri dev shell
make test      # run all Rust + frontend tests
make lint      # clippy, eslint, svelte-check
make check     # everything CI runs — run before opening a PR
```

See [`AGENTS.md`](./AGENTS.md) for project orientation and conventions, and [`docs/implementation_plans/2026-05-12-v1.md`](./docs/implementation_plans/2026-05-12-v1.md) for the milestone roadmap.

## License

Apache 2.0. See [LICENSE](./LICENSE).
