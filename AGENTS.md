# AGENTS.md

Playbook for AI agents (and humans) working on Switchboard. High-level orientation; per-milestone mechanics live in `docs/implementation_plans/`, and crate internals are documented in their source.

## What this project is

Switchboard is a macOS desktop app for orchestrating multiple AI coding agents (Claude Code, Codex, etc.) inside one project context. The canonical "what is Switchboard and why" lives in `docs/system-design.md`.

The current milestone is **M2** — both harnesses through the same abstraction, unified project transcript with per-agent attribution, transcript rehydration from harness session files. See `docs/implementation_plans/2026-05-12-v1-m2.md` for the active plan and `docs/implementation_plans/2026-05-12-v1.md` for the full v1 roadmap.

## Architecture overview

- **Rust workspace** (`crates/`) built on Tauri 2.x.
  - `crates/app/` — Tauri host. Owns Tauri commands, `AppState`, window, `AppHandleEmitter`. `#[tauri::command]` handlers are thin shims over free functions (`*_impl`) in `commands.rs`; tests target the free functions.
  - `crates/core/` — pure-Rust persistence: `Directory`, `Project`, `AgentRecord`, name validation, JSONL/YAML I/O. No Tauri dependency, no async.
  - `crates/harness/` — per-harness adapters (`HarnessAdapter` trait + `ClaudeCodeAdapter`, `CodexAdapter`, `MockHarnessAdapter`), event types, stream parsers, session-file parsers. No Tauri dependency.
  - `crates/dispatcher/` — `Dispatcher`, `EventEmitter` trait, `AgentIdleGuard`. Drives adapters; owns per-agent in-memory state + `TurnId` generation. No Tauri dependency.
- **Frontend** — Svelte 5 + Vite + TypeScript + Tailwind v4, with shadcn-svelte components. Lives at repo root (`src/`, `index.html`, `vite.config.ts`).
- **Tauri shell** bridges frontend ↔ Rust via `#[tauri::command]` handlers and per-agent event channels.

For each crate's internal mechanics, read the source (the `*_impl` functions are typed and documented) and the milestone plan that introduced it.

## Where things live

- `crates/app/` — Tauri Rust crate.
- `crates/core/`, `crates/harness/`, `crates/dispatcher/` — workspace members.
- `src/` — frontend Svelte/TS sources.
- `tests/` — frontend test setup + integration tests.
- `docs/` — design docs, milestone plans, research notes. Read before changing scope.
- `docs/implementation_plans/` — per-milestone plans. The current milestone's plan is the ground truth for what to build.
- `.github/workflows/` — CI definitions.
- `Makefile` — single source of truth for dev commands.

## How to run / test / lint

All via `make`:

- `make install` — `pnpm install --frozen-lockfile` (one-time / after lockfile changes).
- `make dev` — runs the Tauri dev shell.
- `make test` — runs all Rust + frontend tests.
- `make lint` — runs clippy, eslint, svelte-check.
- `make fmt` — formats Rust + frontend.
- `make check` — everything CI runs (fmt check, lint, test, type-check). Run this before opening a PR.
- `make clean` — removes build artifacts.

Prerequisites: see `README.md`. Rust toolchain pinned in `rust-toolchain.toml`; Node in `.nvmrc`; pnpm via `packageManager` in `package.json` (`corepack enable`).

## Version pinning policy

- `package.json` and `Cargo.toml` use caret-range constraints (`^x.y.z` / bare versions).
- `pnpm-lock.yaml` and `Cargo.lock` are **committed** — they are the source of truth for exact resolved versions.
- CI uses `pnpm install --frozen-lockfile` and `cargo build --locked` for byte-identical reproducibility.
- Manifest ranges document the supported semver range; the lockfile pins the exact tested version.

## Coding conventions

### Rust

- Edition 2024. Workspace clippy lints: `clippy::all` + `clippy::pedantic` with a targeted allowlist in `Cargo.toml`'s `[workspace.lints.clippy]`. When a pedantic lint fires noisily on a common readable pattern, add it to the allowlist with a one-line rationale comment. Don't reactively drop to "just `clippy::all`."
- `thiserror` for typed errors at module boundaries.
- No `unwrap` / `expect` outside `main` or test code; bubble via `Result`.
- `#[non_exhaustive]` on enums that cross IPC or evolve between milestones — adding a new variant should not be a breaking change for consumers.
- Subprocess gotchas (load-bearing for adapter code):
  - Use `tokio::io::BufReader`, not `std::io::BufReader`, for async pipes. `tokio::process::ChildStdout` doesn't implement `std::io::Read`.
  - Use `Stdio::null()` for stdin we never write to. Prevents pipe-full deadlocks and stalls on harnesses that try to read interactively (see `docs/research/codex-cli-observed.md`).

### TypeScript / Svelte

- `strict: true` in tsconfig; no `any`.
- Svelte 5 runes (`$state`, `$derived`, `$effect`).
- Wire-format types match Rust `#[serde(tag = "type", rename_all = "snake_case")]` — TS uses discriminated unions with snake_case keys. New variants land additively (`#[non_exhaustive]` on the Rust side; reducer default branches on the TS side that degrade gracefully on unknown discriminants).
- `DateTime<Utc>` serializes as ISO-8601 string; consumers convert at the boundary if they need `Date` objects.

### Both languages

- Type hints on every function signature.
- No comments unless the *why* is non-obvious. Identifiers explain *what*.
- No imports inside functions unless absolutely necessary.

### Testing

- Test behavior, not implementation. Skip trivial coverage (getter/setter, type-system tautologies).
- Cover edge cases and error paths.
- Deterministic — no time-of-day or wall-clock dependencies in unit tests.
- **For Svelte components that wrap IPC + event subscriptions + reactive state:** pure-reducer tests are insufficient. Also write component-level tests that mock `invoke` and `listen`, capture the event-listener callback, and exercise realistic event sequences — including ordering races (events arriving before the IPC reply resolves), terminal-state handling, and error paths. Frontend bugs tend to live in the wrapping component, not the reducer.
- **Async flush in component tests:** use `await tick()` (from `svelte`) or `await waitFor(...)` for presence assertions on rendered state — both wait for Svelte's reactive scheduler. `await Promise.resolve()` flushes one microtask, which is OK for absence assertions but fragile for presence.

## Cross-cutting invariants

Project-wide rules that apply across all milestones. Milestone-specific mechanics (dispatcher internals, exact wire shapes, lock orders, file paths) live in the milestone plan that introduced them and in the relevant crate's source — not here.

- **Auth: subscription / tier only.** v1 supports `claude login` and `codex login` exclusively. API-key flows (`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`) are not supported. See `docs/system-design.md` §2.
- **Project = unit of work.** A project hosts N equally first-class agents. UI is one unified transcript stream with per-agent attribution, plus a per-agent overview sidebar. **No singleton "active" or "focused" agent at the model level** — all agents in a loaded project are equally live. See system-design §7.
- **Transcript source-of-truth = harness session files.** Switchboard does not maintain its own persistent transcript store. On project open, Switchboard reads `~/.claude/projects/.../*.jsonl` (Claude Code) and `~/.codex/sessions/YYYY/MM/DD/rollout-*-*.jsonl` (Codex), parses both into a normalized `Turn` shape, and merges chronologically.
- **Cost / quota surface.** Dollars for Claude Code (from `total_cost_usd`), opaque rate-limit signal for Codex (from session-file `token_count.rate_limits`). **No raw tokens displayed in UI**, no per-model pricing tables shipped, no cross-harness dollar aggregation.
- **IDs are UUID v7.** `AgentId`, `ProjectId`, `TurnId`, Claude `session_id`. Time-ordered, serde-friendly, opaque to consumers. Globally unique — the keying basis for per-agent state and per-agent event channels.
- **Name normalization.** Agent and project names match `[A-Za-z0-9_-]+`; uniqueness check is `lowercase + hyphen→underscore` canonicalization (`Reviewer-A` and `reviewer_a` collide). Stored verbatim; canonicalization is only for the uniqueness check. See system-design §3.
- **Append-only persistence.** `projects.jsonl`, per-project `registry.jsonl`, and Codex `sessions/<agent_id>.jsonl` sidecars are write-once-per-record. No deletion in v1. Corruption in Switchboard-owned JSONL fails loud (typed `CoreError::CorruptJsonl`); corruption in harness-owned session files skip-with-warning (the harness wrote the bad line; refusing to render history is hostile UX).
- **Stream contract.** Every turn produces exactly one `TurnEnd` event. Adapters synthesize one on stream truncation. `TurnStart` is dispatcher-emitted, never adapter-emitted (type-enforced — `AdapterEvent` has no `TurnStart` variant). `TurnEnd` is terminal for a *turn*, not for the per-agent channel — agent-scoped events (`SessionMeta`, `RateLimitEvent`) can flow at any time.
- **Session-id uniqueness across all loaded projects.** No two Switchboard agents may target the same underlying harness `session_id` — concurrent dispatch to the same harness session corrupts (per `docs/research/same-session-parallel-invocation.md`). Enforced at agent creation / attach time, scanning all projects in the bound directory.
- **Filesystem layout.** All Switchboard state lives at `<directory>/.switchboard/` — directly under the user's working directory, not in `~/.switchboard/`. `config.yaml`, `workflows/`, and `prompts/` are intended to be git-tracked; everything else is runtime data the user should `.gitignore` themselves. See system-design §3 for the full layout.

## Authoritative docs

- `docs/system-design.md` — canonical design (the "what and why").
- `docs/implementation_plans/2026-05-12-v1.md` — v1 milestone roadmap.
- `docs/implementation_plans/2026-05-12-v1-m1.md` — M1 plan (shipped baseline).
- `docs/implementation_plans/2026-05-12-v1-m2.md` — M2 plan (active).
- `docs/research/` — harness ground-truth notes:
  - `claude-code-cli-observed.md`, `claude-code-headless.md` — Claude Code CLI behavior.
  - `codex-cli-observed.md`, `codex-noninteractive.md` — Codex CLI behavior. **Most M2-load-bearing reference.**
  - `harness-comparison.md` — cross-harness comparison driving the per-harness adapter design.
  - `same-session-parallel-invocation.md` — why we enforce session-id uniqueness at the app layer.
