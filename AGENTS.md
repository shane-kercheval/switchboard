# AGENTS.md

Playbook for AI agents (and humans) working on Switchboard. Living doc — extended each sub-milestone.

## What this project is

Switchboard is a macOS desktop app for orchestrating multiple AI coding agents (Claude Code, Codex, etc.) inside one project context. The canonical "what is Switchboard and why" lives in `docs/system-design.md`.

The current milestone is **M1** — the smallest end-to-end vertical slice with Claude Code as the only harness. See `docs/implementation_plans/2026-05-12-v1-m1.md` for the sub-milestone-by-sub-milestone plan.

## Architecture overview

- **Rust workspace** (`crates/`) built on Tauri 2.x.
  - `crates/app/` — Tauri host. Owns Tauri commands, app state, window. Wired to `crates/core` and `crates/harness` in M1.4.
  - `crates/core/` — pure-Rust persistence layer: `Directory`, `Project`, `AgentRecord`, name validation, JSONL/YAML I/O. No Tauri dependency. Future home of the `Dispatcher` (M1.4).
  - `crates/harness/` — per-harness adapters (M1.3). `ClaudeCodeAdapter`, `MockHarnessAdapter`, event types, stream parser. No Tauri dependency.
- **Frontend** — plain Svelte 5 + Vite + TypeScript + Tailwind v4. Lives at repo root (`src/`, `index.html`, `vite.config.ts`). shadcn-svelte will be initialized in M1.5 when the first UI components land — peer deps (`bits-ui`, `tw-animate-css`) are already installed so `shadcn-svelte init` will be a no-op on the install side.
- **Tauri shell** glues frontend to Rust via `#[tauri::command]` handlers and per-agent event channels.

See `docs/implementation_plans/2026-05-12-v1.md` for the milestone roadmap.

## Where things live

- `crates/app/` — Tauri Rust crate (`Cargo.toml`, `src/main.rs`, `src/lib.rs`, `tauri.conf.json`, `capabilities/`, `icons/`).
- `crates/` — future workspace members.
- `src/` — frontend Svelte/TS sources.
- `tests/` — frontend test setup + integration tests.
- `docs/` — design docs, milestone plans, research notes. Read these before changing scope.
- `docs/implementation_plans/` — per-milestone plans. The plan for the current milestone is the ground truth for what to build.
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

Prerequisites: see `README.md`. The Rust toolchain is pinned in `rust-toolchain.toml`; the Node version is pinned in `.nvmrc`; pnpm is pinned via `packageManager` in `package.json` (installed via `corepack enable`).

## Version pinning policy

- `package.json` and `Cargo.toml` use caret-range constraints (`^x.y.z` / bare versions).
- `pnpm-lock.yaml` and `Cargo.lock` are **committed** — they are the source of truth for exact resolved versions.
- CI uses `pnpm install --frozen-lockfile` and `cargo build --locked` for byte-identical reproducibility.
- Manifest ranges document the supported semver range; the lockfile pins the exact tested version.

## Coding conventions

- **Rust**
  - Edition 2024 across the workspace.
  - Workspace clippy lints: `clippy::all` + `clippy::pedantic`, with a targeted allowlist in `Cargo.toml`'s `[workspace.lints.clippy]` for the lints that fire on common, readable patterns. The allowlist is the safety valve — when a pedantic lint generates real noise as more code lands, add the lint name to the allowlist with a one-line comment explaining why. Don't pare back to "just `clippy::all`" reactively; the allowlist surfaces useful lints we'd otherwise miss.
  - `thiserror` for typed errors at module boundaries.
  - All public functions: no `unwrap`/`expect` outside `main`/test code; bubble errors via `Result`.
  - `tokio::io::BufReader` (not `std::io::BufReader`) for async subprocess pipes — `tokio::process::ChildStdout` doesn't implement `std::io::Read`.
  - `Stdio::null()` for subprocess stdin where we never write to it; prevents pipe-full deadlocks.
  - `#[non_exhaustive]` on enums that cross IPC or evolve between milestones.
- **TypeScript / Svelte**
  - `strict: true` in tsconfig; no `any`.
  - Svelte 5 runes (`$state`, `$derived`, `$effect`).
  - Wire-format types match Rust `#[serde(tag = "type", rename_all = "snake_case")]` — TS uses discriminated unions.
- **Both**
  - No comments unless the _why_ is non-obvious. Identifiers explain _what_.
  - Type hints on every function signature.
  - No imports inside functions unless absolutely necessary.

## Key invariants (extended per sub-milestone)

**Filesystem layout (M1.2, system-design §3).** All Switchboard state lives at `<directory>/.switchboard/`, directly under the user's working directory (not in `~/.switchboard/`). Layout:

```
<directory>/.switchboard/
  config.yaml                          # directory-level config; version: 1
  workflows/                           # YAML workflow files (M5+, empty in M1)
  prompts/                             # local prompt providers (M4+, empty in M1)
  projects.jsonl                       # append-only index: {id, name, created_at}
  projects/<project-id>/
    config.yaml                        # per-project config; version: 1, name, created_at
    registry.jsonl                     # append-only AgentRecord stream
    # NOT created in M1: instance.lock (M3), sessions/ (M2), runs/ (M5)
```

`config.yaml`, `workflows/`, and `prompts/` are intended to be git-tracked. Everything else is runtime data the user should `.gitignore` themselves — Switchboard does NOT modify the user's `.gitignore`.

**Multi-project model (M1.2).** A working directory hosts N projects. A "project" is a task-scoped grouping of agents (e.g., `backend-feature`, `task-2`) — not a 1:1 mapping with the directory. The M1.5 UI displays one active project at a time; the project switcher itself is M3. Project unload is not a v1 concept; once opened, projects stay in memory for the app session.

**Append-only persistence (M1.2).** `projects.jsonl` and per-project `registry.jsonl` are write-once-per-record. No deletion in v1. Corrupted JSONL lines surface as a typed `CoreError::CorruptJsonl { path, line_number, line, source }` — never silently skipped.

**Name normalization rule (M1.2, system-design §3 + §4 P1).** Agent names (within a project) and project names (within a directory) follow the same rule:

- Allowed characters: `^[A-Za-z0-9_-]+$`. No leading-character constraint — digit-first / hyphen-first / underscore-first names are all valid.
- Uniqueness check: `lowercase + hyphen→underscore` canonicalization. `Reviewer-A`, `reviewer_a`, and `REVIEWER-A` are duplicates. The original (verbatim) name is what gets stored.
- Same-named agents in _different_ projects in the same directory are fine — uniqueness is project-scoped.
- Same-named projects in _different_ directories are fine — uniqueness is directory-scoped.

**ID convention (M1.2).** All IDs (`AgentId`, `ProjectId`, future `TurnId`, Claude `session_id`) are UUID v7. Time-ordered, serde-friendly, opaque to consumers.

**Pre-generated Claude session IDs (M1.2).** For `HarnessKind::ClaudeCode` agents, `AgentRecord.session_id` is generated at registration time and stored on the record. The M1.3 adapter uses `--session-id <uuid>` for the first turn (creates the session) and `--resume <uuid>` for subsequent turns (resumes it), distinguished by checking whether `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` exists. For future Codex agents (M2+), `session_id` stays `None` — Codex assigns its own ID and stores it in a per-agent sidecar.

**Stream contract (M1.3).** Every turn produces exactly one `TurnEnd` event — the final event on the stream. Adapters must synthesize `TurnEnd(Failed { kind: AdapterFailure })` if stdout closes without a `result` event (truncated stream). `TurnStart` is _not_ emitted by adapters — it is dispatcher-owned (M1.4) and synthesized before the stream is handed to consumers. This invariant is type-enforced: `AdapterEvent` has no `TurnStart` variant.

**Adapter/dispatcher boundary (M1.3).** `AdapterEvent` carries only `ContentChunk` and `TurnEnd`. `NormalizedEvent` adds `TurnStart` (constructed by the M1.4 dispatcher). `From<AdapterEvent> for NormalizedEvent` lifts adapter events to the wire format. Consumers on the frontend always see `NormalizedEvent`.

**`MockHarnessAdapter` (M1.3).** `MockScenario::Streaming` emits 3 `ContentChunk`s then `TurnEnd(Completed)`. `MockScenario::Panic` intentionally violates the stream contract (panics before `TurnEnd`) — its only legitimate use is testing the M1.4 dispatcher's `AgentIdleGuard` Drop path. Do not use `MockScenario::Panic` for any other purpose.

**Exit-code reconciliation (M1.3).** If `TurnEnd(Completed)` is emitted and the subprocess then exits non-zero, the adapter logs `tracing::warn!` only — it does not emit a second `TurnEnd`. Consumers always see exactly one terminal event.

## Authoritative docs

- `docs/system-design.md` — canonical design.
- `docs/implementation_plans/2026-05-12-v1.md` — full v1 roadmap.
- `docs/implementation_plans/2026-05-12-v1-m1.md` — current milestone plan.
- `docs/research/claude-code-headless.md`, `docs/research/claude-code-cli-observed.md` — ground truth for the Claude Code CLI.
