# M1 implementation plan (Claude Code only)

> **Audience:** the AI coding agent implementing M1. Read this entire doc, plus the prerequisites listed below, **before writing any code**. Stop after each sub-milestone for human review.

## What M1 is

M1 builds the **smallest possible end-to-end implementation** of Switchboard — every architectural layer wired together (Tauri app shell, project filesystem, harness adapter trait + one concrete adapter, dispatcher, single-pane UI), with **Claude Code as the only harness**.

The goal isn't to ship a feature-complete app — it's to prove the architecture works as a coherent whole before adding more on top of it. M1 is the foundation everything else builds on:

- **M2** validates the per-harness adapter abstraction by adding Codex through the same trait, expands the normalized event vocabulary, and adds integration test infrastructure.
- **M3** adds multi-pane UI, dispatcher contention enforcement, and per-turn cancel.
- **M4** adds prompt providers (slash commands).
- **M5+** add workflows, pause-for-user, iteration, and the rest of v1.

If any architectural layer is missing or broken in M1, M2 can't validate the adapter abstraction by plugging in a second harness, and every subsequent milestone is pushing on rotten foundations.

**Concretely**, after M1 a user can: open Switchboard, create a project bound to a working directory, spawn one Claude Code agent named `assistant`, send "What's 2+2?", see "4" stream into the pane. macOS only. That's the entire user-visible surface — small, but every layer underneath it is real.

## How to use this plan

1. Read these files first (in order):
   - `docs/system-design.md` — the canonical "what is Switchboard and why." Sections 3 (core concepts — agent name normalization), 4 (functional primitives — what we're orchestrating), 7 (user-facing model — agent contention; only the M1-relevant parts), 9 (harness integration — per-harness adapter trait, normalized event stream, Claude Code specifics), 10 (form factor — platform / tray notes; M1-irrelevant parts can be skimmed).
   - `docs/v1-plan.md` — the M1 section in particular, plus the "Critical path" preamble.
   - `docs/research/claude-code-headless.md` and `docs/research/claude-code-cli-observed.md` — these are the ground-truth references for the Claude Code CLI surface. The CLI's observed behavior (event types, `--session-id`, `--include-partial-messages`, exit codes, single-process model) is more authoritative than anything reconstructed from memory.
2. Implement sub-milestones M1.1 → M1.5 in order. Each sub-milestone is self-contained: code + tests + doc updates. Stop after each one, summarize what landed, and wait for human review before continuing.
3. **Ask clarifying questions if you hit something the plan is silent on.** Otherwise the plan is committed — implement as written. Do not invent behavior the spec doesn't cover; surface the gap.
4. Per `~/.claude/CLAUDE.md`: never remove or skip tests/functionality to get tests to pass; never commit on the user's behalf; never add Claude as author/co-author.

## Definition of done for M1 (as a whole)

The M1 acceptance from `docs/v1-plan.md`:

> Maintainer can `cargo tauri dev`, create a project, spawn an agent named `assistant`, send "What's 2+2?", see "4" stream into the pane. Hygiene CI passes on the M1 branch / merge to main.

Do not consider M1 done until **all five sub-milestones are merged** and this end-to-end flow works on a clean macOS checkout.

## What's deliberately out of scope for M1

These belong to later milestones — do not implement them, even if "easy":

- Codex adapter (M2)
- Multi-agent UI / agent selector (M2)
- Process-group spawn / SIGTERM-to-process-group (M2; M1 spawns the bare subprocess and lets the OS clean up on app exit — Claude Code is a single process per `claude-code-cli-observed.md` so this is safe for M1)
- Per-turn cancellation (M3)
- Project-level `flock` for multi-instance protection (M3)
- Slash commands / prompt providers (M4)
- Workflows (M5)
- First-launch acknowledgement dialog (M7)
- Tray, walk-away, signing, auto-updater (M7)

If the implementing agent finds a "clearly minor" expansion of M1 scope tempting, **stop and ask**. M1's value is its smallness — the smaller the end-to-end slice, the faster M2 can validate the architecture by adding the second harness on top of it.

---

## Sub-milestone M1.1 — Repo scaffolding + Tauri shell + hygiene CI

### Goal & outcome

A booting Tauri 2.x desktop app, with a frontend stack (Svelte 5 + Tailwind + shadcn-svelte), a Cargo workspace ready for additional crates, and a hygiene CI workflow that's green on `main`. Nothing user-visible beyond an empty window.

After this sub-milestone:
- `cargo tauri dev` opens a window
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` all pass locally
- Frontend builds and lints cleanly
- A trivial round-trip Tauri command (`ping` returns `"pong"`) demonstrates the IPC plumbing works
- GitHub Actions hygiene workflow is green on the M1.1 PR

### Implementation outline

1. **Initialize Tauri 2.x app** using the official template. Reference: <https://tauri.app/start/create-project/>.
   - Frontend: Svelte 5 with TypeScript + Vite + Tailwind. shadcn-svelte: <https://shadcn-svelte.com/>.
   - Pick a package manager (recommend `pnpm`). Pin it in `package.json`'s `packageManager` field.
   - **Pin exact versions** for `shadcn-svelte`, `bits-ui`, and peers — no `^` or `~` ranges and no `"latest"`. For an AI-agent-consumed plan, floating versions are a reproducibility hazard, and `bits-ui` peer-dep churn against Svelte 5 runes makes this concrete.
   - **Install and initialize `tauri-plugin-dialog`** (Rust: `tauri-plugin-dialog` crate; frontend: `@tauri-apps/plugin-dialog`). First used in M1.5's folder picker, but adding plugins mid-stream involves Cargo.toml + capabilities config + npm install — wire it now to avoid that yak-shave later.
   - The Tauri app's bundle identifier should be something like `com.switchboard.app`.
2. **Set up the Cargo workspace.** The Tauri template generates a single-crate layout; restructure into a workspace so M1.2/M1.3/M1.4 can add `crates/core` (project model, registry) and `crates/harness` (adapter trait + Claude Code adapter) cleanly. Concretely:
   - Root `Cargo.toml` with a `[workspace]` block listing members. Tauri's generated crate (default `src-tauri/`) becomes `crates/app/` (or stays as `src-tauri/` — pick the convention you prefer; the rest of the plan assumes `crates/app/`). Future crates land alongside as `crates/core/`, `crates/harness/`, etc.
   - `[workspace.package]` block defining shared `edition = "2021"` (or `"2024"` if pinned Rust supports it), `version`, `authors`, `license`, `repository` — member crates inherit via `package.edition.workspace = true` etc.
   - `[workspace.dependencies]` block pinning shared deps **once** (each member crate uses `serde = { workspace = true }`). At minimum: `tokio` (with `["full"]` features for now; can narrow later), `serde` (with `derive`), `serde_json`, `serde_yaml`, `thiserror`, `uuid` (with `["v7", "serde"]`), `chrono` (with `["serde"]`), `tracing`, `tracing-subscriber`, `which`, `async-trait`, `futures`, `tempfile` (dev-dep for tests). M1.3 will add to this list.
   - `[workspace.lints]` block (Cargo 1.74+, supported in 2026) defining shared rustc + clippy lints. Set `clippy.pedantic = "warn"` is overkill; start with `clippy.all = "warn"` plus a few targeted pedantic lints. Member crates pull in via `[lints] workspace = true`.
   - **Commit `Cargo.lock`** — Switchboard ships as a binary, not a library; reproducible builds require committed lockfile.
   - `rustfmt.toml` — keep minimal. Just `edition = "2021"` (or 2024) and `max_width = 100`. Default rustfmt is fine; don't over-customize.
3. **Project hygiene + agent context.** Files that aren't code but make the repo navigable for humans and AI agents alike:
   - **`Makefile`** — single source of truth for dev commands so neither the README nor agents have to remember invocations. Targets: `dev` (`cargo tauri dev`), `test` (`cargo test --all-features` + `pnpm test`), `lint` (`cargo clippy --all-targets --all-features -- -D warnings` + `pnpm lint`), `fmt` (`cargo fmt` + `pnpm format` if applicable), `check` (everything CI runs locally — `fmt --check`, `lint`, `test`), `clean`. Keep it small; one-liners per target.
   - **`AGENTS.md`** — playbook for AI agents (and humans) working on the codebase. Living doc, extended each sub-milestone. Initial sections:
     - **What this project is** (1 paragraph + pointer to `docs/system-design.md`)
     - **Architecture overview** (Rust workspace layout, Svelte frontend, Tauri shell — pointer to `docs/v1-plan.md` for milestone roadmap)
     - **Where things live** (`crates/`, `docs/`, `.github/`, `Makefile`, etc.)
     - **How to run / test / lint** (point at Makefile)
     - **Coding conventions** (no comments unless WHY is non-obvious; type hints everywhere; thiserror for typed errors; `Stdio::null()` for subprocess stdin; per-agent event channel `agent:<id>` with reducer-side `turn_id` filter; etc. — extend per sub-milestone as patterns emerge)
     - **Key invariants** (registry is append-only; exactly one terminal `TurnEnd` per turn; dispatcher is the single chokepoint for harness calls; etc. — extend per sub-milestone)
     - **Authoritative docs** (system-design.md, v1-plan.md, m{N}-implementation-plan.md, research notes)
   - **`CLAUDE.md`** — one line: `@AGENTS.md`. The `@` prefix tells Claude Code to inline AGENTS.md when loading project context. Keeps Claude-specific entrypoint trivial; AGENTS.md is the harness-neutral source of truth.
   - **`rust-toolchain.toml`** — pin Rust stable channel current at M1.1 start (e.g., `[toolchain] channel = "1.83.0", components = ["rustfmt", "clippy"], profile = "minimal"`). Reproducible builds; clear "what Rust version this builds against."
   - **`.nvmrc`** — pin the Node version (`pnpm`'s `packageManager` field pins pnpm itself; `.nvmrc` pins the Node runtime under it).
   - **`.editorconfig`** — basic formatting consistency (tabs vs spaces by file type, trim trailing whitespace, final newline). Cross-IDE; respected by VS Code, JetBrains, Vim, etc. Keeps formatting drift out of diffs.
   - **`.gitignore`** — verify Tauri's generated `.gitignore` covers `/target/`, `/node_modules/`, `/dist/`, build artifacts, OS files (`.DS_Store`), and Tauri-specific paths (`/crates/app/target/`). Add anything missing.
4. **Add a trivial round-trip command.** A `ping(name: String) -> String` Tauri command and a Svelte component that calls it on mount and renders the response. This proves IPC end-to-end.
5. **Frontend test infra.** Set up Vitest + `@testing-library/svelte`. Write one trivial smoke test (e.g., the App component mounts without throwing). Wire `pnpm test` into CI from day 1. This makes M1.5's component tests pure additive work — no setup-plus-tests combo PR.
6. **Hygiene CI.** Create `.github/workflows/hygiene.yml`. macOS runner. Steps: checkout → setup Node + pnpm → setup Rust toolchain (stable) → `pnpm install` → run via `make check` (which runs `pnpm lint`, `pnpm test`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features`). Using `make check` keeps CI and local in sync — same command, same set of checks.
7. **README.md update.** Add a "Local development" section. Don't duplicate Makefile commands; reference them. Roughly: "Prerequisites: Node + pnpm (see `.nvmrc`), Rust (see `rust-toolchain.toml`), Xcode CLT (macOS). Common commands: `make dev`, `make test`, `make check`. See `AGENTS.md` for project context, `docs/v1-plan.md` for the milestone roadmap."

### Code snippets

The Tauri command shape — keep this pattern uniform across milestones:

```rust
#[tauri::command]
async fn ping(name: String) -> Result<String, String> {
    Ok(format!("pong, {name}"))
}
```

Frontend invocation (Svelte 5, TypeScript):

```typescript
import { invoke } from "@tauri-apps/api/core";
const reply = await invoke<string>("ping", { name: "world" });
```

### Testing strategy

- **Rust unit test for `ping`** — calls the function directly, asserts the reply.
- **Frontend smoke test** — Vitest + `@testing-library/svelte` is set up in this milestone (per implementation outline step 5). The smoke test confirms the App component mounts.
- **`make check` runs clean locally** — proves the Makefile + workspace + lint + test wiring all hang together. CI then runs `make check` and matches.
- **CI itself is the test of CI** — confirm the hygiene workflow runs green on the M1.1 PR before merging.

### Docs to update

- `README.md` — local dev section per implementation outline step 7.
- `AGENTS.md` — initial scaffold per implementation outline step 3.
- No spec changes expected.

### Manual smoke test

After CI passes and you've reviewed the diff, verify by hand. **If anything below fails, the PR isn't ready regardless of unit-test results.**

1. **Fresh clone smoke** — `git clone` to a scratch directory; install prerequisites per the new README; run `make dev` → an empty Tauri window opens within ~30s.
2. **`make test`** → exits 0; output shows Rust + Vitest tests both ran.
3. **`make lint`** → exits 0 (no clippy warnings, no frontend lint errors).
4. **`make fmt`** — running it should be a no-op on a freshly-checked-out tree (already formatted).
5. **`make check`** → exits 0. Same set of checks CI runs.
6. **Ping round-trip** — in the running app, open WebView devtools, look for the rendered ping response in the UI (or call `await window.__TAURI__.core.invoke('ping', { name: 'world' })` in the console and confirm it returns `'pong, world'`).
7. **README sanity** — re-read the README "Local development" section and confirm the commands listed actually work as described. The fresh-clone smoke above is the ground-truth check for this.

Open a PR titled `M1.1: Tauri shell + hygiene CI`. Wait for human review.

---

## Sub-milestone M1.2 — Working directory + project filesystem + agent registry

### Goal & outcome

Pure-Rust persistence layer for working directories, projects (multiple per directory), and agent registries — no UI, no harness yet. After this sub-milestone:
- A `Directory` type wraps a canonicalized on-disk path that may host zero or more Switchboard projects under `<directory>/.switchboard/`.
- A `Project` type represents a task-scoped grouping of agents within a directory. `ProjectId` is UUID v7. Multiple projects can coexist under one directory.
- A `ProjectIndex` (the `projects.jsonl` file) is the append-only list of projects in a directory.
- `Agent` records can be appended to and listed from a project's registry.
- Agent name uniqueness (including hyphen↔underscore normalization per `system-design.md` §3 Primitive 1) is enforced **per project** — same name can exist in different projects in the same directory.
- Project name uniqueness within a directory is enforced (so the user can disambiguate). Cross-directory name collisions are fine.

**Scope notes:**
- "Working directory" = the on-disk directory (typically a git repo) the user binds to. "Project" = a task-scoped grouping under it. Multiple projects per directory; see system-design §3.
- The directory's `.switchboard/` folder lives **directly inside the working directory** (not in `~/.switchboard/...`). Per system-design §3.
- Both the **agent registry** and the **project index** hold **N entries from day one** (forward-compatible with the project switcher UI in M3 and the agent selector UI in M2). M1.5's UI exposes one active project displaying one agent — switchers come in later milestones.
- Switchboard does **not** modify the user's `.gitignore`. The directory-level `config.yaml`, `workflows/`, and `prompts/` are intended to be git-tracked; everything else under `.switchboard/` is runtime data the user should `.gitignore` themselves. Switchboard touching `.gitignore` would be invasive for a tool that touches user repos.
- **User-global config (OS-conventional path via the `directories` crate; for `local_prompt_dirs` and MCP provider configs per `system-design.md` §6) is M4+ scope — M1 does not read or write it.** M1 has no prompts (M4) and no MCP (M4), so nothing in M1's scope materializes the user-global layer. The `directories` crate dependency, the schema, the override-vs-fallback resolution logic, and the tests all land alongside prompt providers in M4. Implementing agent: do not add user-global config handling to M1; the silence is intentional.

### Implementation outline

1. **Crate layout.** Add `crates/core` to the workspace. This crate is pure Rust (no Tauri dependency); the Tauri crate depends on it. This separation lets us test the model layer without spinning up Tauri.
2. **`Directory` type** — wraps an on-disk path; canonicalizes on construction; reads/writes the directory-level `.switchboard/` contents:
   ```rust
   pub struct Directory {
       pub path: PathBuf,                  // canonicalized via std::fs::canonicalize
   }

   impl Directory {
       /// Wraps a path; canonicalizes; does NOT require .switchboard/ to exist.
       pub fn at(path: &Path) -> Result<Directory>;
       /// Returns true if <path>/.switchboard/ exists.
       pub fn has_switchboard(&self) -> bool;
       /// Creates <path>/.switchboard/{config.yaml, workflows/, prompts/, projects.jsonl, projects/} if missing. Idempotent.
       pub fn init(&self) -> Result<()>;
       /// Lists projects from projects.jsonl. Returns empty Vec if init() not yet called.
       pub fn list_projects(&self) -> Result<Vec<ProjectSummary>>;
       pub fn create_project(&self, name: &str) -> Result<Project>;
       pub fn open_project(&self, id: ProjectId) -> Result<Project>;
   }

   pub struct ProjectSummary {
       pub id: ProjectId,
       pub name: String,
       pub created_at: DateTime<Utc>,
   }
   ```
3. **`Project` type** — represents one task-scoped project under a directory:
   ```rust
   pub struct Project {
       pub id: ProjectId,                  // UUID v7
       pub name: String,                   // user-supplied; unique within its directory
       pub directory: PathBuf,             // canonicalized; convenience
       pub config: ProjectConfig,
       pub root: PathBuf,                  // <directory>/.switchboard/projects/<project-id>/
       pub registry_path: PathBuf,         // <root>/registry.jsonl
   }

   pub type ProjectId = Uuid;              // UUID v7 (consistent with AgentId)
   ```
4. **`DirectoryConfig` and `ProjectConfig`** — both serialized to YAML.
   - `DirectoryConfig` (`<directory>/.switchboard/config.yaml`): minimal — `version: 1`, otherwise empty in M1 (placeholder for future MCP/harness config per system-design §6).
   - `ProjectConfig` (`<directory>/.switchboard/projects/<project-id>/config.yaml`): minimal — `version: 1`, `name`, `created_at`, otherwise empty in M1.
   - Both error with typed `UnsupportedConfigVersion { found, expected: 1 }` on mismatch.
5. **Project index — `projects.jsonl`.** Append-only at `<directory>/.switchboard/projects.jsonl`. One `ProjectSummary`-shaped record per line. `list_projects` reads it; `create_project` appends.
6. **Agent registry.** Per-project, append-only JSONL at `<directory>/.switchboard/projects/<project-id>/registry.jsonl`. One agent per line. Schema:
   ```rust
   #[derive(Serialize, Deserialize)]
   pub struct AgentRecord {
       pub id: AgentId,                  // UUID v7 (time-ordered; via the uuid crate's v7 feature) — generated on create
       pub project_id: ProjectId,        // the project this agent belongs to (defensive denormalization — registry path also encodes it)
       pub name: String,                 // user-supplied, unique within this project
       pub harness: HarnessKind,         // enum, M1 only ClaudeCode
       pub session_id: Option<Uuid>,     // Claude session UUID (v7, same convention as AgentId). Set at create-time for Claude Code agents — pre-generated by Switchboard and passed to Claude via `--session-id <uuid>` (see M1.3 step 3). For Codex agents (M2+), this stays None — Codex assigns its own session ID from the stream and stores it in a per-agent sidecar (see M2 plan). Optional from the start so M2's Codex adapter doesn't force a schema migration.
       pub created_at: DateTime<Utc>,
   }
   ```
7. **Name normalization.** Per `system-design.md` §3 Primitive 1, agent names that differ only in hyphen vs. underscore are duplicates **within a project**. Canonicalize by replacing `-` with `_` and lowercasing for the *uniqueness check only* — store the original name as the user typed it. Reject duplicates with a typed error. Two projects in the same directory CAN both have an agent named `assistant` — name uniqueness is project-scoped.
8. **Project name uniqueness.** Within a directory, project names must be unique (so the user can disambiguate "backend-feature" vs "frontend-feature"). Same canonicalization rule as agents: hyphens → underscores, lowercase, for the uniqueness check only. Cross-directory collisions are fine.
9. **Project API surface** (on `Project`):
   - `register_agent(&self, name: &str, harness: HarnessKind) -> Result<AgentRecord>` — generates IDs, validates name uniqueness within this project, appends to registry.
   - `list_agents(&self) -> Result<Vec<AgentRecord>>` — reads the project's registry.
10. **Errors.** Use `thiserror`. Distinguish I/O errors, validation errors (bad name, duplicate, unsupported config version), and corruption errors (malformed JSONL line).

### Testing strategy

Use `tempfile::TempDir` for isolation. All tests are unit tests in the `core` crate:

- **Directory roundtrip.** `Directory::at(tmp).init()` creates expected layout; `has_switchboard()` returns true; `list_projects()` returns empty.
- **Single-project roundtrip.** Create a directory + one project, register two agents, reopen the project, list — same data back.
- **Multi-project per directory.** Create a directory + two projects with different names. List projects → both appear. Each has its own agents (created independently). Project A's agents do not appear in Project B's `list_agents()`.
- **Same agent name in two projects** — `Project A.register_agent("assistant")` and `Project B.register_agent("assistant")` both succeed. Name uniqueness is project-scoped.
- **Duplicate agent name within a project (verbatim).** Registering `assistant` twice in the same project fails.
- **Duplicate agent name (hyphen↔underscore) within a project.** `agent-a` then `agent_a` in the same project fails.
- **Duplicate project name within a directory.** Same hyphen↔underscore normalization rule: `feature-a` then `feature_a` in the same directory fails.
- **Same project name across directories.** Two different directories both with a project named `feature-a` succeed.
- **Empty / whitespace-only agent or project name.** Rejected.
- **Reserved characters in name.** Per `system-design.md` §3, the spec is `^[A-Za-z0-9_-]+$` (no leading-character constraint). Test rejection of empty, whitespace, and characters outside the class. Test acceptance of digit-first, hyphen-first, and underscore-first names so the spec rule is enforced rather than accidentally tightened. If during implementation a stricter rule seems warranted (e.g., digit-first names create awkward template-variable identifiers in fan-in contexts), surface the proposal — don't silently tighten.
- **`Directory::init` is idempotent** — calling twice on the same directory leaves the existing structure intact (doesn't wipe projects, doesn't error).
- **`Directory::open_project` on an unknown ID** fails cleanly.
- **Path canonicalization** — `Directory::at` resolves symlinks; relative paths are made absolute; permission-denied directories surface a typed error (not a panic).
- **Unsupported config version** — write a `config.yaml` with `version: 99`; opening it returns `UnsupportedConfigVersion { found: 99, expected: 1 }`.
- **Corrupted registry / projects.jsonl line** — append a malformed line by hand, then `list_agents` / `list_projects` returns a typed error pointing at the bad line. (Don't silently skip — corruption should surface.)

### Docs to update

- New section in `system-design.md` is **not** needed; §3 already describes the directory layout and the multi-project-per-directory model.
- `docs/v1-plan.md`'s deferred-detail callout ("Persistence schema details (10.3)") — note that M1 lands the registry + project-index shapes; the runs/checkpoint shape is still M5.
- **`AGENTS.md`** — add the registry-is-append-only invariant, the hyphen↔underscore name normalization rule (applies to both agent names within a project AND project names within a directory), the directory layout (`<directory>/.switchboard/{config.yaml, workflows/, prompts/, projects.jsonl, projects/<project-id>/...}`), and the multi-project-per-directory model. Note explicitly that "project" is a task-scoped grouping (not 1:1 with a directory) — easy concept to get wrong on first encounter.

### Manual smoke test

M1.2 is backend-only — no UI, no Tauri integration yet. Verification is short. **If anything below fails, the PR isn't ready regardless of unit-test results.**

1. **`make test`** → exits 0; output includes the new `crates/core` tests for `Directory`, `Project`, agent registry, and project index.
2. **`cargo test -p switchboard-core`** → directory + multi-project + agent registry tests all pass (roundtrip, multi-project-per-directory, project-scoped name uniqueness, corruption, regex acceptance/rejection).
3. **Ad-hoc multi-project tempdir smoke** — drop into a `#[test]` and verify the multi-project case end-to-end: `Directory::at(tmp).init()`; create two projects with different names; register two agents in each (use `assistant` as a name in BOTH projects to confirm same-name-different-projects works); reopen the directory; list projects (see both); list agents in each (see correct two per project, no cross-talk). Optional sanity check beyond unit tests.
4. **Inspect the on-disk layout** after the smoke above — `find <tmp>/.switchboard -type f` should show: `config.yaml`, `projects.jsonl`, `projects/<id-1>/{config.yaml, registry.jsonl}`, `projects/<id-2>/{config.yaml, registry.jsonl}`. Layout matches system-design §3.
5. **`make check`** → exits 0.

Open a PR titled `M1.2: project filesystem + agent registry`. Wait for human review.

---

## Sub-milestone M1.3 — Normalized event types + Claude Code adapter

### Goal & outcome

A working `claude -p` integration behind the per-harness adapter trait. After this sub-milestone:
- The minimal normalized event vocabulary (TurnStart, ContentChunk, TurnEnd) exists as Rust types.
- A `HarnessAdapter` trait is defined.
- A `ClaudeCodeAdapter` implementation spawns `claude -p`, parses the stream-json output, and emits normalized events on a channel.
- Unit tests cover the parser using a **fake harness binary fixture** that emits canned stream-json lines.
- One opt-in **live integration test** (gated by env var, e.g., `SWITCHBOARD_LIVE_HARNESS=1`) actually runs `claude -p` and verifies the adapter handles a real round-trip. This is not part of CI for M1; it'll join the integration suite in M2.

No Tauri integration in this sub-milestone — that's M1.4. Keep this work in `crates/core` (or a new sibling like `crates/harness`).

### Implementation outline

1. **Normalized event types.** Two enums: `AdapterEvent` (what adapters emit) and `NormalizedEvent` (what crosses IPC). The split makes "TurnStart is dispatcher-owned, never adapter-emitted" a type-level invariant rather than a doc convention.

   ```rust
   // Adapter-emitted: parser produces these. TurnStart is NOT here — it is
   // dispatcher-owned. Excluding it from this enum makes the invariant
   // type-enforced; a future adapter author cannot accidentally emit TurnStart.
   pub type TurnId = Uuid;  // UUID v7 (consistent with AgentId, ProjectId — one UUID convention across the codebase)

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(tag = "type", rename_all = "snake_case")]
   #[non_exhaustive]
   pub enum AdapterEvent {
       ContentChunk { turn_id: TurnId, text: String },
       TurnEnd { turn_id: TurnId, outcome: TurnOutcome, ended_at: DateTime<Utc> },
   }

   // Wire format: what crosses IPC to the frontend. The dispatcher constructs
   // TurnStart at dispatch time; AdapterEvent is lifted into the rest via From.
   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(tag = "type", rename_all = "snake_case")]
   #[non_exhaustive]
   pub enum NormalizedEvent {
       TurnStart { turn_id: TurnId, started_at: DateTime<Utc> },
       ContentChunk { turn_id: TurnId, text: String },
       TurnEnd { turn_id: TurnId, outcome: TurnOutcome, ended_at: DateTime<Utc> },
   }

   impl From<AdapterEvent> for NormalizedEvent {
       fn from(e: AdapterEvent) -> Self {
           match e {
               AdapterEvent::ContentChunk { turn_id, text } =>
                   NormalizedEvent::ContentChunk { turn_id, text },
               AdapterEvent::TurnEnd { turn_id, outcome, ended_at } =>
                   NormalizedEvent::TurnEnd { turn_id, outcome, ended_at },
           }
       }
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(tag = "status", rename_all = "snake_case")]
   #[non_exhaustive]
   pub enum TurnOutcome {
       Completed,
       Failed { kind: FailureKind, message: String },
       // Future: Cancelled { source: CancelSource } — added in M3 when per-turn
       // cancel lands. Cancellation is intentional, not a failure — its own
       // top-level variant.
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "snake_case")]
   #[non_exhaustive]
   pub enum FailureKind {
       /// Harness reported `is_error` in its terminal `result` event. Caused by
       /// model/API issues — bad model name, rate limit, transient API error,
       /// invalid prompt content. Retry semantics: depends on cause; the
       /// caller should look at the message before retrying.
       HarnessError,
       /// Synthesized by the adapter when the subprocess died, the parser hit
       /// malformed JSON, or stdout EOF arrived without a terminal `result`
       /// event. Caused by infrastructure — process crash, OOM, network drop
       /// mid-stream, etc. Retry semantics: typically transient.
       AdapterFailure,
       // Future: Timeout — added when (and if) we land an active per-turn
       // hard timeout (separate from passive stall detection — see
       // system-design.md §12 open question 10.19).
   }
   ```

   The `#[serde(tag = "type", rename_all = "snake_case")]` on `NormalizedEvent` pins the IPC wire format for the frontend (M1.5 writes a matching TypeScript discriminated union). `DateTime<Utc>` serializes to an ISO-8601 string by default — the TS type uses `string`, not `Date`. Future milestones add ToolStarted, ToolCompleted, RateLimitEvent, SessionMeta — `#[non_exhaustive]` keeps adding variants in M2 non-breaking, and the matching tag attribute on both enums must be preserved to maintain the wire contract. New adapter-emitted variants get added to *both* enums; new dispatcher-emitted variants (rare — TurnStart is the only one for now) go in `NormalizedEvent` only.

   **Why `kind` on `Failed` (not just `message`):** the discriminator is load-bearing for M5's partial-failure rule (the human-in-the-loop pause we filed in `v1-plan.md` M5). When one agent in a fan-in fails, the workflow needs to distinguish "harness reported a model error" (`HarnessError` — bad prompt, retry won't help) from "subprocess crashed" (`AdapterFailure` — transient, retry might help). The UI can use the same discriminator to surface different affordances ("retry" vs "this is a Switchboard problem, restart the agent"). Internal Rust code at the adapter layer can carry richer cause detail in logs without surfacing it on the wire.

2. **`HarnessAdapter` trait.** The minimum surface for M1:
   ```rust
   #[async_trait]
   pub trait HarnessAdapter: Send + Sync {
       async fn dispatch(
           &self,
           agent: &AgentRecord,
           project_root: &Path,
           prompt: &str,
       ) -> Result<EventStream, DispatchError>;
   }

   pub type EventStream = Pin<Box<dyn Stream<Item = AdapterEvent> + Send>>;

   #[derive(Debug, thiserror::Error)]
   pub enum DispatchError {
       #[error("claude binary not found on PATH")]
       BinaryNotFound,
       #[error("failed to spawn claude subprocess: {0}")]
       SpawnFailed(#[from] std::io::Error),
   }
   ```

   **Error routing** — two paths, never confused:
   - `DispatchError` covers failures *before* the stream is established (binary missing, spawn syscall failed). The dispatcher (M1.4) handles these by leaving the agent in `Idle` and surfacing the error to the caller.
   - Failures *after* the stream starts (subprocess died, parser hit malformed JSON, EOF without terminal `result` event) surface as a synthesized `AdapterEvent::TurnEnd { outcome: Failed }` on the stream — never as a `DispatchError`. The turn completes cleanly from the dispatcher's perspective.
   
   The stream completes when the parser observes the harness's terminal `result` event (becomes `AdapterEvent::TurnEnd`). Subprocess lifecycle continues until reaped per step 8.

3. **`ClaudeCodeAdapter`.** Constructs the command line:
   ```
   claude -p \
     --output-format stream-json \
     --include-partial-messages \
     --verbose \
     --dangerously-skip-permissions \
     --session-id <uuid>          # always — see probe step below
     # OR --resume <uuid>         # only if probe shows --session-id is not idempotent
   ```
   **Before implementing, probe `--session-id` idempotency:** run `claude -p --session-id <fresh-uuid> 'hi'` against a UUID that has no on-disk session yet. Does it create the session and run the turn (idempotent — best case), or does it fail demanding `--resume`?
   
   - **If idempotent** → always pass `--session-id <uuid>`, no branching needed at all. Eliminates a code path entirely.
   - **If not idempotent** → pass `--session-id <uuid>` on the first turn and `--resume <uuid>` thereafter. Choose based on the existence of `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` (ground truth from Claude's own data). **Do not add a mutable `initialized: bool` field to `AgentRecord`.** Two reasons: (a) it violates the registry's append-only invariant; (b) ground truth is more robust — if the user manually deletes a session file, the FS check sees "no file" and the adapter creates a fresh session via `--session-id`, but a registry flag would still say `initialized: true` and the adapter would call `--resume`, which Claude rejects (file's gone), leaving the agent stuck.

   The session UUID itself is pre-generated as a UUID v7 at agent creation time (same convention as `AgentId` — keeps one UUID version across the codebase; Claude treats the UUID as opaque so v4 vs v7 is functionally identical) and stored on `AgentRecord.session_id` (see M1.2).
   
   Per `claude-code-cli-observed.md`, `--include-partial-messages` requires `stream-json` output. Per the research notes, `--verbose` is required alongside `--include-partial-messages` — confirm by running `claude --help`, but plan for it being mandatory. The `--input-format stream-json` flag is **not** used in M1 (positional prompt input only — see step 5).

4. **Stream-json parser.** Use `tokio::process::Command` with piped stdout. Read line-by-line via `BufReader::lines()`. Parse each line as JSON; map to `NormalizedEvent` per the table below.

   | stream-json event | Normalized event |
   |---|---|
   | `{type: "stream_event", event: {type: "content_block_delta", delta: {type: "text_delta", text: "..."}}}` | `ContentChunk { text }` |
   | `{type: "result", is_error: false, ...}` | `TurnEnd { outcome: Completed }` |
   | `{type: "result", is_error: true, result: "..."}` or `api_error_status != null` | `TurnEnd { outcome: Failed { message } }` |
   | `{type: "system", subtype: "init"}`, `{type: "assistant", ...}`, `{type: "user", ...}` (synthetic tool result), `{type: "rate_limit_event", ...}`, all other types | Ignored in M1 |

   **Precedence rule (text source):** with `--include-partial-messages` enabled (M1's default), `content_block_delta` deltas are the **only** source of `ContentChunk` text. Terminal `assistant` message text blocks are explicitly ignored — they would double-emit otherwise. If `--include-partial-messages` is ever disabled in the future, fall back to `assistant` text blocks. The two are mutually exclusive — never emit both.

   **TurnStart is not in this table.** The dispatcher (M1.4) constructs `TurnStart` at dispatch time — before the harness subprocess even boots — so the UI sees "processing" instantly. The parser emits only `ContentChunk` and `TurnEnd` for M1.

   Verify the exact JSON shape of the partial-message events by running a probe locally — the cli-observed note doesn't fully document the partial-message format. Update the table if the actual format differs.

5. **Subprocess input.** Pass the prompt as a positional argument: `claude -p "<prompt>" ...`. M1 does not use `--input-format stream-json` — simpler, avoids verifying the stream-json input format, and we can introduce streaming-input later if mid-turn message support becomes needed.

6. **Subprocess working directory.** `Command::current_dir(project_root)` so Claude Code's cwd matches the user's bound project.

7. **Error handling.** Distinguish two cases:
   - **`claude` binary not found on PATH** — at adapter construction (or first dispatch), use `which::which("claude")` to detect. Return a typed `BinaryNotFound` error from the adapter constructor — *not* via `TurnEnd(Failed)`. This surfaces as an app-level banner (M1.5), not a per-turn failure. Minimum-version assertion is deferred to M7.
   - **Subprocess failure mid-turn** → adapter synthesizes a terminal `TurnEnd(Failed { kind, message })` and ends the stream. Don't panic. Map the cause to `FailureKind`:
     - **Harness's terminal `result` event reports `is_error: true`** (or `api_error_status != null`) → `Failed { kind: HarnessError, message: <result.result text> }`.
     - **Subprocess died, parser hit malformed JSON, stdout EOF arrived without terminal event, non-zero exit before terminal event** → `Failed { kind: AdapterFailure, message: <description of the cause> }`.
   
   **Stream contract** — consumers always receive exactly one terminal `TurnEnd` per turn. The adapter owns this guarantee: if the subprocess dies without emitting `result`, the adapter must synthesize `TurnEnd(Failed { kind: AdapterFailure, ... })`. Frontend state machines must not have to handle "stream ended without TurnEnd" as a distinct case.

8. **Subprocess lifecycle ownership.** The dispatch task owns the `Child` handle from `Command::spawn()`. Concurrently:
   1. Read stdout line-by-line via `BufReader::lines()` — drives the parser → normalized events.
   2. Drain stderr concurrently in a separate `tokio::spawn`'d task that reads to EOF and logs the contents. Don't let stderr's pipe buffer fill — that can deadlock the subprocess.
   3. After the terminal `result` event is observed (parser emits `TurnEnd`) and the stdout reader finishes (EOF), `await child.wait()` to reap.
   
   **Exit-code reconciliation policy for M1:** if the parser already emitted `TurnEnd(Completed)` but `child.wait()` reports a non-zero exit code, **log the discrepancy** but do not re-emit a different terminal event (you can't un-emit). M2's Codex work introduces more variance and is when we revisit whether to hold terminal emission until reconciliation. For M1, log-only is enough.

9. **`MockHarnessAdapter`** — second implementation of `HarnessAdapter` that produces canned events programmatically (no subprocess at all). Purpose: **dev-time iteration on the M1.5 UI without `claude` installed or authenticated**, plus useful as a deterministic test harness for the dispatcher beyond what `RecordingEmitter` covers (which only tests emission, not adapter behavior).

   Live alongside `ClaudeCodeAdapter` in the harness crate:
   ```rust
   pub struct MockHarnessAdapter {
       // M1: keep simple. One canned scenario.
       // Future: configurable scenarios, error injection, latency simulation.
   }

   #[async_trait]
   impl HarnessAdapter for MockHarnessAdapter {
       async fn dispatch(&self, agent: &AgentRecord, _project_root: &Path, prompt: &str)
           -> Result<EventStream, DispatchError>
       {
           // Construct an EventStream that yields:
           //   ContentChunk { turn_id: <generated>, kind: Text, text: "Mock response to: " }
           //   ContentChunk { turn_id: <generated>, kind: Text, text: <prompt> }
           //   ContentChunk { turn_id: <generated>, kind: Text, text: " — replied by mock harness." }
           //   TurnEnd { turn_id: <generated>, outcome: Completed, ended_at: <now>,
           //              usage: Some(TurnUsage { input_tokens: <prompt.len() as u64 / 4>, output_tokens: 12, ... }) }
           //
           // Use tokio::time::sleep(50ms) between chunks to simulate streaming. The dispatcher's
           // TurnId is generated by the dispatcher itself per M1.4 step 5; the adapter reads it
           // off `agent` or accepts it via constructor. Match the actual TurnId flow.
           ...
       }
   }
   ```

   **Selection at runtime.** App startup reads `SWITCHBOARD_HARNESS` env var:
   - Unset or `claude` → construct `Arc<dyn HarnessAdapter>` as `ClaudeCodeAdapter` (default — production behavior).
   - `mock` → construct as `MockHarnessAdapter` (no `claude` lookup, no `which::which("claude")` check, no `BinaryNotFound` banner).
   - Any other value → panic with a clear error listing valid values. (Don't silently fall back to default; that's a footgun.)

   The selection lives in `crates/app`'s startup code where `AppState` is constructed (see M1.4 step 3).

   **Scope limits for M1:**
   - Single canned scenario (text streaming + clean `TurnEnd(Completed)`). Future milestones can add `MockHarnessAdapter::with_scenario(MockScenario::ErrorMode)` etc. when needed.
   - Not exposed in user-facing UI (no "demo mode" toggle in M1). Env-var/dev-only.
   - Documented in the README's "Local development" section so the implementing agent (and future contributors) know it exists.

### Code snippets

Subprocess spawn + line-reading skeleton (illustrative):

```rust
let mut child = Command::new("claude")
    .args(args)
    .current_dir(project_root)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
let stdout = child.stdout.take().expect("piped");
let mut lines = BufReader::new(stdout).lines();
while let Some(line) = lines.next_line().await? {
    let event: serde_json::Value = serde_json::from_str(&line)?;
    if let Some(normalized) = parse_event(event) {
        tx.send(normalized).await?;
    }
}
```

### Testing strategy

- **Fake harness fixture.** Build a tiny Rust binary (e.g., `crates/harness/tests/fixtures/fake-claude/`) that takes a fixture-name CLI argument and dumps a canned stream-json file from a `fixtures/` directory. The adapter under test points its `Command::new()` at this fake binary instead of `claude` (via a `claude_binary_path` field on the adapter — defaults to `"claude"` in production, overridable in tests).
- **Fixtures to capture.** Each is a real `.jsonl` recording of `claude -p` on a representative input:
  - Trivial text-only turn ("What's 2+2?") — confirms parser emits multiple `ContentChunk`s → `TurnEnd(Completed)`. **Also assert no double-emit:** the concatenated `ContentChunk` text length is approximately equal to the terminal `assistant` message text length. Use a tolerance (or a "deltas-not-also-emitting-from-assistant" check), not strict equality, since the harness may normalize whitespace between deltas and the final message.
  - Tool-using turn (the model uses Read or Bash) — confirms tool events are silently skipped, only text becomes `ContentChunk`s
  - Failed turn (e.g., `--model invalid-model-name`) — confirms `TurnEnd(Failed)` with the error message
  - Empty / whitespace-only prompt — confirms what Claude Code does and we handle it
  
  Capture these by running `claude -p ... --output-format stream-json > fixture.jsonl` once and committing them to the repo.
- **Wire-format roundtrip test.** A `serde_json::to_value` + `from_value` roundtrip on each `NormalizedEvent` variant confirms the on-the-wire shape — `{"type": "turn_start", ...}`, `{"type": "content_chunk", ...}`, `{"type": "turn_end", "turn_id": "...", "outcome": {"status": "completed"}, "ended_at": "..."}`. Asserts the snake_case discriminator strings match what M1.5's TS type expects. Also test the `From<AdapterEvent> for NormalizedEvent` lift — every `AdapterEvent` variant maps to its `NormalizedEvent` counterpart with no field loss.
- **Parser unit tests.** Feed each fixture through the parser; assert the expected `NormalizedEvent` sequence.
- **Process-spawn unit test.** Spawn the fake harness, drain the stream, assert events. This validates the subprocess plumbing without hitting real `claude`.
- **Live integration test.** Gated behind `#[ignore]` plus an env-var check (`SWITCHBOARD_LIVE_HARNESS=1`). Runs `claude -p` for real, asserts that "What's 2+2?" produces some `ContentChunk` containing `"4"` and a `TurnEnd(Completed)`. Run this manually before merging M1.3 — do not enable in CI for M1 (M2 stands up the integration CI workflow).
- **Negative tests.**
  - Subprocess exits with status 1 → adapter emits `TurnEnd(Failed)` with a useful message.
  - Subprocess emits malformed JSON on a line → adapter emits `TurnEnd(Failed)`.
  - **Subprocess emits stdout EOF *without* a terminal `result` event** (simulate by truncating a fixture mid-stream) → adapter synthesizes `TurnEnd(Failed)`. Validates the stream contract from step 7.
  - Subprocess writes massive stderr → no deadlock (stderr drainer keeps the pipe clear).
  - Subprocess exits non-zero *after* the parser already saw a `result` event → discrepancy is logged but the parser's terminal event is the one consumers see (per M1 reconciliation policy).
  - **`claude` binary not found** (`which::which("claude-does-not-exist")` returns `Err`) → adapter constructor returns `BinaryNotFound`; no subprocess is spawned; no `TurnEnd(Failed)` is emitted on this path.
- All negative tests must produce typed errors or synthesized `TurnEnd(Failed)`s — never a panic.
- **`MockHarnessAdapter` test.** Drain the stream from `MockHarnessAdapter::dispatch(...)`; assert the canned sequence (`ContentChunk`s × N → `TurnEnd(Completed)` with `usage` populated). Verifies the trait is satisfied correctly and the dispatcher (M1.4) can be tested end-to-end against it without spawning real `claude`. Single test for the single canned scenario; expand when more scenarios land.

### Docs to update

- `docs/research/claude-code-cli-observed.md` — if any of the capture/probe work in M1.3 reveals new behavior (especially around `--include-partial-messages` and `--input-format stream-json`), append a "Findings during M1.3" subsection.
- No spec changes expected unless something contradicts `system-design.md` §9 — surface that to the user immediately.
- **`AGENTS.md`** — add the exactly-one-terminal-event stream contract, the AdapterEvent vs NormalizedEvent split rationale (TurnStart is dispatcher-owned, never adapter-emitted), the parser-precedence rule (deltas authoritative when `--include-partial-messages` enabled), the subprocess-lifecycle pattern (`Stdio::null()` at spawn, drain stderr concurrently, `child.wait()` after terminal event), the FailureKind discriminator on TurnEnd, and the **`MockHarnessAdapter` for dev-time UI iteration without real `claude`** (selected via `SWITCHBOARD_HARNESS=mock` env var).

### Manual smoke test

M1.3 is backend-only — adapter + parser, no Tauri integration. Live verification requires `claude` installed and authenticated. **If anything below fails, the PR isn't ready regardless of unit-test results.**

1. **`make test`** → exits 0; output includes the harness crate's fake-fixture parser tests.
2. **Probe `--session-id` idempotency by hand** (per the M1.3 step 3 instruction). Run `claude -p --session-id <fresh-uuid> 'reply with ack'` against a UUID Claude has never seen. Did the implementing agent's reported probe result match? Confirm the implementation took the right branch (always-pass `--session-id` if idempotent; first-vs-subsequent if not).
3. **Live integration test** — `SWITCHBOARD_LIVE_HARNESS=1 cargo test -- --ignored` runs the real-`claude` smoke test. Should complete in ~5–10s. Expect at least one `ContentChunk` containing `"4"` (or the model's answer to "What's 2+2?") and a clean `TurnEnd(Completed)`.
4. **Negative path manual check** — temporarily rename or remove `claude` from your PATH, then run the live integration test. Should see the typed `BinaryNotFound` error, NOT a generic `TurnEnd(Failed)`. Restore PATH afterwards.
5. **Stderr drain check** — run the live integration test once with `RUST_LOG=debug` (or whatever the project's log config ends up as) and confirm stderr from `claude` is being read and logged, not silently dropped.
6. **`make check`** → exits 0.

Open a PR titled `M1.3: normalized events + Claude Code adapter`. Wait for human review.

---

## Sub-milestone M1.4 — Dispatcher + Tauri command surface

### Goal & outcome

Wire the harness adapter into the Tauri app. After this sub-milestone:
- A `Dispatcher` type holds in-memory per-agent state and is the single entry point for sending a message to an agent. Agent IDs are globally unique (UUID v7) so the dispatcher's keying needs no project context.
- App state holds **one bound working directory** and **N projects under it** (multi-project from day 1) per system-design §3 + M1.2; the M1 UI exposes one **active project** at a time (project switcher lands in M3). Multi-directory is not in scope for v1.
- Tauri commands for working-directory + project lifecycle (`init_directory`, `list_projects`, `create_project`, `open_project`, `set_active_project`) and per-agent operations (`create_agent`, `list_agents`, `send_message`) are exposed.
- Streaming events from the adapter are forwarded to the frontend via Tauri events.
- Unit + integration tests cover the dispatcher (using the fake harness from M1.3).
- No UI yet — that's M1.5. Test by invoking commands from the Rust side or via the Tauri devtools console.

This sub-milestone establishes the chokepoint pattern that M3 will harden into the formal "single dispatcher" with contention enforcement. For M1, it's just the entry point shape — concurrency hardening is M3.

### Implementation outline

1. **`Dispatcher`.** Owns `HashMap<AgentId, AgentState>` behind a `std::sync::Mutex` (sync mutex; state ops never `.await` while held — required for `AgentIdleGuard::Drop` to work, see step 6). `AgentState` for M1 is just `{ status: Idle | InFlight }` — enough to refuse a `send_message` if the agent already has a turn in flight. **This is a minimal local guardrail, not the M3 chokepoint.** Return a generic "agent is busy" error string. M3 owns the actual error taxonomy (typed errors, structured contention reasons, UI gating treatment) — do not preemptively model those in M1.
2. **`EventEmitter` trait** (defined alongside the dispatcher in `crates/core`):
   ```rust
   pub trait EventEmitter: Send + Sync {
       fn emit(&self, name: &str, payload: serde_json::Value);
   }
   ```
   The Tauri-facing crate provides an `AppHandleEmitter` that wraps `tauri::AppHandle::emit`. Tests use a `RecordingEmitter` (`Mutex<Vec<(String, Value)>>`). The dispatcher takes `Arc<dyn EventEmitter>`. This makes the dispatcher fully unit-testable without spinning up Tauri.
3. **App state.** A `tauri::State<AppState>` shape (multi-project from day 1):
   ```rust
   pub struct AppState {
       pub directory: Mutex<Option<Directory>>,                  // currently-bound working directory (one at a time in v1)
       pub projects: Mutex<HashMap<ProjectId, Project>>,         // all loaded projects in this directory
       pub active_project_id: Mutex<Option<ProjectId>>,          // which one the UI is currently viewing
       pub dispatcher: Arc<Dispatcher>,                          // global; agent_ids are unique across projects
       pub claude_adapter: Arc<dyn HarnessAdapter>,              // singleton, constructed at startup (real or mock — see harness selection below)
       pub event_emitter: Arc<dyn EventEmitter>,                 // wraps tauri::AppHandle
   }
   ```
   The M1.5 UI picks one `active_project_id`; project switcher (UI for changing it) lands in M3. Background activity for non-active projects keeps running because the dispatcher is global and event channels are agent-scoped — see step 5.

   **Harness selection at startup.** Read the `SWITCHBOARD_HARNESS` env var to construct `claude_adapter`:
   - Unset or `"claude"` → `ClaudeCodeAdapter` (production default; runs `which::which("claude")` and surfaces `BinaryNotFound` to the M1.5 banner if missing).
   - `"mock"` → `MockHarnessAdapter` (per M1.3 step 9). Skips the binary check entirely; useful for UI iteration without `claude` installed.
   - Any other value → panic at startup with a clear error listing valid values. Do NOT silently fall back to default — that's a footgun.
   
   This is the single switch the implementing agent (and any future contributor) flips to develop the UI without burning real-claude quota.
4. **Tauri commands.** Working-directory + project lifecycle commands take or return path / id strings; per-agent commands take `agent_id` (no `project_id` needed since agent_ids are globally unique and the dispatcher routes via `AgentRecord.project_id`).
   ```rust
   #[tauri::command]
   async fn check_claude_binary() -> Result<(), String>;  // surfaces BinaryNotFound for the M1.5 banner

   // Working directory + project lifecycle
   #[tauri::command]
   async fn pick_directory(path: String) -> Result<DirectoryInfo, String>;
   // Returns canonicalized path + whether .switchboard/ exists + project list (empty if not yet inited).

   #[tauri::command]
   async fn init_directory(state: State<'_, AppState>, path: String) -> Result<DirectoryInfo, String>;
   // Idempotent — creates .switchboard/ if missing; binds AppState.directory.

   #[tauri::command]
   async fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectSummary>, String>;
   // Lists projects in the currently-bound directory.

   #[tauri::command]
   async fn create_project(state: State<'_, AppState>, name: String) -> Result<ProjectSummary, String>;

   #[tauri::command]
   async fn open_project(state: State<'_, AppState>, project_id: String) -> Result<ProjectSummary, String>;
   // Loads the project into AppState.projects (no-op if already loaded).

   #[tauri::command]
   async fn set_active_project(state: State<'_, AppState>, project_id: String) -> Result<(), String>;
   // Pure UI-state change — does not affect background dispatch.

   // Per-agent operations (operate on the active project unless agent_id is given explicitly)
   #[tauri::command]
   async fn create_agent(state: State<'_, AppState>, name: String) -> Result<AgentRecord, String>;
   // Creates the agent in the active project.

   #[tauri::command]
   async fn list_agents(state: State<'_, AppState>, project_id: Option<String>) -> Result<Vec<AgentRecord>, String>;
   // Defaults to active project; can ask for a specific project's agents.

   #[tauri::command]
   async fn send_message(state: State<'_, AppState>, agent_id: String, prompt: String) -> Result<TurnId, String>;
   // No project_id needed — agent_id is globally unique (UUID v7); dispatcher resolves the project via AgentRecord.
   ```
   Returning `String` for the error type is a Tauri convention; map `thiserror`-typed errors to `to_string()` at the boundary. **`send_message` returns the `TurnId` synchronously** — the dispatcher generates it before spawning the harness, lets the UI scope its event subscription to that turn (see step 5), and emits `TurnStart` immediately so the user sees "processing" the moment they hit Send.
5. **Turn lifecycle and event forwarding.** The ordering here is load-bearing — it satisfies both the round-2 invariant (pre-stream `DispatchError` returns synchronously, never leaving an orphan `TurnStart` on the wire) AND the concurrent-send-race protection (two simultaneous `send_message` calls for the same agent: only one passes the Idle check). The `AgentIdleGuard` plays both roles.
   - **(a)** Acquire `AgentIdleGuard` under the state lock — this transitions agent state from `Idle` to `InFlight` atomically. If the agent is not `Idle`, return `Err(Busy)` to the caller. Release the lock immediately after the guard is constructed (the guard handle, not the lock itself, is what's held going forward — `std::sync::Mutex` is never held across `.await`).
   - **(b)** Call `adapter.dispatch(...)`. **The state lock is not held during this `.await`** — concurrent `send_message` calls for the same agent see `InFlight` and return `Err(Busy)`.
     - If `Err(DispatchError)`: the `AgentIdleGuard` drops on early return → state restored to `Idle` automatically via RAII. Return the error synchronously to the caller. **No `TurnStart` was emitted** — the wire stays clean.
     - If `Ok(EventStream)`: continue.
   - **(c)** Generate fresh `TurnId`. Emit `TurnStart` via `EventEmitter` — this is the first normalized event on the per-agent channel for this turn, so the frontend can show "processing" before any harness-emitted event is consumed from the stream. (The harness subprocess is already running by this point — `adapter.dispatch()` spawned it in step (b) — but no `AdapterEvent` has been forwarded yet.)
   - **(d)** Spawn the drain task with ownership transferred (via `move`) of the `EventStream`, the `AgentIdleGuard`, and an `Arc<dyn EventEmitter>`. Return `TurnId` to the caller synchronously.
   - The drain task lifts each `AdapterEvent` into a `NormalizedEvent` via `From<AdapterEvent>`, emits each via the `EventEmitter`, then drops the `AgentIdleGuard` on terminal-event observation → state restored to `Idle`.
   - **Two-state model: `AgentState { status: Idle | InFlight }`.** `InFlight` covers both "reserved for dispatch" (after the `AgentIdleGuard` is acquired but before `adapter.dispatch()` returns) and "actively running a turn" (after the stream is established). Concurrent sends in either window return `Err(Busy)` to the caller — the user-facing error message is the same; M1 doesn't need a finer state distinction. (M3 may add structured contention reasons.)
   - **Event name pattern: `agent:<agent_id>`** (per-agent — one channel for the lifetime of the agent, not per-turn). Each event payload carries its own `turn_id`. The M1.5 reducer subscribes once when the AgentPane mounts (not per turn) and filters events by the current `turn_id` to discriminate between turns.
   - **Why per-agent, not per-turn:** a per-turn channel name (`agent:<id>:turn:<turn_id>`) would require the frontend to subscribe AFTER receiving the `turn_id` from the IPC reply — but the dispatch task emits `TurnStart` concurrently, and the IPC reply and the event cross the WebView bridge in undefined order. If `TurnStart` arrives first, the listener doesn't exist yet and the event is silently dropped (the worst kind of bug — intermittent, environment-dependent). The per-agent channel eliminates the race because the listener exists before any event can fire. The reducer's `turn_id` filter is the load-bearing defense against cross-turn event leakage (see the M1.5 reducer test "late event from prior turn ignored").
   - **Backpressure: M1 emits each event naively, one Tauri event per `NormalizedEvent`.** This will not scale to M3's multi-pane fan-out (one fan-out turn × N agents × hundreds of token deltas). M3 expansion must address this — design space includes the §10 ring buffer, coalescing windows, rate limiting, or size caps. See the deferred-from-M1 callout in `v1-plan.md` M3.
   - **Background activity for non-active projects keeps running.** The dispatcher is global; agent IDs are globally unique; event channels are per-agent. Switching the active project in the UI (M3) is purely a display change — agents in other projects keep streaming, workflows in other projects keep running, events keep firing on their per-agent channels. The frontend reducer routes events to the right agent's transcript regardless of which project is currently active. **M1 carve-out:** in M1 only one project is ever loaded at a time (the one the user just opened/created). The multi-project subscription model is *available* via the AppState shape but not *exercised* until M3's project switcher actually loads multiple projects. Don't over-build M1's subscription path against multi-project scenarios that won't fire — M3 puts the property under real load and is when subscription edge cases (e.g., listener teardown on project unload) need real attention.
6. **Two invariants the dispatcher must guarantee** (keep these mentally separate — they're independent guarantees with different owners):
   - **Dispatcher invariant — agent always returns to Idle.** Implementation: hold an `AgentIdleGuard` for the lifetime of the dispatch task. Its `Drop` impl flips state back to `Idle`. This holds even on panic, channel drop, or any other early termination path. (RAII pattern, like `std::sync::MutexGuard` — must use `std::sync::Mutex` for the state map per step 1, since `Drop` runs synchronously and `tokio::sync::Mutex::lock()` is async.) **Owner: dispatcher.** Ensures backend state coherence.
   - **Stream contract — consumers always receive exactly one terminal event per turn.** **Owner: adapter** (per M1.3 step 7): if the subprocess dies without emitting `result`, the adapter synthesizes `TurnEnd(Failed)`. Ensures frontend stream coherence — the M1.5 reducer never has to handle "stream ended without TurnEnd" as a distinct case.
7. **AgentState lifecycle for crash recovery.** Out of scope for M1 — if Switchboard crashes mid-turn, the next launch starts with all agents `Idle` (registry doesn't track in-flight state). M5 introduces step-boundary checkpointing for workflow runs; per-agent crash recovery for individual turns is implicit in that work.

### Testing strategy

- **Dispatcher unit tests** (no Tauri — use `RecordingEmitter` for assertions).
  - `send_message` to an idle agent transitions to InFlight, runs the turn, transitions back to Idle.
  - `send_message` returns a `TurnId` synchronously; the dispatch task emits `TurnStart` (with that `TurnId`) on `agent:<id>` before any parser events arrive (assert via `RecordingEmitter`'s recorded sequence).
  - Concurrent `send_message` calls to the same agent: the second returns the busy error.
  - Concurrent `send_message` calls to *different* agents both run; their event streams don't cross-contaminate (assert by event name — each agent has its own `agent:<id>` channel).
  - **Cross-project concurrency:** create one project with `assistant-A` and another project (same directory) with `assistant-B`; concurrent `send_message` to both completes; events on each agent's channel arrive correctly with no cross-talk. Confirms the dispatcher handles multi-project correctly even though M1 UI only shows one.
  - A failed turn (fake harness emits an error fixture) leaves the agent back in Idle, not stuck in InFlight.
  - **Panic test:** a panicking dispatch task does not leave the agent stuck `InFlight` — `AgentIdleGuard`'s `Drop` impl restores state. Use a force-panic adapter to validate.
  - **Stream-contract test:** an adapter that ends its `AdapterEvent` stream without a terminal `AdapterEvent::TurnEnd` — the dispatcher's drain loop must observe exactly one `TurnEnd` per turn (the adapter, per M1.3 step 7, synthesizes `TurnEnd(Failed)` if the upstream subprocess dies silent). Catches regression if the adapter ever fails to do so.
- **`EventEmitter` testing.** Use `RecordingEmitter` to assert exact event sequences emitted per turn. Happy path: `turn_start` → `content_chunk`×N → `turn_end(completed)`. Failed path: `turn_start` → `turn_end(failed)`. All events on the per-agent name `agent:<id>`; payloads carry the `turn_id`.
- **Tauri command tests.** Tauri's testing story is limited. Each command is a thin shim around a free function that takes state explicitly; unit-test the free function. Don't try to test the `#[tauri::command]` wrapper itself.
- **End-to-end Tauri smoke** is deferred to M1.5 (manual verification: open devtools, listen, send a message, see events stream).

### Docs to update

- No new doc files. If the `Dispatcher` shape diverges meaningfully from `system-design.md` §7, surface that as a discussion before changing the spec.
- **`AGENTS.md`** — add the dispatcher-is-the-single-chokepoint rule, the `EventEmitter` trait + `RecordingEmitter` testing pattern, the `AgentIdleGuard` RAII pattern (with `std::sync::Mutex` rather than `tokio::sync::Mutex`), the dispatch ordering (acquire `AgentIdleGuard` first → state transitions `Idle → InFlight` under lock; release lock; call `adapter.dispatch()` with lock released; `DispatchError` causes guard to drop on early return → state restored to `Idle` automatically; `TurnStart` fires *only after* `dispatch()` returns `Ok` so no orphan `TurnStart` ever lands on the wire), the per-agent event channel `agent:<id>` convention, and the multi-project AppState shape (one bound directory at a time, N projects loaded, one active project for UI display, dispatcher is global because agent IDs are globally unique).

### Manual smoke test

M1.4 wires the dispatcher into Tauri commands but ships no UI — verification is via devtools console. Requires `claude` installed and authenticated for the real-harness path. **If `claude` isn't available, you can do most of the verification with `SWITCHBOARD_HARNESS=mock make dev` instead** — the mock harness (M1.3 step 9) returns canned responses end-to-end, exercising the full dispatcher + Tauri command + event-emission path without a real subprocess. Real-harness verification still happens before merge for the live integration test, but local dev iteration doesn't need it. **If anything below fails, the PR isn't ready regardless of unit-test results.**

1. **`make test`** → exits 0; output includes dispatcher unit tests using `RecordingEmitter` plus the cross-project-concurrency test.
2. **`make dev`** → app window opens (still empty UI from M1.1).
3. **Open WebView devtools** and exercise the command surface manually. Suggested sequence:
   ```javascript
   // Confirm the binary check works
   await window.__TAURI__.core.invoke('check_claude_binary');  // returns null/ok

   // Bind a working directory + create a project
   const dirInfo = await window.__TAURI__.core.invoke('init_directory', { path: '/tmp/sw-smoke' });
   const projA = await window.__TAURI__.core.invoke('create_project', { name: 'project-a' });
   await window.__TAURI__.core.invoke('set_active_project', { projectId: projA.id });

   // Create an agent in the active project
   const agent = await window.__TAURI__.core.invoke('create_agent', { name: 'assistant' });

   // Subscribe to events for that agent
   const unlisten = await window.__TAURI__.event.listen(`agent:${agent.id}`, e => console.log(e));

   // Send a message
   const turnId = await window.__TAURI__.core.invoke('send_message', { agentId: agent.id, prompt: "What's 2+2?" });
   ```
   Expect to see in the console: `turn_start` event → multiple `content_chunk` events with text → `turn_end` with `outcome: { status: 'completed' }` and a populated `usage` field. Each event payload's `turn_id` matches the one returned synchronously.
4. **Multi-project concurrency** — create a second project in the same directory:
   ```javascript
   const projB = await window.__TAURI__.core.invoke('create_project', { name: 'project-b' });
   await window.__TAURI__.core.invoke('set_active_project', { projectId: projB.id });
   const agentB = await window.__TAURI__.core.invoke('create_agent', { name: 'assistant' });   // same name as project-a's agent — succeeds
   const unlistenB = await window.__TAURI__.event.listen(`agent:${agentB.id}`, e => console.log('B:', e));

   // Dispatch to A's agent (still in flight) AND B's agent simultaneously
   const turnA = await window.__TAURI__.core.invoke('send_message', { agentId: agent.id, prompt: 'count to 5' });
   const turnB = await window.__TAURI__.core.invoke('send_message', { agentId: agentB.id, prompt: 'count to 3' });
   ```
   Both should stream concurrently; events arrive on the right per-agent channel; no cross-talk. Confirms the architecture supports multi-project even before the UI does.
5. **Concurrent dispatch refusal (same agent)** — call `send_message` twice fast for the same agent; the second returns the "agent is busy" error.
6. **Missing-binary path** — temporarily remove `claude` from PATH, restart the app, run `check_claude_binary` → `BinaryNotFound`. Restore PATH.
7. **`make check`** → exits 0.

Open a PR titled `M1.4: dispatcher + Tauri command surface`. Wait for human review.

---

## Sub-milestone M1.5 — Single-pane agent UI

### Goal & outcome

The first sub-milestone with a user-facing surface — M1.1–M1.4 are all backend / infrastructure with no UI. After this sub-milestone lands, the M1 acceptance flow works end-to-end:
- Launch app → no directory → "Open working directory" button → native folder picker.
- Folder selected → if it has no `.switchboard/`, prompt to initialize; if it has projects, list them and let the user pick one or create a new one.
- Project active → if no agents, "Create agent" button → name input (defaults to `assistant`) → creates the agent in the active project.
- Agent exists → single-pane view with output area on top, compose bar on bottom.
- Type "What's 2+2?" → press Send (or Cmd+Enter) → output streams in real time → "4" appears.
- App title bar shows `<project-name> — <directory-basename>` so the user always knows where they are.

**UX scope (M1 only, on purpose):**
- One bound working directory at a time. No multi-directory support.
- One **active project** displayed in the pane. The directory may have multiple projects (M1.2 supports this), but the M1 UI shows one project's pane at a time. **Project switcher UI lands in M3.**
- Default project name = directory basename when the user creates the first project in a fresh directory (e.g., directory `switchboard` → suggested project name `switchboard`). User can override.
- No "recent directories" list. No project-name auto-discovery from git remote, etc. These land in later milestones.

### Implementation outline

1. **Startup binary check.** On app startup, dispatch the `check_claude_binary` Tauri command (defined in M1.4). If it returns `BinaryNotFound`, render a top-of-app banner: "Claude Code not found on PATH. Install from <https://claude.com/code>." Banner persists across navigation. Project creation and agent creation are still allowed (so the user can configure things even without `claude` installed); `send_message` will fail until `claude` is installed and the user re-runs the check (or re-launches Switchboard).
2. **App routing.** Four states: no-directory (welcome screen), directory-bound-no-projects (create-project prompt), directory-bound-no-active-agent (project active, but no agents in it yet), and active-pane (project + agent active, single-pane view). Use a Svelte `$state` rune.
3. **Folder picker + project lifecycle flow.** Use Tauri's `@tauri-apps/plugin-dialog` (`open({ directory: true })`) to let the user pick a directory. Then call `pick_directory(path)` which returns `{ path: <canonical>, has_switchboard: bool, projects: ProjectSummary[] }`. Render based on the result:
   - **No `.switchboard/`** → "Initialize Switchboard in this directory and create a project named `<directory-basename>`?" CTA. On confirm: call `init_directory(path)` then `create_project(name=<basename>)` then `set_active_project(id)`.
   - **`.switchboard/` exists, projects empty** → "Create a project here?" CTA with project-name field defaulting to `<directory-basename>`. On confirm: `create_project(name)` then `set_active_project(id)`.
   - **`.switchboard/` exists, projects non-empty** → list the existing projects (each with its name + created_at) plus a "Create another project" option. Picking one calls `set_active_project(id)`; "Create another" reveals a name field and calls `create_project(name)` + `set_active_project(id)`.
   
   This makes the multi-project model visible from the first interaction without overwhelming the single-project case.
4. **Welcome screen.** Single CTA: "Open working directory."
5. **App title bar / location indicator.** Once a directory is bound and a project is active, render the title bar (or a top breadcrumb if title bar is awkward) as `<project-name> — <directory-basename>`. The user always knows where they are. Implementation note: Tauri lets you set the window title at runtime via `app.get_webview_window("main").set_title(...)` from Rust, or via the JS API.
6. **Create-agent prompt.** Single text field with default `"assistant"`, validates against the same regex used in M1.2, shows the duplicate-name error inline if rejected. Creates the agent in the active project.
7. **Single-pane view.**
   - Top: scrollable output area. Each `ContentChunk`'s `text` is appended in order. Scroll auto-pins to bottom unless the user has scrolled up. Each completed turn is visually separated from the next (a subtle divider).
   - Bottom: compose bar — multi-line textarea, Send button. Cmd+Enter submits.
   - Status indicator (small dot or label): "idle" / "processing" / "error".

#### NormalizedEvent TypeScript type

This must match the Rust `#[serde(tag = "type", rename_all = "snake_case")]` definition from M1.3. Hand-write it (or generate via `tauri-specta` if you adopt that — but for M1 hand-written is fine):

```typescript
type TurnId = string;  // UUID v7 (lowercased hyphenated string — same convention as AgentId, ProjectId)

type NormalizedEvent =
  | { type: "turn_start"; turn_id: TurnId; started_at: string /* ISO-8601 UTC */ }
  | { type: "content_chunk"; turn_id: TurnId; text: string }
  | { type: "turn_end"; turn_id: TurnId; outcome: TurnOutcome; ended_at: string };

type TurnOutcome =
  | { status: "completed" }
  | { status: "failed"; kind: FailureKind; message: string };
  // Future: | { status: "cancelled"; source: CancelSource } — added in M3 when per-turn cancel lands.

type FailureKind = "harness_error" | "adapter_failure";
  // Future: "timeout" — added if/when an active per-turn timeout lands.
```

`started_at` / `ended_at` are ISO-8601 strings (serde's default for `DateTime<Utc>`), not JS `Date` objects. Convert at the boundary if you need `Date`.

**The reducer's default branches are load-bearing for forward-compat in two places:** (a) for unknown `outcome.status` values (M3 will add `"cancelled"`) — fall through to a generic "turn ended in an unknown way" rendering and `console.warn`; (b) for unknown `kind` values within `Failed` (future variants like `"timeout"`) — fall through to displaying the `message` as-is, which is always present. The frontend should never crash on a future-vintage backend; it should degrade.

#### Reducer shape

The reducer is a pure function `(transcript: AgentTranscript, event: NormalizedEvent) => AgentTranscript` that drives the rendered view. Pin its types now — leaving them implicit risks redesign-mid-implementation:

```typescript
type Turn =
  | { id: TurnId; role: "user"; text: string; submittedAt: string }
  | {
      id: TurnId;
      role: "agent";
      text: string;            // accumulated ContentChunk text
      status: "streaming" | "complete" | "failed";
      error?: string;          // populated when status is "failed"
      startedAt: string;
      endedAt?: string;
    };

type AgentTranscript = { agentId: string; turns: Turn[] };
```

User turns are appended to `turns` synchronously at submit time. Agent turns are appended on `turn_start` and updated as `content_chunk` and `turn_end` events arrive.

8. **Event subscription.** Subscribe to **`agent:<id>`** (per-agent, not per-turn) when the AgentPane mounts. Subscription persists for the lifetime of the AgentPane — unsubscribe on unmount, not per turn. Each incoming event carries its own `turn_id`; the reducer applies the event to the matching turn in `transcript.turns` and silently ignores events whose `turn_id` doesn't match any known turn. (See M1.4 step 5 for why per-agent, not per-turn — the per-turn channel design has a TurnStart subscription race.) The reducer applies each event per the table:

   | Event | Reducer effect |
   |---|---|
   | `turn_start` | Append a new `agent`-role Turn with `status: "streaming"`, empty `text`, the timestamps from the event |
   | `content_chunk` | Append `text` to the streaming turn's `text` field |
   | `turn_end` (completed) | Set `status: "complete"`, set `endedAt` |
   | `turn_end` (failed) | Set `status: "failed"`, populate `error`, set `endedAt` |

9. **Send flow.** On Send: append the user's prompt as a `user`-role Turn synchronously, call `send_message` to get the `TurnId`, store it as the current in-flight turn id, set status to "processing." (No subscription action — the per-agent subscription was already established at mount time.) Lock the Send button until `turn_end` fires for this `turn_id`.
10. **Component structure (suggested).**
   - `AppShell.svelte` — root, manages app state, hosts the binary-not-found banner.
   - `WelcomeScreen.svelte` — no-project state.
   - `ProjectView.svelte` — project-open state, hosts agent UI.
   - `AgentPane.svelte` — output area + compose bar; owns the per-agent transcript.
   - `ComposeBar.svelte` — extracted for testability.
11. **Styling.** Tailwind utility classes; shadcn-svelte components for the button, dialog, textarea (using the versions pinned in M1.1).

### Testing strategy

Vitest + `@testing-library/svelte` are already set up from M1.1.

- **Reducer unit tests** (pure function, no DOM):
  - Empty transcript + (`turn_start`, `content_chunk`×N, `turn_end(completed)`) → one complete agent Turn whose `text` equals the concatenated chunk text.
  - Empty transcript + (`turn_start`, `content_chunk`×N, `turn_end(failed)` with message) → one failed Turn with `status: "failed"`, populated `error`, partial accumulated text preserved.
  - Multiple sequential turns concatenate correctly into the `turns` array; turn order matches arrival order.
  - **Late event from a prior turn** (different `turn_id`) is ignored — this is the load-bearing defense against cross-turn event leakage now that the subscription is per-agent rather than per-turn (see M1.4 step 5). Don't skip this test.
- **Component-level tests** for `ComposeBar`:
  - Cmd+Enter triggers submit
  - Empty / whitespace-only input doesn't submit
  - Disabled state when "processing"
- **End-to-end manual test** (the M1 acceptance flow). Document the steps in `README.md` under "Try it out."
- **Optional: WebDriver smoke test** via `tauri-driver`. If straightforward to set up, do it; if it adds significant complexity, defer to M2.

### Docs to update

- `README.md` — "Try it out" section with the M1 acceptance steps. Include a brief note about `SWITCHBOARD_HARNESS=mock` for development without `claude` installed (canned streaming responses; useful for iterating on UI without burning real-claude quota).
- A short user-facing note (could be in README or a separate `docs/getting-started.md`) — but only if the user asks for it. Don't create new docs unprompted per the global instructions.
- **`AGENTS.md`** — add the per-agent transcript reducer shape (TS types pinned in M1.5), the per-agent subscription model (subscribe on AgentPane mount, reducer filters by `turn_id`), the binary-not-found banner pattern (once-per-app-launch via `check_claude_binary` startup probe), and the wire-format ↔ TS-type mapping convention (`#[serde(tag = "type", rename_all = "snake_case")]` on Rust enums → discriminated union in TS).

### Manual smoke test

This is the closing manual test for the entire M1 acceptance flow — the moment of truth. Requires `claude` installed and authenticated for the real-harness path (steps below assume `SWITCHBOARD_HARNESS` unset / default). **For UI iteration during M1.5 development, use `SWITCHBOARD_HARNESS=mock make dev`** — the mock harness from M1.3 step 9 returns canned streaming text without needing real `claude`, lets you iterate on the UI without burning quota or requiring auth setup. Run the smoke test below against the real harness before merging. **If anything below fails, the PR isn't ready regardless of unit-test results.**

1. **`make test`** → exits 0; output includes new reducer + ComposeBar component tests.
2. **`make dev`** → app window opens within ~30s. No banner if `claude` is on PATH; if missing, see the binary-not-found banner with install link copy.
3. **Welcome screen** → "Open working directory" button visible.
4. **Folder picker** → click "Open working directory," native folder picker opens, pick a fresh empty directory (e.g., `/tmp/sw-smoke-1`).
5. **First-time-init CTA** → since the folder has no `.switchboard/`, you see "Initialize Switchboard in this directory and create a project named `sw-smoke-1`?" — confirm.
6. **Title bar** → reads something like `sw-smoke-1 — sw-smoke-1` (project name + directory basename; default project name was the directory's name).
7. **No-agents state** → app prompts "Create agent."
8. **Create agent** → name field defaults to `"assistant"`. Submit.
9. **Single-pane view appears** → output area on top (empty), compose bar on bottom, status indicator shows "idle."
10. **Send "What's 2+2?"** → press Cmd+Enter (and separately try the Send button).
    - User turn appears in transcript immediately (optimistic).
    - Status flips to "processing"; Send button disables.
    - Agent reply streams into the pane character-by-character ("4" or similar correct answer).
    - On TurnEnd, status returns to "idle"; Send button re-enables.
11. **Send a follow-up** → e.g., "What about times two?" → confirm the model recalls prior context (proves session resume works), reply streams.
12. **Reload the app** (close and `make dev` again) → re-open the same directory → see "Open this project?" CTA listing the existing project — confirm, agent is still there → send another message → still works (proves persistence end-to-end).
13. **Existing-directory flow with multiple projects** — close app, restart, pick the same `/tmp/sw-smoke-1` directory → see the existing project listed; alongside it, "Create another project" → enter a new name (e.g., `task-2`) → new project becomes active. Confirm: title bar updates; no agents in this new project; create one and send a message — runs independently of the first project's agent. Both projects' state coexist on disk under `<directory>/.switchboard/projects/`.
14. **Empty/whitespace prompt rejection** → try sending with empty input or only spaces → Send button stays disabled or backend rejects (depending on which gate fires first).
15. **Late-event filter** — open devtools console, watch the events on `agent:<id>` channel as you send a message. After TurnEnd fires, no further events should arrive on the channel (until you send the next message). The reducer's `turn_id` filter catches any stragglers; nothing should leak into the next turn's display.
16. **`make check`** → exits 0.

### M1 close-out

After M1.5 merges, M1 is done. Final check on a fresh clone:
- `git clone` to a brand-new directory.
- Follow the README "Local development" + "Try it out" steps from scratch.
- Hit every step 1–13 above. Anything that requires "see prior README" or "ask in chat" is a README bug — fix it.

Open a PR titled `M1.5: single-pane agent UI`. Wait for human review.

After merge, M1 is done. Run the acceptance test on a fresh checkout (clone the repo to a new directory, follow the README steps) before declaring victory.

---

## Notes for the implementing agent

- **Type hints / signatures.** Per global instructions, all function signatures (Rust + TypeScript) should be fully typed. Rust's type system enforces this; for TypeScript, use `strict: true` in `tsconfig.json` and don't reach for `any`.
- **No imports inside functions** unless absolutely necessary (per global instructions). For Rust this is rarely a temptation; for TypeScript, all imports live at the top of the module.
- **No commits.** Stage and prepare commits, but **do not commit** — the user commits manually.
- **No comments unless the why is non-obvious** (per CLAUDE.md). The code structure should be self-explanatory.
- **Stop after each sub-milestone.** Hand back to the user with: (1) what landed, (2) what tests pass, (3) any open questions or surprises that came up. Do not start the next sub-milestone until the user signals to proceed.
- **If a sub-milestone surfaces a question this plan didn't anticipate** — pause and ask. Don't pattern-match to "the spec probably says..." — the spec is a few hundred lines; check it.
