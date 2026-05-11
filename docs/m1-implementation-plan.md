# M1 implementation plan: Walking skeleton (Claude Code only)

> **Audience:** the AI coding agent implementing M1. Read this entire doc, plus the prerequisites listed below, **before writing any code**. Stop after each sub-milestone for human review.

## How to use this plan

1. Read these files first (in order):
   - `docs/system-design.md` — the canonical "what is Switchboard and why." Sections 3 (core concepts — agent name normalization), 4 (functional primitives — what we're orchestrating), 7 (user-facing model — agent contention; only the M1-relevant parts), 9 (harness integration — per-harness adapter trait, normalized event stream, Claude Code specifics), 10 (form factor — platform / tray notes; M1-irrelevant parts can be skimmed).
   - `docs/v1-plan.md` — the M1 section in particular, plus the "Critical path" preamble.
   - `docs/research/claude-code-headless.md` and `docs/research/claude-code-cli-observed.md` — these are the ground-truth references for the Claude Code CLI surface. The CLI's observed behavior (event types, `--session-id`, `--include-partial-messages`, exit codes, single-process model) is more authoritative than anything reconstructed from memory.
2. Resolve the **Open questions** below with the user before starting.
3. Implement sub-milestones M1.1 → M1.5 in order. Each sub-milestone is self-contained: code + tests + doc updates. Stop after each one, summarize what landed, and wait for human review before continuing.
4. Ask clarifying questions when uncertain. Do not invent behavior the spec is silent on — surface the gap.
5. Per `~/.claude/CLAUDE.md`: never remove or skip tests/functionality to get tests to pass; never commit on the user's behalf; never add Claude as author/co-author.

## Open questions to resolve before starting

These are decisions the plan currently leaves to the user. Confirm them before touching M1.1.

1. **Session ID strategy.** Recommend pre-generating a UUID v4 in Switchboard. **First, probe whether `claude -p --session-id <new-uuid> '...'` succeeds when the session does not yet exist** (i.e., is `--session-id` create-or-resume idempotent?). If yes — always pass `--session-id <uuid>`, no first-turn-vs-subsequent branch needed (best M1 simplification). If no — pass `--session-id <uuid>` on the first turn and `--resume <uuid>` thereafter, choosing based on the existence of `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` (ground truth from Claude's own data, **not** a registry field — that would violate the registry's append-only invariant). Confirm this is OK.
2. **Single-agent vs N-agent registry in M1.** The M1 acceptance only requires one agent (`assistant`). Recommend the registry persists *N* agents from day one (forward-compatible with M2's multi-agent UI), but the M1 UI exposes only one — no agent selector. Confirm.
3. **Working directory binding.** A project is a 1:1 binding to a working directory on disk. The `.switchboard/` folder lives directly inside that directory. Confirm: (a) `.switchboard/` should be created in the project root (not in `~/.switchboard/projects/<name>/`); (b) Switchboard does **not** add `.switchboard/` to `.gitignore` automatically — the user controls that.
4. **shadcn-svelte version pinning.** shadcn-svelte tracks Svelte 5 (uses runes). **Pin exact versions** of `shadcn-svelte` CLI, `bits-ui`, and any peer deps in `package.json` and the lockfile at the version current when M1.1 starts. Do not use `latest` — for an AI-agent-consumed plan, floating versions are a reproducibility hazard, and `bits-ui` peer-dep churn against Svelte 5 runes makes this concrete. Stack composition itself (Tauri 2.x + Svelte 5 + Tailwind + shadcn-svelte) is settled per `v1-plan.md` M1 scope.
5. **Streaming granularity.** Pass `--include-partial-messages` so the UI receives token-by-token deltas (matches the "see streaming text" UX in the M1 acceptance). Confirm.
6. **Project creation UX.** For M1, picking a project = a native folder-picker dialog. No project name field, no "recent projects" list (defer to M2). Confirm.
7. **CI scope.** Hygiene CI = GitHub Actions, single workflow file, runs `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, frontend `pnpm lint` and `pnpm test`. **Vitest + `@testing-library/svelte` are set up in M1.1 with one trivial smoke test** so `pnpm test` has something to run from day 1, and M1.5's component tests are pure additive work. macOS runner only for M1 (cross-platform builds are M7). Confirm.

If the user accepts the recommendations as-stated, proceed. If they push back on any, revise the plan section that depends on the answer **before** starting that sub-milestone.

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

If the implementing agent finds a "clearly minor" expansion of M1 scope tempting, **stop and ask**. The M1 walking skeleton's value is its smallness.

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
   - **Pin exact versions** for `shadcn-svelte`, `bits-ui`, and peers per OQ4 — no `^` or `~` ranges and no `"latest"` for these deps.
   - **Install and initialize `tauri-plugin-dialog`** (Rust: `tauri-plugin-dialog` crate; frontend: `@tauri-apps/plugin-dialog`). First used in M1.5's folder picker, but adding plugins mid-stream involves Cargo.toml + capabilities config + npm install — wire it now to avoid that yak-shave later.
   - The Tauri app's bundle identifier should be something like `com.switchboard.app`.
2. **Set up the Cargo workspace.** The Tauri Rust crate (e.g., `crates/app`) is one workspace member. Future M1.2/M1.3/M1.4 work will likely add `crates/core` (project model, registry, harness adapter) and may further split. For M1.1, just establish a workspace `Cargo.toml` so adding crates later is trivial.
3. **Add a trivial round-trip command.** A `ping(name: String) -> String` Tauri command and a Svelte component that calls it on mount and renders the response. This proves IPC end-to-end.
4. **Frontend test infra.** Set up Vitest + `@testing-library/svelte`. Write one trivial smoke test (e.g., the App component mounts without throwing). Wire `pnpm test` into CI from day 1. This makes M1.5's component tests pure additive work — no setup-plus-tests combo PR.
5. **Hygiene CI.** Create `.github/workflows/hygiene.yml`. macOS runner. Steps: checkout → setup Node + pnpm → setup Rust toolchain (stable) → `pnpm install` → `pnpm lint` → `pnpm test` → `cargo fmt --check` → `cargo clippy --all-targets --all-features -- -D warnings` → `cargo test --all-features`.
6. **README.md update.** Add a "Local development" section with the `cargo tauri dev` command and prerequisites (Node, pnpm, Rust toolchain, Xcode CLT for macOS).

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
- **Frontend smoke test** — Vitest + `@testing-library/svelte` is set up in this milestone (per implementation outline step 4). The smoke test confirms the App component mounts.
- **CI itself is the test of CI** — confirm the hygiene workflow runs green on the M1.1 PR before merging.

### Docs to update

- `README.md` — local dev section.
- No spec changes expected.

### Stop for review after M1.1

Open a PR titled `M1.1: Tauri shell + hygiene CI`. Wait for human review.

---

## Sub-milestone M1.2 — Project filesystem + agent registry

### Goal & outcome

Pure-Rust persistence layer for a Switchboard project — no UI, no harness yet. After this sub-milestone:
- A `Project` type can be created at a given working directory, scaffolding `<project>/.switchboard/{config.yaml, state/registry.jsonl}`.
- An existing project can be loaded by its path.
- `Agent` records can be appended to and listed from the registry.
- Agent name uniqueness (including hyphen↔underscore normalization per `system-design.md` §3 Primitive 1) is enforced at the registry layer.

### Implementation outline

1. **Crate layout.** Add `crates/core` to the workspace. This crate is pure Rust (no Tauri dependency); the Tauri crate depends on it. This separation lets us test the model layer without spinning up Tauri.
2. **`Project` type.** Roughly:
   ```rust
   pub struct Project {
       pub root: PathBuf,        // user-bound working directory
       pub config: ProjectConfig,
       pub registry_path: PathBuf, // <root>/.switchboard/state/registry.jsonl
   }
   ```
3. **`ProjectConfig`** (serialized to `<root>/.switchboard/config.yaml`). Start minimal — version field, anything else needed later. Use `serde_yaml`.
4. **Agent registry.** Append-only JSONL. One agent per line. Schema:
   ```rust
   #[derive(Serialize, Deserialize)]
   pub struct AgentRecord {
       pub id: AgentId,            // UUID v7 (time-ordered; via the uuid crate's v7 feature) — generated on create
       pub name: String,           // user-supplied, validated for uniqueness
       pub harness: HarnessKind,   // enum, M1 only ClaudeCode
       pub session_id: Uuid,       // Claude Code session UUID
       pub created_at: DateTime<Utc>,
   }
   ```
5. **Name normalization.** Per `system-design.md` §3 Primitive 1, agent names that differ only in hyphen vs. underscore are duplicates. Canonicalize by replacing `-` with `_` and lowercasing for the *uniqueness check only* — store the original name as the user typed it. Reject duplicates with a typed error.
6. **API surface.** On `Project`:
   - `Project::create(root: &Path) -> Result<Project>` — fails if `.switchboard/` already exists. Creates `state/` and an **empty `state/registry.jsonl`** (so `list_agents` always opens a real file — no missing-file branch in the read path).
   - `Project::open(root: &Path) -> Result<Project>` — fails if `.switchboard/config.yaml` doesn't exist.
   - `register_agent(&self, name: &str, harness: HarnessKind) -> Result<AgentRecord>` — generates IDs, validates name uniqueness, appends to registry.
   - `list_agents(&self) -> Result<Vec<AgentRecord>>` — reads the JSONL.
7. **Errors.** Use `thiserror`. Distinguish I/O errors, validation errors (bad name, duplicate), and corruption errors (malformed JSONL line).

### Testing strategy

Use `tempfile::TempDir` for isolation. All tests are unit tests in the `core` crate:

- **Roundtrip.** Create a project, register two agents, reopen, list — same data back.
- **Duplicate name (verbatim).** Registering `assistant` twice fails with the duplicate-name error.
- **Duplicate name (hyphen↔underscore).** `agent-a` then `agent_a` fails. `Agent_A` then `agent-a` fails. (Case-insensitive too if we go that route — confirm in the open-questions answers.)
- **Empty / whitespace-only name.** Rejected.
- **Reserved characters in name.** Per `system-design.md` §3, the spec is `^[A-Za-z0-9_-]+$` (no leading-character constraint). Test rejection of empty, whitespace, and characters outside the class. Test acceptance of digit-first, hyphen-first, and underscore-first names so the spec rule is enforced rather than accidentally tightened. If during implementation a stricter rule seems warranted (e.g., digit-first names create awkward template-variable identifiers in fan-in contexts), surface the proposal — don't silently tighten.
- **`Project::create` on a path that already has `.switchboard/`** fails cleanly.
- **`Project::open` on a path with no `.switchboard/`** fails cleanly.
- **Corrupted registry line** — append a malformed line to the JSONL by hand, then `list_agents` returns a typed error pointing at the bad line. (Don't silently skip — corruption should surface.)

### Docs to update

- New section in `system-design.md` is **not** needed; the persistence schema is already described at intent level there. If concrete schema details are useful for future readers, append them under the existing §10 or §3 sections; don't create a new top-level section.
- `docs/v1-plan.md`'s deferred-detail callout (line 243, "Persistence schema details (10.3)") — note that M1 lands the registry shape; the runs/checkpoint shape is still M5.

### Stop for review after M1.2

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
   - **If not idempotent** → pass `--session-id <uuid>` on the first turn and `--resume <uuid>` thereafter. Choose based on the existence of `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` (ground truth from Claude's own data). **Do not add a mutable `initialized: bool` field to `AgentRecord`** — that violates the registry's append-only invariant and drifts from ground truth (user deletes session file → registry stays stale).
   
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

### Docs to update

- `docs/research/claude-code-cli-observed.md` — if any of the capture/probe work in M1.3 reveals new behavior (especially around `--include-partial-messages` and `--input-format stream-json`), append a "Findings during M1.3" subsection.
- No spec changes expected unless something contradicts `system-design.md` §5 — surface that to the user immediately.

### Stop for review after M1.3

Open a PR titled `M1.3: normalized events + Claude Code adapter`. Wait for human review.

---

## Sub-milestone M1.4 — Dispatcher + Tauri command surface

### Goal & outcome

Wire the harness adapter into the Tauri app. After this sub-milestone:
- A `Dispatcher` type holds in-memory per-agent state and is the single entry point for sending a message to an agent.
- Tauri commands `create_project`, `open_project`, `list_agents`, `create_agent`, `send_message` are exposed.
- Streaming events from the adapter are forwarded to the frontend via Tauri events.
- Unit + integration tests cover the dispatcher (using the fake harness from M1.3).
- No UI yet — that's M1.5. Test by invoking commands from the Rust side or via the Tauri devtools console.

This sub-milestone establishes the chokepoint pattern that M3 will harden into the formal "single dispatcher" with contention enforcement. For M1, it's just the entry point shape — concurrency hardening is M3.

### Implementation outline

1. **`Dispatcher`.** Owns `HashMap<AgentId, AgentState>` behind a `tokio::sync::Mutex`. `AgentState` for M1 is just `{ status: Idle | InFlight }` — enough to refuse a `send_message` if the agent already has a turn in flight. **This is a minimal local guardrail, not the M3 chokepoint.** Return a generic "agent is busy" error string. M3 owns the actual error taxonomy (typed errors, structured contention reasons, UI gating treatment) — do not preemptively model those in M1.
2. **`EventEmitter` trait** (defined alongside the dispatcher in `crates/core`):
   ```rust
   pub trait EventEmitter: Send + Sync {
       fn emit(&self, name: &str, payload: serde_json::Value);
   }
   ```
   The Tauri-facing crate provides an `AppHandleEmitter` that wraps `tauri::AppHandle::emit`. Tests use a `RecordingEmitter` (`Mutex<Vec<(String, Value)>>`). The dispatcher takes `Arc<dyn EventEmitter>`. This makes the dispatcher fully unit-testable without spinning up Tauri.
3. **App state.** A `tauri::State<AppState>` that holds `Option<Project>` and the `Dispatcher`. For M1, only one project can be open at a time.
4. **Tauri commands.**
   ```rust
   #[tauri::command]
   async fn check_claude_binary() -> Result<(), String>;  // surfaces BinaryNotFound for the M1.5 banner

   #[tauri::command]
   async fn check_project_status(root: String) -> Result<ProjectStatus, String>;
   // ProjectStatus = NotAProject | AlreadyAProject — the M1.5 folder picker calls
   // this before deciding whether to offer create or open. Avoids errors-as-control-flow.

   #[tauri::command]
   async fn create_project(state: State<'_, AppState>, root: String) -> Result<ProjectInfo, String>;

   #[tauri::command]
   async fn open_project(state: State<'_, AppState>, root: String) -> Result<ProjectInfo, String>;

   #[tauri::command]
   async fn create_agent(state: State<'_, AppState>, name: String) -> Result<AgentRecord, String>;

   #[tauri::command]
   async fn list_agents(state: State<'_, AppState>) -> Result<Vec<AgentRecord>, String>;

   #[tauri::command]
   async fn send_message(state: State<'_, AppState>, agent_id: String, prompt: String) -> Result<TurnId, String>;
   ```
   Returning `String` for the error type is a Tauri convention; map `thiserror`-typed errors to `to_string()` at the boundary. **`send_message` returns the `TurnId` synchronously** — the dispatcher generates it before spawning the harness, lets the UI scope its event subscription to that turn (see step 5), and emits `TurnStart` immediately so the user sees "processing" the moment they hit Send.
5. **Turn lifecycle and event forwarding.**
   - On `send_message`: dispatcher generates a fresh `TurnId`, locks the agent state, checks `Idle`, transitions to `InFlight` (acquiring an `AgentIdleGuard` — see step 6), releases the lock, returns the `TurnId` to the caller, and spawns the dispatch task.
   - The dispatch task **immediately emits `TurnStart`** via the `EventEmitter` (before the harness subprocess even boots). Then it spawns the harness adapter via the `HarnessAdapter` trait, drains the resulting `EventStream` (typed as `AdapterEvent`), lifts each event into a `NormalizedEvent` via `From<AdapterEvent>`, and forwards via the emitter.
   - **Event name pattern: `agent:<agent_id>`** (per-agent — one channel for the lifetime of the agent, not per-turn). Each event payload carries its own `turn_id`. The M1.5 reducer subscribes once when the AgentPane mounts (not per turn) and filters events by the current `turn_id` to discriminate between turns.
   - **Why per-agent, not per-turn:** a per-turn channel name (`agent:<id>:turn:<turn_id>`) would require the frontend to subscribe AFTER receiving the `turn_id` from the IPC reply — but the dispatch task emits `TurnStart` concurrently, and the IPC reply and the event cross the WebView bridge in undefined order. If `TurnStart` arrives first, the listener doesn't exist yet and the event is silently dropped (the worst kind of bug — intermittent, environment-dependent). The per-agent channel eliminates the race because the listener exists before any event can fire. The reducer's `turn_id` filter is the load-bearing defense against cross-turn event leakage (see the M1.5 reducer test "late event from prior turn ignored").
   - **Backpressure: M1 emits each event naively, one Tauri event per `NormalizedEvent`.** This will not scale to M3's multi-pane fan-out (one fan-out turn × N agents × hundreds of token deltas). M3 expansion must address this — design space includes the §10 ring buffer, coalescing windows, rate limiting, or size caps. See the deferred-from-M1 callout in `v1-plan.md` M3.
6. **Two invariants the dispatcher must guarantee** (keep these mentally separate — they're independent guarantees with different owners):
   - **Dispatcher invariant — agent always returns to Idle.** Implementation: hold an `AgentIdleGuard` for the lifetime of the dispatch task. Its `Drop` impl flips state back to `Idle`. This holds even on panic, channel drop, or any other early termination path. (RAII pattern, like `tokio::sync::MutexGuard`.) **Owner: dispatcher.** Ensures backend state coherence.
   - **Stream contract — consumers always receive exactly one terminal event per turn.** **Owner: adapter** (per M1.3 step 7): if the subprocess dies without emitting `result`, the adapter synthesizes `TurnEnd(Failed)`. Ensures frontend stream coherence — the M1.5 reducer never has to handle "stream ended without TurnEnd" as a distinct case.
7. **AgentState lifecycle for crash recovery.** Out of scope for M1 — if Switchboard crashes mid-turn, the next launch starts with all agents `Idle` (registry doesn't track in-flight state). M5 introduces step-boundary checkpointing for workflow runs; per-agent crash recovery for individual turns is implicit in that work.

### Testing strategy

- **Dispatcher unit tests** (no Tauri — use `RecordingEmitter` for assertions).
  - `send_message` to an idle agent transitions to InFlight, runs the turn, transitions back to Idle.
  - `send_message` returns a `TurnId` synchronously; the dispatch task emits `TurnStart` (with that `TurnId`) on `agent:<id>` before any parser events arrive (assert via `RecordingEmitter`'s recorded sequence).
  - Concurrent `send_message` calls to the same agent: the second returns the busy error.
  - Concurrent `send_message` calls to *different* agents both run; their event streams don't cross-contaminate (assert by event name — each agent has its own `agent:<id>` channel).
  - A failed turn (fake harness emits an error fixture) leaves the agent back in Idle, not stuck in InFlight.
  - **Panic test:** a panicking dispatch task does not leave the agent stuck `InFlight` — `AgentIdleGuard`'s `Drop` impl restores state. Use a force-panic adapter to validate.
  - **Stream-contract test:** an adapter that ends its `AdapterEvent` stream without a terminal `AdapterEvent::TurnEnd` — the dispatcher's drain loop must observe exactly one `TurnEnd` per turn (the adapter, per M1.3 step 7, synthesizes `TurnEnd(Failed)` if the upstream subprocess dies silent). Catches regression if the adapter ever fails to do so.
- **`EventEmitter` testing.** Use `RecordingEmitter` to assert exact event sequences emitted per turn. Happy path: `turn_start` → `content_chunk`×N → `turn_end(completed)`. Failed path: `turn_start` → `turn_end(failed)`. All events on the per-agent name `agent:<id>`; payloads carry the `turn_id`.
- **Tauri command tests.** Tauri's testing story is limited. Each command is a thin shim around a free function that takes state explicitly; unit-test the free function. Don't try to test the `#[tauri::command]` wrapper itself.
- **End-to-end Tauri smoke** is deferred to M1.5 (manual verification: open devtools, listen, send a message, see events stream).

### Docs to update

- No new doc files. If the `Dispatcher` shape diverges meaningfully from `system-design.md` §7, surface that as a discussion before changing the spec.

### Stop for review after M1.4

Open a PR titled `M1.4: dispatcher + Tauri command surface`. Wait for human review.

---

## Sub-milestone M1.5 — Single-pane agent UI

### Goal & outcome

The actual user-facing walking skeleton. After this sub-milestone, the M1 acceptance flow works end-to-end:
- Launch app → no project → "Open project" button → native folder picker → if folder has no `.switchboard/`, prompt to create; if it does, open.
- Project open → if no agents, "Create agent" button → name input (defaults to `assistant`) → creates the agent.
- Agent exists → single-pane view with output area on top, compose bar on bottom.
- Type "What's 2+2?" → press Send (or Cmd+Enter) → output streams in real time → "4" appears.

### Implementation outline

1. **Startup binary check.** On app startup, dispatch the `check_claude_binary` Tauri command (defined in M1.4). If it returns `BinaryNotFound`, render a top-of-app banner: "Claude Code not found on PATH. Install from <https://claude.com/code>." Banner persists across navigation. Project creation and agent creation are still allowed (so the user can configure things even without `claude` installed); `send_message` will fail until `claude` is installed and the user re-runs the check (or re-launches Switchboard).
2. **App routing.** Three states: no-project (welcome screen), project-open-no-agents (create agent prompt), project-open-with-agent (single-pane view). Use a Svelte `$state` rune.
3. **Folder picker + create/open flow.** Use Tauri's `@tauri-apps/plugin-dialog` (`open({ directory: true })`) to let the user pick a folder. Once a folder is selected, **call `check_project_status(root)`** (M1.4 command) to determine whether the folder is already a Switchboard project. Render distinct CTAs based on the result:
   - `NotAProject` → "Create Switchboard project here?" CTA → calls `create_project(root)` on confirm.
   - `AlreadyAProject` → "Open this Switchboard project?" CTA → calls `open_project(root)` on confirm.
   
   This avoids using errors as control flow (the alternative — call `open_project` first and handle a `NotAProject` error — conflates "this isn't a project" with actual error states and produces confusing UX).
4. **Welcome screen.** Single CTA: "Open or create project."
5. **Create-agent prompt.** Single text field with default `"assistant"`, validates against the same regex used in M1.2, shows the duplicate-name error inline if rejected.
6. **Single-pane view.**
   - Top: scrollable output area. Each `ContentChunk`'s `text` is appended in order. Scroll auto-pins to bottom unless the user has scrolled up. Each completed turn is visually separated from the next (a subtle divider).
   - Bottom: compose bar — multi-line textarea, Send button. Cmd+Enter submits.
   - Status indicator (small dot or label): "idle" / "processing" / "error".

#### NormalizedEvent TypeScript type

This must match the Rust `#[serde(tag = "type", rename_all = "snake_case")]` definition from M1.3. Hand-write it (or generate via `tauri-specta` if you adopt that — but for M1 hand-written is fine):

```typescript
type TurnId = string;  // ULID or UUID v4

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

7. **Event subscription.** Subscribe to **`agent:<id>`** (per-agent, not per-turn) when the AgentPane mounts. Subscription persists for the lifetime of the AgentPane — unsubscribe on unmount, not per turn. Each incoming event carries its own `turn_id`; the reducer applies the event to the matching turn in `transcript.turns` and silently ignores events whose `turn_id` doesn't match any known turn. (See M1.4 step 5 for why per-agent, not per-turn — the per-turn channel design has a TurnStart subscription race.) The reducer applies each event per the table:

   | Event | Reducer effect |
   |---|---|
   | `turn_start` | Append a new `agent`-role Turn with `status: "streaming"`, empty `text`, the timestamps from the event |
   | `content_chunk` | Append `text` to the streaming turn's `text` field |
   | `turn_end` (completed) | Set `status: "complete"`, set `endedAt` |
   | `turn_end` (failed) | Set `status: "failed"`, populate `error`, set `endedAt` |

8. **Send flow.** On Send: append the user's prompt as a `user`-role Turn synchronously, call `send_message` to get the `TurnId`, store it as the current in-flight turn id, set status to "processing." (No subscription action — the per-agent subscription was already established at mount time.) Lock the Send button until `turn_end` fires for this `turn_id`.
9. **Component structure (suggested).**
   - `AppShell.svelte` — root, manages app state, hosts the binary-not-found banner.
   - `WelcomeScreen.svelte` — no-project state.
   - `ProjectView.svelte` — project-open state, hosts agent UI.
   - `AgentPane.svelte` — output area + compose bar; owns the per-agent transcript.
   - `ComposeBar.svelte` — extracted for testability.
10. **Styling.** Tailwind utility classes; shadcn-svelte components for the button, dialog, textarea (using the versions pinned in M1.1 per OQ4).

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

- `README.md` — "Try it out" section with the M1 acceptance steps.
- A short user-facing note (could be in README or a separate `docs/getting-started.md`) — but only if the user asks for it. Don't create new docs unprompted per the global instructions.

### Stop for review after M1.5

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
