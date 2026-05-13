# AGENTS.md

Playbook for AI agents (and humans) working on Switchboard. Living doc — extended each sub-milestone.

## What this project is

Switchboard is a macOS desktop app for orchestrating multiple AI coding agents (Claude Code, Codex, etc.) inside one project context. The canonical "what is Switchboard and why" lives in `docs/system-design.md`.

The current milestone is **M1** — the smallest end-to-end vertical slice with Claude Code as the only harness. See `docs/implementation_plans/2026-05-12-v1-m1.md` for the sub-milestone-by-sub-milestone plan.

## Architecture overview

- **Rust workspace** (`crates/`) built on Tauri 2.x.
  - `crates/app/` — Tauri host. Owns Tauri commands, app state, window. Depends on `crates/core` and `crates/harness` (added in M1.2/M1.3).
  - Future: `crates/core/` (project filesystem, agent registry, dispatcher), `crates/harness/` (per-harness adapters).
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
  - Workspace clippy lints: `clippy::all` + `clippy::pedantic` with a small allowlist for the noisiest pedantic lints (`module_name_repetitions`, `missing_errors_doc`, `missing_panics_doc`, `must_use_candidate`). The allowlist is the safety valve — if a pedantic lint generates real noise as more code lands, add it to the allowlist with a comment explaining why. Don't pare back to "just `clippy::all`" reactively; the allowlist surfaces useful lints we'd otherwise miss.
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

- _(none for M1.1 — extended starting M1.2.)_

## Authoritative docs

- `docs/system-design.md` — canonical design.
- `docs/implementation_plans/2026-05-12-v1.md` — full v1 roadmap.
- `docs/implementation_plans/2026-05-12-v1-m1.md` — current milestone plan.
- `docs/research/claude-code-headless.md`, `docs/research/claude-code-cli-observed.md` — ground truth for the Claude Code CLI.
