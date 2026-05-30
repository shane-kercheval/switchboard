# AGENTS.md

Playbook for AI agents (and humans) working on Switchboard. High-level orientation; per-milestone mechanics live in `docs/implementation_plans/`, and crate internals are documented in their source.

## What this project is

Switchboard is a macOS desktop app for orchestrating multiple AI coding agents (Claude Code, Codex, etc.) inside one project context. The canonical "what is Switchboard and why" lives in `docs/system-design.md`. Active and past implementation plans live under `docs/implementation_plans/`.

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

### Adding or updating dependencies

**Always use the CLI tools — never hand-edit `Cargo.toml` / `package.json` version strings.** Hand-editing is how stale training-data versions land in the repo: a typed `"0.30"` looks fine but silently pins the `0.30.x` line even when `0.31.x` is the current latest, because Cargo's caret semantics on `0.x` versions don't bridge minor bumps.

- **Rust (Cargo)**: `cargo add <crate>` queries crates.io live and writes the current latest. Flags as needed:
  - `--dev` for `[dev-dependencies]`.
  - `--package <crate>` to target a specific workspace member.
  - `--no-default-features --features <feat>` to opt out of defaults.
  - `--target 'cfg(unix)'` for platform-conditional deps.
  - To bump an existing dep to the latest within its range: `cargo update -p <crate>`. To bump across a major boundary: re-run `cargo add <crate>` (rewrites the manifest line to the current latest).
- **Frontend (pnpm)**: `pnpm add <pkg>` (or `pnpm add -D <pkg>` for `devDependencies`) — same principle. `pnpm update --latest <pkg>` to bump across majors.

After either command, commit both the manifest change and the lockfile diff in one commit.

## Coding conventions

### Rust

- Edition 2024. Workspace clippy lints: `clippy::all` + `clippy::pedantic` with a targeted allowlist in `Cargo.toml`'s `[workspace.lints.clippy]`. When a pedantic lint fires noisily on a common readable pattern, add it to the allowlist with a one-line rationale comment. Don't reactively drop to "just `clippy::all`."
- `thiserror` for typed errors at module boundaries.
- No `unwrap` / `expect` outside `main` or test code; bubble via `Result`.
- `#[non_exhaustive]` on enums that cross IPC or evolve between milestones — adding a new variant should not be a breaking change for consumers.

### TypeScript / Svelte

- `strict: true` in tsconfig; no `any`.
- Svelte 5 runes (`$state`, `$derived`, `$effect`).
- Wire-format types match Rust `#[serde(tag = "type", rename_all = "snake_case")]` — TS uses discriminated unions with snake_case keys. New variants land additively (`#[non_exhaustive]` on the Rust side; reducer default branches on the TS side that degrade gracefully on unknown discriminants).
- `DateTime<Utc>` serializes as ISO-8601 string; consumers convert at the boundary if they need `Date` objects.
- **Styling & components: see `docs/ui-conventions.md`.** Reach for a `src/lib/components/ui/` primitive before hand-rolling; a component that needs a color names a semantic token (`bg-surface`, `text-status-failed`), never a raw palette hue. That doc is the source of truth for the token model, the primitive set, and theming.

### Both languages

- Type hints on every function signature.
- No comments unless the _why_ is non-obvious. Identifiers explain _what_.
- No imports inside functions unless absolutely necessary.
- **No milestone or pass references in code** (`// M2.3 contract`, `// Per the M1.5 plan`, `// Added in M2.6`). Describe the rule directly; chronology lives in `git blame` and PR descriptions.

### Testing

- Test behavior, not implementation. Skip trivial coverage (getter/setter, type-system tautologies).
- Cover edge cases and error paths.
- Deterministic — no time-of-day or wall-clock dependencies in unit tests.
- **For Svelte components that wrap IPC + event subscriptions + reactive state:** pure-reducer tests are insufficient. Also write component-level tests that mock `invoke` and `listen`, capture the event-listener callback, and exercise realistic event sequences — including ordering races (events arriving before the IPC reply resolves), terminal-state handling, and error paths. Frontend bugs tend to live in the wrapping component, not the reducer.
- **Async flush in component tests:** use `await tick()` (from `svelte`) or `await waitFor(...)` for presence assertions on rendered state — both wait for Svelte's reactive scheduler. `await Promise.resolve()` flushes one microtask, which is OK for absence assertions but fragile for presence.

#### Test-type vocabulary

Use these terms consistently in code, comments, commits, and plan docs. The Rust types are Cargo-defined; the "fixture-driven" / "live" distinction is project-specific.

- **Unit test** — `#[cfg(test)] mod tests { ... }` inside a `src/<module>.rs` file. Compiled into the parent crate, so it can call **private items**. Best for internal helpers, state machines, edge cases. Runs in `cargo test` / `make test`.
- **Integration test** (Cargo term) — any `.rs` file under a crate's `tests/` directory. Each file compiles into its own test binary that links against the crate as an external consumer; can only call **`pub` items**. Best for end-to-end behavior through the public API. Also runs in `cargo test` / `make test`.
- **Fixture-driven integration test** — subset of integration tests that exercise the public API using recorded `*.jsonl` fixtures, the `fake_claude` test binary, `MockHarnessAdapter`, or other stubs. Hermetic, fast, no external dependencies. Counted in default `make test` / `make check`.
- **Live test** — subset of integration tests that spawn the real `claude` / `codex` CLI. Marked `#[ignore = "requires <harness> installed — run with: make test-live"]`. Costs subscription quota. Run via `make test-live`, not default `make test`. See "Live testing against real harnesses" above for the full policy.

In short: **live ⊂ integration ⊂ all tests**, and fixture-driven is the other (non-live) half of the integration set.

### Live testing against real harnesses

Adapter correctness depends on behavior we don't control: harness event vocabularies, exit-code semantics, stream timing, session-file layout. CLI vendors (Anthropic, OpenAI) ship updates that shift these contracts — sometimes silently. Mocked-only tests would lock in our _current understanding_ of the harnesses and keep passing forever even after upstream drift breaks production. Live tests are how we notice.

**Live tests are developer-local, not CI.** Subscription auth tokens (the only supported auth in v1) tend to rotate on use and can be device-bound, which makes them brittle as GitHub Actions secrets and creates a non-trivial blast radius if leaked. We accept the trade-off: upstream CLI changes are detected reactively (when a developer runs `make test-live`) rather than proactively via scheduled CI. Revisit if a clean auth model emerges.

- **Convention.** Live tests are normal `cargo test` tests marked `#[ignore = "requires <harness> installed — run with: make test-live"]`. They live alongside the fixture-driven tests (e.g., `crates/harness/tests/live.rs`).
- **Runner.** `make test-live` runs `cargo test -- --ignored` against `switchboard-harness`, `switchboard-dispatcher`, and `switchboard-app`. Any `#[ignore]`-gated test in those crates participates. Per-harness targets (`make test-live-claude` / `-codex` / `-gemini` / `-antigravity`) run just one harness's live tests — useful for spending quota only on the harness you changed (e.g. after a CLI version bump). The default `make test` / `make check` paths **do not** run live tests — they stay fast and offline.
- **Live-test naming convention (load-bearing for the per-harness targets).** Every live test name MUST start with `live_<harness>_`, where `<harness>` is `claude` / `codex` / `gemini` / `antigravity` — e.g. `live_codex_resume_reuses_session`, `live_gemini_full_stack_emits_…`, `live_antigravity_check_auth_…`. The harness comes first; any layer/descriptor (`full_stack`, `check`, …) goes after it. The `make test-live-<harness>` targets are identical positive filters on that name. **A live test that doesn't carry its harness name silently drops out of its `test-live-<harness>` target** (false confidence after a CLI bump), so it's caught only by the full `make test-live`. Name new live tests accordingly.
- **Authentication.** Live tests rely on the developer's logged-in `claude` / `codex` session (subscription auth, no API keys). If a test fails with an auth-flavored error, run `claude login` / `codex login` and retry.
- **Cost discipline.** Every live prompt is constrained to a tiny response (e.g., `"Reply with the single word 'ack'"`) so the whole suite costs cents and finishes in minutes. The constraint is per-test response size, not test count — add as many small live tests as the surface needs.
- **What to cover.** Any change that affects how an adapter talks to the real CLI — new subprocess flags, new event types we parse, new session-file fields we read, new spawn behavior (process groups, stdio handles) — should land with a live test that exercises the change end-to-end. The fixture-based tests prove the parser handles a recorded shape; the live tests prove that shape still arrives from the current CLI version.
- **When to run.** Before merging any adapter-touching PR. After a new release of Claude Code or Codex, to catch upstream regressions before they hit users. Periodically as a sanity check even when nothing has changed locally (CLI vendors can ship server-side changes that affect stream content without a client version bump).

## Cross-cutting invariants

Project-wide rules that apply across all milestones. Milestone-specific mechanics (dispatcher internals, exact wire shapes, lock orders, file paths) live in the milestone plan that introduced them and in the relevant crate's source — not here.

- **Project = unit of work.** A project hosts N equally first-class agents. UI is one unified transcript stream with per-agent attribution, plus a per-agent overview sidebar. **No singleton "active" or "focused" agent at the model level** — all agents in a loaded project are equally live.
- **Vocabulary: send vs. turn.** A _send_ is one dispatch action (one compose-bar submit, or later one workflow step) targeting 1..N recipients; each recipient's request→response cycle is a _turn_, so a multi-recipient send creates N independent turns. Use this vocabulary in code, comments, and tests. Canonical definition: system-design §7 "Sends and turns."
- **Conversation source-of-truth is split (see system-design §3).** Harness session files own **agent-produced content** (responses, tool calls) — on project open Switchboard reads them (`~/.claude/projects/.../*.jsonl`, `~/.codex/sessions/.../rollout-*.jsonl`, etc.), parses into a normalized `Turn` shape, and merges chronologically. Switchboard owns the **user's side** — a per-project append-only conversation journal (`journal.jsonl`) of user _sends_ (written at turn-start; a `send_id` groups a fan-out so the user's message renders once) and _outcome markers_ for every non-completed turn (failed or cancelled — no content). It does **not** store agent content itself. The two partition: harness files supply _completed_-turn content, the journal supplies _non-completed_-turn outcomes (no correlation/dedup). Durable history begins at turn-start, so queued-but-unstarted messages are live-UI-only.
- **Harness registries (MCP servers, skills) are harness-owned, like transcripts.** They come from each harness's config files and skills directories (see system-design §9 for per-harness sources and the per-harness research docs in `docs/research/` for file shapes). Switchboard reads them as inputs; it does not maintain its own copies. Loader failures degrade to empty lists with a warning — these registries are display-only, not load-bearing for dispatch.
- **Filesystem layout.** Per-directory Switchboard state lives at `<directory>/.switchboard/` — directly under the user's working directory, not in `~/.switchboard/`. `config.yaml`, `workflows/`, and `prompts/` are intended to be git-tracked; everything else is runtime data the user should `.gitignore` themselves. User-global state lives in the OS-conventional config dir resolved via the `directories` crate (illustrative `~/.config/switchboard/` on Linux; `~/Library/Application Support/switchboard/` on macOS): personal `config.yaml`/`prompts/`, plus `workspace.yaml` — the app-managed set of working directories the user works across, the source for the flat cross-directory project list. See system-design §3 for the full layout.

## Authoritative docs

- `docs/system-design.md` — canonical design (the "what and why").
- `docs/ui-conventions.md` — frontend styling + component conventions (token model, `ui/` primitives, theming).
- `docs/implementation_plans/` — per-milestone plans and the v1 roadmap. Read the active milestone plan before changing scope; it's the ground truth for what to build.
- `docs/research/` — harness ground-truth notes:
  - `harness-behavior.md` — **the single source of truth for harness behavior**: how each harness behaves in the scenarios we care about (failures, auth, quota, metadata), how our adapter/frontend handles it, and the gap register. Start here.
  - `harness-update-review.md` — playbook for reviewing a harness CLI version bump for impact on Switchboard (what to read, the dependency surface to check, how to record findings).
  - `same-session-parallel-invocation.md` — why we enforce session-id uniqueness at the app layer.
  - `archive/` — frozen, no-longer-maintained provenance, including the per-harness `*-cli-observed.md` probes (raw strings, exit codes, fixtures) that `harness-behavior.md` distills from. Cite for evidence; trust `harness-behavior.md` for current behavior. `docs/system-design.md` §9 is the design-level capability matrix; `harness-behavior.md` is its operational companion.
- `README.md` → "Harness support and limitations" — the **user-facing** register of harness behaviors and limitations. Add an entry when a harness constraint is something a _user_ would hit and want explained in product terms: a capability one harness lacks (a model that can't be selected), an environment that won't work (a path that breaks an agent), a choice gated outside Switchboard (plan-limited models). Keep each to one or two plain-language lines; lead with the user-visible symptom, not the mechanism. This is **not** where adapter internals, wire shapes, exit codes, or gap-register detail go — those stay in `docs/research/harness-behavior.md`. Rule of thumb: if a user would file it as "why doesn't X work," it belongs in the README; if only a developer cares how we detect or handle it, it belongs in the research doc.
