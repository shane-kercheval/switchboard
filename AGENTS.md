# AGENTS.md

Playbook for AI agents (and humans) working on Switchboard. Living doc — extended each sub-milestone.

## What this project is

Switchboard is a macOS desktop app for orchestrating multiple AI coding agents (Claude Code, Codex, etc.) inside one project context. The canonical "what is Switchboard and why" lives in `docs/system-design.md`.

The current milestone is **M1** — the smallest end-to-end vertical slice with Claude Code as the only harness. See `docs/implementation_plans/2026-05-12-v1-m1.md` for the sub-milestone-by-sub-milestone plan.

## Architecture overview

- **Rust workspace** (`crates/`) built on Tauri 2.x.
  - `crates/app/` — Tauri host. Owns Tauri commands, `AppState`, window, `AppHandleEmitter`. Thin shims over free functions (`*_impl`) defined in `commands.rs`; the free functions are what tests target.
  - `crates/core/` — pure-Rust persistence layer: `Directory`, `Project`, `AgentRecord`, name validation, JSONL/YAML I/O. No Tauri dependency, no async.
  - `crates/harness/` — per-harness adapters: `HarnessAdapter` trait, `ClaudeCodeAdapter`, `MockHarnessAdapter`, event types, stream parser. No Tauri dependency.
  - `crates/dispatcher/` (M1.4) — `Dispatcher`, `EventEmitter` trait + `RecordingEmitter` test double, `AgentIdleGuard`. Drives adapters; owns per-agent in-memory state and `TurnId` generation. No Tauri dependency — bridged to Tauri via `AppHandleEmitter` in `crates/app`.
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
  - **Test the orchestration glue, not just the pure helpers.** Reducer / form components have unit tests for state-transition tables and isolated keyboard/click behaviour. _Components that wrap IPC + event subscriptions + reactive state_ must additionally have integration tests that mock `invoke` and `listen`, capture the event-listener callback, and verify state transitions across realistic event sequences — including ordering races (events arriving before the IPC reply resolves), terminal-state handling (heartbeat timeouts, failed turns), and error paths (IPC throws). Reducer tests alone are not sufficient — every M1.5 frontend bug lived in the wrapping component, not the reducer.
- **Both**
  - No comments unless the _why_ is non-obvious. Identifiers explain _what_.
  - Type hints on every function signature.
  - No imports inside functions unless absolutely necessary.

## Key invariants (extended per sub-milestone)

**Filesystem layout (M1.2, system-design §3).** All Switchboard state lives at `<directory>/.switchboard/`, directly under the user's working directory (not in `~/.switchboard/`). Layout:

```
<directory>/.switchboard/
  config.yaml                          # directory-level config; version: 1
  workflows/                           # YAML workflow files (M6+, empty in M1)
  prompts/                             # local prompt providers (M5+, empty in M1)
  projects.jsonl                       # append-only index: {id, name, created_at}
  projects/<project-id>/
    config.yaml                        # per-project config; version: 1, name, created_at
    registry.jsonl                     # append-only AgentRecord stream
    # NOT created in M1: instance.lock (M4), sessions/ (M2), runs/ (M6)
```

`config.yaml`, `workflows/`, and `prompts/` are intended to be git-tracked. Everything else is runtime data the user should `.gitignore` themselves — Switchboard does NOT modify the user's `.gitignore`.

**Multi-project model (M1.2).** A working directory hosts N projects. A "project" is a task-scoped grouping of agents (e.g., `backend-feature`, `task-2`) — not a 1:1 mapping with the directory. The M1.5 UI displays one active project at a time; the project switcher itself is M4. Project unload is not a v1 concept; once opened, projects stay in memory for the app session.

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

**`MockHarnessAdapter` (M1.3, extended M1.4).** `MockScenario::Streaming` emits 3 `ContentChunk`s then `TurnEnd(Completed)`. Two fault-injection scenarios exist solely to exercise the dispatcher's state-recovery path — both intentionally violate the stream contract and **must never appear in production code paths**: `MockScenario::Panic` (producer task panics mid-stream) and `MockScenario::TruncatedStream` (sender dropped without `TurnEnd`). Use `Panic` only for the `AgentIdleGuard` Drop-under-panic test; use `TruncatedStream` for testing the drain loop's behaviour on a clean truncation. A missing terminal event in production is an adapter bug, not an acceptable outcome — see the dispatcher trust rule below.

**Exit-code reconciliation (M1.3).** If `TurnEnd(Completed)` is emitted and the subprocess then exits non-zero, the adapter logs `tracing::warn!` only — it does not emit a second `TurnEnd`. Consumers always see exactly one terminal event.

**Dispatcher chokepoint (M1.4).** `Dispatcher::send_message` is the single entry point for sending a turn to an agent. Two invariants are independent and have different owners:

- _Agent always returns to Idle_ — **dispatcher-owned.** Held via `AgentIdleGuard` for the lifetime of the dispatch task. RAII `Drop` flips state back to `Idle` on any termination path (success, error, panic). Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) because `Drop` runs synchronously; the lock is never held across `.await`.
- _Exactly one terminal event per turn_ — **adapter-owned** (per M1.3). The dispatcher's drain loop trusts the contract and does not synthesize `TurnEnd` on its own. Single ownership is the design — fallback synthesis at the dispatcher layer would split ownership and mask adapter bugs. If the drain loop observes stream-end without a terminal event, that's an adapter contract violation: the dispatcher logs `tracing::warn!` (so the regression is visible), restores agent state via the guard, and lets the failure surface to the M1.5 reducer (which is responsible for handling "no terminal event observed within N seconds" as an error state).

**Dispatch ordering (M1.4).** `send_message` is load-bearing:

1. Acquire `AgentIdleGuard` under the state lock (`Idle` → `InFlight`). Concurrent sends to the same agent → `Err(Busy)`.
2. Generate fresh `TurnId` (UUID v7) **before** calling `adapter.dispatch()`. The dispatcher owns `TurnId` generation; adapters never generate them.
3. Call `adapter.dispatch(.., turn_id)` — the `TurnId` is passed in so the adapter can embed it in every emitted `AdapterEvent`. On `Err`, the guard drops on early return → state restored to `Idle`. **No `TurnStart` was emitted** — the wire stays clean.
4. Emit `TurnStart` (with the same `turn_id`) only after `dispatch()` returns `Ok`.
5. Spawn the drain task with ownership of the stream, guard, and emitter. Return `DispatchHandle { turn_id, join }` to the caller.

**`EventEmitter` trait (M1.4).** Production: `AppHandleEmitter` (wraps `tauri::AppHandle::emit`). Tests: `RecordingEmitter` (collects `(name, payload)` tuples). The dispatcher takes `Arc<dyn EventEmitter>`, so it's unit-testable without spinning up Tauri.

**Per-agent event channel (M1.4).** The channel name is `agent:<agent_id>` — one channel for the lifetime of the agent, **not** per-turn. Each event payload carries its own `turn_id`; the M1.5 reducer filters by `turn_id` to discriminate between turns. Per-turn channel names would race with the dispatch IPC reply (listener wouldn't exist when `TurnStart` fires); per-agent eliminates that race by definition.

**`AppState` shape (M1.4, multi-project from day 1).** `{ directory: Mutex<Option<Directory>>, projects: Mutex<HashMap<ProjectId, Project>>, active_project_id: Mutex<Option<ProjectId>>, dispatcher: Arc<Dispatcher>, adapter: Arc<dyn HarnessAdapter>, emitter: Arc<dyn EventEmitter> }`. One bound directory at a time (multi-directory is not in scope for v1). N projects loaded; one active project drives UI display. The dispatcher is global because agent IDs are globally unique; switching the active project does not stop background activity on agents in other projects.

**`AgentRecord` lookup for `send_message` (M1.4).** Scans `AppState.projects` for the project whose registry contains the requested `agent_id`. No implicit "active project" routing — the agent ID is globally unique (UUID v7). M1 reads disk on each lookup (registries are small, one project loaded); an in-memory cache is an M4+ optimization, deliberately deferred.

**Harness selection at startup (M1.4).** Read `SWITCHBOARD_HARNESS` env var:

- Unset or `"claude"` → `ClaudeCodeAdapter` (production default).
- `"mock"` → `MockHarnessAdapter` (useful for UI iteration without `claude` installed; identical behaviour at the dispatcher boundary).
- Any other value → panic at startup with a clear error.

**Tauri command pattern (M1.4).** Each `#[tauri::command]` is a thin shim over a free function named `<command>_impl(state: &AppState, ...) -> Result<T, AppError>`. The shim parses UUIDs from strings (Tauri IPC types) and maps `AppError` to `String` (Tauri convention). Unit tests target the free functions; the `#[tauri::command]` wrapper itself is not tested.

**Wire-format ↔ TS type mapping (M1.5).** Rust enums use `#[serde(tag = "type", rename_all = "snake_case")]`; TS types are hand-written discriminated unions in `src/lib/types.ts` that match the Rust shape literally. `DateTime<Utc>` serializes as an ISO-8601 string; consumers convert at the boundary if they need `Date` objects. New variants land additively (`#[non_exhaustive]` on the Rust side, reducer default branches on the TS side that degrade gracefully on unknown discriminants).

**Per-agent transcript reducer (M1.5).** The reducer is a pure function `(transcript, ReducerInput) → transcript` in `src/lib/reducer.ts`. `ReducerInput` is the wire-format `NormalizedEvent` union plus a frontend-synthesized `{ type: "heartbeat_timeout", turn_id }` variant. The reducer is the **single source of truth** for transcript state — component effects (heartbeat timer, IPC subscription) push events into the reducer rather than mutating transcripts directly. Cross-turn isolation is enforced two ways: events for unknown `turn_id`s are dropped, and events for turns already in a terminal state (`complete` or `failed`) are also dropped (the dispatcher's drain task may continue emitting after the UI has heartbeat-timed-out the turn).

**Per-agent event subscription (M1.5).** `AgentPane.svelte` subscribes to `agent:<id>` on mount via `@tauri-apps/api/event::listen` and unsubscribes on unmount. One subscription per AgentPane lifetime, **not** per turn — see M1.4's per-agent channel rationale (per-turn channels race with the IPC reply).

**Binary-not-found banner (M1.5).** `App.svelte` calls `check_claude_binary` once at mount. On failure, renders a non-blocking red banner at the top with the install link. Banner persists across all phases (welcome / directory-selector / no-agent / active). Send attempts will fail until the user installs `claude` and reloads the app; UI flow still works for project/agent creation without `claude` present.

**Heartbeat timeout (M1.5).** Frontend defense against adapter contract violations (M1.4 §7). `HEARTBEAT_TIMEOUT_MS = 60_000` in `src/lib/types.ts`. The AgentPane component owns the timer; it resets on each `content_chunk` for the in-flight turn and fires a `heartbeat_timeout` reducer input when no chunk arrives within the window. The reducer transitions the turn to `failed` with a "no response from harness — retry?" message. **M2 caveat**: when tool calls land, this rule becomes unsafe (a long tool execution can legitimately emit zero `content_chunk`s for minutes). Revisit then.

**M1.5 known limitations.** One bound directory at a time (multi-directory deferred to never-in-v1). One displayed agent per project at a time — most-recently-created wins, deterministic tiebreak `created_at desc, id desc`. M4 adds the agent switcher; until then, in-flight turns on agents that are no longer displayed continue to run on their per-agent channel but are effectively orphaned in the UI. Transcripts are in-memory only (no persistence across app reloads); projects + agents persist on disk under `<directory>/.switchboard/`.

## Authoritative docs

- `docs/system-design.md` — canonical design.
- `docs/implementation_plans/2026-05-12-v1.md` — full v1 roadmap.
- `docs/implementation_plans/2026-05-12-v1-m1.md` — current milestone plan.
- `docs/research/claude-code-headless.md`, `docs/research/claude-code-cli-observed.md` — ground truth for the Claude Code CLI.
