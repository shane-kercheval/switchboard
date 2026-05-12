# M2 implementation plan: Both harnesses through the same abstraction

> **Audience:** the AI coding agent implementing M2. Read this entire doc, plus the prerequisites listed below, **before writing any code**. Stop after each sub-milestone for human review.

## How to use this plan

1. **M1 must be complete and merged before M2 starts.** This plan assumes M1's deliverables are in place: Tauri shell, `crates/core` with `Project`/`AgentRecord`/registry, `crates/harness` (or wherever it landed) with the `HarnessAdapter` trait + `ClaudeCodeAdapter`, dispatcher with `EventEmitter`, single-pane UI, hygiene CI. If you can't run the M1 acceptance flow on a fresh checkout, stop and fix M1 before starting M2.

2. **Read these files first** (in order):
   - `docs/v1-plan.md` — M2 section in particular, plus the "Critical path" preamble.
   - `docs/system-design.md` — sections 3 (core concepts), 4 (functional primitives), 7 (user-facing model — agent contention), 9 (harness integration — the per-harness adapter design and normalized event vocabulary; M2 is where this expands fully), 10 (form factor — only the M2-relevant bits).
   - `docs/research/codex-cli-observed.md` — **most M2-load-bearing file.** Comprehensive ground-truth on Codex CLI behavior: stream events, session-file format, cancellation, errors, two-process model, rate limits in session file, the stall-mitigation guidance, etc. Treat as authoritative.
   - `docs/research/codex-noninteractive.md` — docs-derived companion to the above; useful for context on what Codex says about itself vs what we observed.
   - `docs/research/harness-comparison.md` — cross-harness comparison that drove the per-harness adapter design.
   - `docs/research/claude-code-cli-observed.md` — still relevant: M2 expands the Claude Code adapter to emit the full event vocabulary (ToolStarted, ToolCompleted, RateLimitEvent, SessionMeta).
   - `docs/m1-implementation-plan.md` — to understand what's in place. M2 builds on M1's abstractions; don't redesign them, extend them.

3. **Resolve the Open Questions** below with the user before starting. Several have implementation-shaping consequences.

4. **Implement sub-milestones M2.1 → M2.7 in order.** Each is self-contained: code + tests + doc updates. Stop after each one, summarize what landed, wait for human review before continuing.

5. **Ask clarifying questions when uncertain.** Don't invent behavior the spec is silent on — surface the gap.

6. **Per `~/.claude/CLAUDE.md`:** never remove or skip tests/functionality to get tests to pass; never commit on the user's behalf; never add Claude as author/co-author; type-hint all functions in any TypeScript or Python; in Rust, prefer explicit signatures over inference for public APIs.

## Open questions to resolve before starting

These are decisions the plan currently leaves to the user. Confirm them before touching M2.1.

1. **Codex authentication — dev and CI.** Codex authenticates via `~/.codex/auth.json` (after interactive `codex login`) or `OPENAI_API_KEY` environment variable. Recommend: developers run `codex login` once on their machine; CI uses `OPENAI_API_KEY` from a GitHub Actions secret. Confirm this is the workflow and that you have access to a Codex-eligible account / API key for CI. (If Codex is using a ChatGPT-Plus / Pro subscription rather than API billing, the auth model differs — confirm.)

2. **Codex session-id storage strategy.** Per `system-design.md` §9 capability matrix, Codex assigns its own session_id from the first stream event (`thread.started` event carries `thread_id`). M1's `AgentRecord.session_id` is `Option<Uuid>` (per the M1 amendment for this OQ — see `m1-implementation-plan.md` M1.2): pre-populated for Claude Code agents at create time (used as `--session-id`), `None` for Codex agents. Where does Codex's harness-assigned session ID live?
   - **(c1) — recommended.** Codex agents get a session-link sidecar file at `<project>/.switchboard/state/sessions/<agent_id>.jsonl` — append-only, one record per dispatch, capturing `{ session_id: <thread_id>, original_start_date_utc: <YYYY-MM-DD>, started_at: <RFC3339> }`. Latest line wins for resume lookups. `original_start_date_utc` is set ONLY on the first dispatch (when Codex first assigns the session_id); subsequent resumes copy it from the prior record (see finding #3 about Codex's date-partitioned session-file path). **Claude Code agents do NOT use this sidecar** — their session_id stays on `AgentRecord`. The asymmetry mirrors the underlying harness asymmetry: Claude Code can pre-assign session_id (we exploit that); Codex can't (we react to it).
   - **(c2) — rejected.** Move BOTH harnesses to the sidecar "for symmetry." This would force M1's `AgentRecord` to drop `session_id` entirely, change M1.3's `--session-id <agent.session_id>` dispatch logic, and reshape M1.2's schema. Trying to hide the harness asymmetry forces avoidable M1 churn.
   
   Confirm (c1). The semantic interpretation of `AgentRecord.session_id` becomes: "the Claude session UUID (when this is a Claude Code agent); `None` for Codex" — clean and unambiguous.

3. **Process-group spawn — M2 vs cancellation in M3.** v1-plan M2 lists "Process-group spawn for both harnesses (`Command::process_group(0)`)" but actual cancellation is M3. Confirm: M2 wires up process-group spawn now (one line in each adapter), M3 implements `killpg` for the actual cancel. M2 also closes stdin after dispatch (stall mitigation per `codex-cli-observed.md` §"Stall hazard").

4. **Codex install method for CI.** Codex CLI isn't on Homebrew (verify). M2.1 will probe the actual install paths Codex officially supports — likely candidates are direct binary download from a published release and/or a package-manager install. Pick one in M2.1 based on what works on a clean GitHub Actions macOS runner. Pin a specific version for CI reproducibility.

5. **Integration test crate layout.** Where do integration tests live?
   - **(a)** Separate crate `crates/integration-tests/` — clean isolation, opt-in via cargo workspace.
   - **(b)** `crates/harness/tests/` (or `crates/core/tests/`) — using Cargo's tests-directory convention; tests live with the code they exercise.
   - **(c)** Tauri app's tests.
   
   **Recommend (b)** — Cargo's `tests/` directory is the canonical home for integration tests in Rust; tests stay close to the code. Confirm.

6. **Integration test cost budget per CI run.** Each test = one real LLM API call. With ~10 tests per run × $0.01–$0.05 per call (small prompts, cheap models), CI cost is roughly $0.10–$0.50 per run. Confirm: (a) acceptable budget; (b) tests should explicitly pin the cheapest available model (e.g., `claude-haiku-4-5` for Claude, GPT-5 mini equivalent for Codex if available); (c) every test prompt constrained to a small expected response per `system-design.md` §9 "Integration testing" guidance.

7. **CI gating for integration suite.** Run on every push to `main` and every PR? PRs from forks won't have secrets — fall back to "skip integration, run unit only" without failing. Recommend yes, every push + every PR with the fork-skip behavior. Confirm.

If the user accepts the recommendations as-stated, proceed. If they push back on any, revise the plan section that depends on the answer **before** starting that sub-milestone.

## Definition of done for M2 (as a whole)

The M2 acceptance from `docs/v1-plan.md`:

> Spawn one Claude Code agent and one Codex agent in the same project; switch between them via the agent selector; each streams correctly through the normalized event pipeline; per-agent metadata (tokens, context utilization) populates correctly for both. Spawning a second agent with a name that collides with the first after hyphen→underscore normalization (e.g., `agent-a` then `agent_a`) is rejected with a clear error. Integration test suite runs locally and in CI against installed harnesses with at least one test per normalized event type.

Do not consider M2 done until **all seven sub-milestones are merged** and this end-to-end flow works on a clean macOS checkout with both `claude` and `codex` installed and authenticated.

## What's deliberately out of scope for M2

These belong to later milestones — do not implement them, even if "easy":

- Per-turn cancellation (M3) — M2 wires up process-group spawn but does NOT add the cancel button or `killpg` call.
- Multi-pane UI (M3) — M2's UI adds the agent selector, but only one pane is visible at a time.
- Project-level `flock` for multi-instance protection (M3).
- Slash commands / prompt providers (M4).
- Workflows (M5).
- Workflow-level cancellation (M5).
- Pause-for-user / iteration (M6).
- First-launch acknowledgement dialog, tray, walk-away, signing, auto-updater (M7).
- Codex fork (deferred to v2+ per resolved 10.14 — Claude Code agents will get a Fork action in M3; Codex agents show an explanatory tooltip).
- Per-turn `total_cost_usd` for Codex (deferred — would require maintaining a per-model pricing table; M2 surfaces token counts only).

If the implementing agent finds a "clearly minor" expansion of M2 scope tempting, **stop and ask**. M2 is already substantial; don't grow it.

---

## Sub-milestone M2.1 — Codex CLI fixture capture + targeted probes

### Goal & outcome

Lock in Codex's actual CLI behavior with captured fixtures and probe the few remaining gaps before adapter code is written. Most Codex behavior is already documented in `codex-cli-observed.md`; this sub-milestone is small.

After this sub-milestone:
- Real `codex exec --json ...` output captured to `crates/<harness>/tests/fixtures/codex/` for each scenario the M2.3 parser needs (text-only turn, tool-using turn, errored turn, permission-denied turn).
- `codex-cli-observed.md` extended with M2.1 findings (a "Findings during M2.1" subsection) covering: exact `turn.failed` payload shape across error categories; permission-denial behavior (whether `permission_denials` analog exists); MCP tool calls (do they flow as `command_execution` items or a separate item type — open question called out in the existing research file).
- The Codex install method (OQ4) verified end-to-end: a fresh checkout / fresh GitHub Actions runner can install and run `codex --version`.

### Implementation outline

1. **Probe `codex exec --json` end-to-end** with the four scenarios listed above. Capture each as `<scenario>.jsonl` under `crates/<harness>/tests/fixtures/codex/`.
   - Trivial text-only: `codex exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox "Reply with the single word ack."`
   - Tool-using: a prompt that requires the model to run a shell command (e.g., "list the files in this directory using ls"). Captures the `command_execution` item flow.
   - Errored: `codex exec --json -m invalid-model-name ...` — captures the `error` event + `turn.failed` shape (already partially documented in research).
   - Permission-denied (best-effort): try a prompt that would trigger a permission denial under a stricter sandbox mode. With `--dangerously-bypass-approvals-and-sandbox`, denials may not fire — note the result either way.
2. **Probe MCP tool flow** (the existing research file flags this as still-to-probe). Configure a Codex MCP server, run a prompt that invokes an MCP tool, capture the stream. Confirm whether MCP tool calls flow as `command_execution` items or a distinct item type.
3. **Probe Codex resume + session-file behavior.** Run `codex exec ...` once and capture the session-file location. Then run `codex exec resume <id> ...` from a different day (or simulate via `date` shifting in a clean environment). Does Codex append to the original session file, or create a new file in the resume-day's directory? Capture before/after listings of `~/.codex/sessions/...`. **This is load-bearing for M2.4's date-partitioning lookup** — the session-link record must store the original spawn date, not the resume date (see M2.3 step 3).
4. **Probe non-interactive auth for both harnesses.** Verify that `claude` and `codex` honor environment-variable auth (`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`) for non-interactive use — i.e., that CI does NOT need an interactive `claude auth` / `codex login` step. The expected answer is yes, env-var alone is sufficient; document the exact mechanism and any quirks. **This blocks M2.7's CI design** — surface the answer here so M2.7 doesn't re-discover it.
5. **Verify Codex install method** (OQ4) — try the candidate installation path(s) on a clean environment (fresh VM, clean directory, or a GitHub Actions runner via a throwaway workflow). Document the chosen path in `codex-cli-observed.md` as a "Install path for CI" subsection. Pin a specific Codex version for CI reproducibility.
6. **Append findings to `docs/research/codex-cli-observed.md`** under a "## Findings during M2.1" subsection. Include the exact JSON shapes you captured (paste real lines, not paraphrased). If anything contradicts the existing research, flag the contradiction clearly so the rest of the M2 plan can be revisited.

### Testing strategy

This sub-milestone is research, not implementation — no Rust tests yet. The validation:
- Each fixture file is a real captured `.jsonl` from `codex exec --json`. Manually inspect them; confirm they contain the event types claimed.
- The install-path probe is "did `codex --version` print something on a fresh environment, yes/no."

### Docs to update

- `docs/research/codex-cli-observed.md` — append "Findings during M2.1" subsection per step 4 above.
- No spec changes expected. If the probe surfaces something that contradicts `system-design.md` §9 (e.g., Codex emits a previously-undocumented terminal event), flag it for discussion before changing the spec.

### How to review this PR

- Inspect the captured fixture files: `ls crates/<harness>/tests/fixtures/codex/`.
- Read the new "Findings during M2.1" subsection in `codex-cli-observed.md`.
- Optional: run one of the probe commands locally to spot-check.

### Stop for review after M2.1

Open a PR titled `M2.1: Codex CLI fixture capture + probes`. Wait for human review.

---

## Sub-milestone M2.2 — Process-group spawn + normalized event vocabulary expansion

### Goal & outcome

Expand the M1 vocabulary to the full M2 surface (still on Claude Code only — Codex comes in M2.3). Refactor the existing Claude Code adapter to use process-group spawn and stdin-close-after-dispatch (foundational for both harnesses).

After this sub-milestone:
- `AdapterEvent` and `NormalizedEvent` (M1.3 step 1) gain four new variants: `ToolStarted`, `ToolCompleted`, `RateLimitEvent`, `SessionMeta`. `From<AdapterEvent> for NormalizedEvent` covers them all.
- The Claude Code adapter parses and emits each new event type from its existing stream-json output (Claude Code's stream already carries tool_use / tool_result content blocks, system/init for SessionMeta, and rate_limit_event events).
- The Claude Code adapter spawns its subprocess in its own process group (`Command::process_group(0)`) and closes stdin after dispatch.
- M1.5's TS types are extended to include the new variants. The reducer's default branch (M1.5 testing #4) keeps the UI from crashing; the new events are accepted without yet rendering anything new (rendering polish is M3+).

### Implementation outline

1. **Extend `AdapterEvent` and `NormalizedEvent`** with the four new variants AND two field additions on existing variants. M2.2 is the "pay the wire-breaks once" milestone — bundling the field additions here means M3+ doesn't pay them later when the reducer has more logic to update.

   Four new variants (match `system-design.md` §9 shapes):
   ```rust
   AdapterEvent::ToolStarted { turn_id, tool_use_id, kind: ToolKind, name: String, input: serde_json::Value }
   AdapterEvent::ToolCompleted { turn_id, tool_use_id, output: String, is_error: bool }
   AdapterEvent::RateLimitEvent { agent_id, info: serde_json::Value }    // harness-specific shape, surfaced as Value
   AdapterEvent::SessionMeta { agent_id, model: String, harness_version: String, tools: Vec<String>,
                                mcp_servers: Vec<McpServerStatus>, skills: Vec<String>, raw: serde_json::Value }

   pub enum ToolKind { Builtin, Mcp, Plugin, Other }   // adjust based on what Claude Code actually surfaces
   ```
   Both enums get the new variants; `From<AdapterEvent> for NormalizedEvent` gets matching arms.

   Two field additions on existing variants (both wire-breaking — bundled here so M2 pays both costs in one place):

   ```rust
   // ContentChunk gains a `kind` field. Field name `text` stays the same (avoid renaming churn).
   ContentChunk { turn_id, kind: ContentKind, text: String }

   pub enum ContentKind {
       Text,        // user-facing assistant text (what M1 emitted)
       Thinking,    // model thinking blocks. M2 still does NOT emit this — Claude Code's thinking_delta
                    // continues to be ignored per M1.3 step 4; Codex thinking is encrypted. The variant
                    // exists in the wire vocabulary so a future v2 reasoning UI can surface it without
                    // a wire-break.
   }

   // TurnEnd gains an optional `usage` field. Critical for the M2 acceptance — per-agent token /
   // context-window metadata flows through here.
   TurnEnd { turn_id, outcome, ended_at, usage: Option<TurnUsage> }

   pub struct TurnUsage {
       pub input_tokens: u64,
       pub output_tokens: u64,
       pub cached_input_tokens: Option<u64>,    // Claude Code, Codex
       pub reasoning_output_tokens: Option<u64>, // Codex only
       pub context_window: Option<u32>,         // Claude Code: from result.modelUsage; Codex: from session-file enrichment in M2.4
   }
   ```

   Claude Code populates `usage` from `result.usage` + `result.modelUsage.<model>.contextWindow`. Codex populates `input_tokens` / `output_tokens` / `reasoning_output_tokens` / `cached_input_tokens` from `turn.completed.usage`; `context_window` arrives via session-file enrichment in M2.4 (so it's `Option`; first turn before enrichment will be `None`).

   **Note on agent_id vs turn_id:** SessionMeta and RateLimitEvent are agent-scoped (not turn-scoped) per system-design §9. ToolStarted/ToolCompleted/ContentChunk/TurnEnd are turn-scoped. Match the spec's field naming.

2. **Update Claude Code adapter parsing.** Per `claude-code-cli-observed.md`:
   - `system/init` event → emit `SessionMeta` (populate model, mcp_servers, tools, skills from the init payload).
   - `assistant` events with `tool_use` content blocks → emit `ToolStarted` (with `tool_use.id` as `tool_use_id`, `tool_use.name` as `name`, `tool_use.input` as `input`).
   - `user` events with `tool_result` content blocks → emit `ToolCompleted` (matching by `tool_use_id`; `is_error` from the tool_result).
   - `rate_limit_event` events → emit `RateLimitEvent` (pass payload as `info`).
   - `assistant` events with `text` content blocks → continue emitting `ContentChunk` (already handled in M1).
   - `result` event → continue emitting `TurnEnd` (already handled in M1, with the M1.3 step 7 failure-kind mapping).

3. **Process-group spawn.** Replace `Command::new("claude")...spawn()` with `Command::new("claude")...process_group(0).spawn()` (Tokio's `Command` exposes this on Unix). This puts the harness in its own process group, so M3's `killpg` will reach the whole tree. M2 doesn't add the `killpg` call — that's M3.

4. **Spawn with `stdin(Stdio::null())`.** Set `.stdin(Stdio::null())` on the `Command` builder before `spawn()`. Child reads from stdin return EOF immediately — there's no inherited terminal stdin to block on, no piped handle to forget to close. Apply the same pattern to both adapters (M2.2 for Claude Code, M2.3 for Codex). Harmless for Claude Code in positional-prompt mode (M1 wasn't reading stdin anyway); load-bearing for Codex per `codex-cli-observed.md` §"Stall hazard" — Codex's known regression where `--dangerously-bypass-approvals-and-sandbox` doesn't fully bypass means a directory-trust prompt can fire and try to read stdin; with `Stdio::null()`, the read returns EOF and Codex errors instead of hanging silently.

5. **Update M1.5 TS types** to include the new event types. The reducer's default branch (already in place per M1.5 testing) handles them gracefully — the UI doesn't yet render tool calls / rate limits / session-meta, but `console.warn` keeps it from crashing. Adding actual UI for these is M3+ scope; for M2.2 just confirm the events arrive at the frontend without breaking the reducer.

### Testing strategy

- **Wire-format roundtrip tests** extended to cover the four new variants AND the field additions on existing variants on both `AdapterEvent` and `NormalizedEvent`. Asserts the snake_case discriminator and field names match what M1.5 expects. Specifically: ContentChunk roundtrip preserves `kind`; TurnEnd roundtrip preserves `usage` (both `Some` and `None` cases).
- **No-double-emit + kind-field test:** existing parser tests assert all emitted ContentChunks carry `kind: Text` (Claude Code's `thinking_delta` continues to be ignored — guards against accidentally surfacing thinking blocks in M2 before there's UI for them).
- **Parser fixture tests** extended:
  - Fixture: turn that uses Read (a built-in tool) — assert the parser emits ToolStarted/ToolCompleted in addition to ContentChunk.
  - Fixture: turn that triggers a `rate_limit_event` — assert RateLimitEvent emitted (may need to capture this fixture deliberately).
  - Fixture: existing trivial turn — assert SessionMeta emitted on the first event (from system/init).
- **Process-group spawn test:** assert the spawned subprocess is in a different process group than the parent (`std::os::unix::process::CommandExt::process_group(0)` makes this happen; verify by checking `getpgid()` of the child differs from the parent's).
- **Stdin EOF test:** spawn the fake harness with a fixture that would block waiting for stdin input — assert it terminates cleanly (because stdin was closed) instead of hanging.

### Docs to update

- `docs/m1-implementation-plan.md` cross-reference — note in M1.3 step 1 that M2.2 expands the event vocabulary; the M1 minimum subset is intentional.
- No `system-design.md` changes expected — §9's vocabulary already specifies the full set.

### How to review this PR

- Run `cargo test -p switchboard-harness` (or wherever the adapter lives) — should see new ToolStarted/ToolCompleted/RateLimitEvent/SessionMeta tests pass.
- Run `SWITCHBOARD_LIVE_HARNESS=1 cargo test -- --ignored` — live integration test should still pass and now emits SessionMeta + tool events when the model uses tools.
- In `cargo tauri dev`, send "list the files in this directory" — open the WebView devtools console; should see `console.warn` lines for the new event types (the reducer's default branch logging them — UI rendering is M3+).

### Stop for review after M2.2

Open a PR titled `M2.2: process-group spawn + event vocabulary expansion`. Wait for human review.

---

## Sub-milestone M2.3 — Codex adapter implementation

### Goal & outcome

A working `CodexAdapter` that implements the same `HarnessAdapter` trait as `ClaudeCodeAdapter`. After this sub-milestone:
- Spawning a Codex agent and sending a message streams correctly through the normalized event pipeline (terminal `TurnEnd` fires, ContentChunks for the model's text).
- Codex's session-id-from-stream model is handled (per OQ2's resolution).
- The Codex adapter uses process-group spawn and closes stdin after dispatch.
- Codex-specific quirks (SIGTERM-exits-0, two-process model, error event ordering) are all handled per `codex-cli-observed.md` guidance.

This sub-milestone does NOT add session-file enrichment — that's M2.4. M2.3 emits ContentChunk and TurnEnd from the stream alone; ToolStarted/ToolCompleted from `command_execution` items; RateLimitEvent and SessionMeta land in M2.4 (since they live in the session file, not the stream).

### Implementation outline

1. **`CodexAdapter` struct** implementing `HarnessAdapter`. Lives in `crates/<harness>/`. Constructor performs `which::which("codex")` for the binary check, mirroring `ClaudeCodeAdapter`'s `BinaryNotFound` pattern.

2. **Command line construction:**
   ```
   codex exec --json \
     --skip-git-repo-check \
     --dangerously-bypass-approvals-and-sandbox \
     -C <project_root> \
     "<prompt>"
   ```
   For resume: `codex exec resume <session-id> --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox -C <project_root> "<prompt>"`. Note: resume is a subcommand under `exec`, not a flag.
   
   **Process-group spawn** + **stdin close after dispatch** per the M2.2 pattern.

3. **Session-id handling** (Codex only — per OQ2 c1 resolution; Claude Code continues to use `AgentRecord.session_id` per M1.3).
   - Before dispatch: look up the most-recent record from `<project>/.switchboard/state/sessions/<agent_id>.jsonl` (latest line wins, file is append-only).
   - If no record → first turn → spawn with `codex exec` (no resume).
   - If a record exists → spawn with `codex exec resume <session_id>`.
   - **On first stream event (`thread.started` with `thread_id`), append a new record to the session-link file *immediately*, before any other parsing or dispatch work that could fail.** This ensures we have a durable link to a Codex session that already exists, even if the process panics or EOFs immediately after. Record shape:
     ```json
     { "session_id": "<thread_id>", "original_start_date_utc": "YYYY-MM-DD", "started_at": "<RFC3339>" }
     ```
     `original_start_date_utc` is set to the **current UTC date ONLY on the first dispatch** (when there was no prior record). On subsequent resumes, **copy `original_start_date_utc` from the prior record** — never use `Utc::today()`. This is load-bearing for M2.4's date-partitioned session-file lookup: Codex sessions resumed days later still live in the original spawn date's directory.
   - **Duplicate records on resume** are explicitly allowed and intended: each new dispatch appends a new record (same `session_id`, same `original_start_date_utc`, fresh `started_at`). The file is append-only; latest line wins for resume lookups; the history is debugging-useful.

4. **Stream parsing.** Read stdout line-by-line; parse each JSON line; map per the table:

   | Codex stream event | Adapter action |
   |---|---|
   | `{type: "thread.started", thread_id: "..."}` | Capture thread_id; persist to session-link file (see step 3). Don't emit any normalized event for this. |
   | `{type: "turn.started"}` | Ignored (TurnStart is dispatcher-emitted). |
   | `{type: "item.completed", item: {type: "agent_message", text: "..."}}` | Emit `ContentChunk { text }`. |
   | `{type: "item.started", item: {type: "command_execution", ...}}` | Emit `ToolStarted` with `name: "command_execution"`, input = the command details. |
   | `{type: "item.completed", item: {type: "command_execution", aggregated_output, exit_code, ...}}` | Emit `ToolCompleted` with `output: aggregated_output`, `is_error: exit_code != 0`. |
   | `{type: "turn.completed", usage: {...}}` | Emit `TurnEnd { outcome: Completed }`. |
   | `{type: "error", message: "..."}` | Buffer the message; the next `turn.failed` will surface it. |
   | `{type: "turn.failed", error: {...}}` | Emit `TurnEnd { outcome: Failed { kind: HarnessError, message } }`. |
   | All other event types | Ignored in M2 (some land in M2.4 via the session file). |
   
   Per `codex-cli-observed.md`: the `error` event before `turn.failed` carries the same info; the adapter can rely solely on `turn.failed` for the terminal signal.

5. **Streaming granularity.** Per `codex-cli-observed.md`, Codex emits an `agent_message` `item.completed` when the message is **complete**, not as deltas — there's no token-by-token streaming in `codex exec --json`. So for Codex, ContentChunk fires once per turn (or once per `agent_message` if the model emits multiple). This is asymmetric with Claude Code (which emits hundreds of small chunks via `--include-partial-messages`). The reducer accumulates either way.

6. **Cancellation detection (foundational for M3).** Per `codex-cli-observed.md` §"Cancellation": Codex's parent exits 0 on SIGTERM, so exit code alone doesn't distinguish "killed" from "completed." For M2, the adapter just needs to handle "stdout EOF without a `turn.completed` or `turn.failed` event" — synthesize `TurnEnd { outcome: Failed { kind: AdapterFailure, message: "stream ended without terminal event" } }` per the M1.3 stream-contract rule.

   **Preserve buffered error messages on EOF.** If the parser saw an `error` event before EOF (per step 4 — Codex sometimes emits `error` then dies before `turn.failed`), the buffered error message is the most useful diagnostic the user has. Synthesize the AdapterFailure with the buffered message concatenated with `" (stream ended without turn.failed)"` rather than the generic "stream ended without terminal event." Don't drop diagnostic content in exactly the failure mode where users most need it.

7. **Subprocess lifecycle** — same pattern as M1.3 step 8: drain stdout (drives parser), drain stderr concurrently (log it), `await child.wait()` after the parser sees the terminal event.

8. **Register Codex with the dispatcher.** **Small AppState reshape:** AppState gains a second adapter handle. Use **named fields** (`claude_adapter: Arc<dyn HarnessAdapter>`, `codex_adapter: Arc<dyn HarnessAdapter>`) rather than `HashMap<HarnessKind, Arc<dyn HarnessAdapter>>` — only two harnesses in v1, named is simpler and lets the dispatcher route via a `match agent.harness { HarnessKind::ClaudeCode => &state.claude_adapter, HarnessKind::Codex => &state.codex_adapter }`. The `create_agent` flow accepts a harness type (M1 hardcoded ClaudeCode; M2 accepts both).

### Testing strategy

- **Fake-harness fixtures** for Codex: replay each captured fixture from M2.1 through the parser and assert the expected `AdapterEvent` sequence.
  - Trivial text turn: ContentChunk + TurnEnd(Completed).
  - Tool-using turn: ToolStarted + ToolCompleted + ContentChunk + TurnEnd(Completed).
  - Error turn (`-m invalid-model`): TurnEnd(Failed { kind: HarnessError, message }).
  - Permission-denied (if probed in M2.1 yields a fixture).
  - **Fixture-assertion discipline:** assert event types and structural shapes only — never specific UUIDs, timestamps, model names, or other capture-specific values. Those vary per capture and would cause flaky tests.
- **Error-buffer-preserved-on-EOF test:** fake harness fixture that emits an `error` event then EOFs (no `turn.failed`) → adapter synthesizes `TurnEnd(Failed{AdapterFailure})` whose message contains the original error text plus the "stream ended without turn.failed" suffix.
- **Live integration test** for Codex (env-var-gated, mirrors the Claude Code one): `SWITCHBOARD_LIVE_HARNESS=1 cargo test -- --ignored` runs `codex exec` for real, asserts a small response includes the expected text.
- **Session-id capture test:** spawn the fake harness, drain the stream, assert the session-link file gets a new line with the captured thread_id.
- **Resume test:** with a session-link file already populated, dispatch and assert the command-line uses `exec resume <id>` not `exec`.
- **Stream-contract test (Codex variant):** fake harness emits stdout EOF without `turn.completed`/`turn.failed` → adapter synthesizes `TurnEnd(Failed { kind: AdapterFailure, ... })`. Same invariant as Claude Code.
- **Two-adapter dispatcher test:** create one Claude Code agent and one Codex agent in the same registry, dispatch to each, assert events flow through both correctly without cross-talk.

### Docs to update

- `docs/research/codex-cli-observed.md` — if M2.3 implementation surfaces anything new (especially around the resume flow or session-id capture timing), append to the M2.1 findings section.
- No spec changes expected; system-design §9 already specifies the per-harness adapter shape and Codex's session-id-from-stream model.

### How to review this PR

- Run `cargo test -p switchboard-harness` for fixture-driven Codex parser tests + the dispatcher two-adapter test.
- Run `SWITCHBOARD_LIVE_HARNESS=1 cargo test -- --ignored` for the live Codex smoke test (requires `codex` installed and authenticated).
- In `cargo tauri dev`, manually create one Claude Code agent and one Codex agent in the same project (UI for switching between them is M2.5; for now use devtools to invoke `create_agent` with each harness type). Send a message to each via devtools console invoke.

### Stop for review after M2.3

Open a PR titled `M2.3: Codex adapter implementation`. Wait for human review.

---

## Sub-milestone M2.4 — Codex session-file enrichment

### Goal & outcome

The Codex adapter reads the session file after each turn's terminal event to fill in metadata the stream omits: `RateLimitEvent`, `SessionMeta` (with `model_context_window`), full reasoning blocks (encrypted; surfaced as opaque metadata), `task_complete` details. Per `system-design.md` §9 and resolved 10.15, this is a committed v1 dependency.

After this sub-milestone:
- A Codex agent's RateLimitEvent fires after each turn (using the rate-limit info from the session file's `token_count` event).
- A Codex agent's SessionMeta fires on first turn (using the session file's `session_meta` event for full info — model, base instructions presence, etc.).
- Per-turn context-window info flows into the metadata channel for Codex parity with Claude Code.

### Implementation outline

1. **Locate the session file.** Per `codex-cli-observed.md`: `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<session-uuid>.jsonl`. The session UUID and the original spawn date both come from the session-link record (M2.3 step 3): `session_id` and `original_start_date_utc`. Build the path using `original_start_date_utc`, **NOT** `Utc::today()` — Codex sessions resumed days after the original spawn still live in the original spawn date's directory.
   ```
   ~/.codex/sessions/<original_year>/<original_month>/<original_day>/rollout-*-<session_id>.jsonl
   ```
   Use a glob (only `<timestamp>` is unknown; the session UUID is unique within the day's directory). If multiple matches (shouldn't happen), pick the most recent. M2.1 step 3 verified Codex's actual resume behavior — confirm the implementation matches what was observed.

2. **Read trigger.** Read the session file when the stream emits `turn.completed` or `turn.failed`. The session file is updated synchronously by Codex; by the time the terminal stream event fires, the session file should be up-to-date for that turn. (If empirically this is wrong — the file is still being written when the stream signals done — add a small retry loop with a short backoff; document the finding.)

3. **Parse session-file events** for what we care about:
   - `session_meta` (line 1) → enrich the SessionMeta event we'll emit on first turn.
   - `event_msg` with `task_started` payload → contains `model_context_window`. Add to SessionMeta on first turn.
   - `event_msg` with `token_count` payload → contains `rate_limits`. Emit `RateLimitEvent` after each turn.
   - Other event types → ignored in M2 (full reasoning blocks etc. land later if/when we have UI for them).

4. **Emit ordering.** Per `system-design.md` §9: "Codex's source (session file) means the event arrives after the terminal event." Emit RateLimitEvent and (first-turn) SessionMeta AFTER the dispatcher has emitted TurnEnd. **This does NOT violate M1.3's stream contract.** The contract is **"exactly one TurnEnd per turn_id"** — that invariant is preserved (the per-turn TurnEnd already fired). RateLimitEvent and SessionMeta are *agent-scoped* events (carry `agent_id`, not `turn_id`) and can flow at any time on the per-agent channel. Stated more pithily: **TurnEnd is terminal for a turn, not terminal for the per-agent channel.** Frontend reducers may correlate by proximity if a "metadata for the just-completed turn" affordance is needed; M2's UI doesn't render either, so no correlation logic ships in M2.

5. **Lookup-strategy mechanics** for the date-partitioned path. If a session was started just before midnight UTC and the turn completes after midnight, the session file is in the previous day's directory. Adapter records the date at spawn time and uses that, not the current date. Document this gotcha.

### Testing strategy

- **Session-file parser tests** with captured session-file fixtures (capture during M2.1 alongside the stream fixtures).
  - Parse a `session_meta` line → produces the expected metadata fields.
  - Parse a `task_started` event → extracts `model_context_window` correctly.
  - Parse a `token_count` event with rate_limits → produces a RateLimitEvent.
  - Malformed line in the session file → typed error, doesn't crash the adapter.
- **End-to-end test:** run a real Codex turn (live integration, env-var-gated), assert that after `turn.completed` arrives in the stream, the adapter emits a RateLimitEvent and (on first turn) a SessionMeta with `model_context_window` populated.
- **Date-boundary test:** simulate the cross-midnight case by mocking the spawn-date. Assert the adapter looks in the spawn-date directory, not the current-date directory.
- **Session file not yet written test:** edge case where the file doesn't exist at the moment the terminal event fires. The adapter should retry briefly, then if still absent, log and emit a degraded SessionMeta/skip RateLimitEvent rather than crash.

### Docs to update

- `docs/research/codex-cli-observed.md` — confirm or revise the session-file-vs-stream timing assumption based on what you see in implementation. The current research note says "the session file is updated synchronously" — verify.

### How to review this PR

- Run `cargo test -p switchboard-harness` — session-file parser tests pass.
- Run `SWITCHBOARD_LIVE_HARNESS=1 cargo test -- --ignored` — live Codex test now sees RateLimitEvent + SessionMeta.
- In `cargo tauri dev`, send a message to a Codex agent — devtools console should show RateLimitEvent and SessionMeta arriving via the per-agent event channel.

### Stop for review after M2.4

Open a PR titled `M2.4: Codex session-file enrichment`. Wait for human review.

---

## Sub-milestone M2.5 — Agent selector UI

### Goal & outcome

Minimal UI for switching between agents in a project. After this sub-milestone:
- The single-pane view shows a selector listing all agents in the current project (with their harness type — Claude Code / Codex — visibly indicated).
- Selecting a different agent switches the pane to display that agent's transcript.
- Each agent's transcript is preserved across switches (you don't lose history when you switch away).
- Per-agent runtime metadata (model, tokens, context utilization, rate limits) is captured into project-level state and available for future UI surfacing (M2 doesn't yet render it; M3+ does).
- The `create_agent` flow accepts a harness type (currently hardcoded in M1).

This is **not multi-pane** — one pane visible at a time. Multi-pane is M3.

**This sub-milestone reshuffles M1.5's frontend ownership model** — moving transcripts and subscriptions from per-pane to project-level. This reshuffle is *required*, not avoidable: per-pane subscriptions would recreate M1.4's TurnStart race when switching agents (the same race we eliminated by going per-agent in the M1 round-2 review). Backend M1 abstractions (`HarnessAdapter`, dispatcher, registry, `EventEmitter` trait) are unchanged.

### Implementation outline

1. **Per-agent transcript state lives at the project level**, not per-component. The current M1.5 `AgentPane` owned the transcript locally — change that. Move transcripts up to a project-level Svelte `$state` map keyed by `agent_id`. Each agent's transcript persists across pane switches (the AgentPane unmounts and remounts with a different `agent_id` prop, but the transcript is read from project state).

2. **Per-agent runtime metadata also lives at the project level** — alongside transcripts but in a separate state map. Pin the shape now to prevent implementer-time improvisation:

   ```typescript
   type AgentRuntime = {
     agentId: string;
     meta?: {
       // populated from SessionMeta event (first turn for the agent)
       model: string;
       contextWindow?: number;          // Claude Code: from SessionMeta. Codex: from session-file enrichment in M2.4.
       tools: string[];
       mcpServers: { name: string; status: string }[];
       skills: string[];
       harnessVersion: string;
     };
     rateLimit?: unknown;               // harness-specific shape, surfaced opaque (UI rendering is M3+).
     lastUsage?: TurnUsage;             // captured from most-recent TurnEnd's `usage` field (added in M2.2).
                                        // LIVES HERE, not in transcript state — don't duplicate.
   };

   type AgentRuntimeMap = { [agentId: string]: AgentRuntime };
   ```

   **State separation:** transcript state holds `turns: Turn[]` per agent (the rendered conversation); runtime state holds `meta` / `rateLimit` / `lastUsage` per agent (operational metadata). They have different reducers wired to the same project-level event subscription. Transcript reducer handles `content_chunk` / `turn_end`'s outcome; runtime reducer handles `session_meta` / `rate_limit_event` and pulls `usage` off `turn_end`. Don't put `lastUsage` in both places — implementer should treat `runtime.lastUsage` as authoritative.

3. **Subscriptions persist for the lifetime of the project**, with explicit lifecycle:
   - **Creation**: when project state becomes active (project opened or created), iterate `list_agents()` and register a per-agent listener (`agent:<agent_id>`) for each. The listener routes events into both transcript and runtime reducers based on event type.
   - **Dynamic agent add**: after `create_agent` succeeds and returns the new `AgentRecord`, register the listener for that agent's id **immediately, before any send can occur**. Don't wait until next state refresh.
   - **Project change**: before swapping the current project (via `open_project` / `create_project` for a different project), atomically tear down all current per-agent listeners (`unlisten()` for each); then swap the current project; then iterate the new project's agent ids and listen. Prevents listener accumulation and cross-project event leaks.
   - **Project close**: tear down all listeners.
   
   Subscriptions are NOT tied to the AgentPane mounting (since switching agents would tear down and recreate them otherwise, recreating the M1.4 race in a different form).

4. **Agent selector component.** Use shadcn-svelte's Select / Dropdown (or simple list — pick the lighter option). Lists all agents from `list_agents()`. Each entry shows `name` + a small badge for harness type (`Claude Code` / `Codex`). Selecting an entry sets the "active agent id" in app state; the AgentPane component re-mounts (or just rerenders) with the new `agent_id`.

5. **Create-agent prompt** (extending M1.5) gets a harness-type chooser — radio buttons or a dropdown with "Claude Code" / "Codex". Pre-fills with whichever the user picked last (or Claude Code if first time). Validates that the binary for the chosen harness is available before creating the agent (call `check_claude_binary` or new `check_codex_binary` Tauri command); rejects creation with a clear inline error if the chosen harness's binary isn't installed.

6. **`check_codex_binary` Tauri command** — mirrors `check_claude_binary` from M1.4. Backend returns `BinaryNotFound` if `which::which("codex")` fails. The startup banner from M1.5 step 1 now checks both binaries independently — show **per-harness banners** ("Claude Code not found on PATH; Claude Code agents will be unavailable until you install it" / "Codex not found on PATH; Codex agents will be unavailable until you install it"). Allow agent creation for any installed harness; the create-agent dialog disables the harness chooser entry that's not installed and shows a small inline note.

### Testing strategy

- **Component-level test for the selector:** mounts with a list of three agents (one Claude Code, two Codex); selecting each triggers the expected `active_agent_id` change.
- **Transcript reducer test:** events arriving on `agent:<id_a>` route into transcript A; events on `agent:<id_b>` route into transcript B; switching the active agent doesn't drop or duplicate events.
- **Runtime reducer test:** SessionMeta arrival populates `runtime.meta`; RateLimitEvent populates `runtime.rateLimit`; TurnEnd's `usage` updates `runtime.lastUsage`; events for unknown `agent_id`s are silently dropped.
- **Subscription persistence test:** mount the app with two agents; subscribe; unmount/remount the AgentPane; assert events continue to be captured into transcripts and runtimes (subscriptions are project-level, not pane-level).
- **Dynamic agent add test:** open a project with one agent; create a second agent via `create_agent`; immediately dispatch to the new agent; assert events arrive correctly (no missed first event).
- **Project swap test:** open project A (with agents); switch to project B (with different agents); assert no listeners remain registered for project A's agent ids; assert project B's agent ids are subscribed; events emitted on project A's channels do not leak into project B's state.
- **Banner UX test:** mount the app with `claude` installed but not `codex`; assert per-harness banner shows for Codex only; the create-agent dialog disables the Codex harness chooser entry.
- **End-to-end manual test:** create one Claude Code agent and one Codex agent in the same project, send messages to each (via devtools first, then via the UI once the selector is wired), switch between them, confirm transcripts persist and update correctly. Send several turns to one agent and verify `runtime.lastUsage` updates after each turn.

### Docs to update

- `README.md` "Try it out" — extend M1.5's flow to include creating a Codex agent + switching between agents.
- M1.5 binary-not-found banner copy may need updating to handle two binaries gracefully.

### How to review this PR

- Run `cargo tauri dev`; create a Claude Code agent and a Codex agent in the same project; switch between them via the selector; send a message to each; verify both stream and transcripts persist when switching.
- Run `pnpm test` for the component tests.

### Stop for review after M2.5

Open a PR titled `M2.5: agent selector UI`. Wait for human review.

---

## Sub-milestone M2.6 — Integration test suite scaffolding

### Goal & outcome

A real integration test suite that exercises both adapters against installed `claude` and `codex` CLIs. After this sub-milestone:
- Tests live in `crates/<harness>/tests/` (per OQ5's resolution).
- Each test is gated behind `#[ignore]` + an env-var check (`SWITCHBOARD_LIVE_HARNESS=1`) so they don't run in unit-test passes.
- Coverage: at least one test per normalized event type per harness (TurnStart, ContentChunk, ToolStarted, ToolCompleted, TurnEnd-Completed, TurnEnd-Failed-HarnessError, TurnEnd-Failed-AdapterFailure, RateLimitEvent, SessionMeta).
- Every test prompt is constrained to a small expected response per `system-design.md` §9.
- Tests use the cheapest available model (per OQ6).

This sub-milestone does NOT add CI — that's M2.7.

### Implementation outline

1. **Test layout.** Per OQ5's resolution, integration tests in `crates/<harness>/tests/integration/`. Use Rust's standard convention: each `.rs` file is a separate test binary.

2. **Test helper module** — a shared `mod common` (in `tests/common/mod.rs`) that provides:
   - `gated()` — checks env var, returns early unless `SWITCHBOARD_LIVE_HARNESS=1` is set.
   - `claude_adapter()` / `codex_adapter()` — constructs the adapter with default test config (cheapest model, etc.).
   - `tempdir_project()` — creates a tempdir-scoped Project for the test.
   - Utility to drain an EventStream into a Vec<AdapterEvent> with a timeout.

3. **Test files (one per adapter-emitted event type per harness).** Each test:
   - Skips if env var not set.
   - Constructs the adapter for one harness.
   - Sends a small prompt deliberately chosen to exercise the target event type.
   - Drains the stream.
   - Asserts the expected event appears at least once.
   
   Specific adapter-level tests:
   - `claude_text.rs` / `codex_text.rs` — "reply with ack" → ContentChunk + TurnEnd(Completed) with `usage` populated.
   - `claude_tool.rs` / `codex_tool.rs` — "list this directory using ls" → ToolStarted + ToolCompleted.
   - `claude_error.rs` / `codex_error.rs` — invalid model → TurnEnd(Failed { kind: HarnessError }).
   - `claude_session_meta.rs` / `codex_session_meta.rs` — first turn → SessionMeta.
   - `claude_rate_limit.rs` / `codex_rate_limit.rs` — any successful turn → RateLimitEvent (Claude Code emits in stream; Codex from session file).
   - Adapter-failure cases — harder to test against a real harness; cover with the existing fake-harness unit tests, not integration. Note this in the test file.
   
   **TurnStart belongs to a dispatcher-level integration test, not adapter-level** (TurnStart is dispatcher-emitted per M1.4; adapters never emit it). Add ONE separate test, e.g., `dispatcher_smoke.rs`: constructs the dispatcher with a `RecordingEmitter`, dispatches a real turn through it (Claude Code is fine — adapter-agnostic at the dispatcher layer), and asserts the full event sequence including TurnStart fires on the per-agent channel.

4. **Cost discipline:** every test uses the cheapest model (Claude haiku-class, Codex equivalent). Every prompt is "reply with ack" or similar one-token-output. Aim for ~$0.01 per test, ~$0.10–$0.20 per full suite run.

5. **Documentation.** A `tests/README.md` explaining how to run the integration suite locally:
   ```
   # Set up auth (one-time)
   claude auth   # for Claude Code
   codex login   # for Codex
   
   # Run integration suite
   SWITCHBOARD_LIVE_HARNESS=1 cargo test -- --ignored
   ```
   Plus a note about expected cost per run.

### Testing strategy

This sub-milestone IS the testing strategy for the adapters. The "tests of the tests" are essentially: do they run, do they assert the right things, do they actually catch regressions?

- Manually break the Claude Code adapter (e.g., make it ignore `system/init`) and confirm the SessionMeta integration test fails. Restore.
- Manually break the Codex adapter (e.g., skip session-file enrichment) and confirm the RateLimitEvent test fails. Restore.
- This is the "validate the test catches the regression" step. It doesn't need to be automated; just do it once during M2.6 to gain confidence.

### Docs to update

- New `tests/README.md` (or `crates/<harness>/tests/README.md`) per step 5 above.
- Top-level `README.md` "Local development" section — add a brief note pointing at the integration test README.

### How to review this PR

- `cargo test` (no env var) → all unit tests pass; integration tests are reported as "ignored."
- `SWITCHBOARD_LIVE_HARNESS=1 cargo test -- --ignored` → all integration tests pass against installed `claude` and `codex`.

### Stop for review after M2.6

Open a PR titled `M2.6: integration test suite scaffolding`. Wait for human review.

---

## Sub-milestone M2.7 — Integration CI workflow

### Goal & outcome

GitHub Actions workflow that runs the integration suite on every push and PR (with secrets), with graceful fallback for fork PRs (no secrets → unit tests only). After this sub-milestone:
- `.github/workflows/integration.yml` exists and runs on `push` and `pull_request`.
- For PRs from collaborators (have access to secrets): installs `claude` and `codex`, runs the integration suite.
- For PRs from forks (no secrets): the workflow exits cleanly without running integration tests; hygiene CI (M1) still runs and blocks merge as usual.
- Integration test costs land on the Switchboard maintainer's API accounts (per OQ6 budget).

### Implementation outline

1. **Workflow file:** `.github/workflows/integration.yml`. macOS runner.

2. **Trigger:** `on: { push: { branches: [main] }, pull_request: {} }`.

3. **Secret-availability gate.** Use a step that checks if the required secrets are present:
   ```yaml
   - name: Check secrets availability
     id: secrets
     env:
       ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
       OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
     run: |
       if [ -z "$ANTHROPIC_API_KEY" ] || [ -z "$OPENAI_API_KEY" ]; then
         echo "available=false" >> $GITHUB_OUTPUT
         echo "::notice::Skipping integration tests — secrets unavailable (likely a fork PR)."
       else
         echo "available=true" >> $GITHUB_OUTPUT
       fi
   ```
   All subsequent integration steps gate on `if: steps.secrets.outputs.available == 'true'`.

4. **Install steps** (only run if secrets available):
   - Setup Node + pnpm + Rust (same as hygiene CI).
   - Install `claude` (per the official install path verified in M2.1).
   - Install `codex` (per OQ4's resolution from M2.1).
   - **Auth via environment variables only** — `ANTHROPIC_API_KEY` and `OPENAI_API_KEY` set from GitHub secrets. M2.1 step 4 verified that both CLIs honor env-var auth for non-interactive use; no `claude auth` / `codex login` interactive step is needed in CI. (If M2.1's probe surfaced any quirks — e.g., one CLI requires an additional config file — apply that finding here.)

5. **Run the suite:** `SWITCHBOARD_LIVE_HARNESS=1 cargo test -- --ignored`. Fails the workflow on any test failure.

6. **Cost surfacing.** Use **actual logged usage** (sum input/output tokens captured from each test's TurnEnd `usage` field — added in M2.2) and apply per-model pricing to compute the real run cost. A final workflow step prints `# tests run`, `total input tokens`, `total output tokens`, `estimated cost USD` as a workflow summary. (Static hardcoded estimates would lie after model price changes; computing from real usage tracks reality.)

### Testing strategy

This is operational config — primary validation is "does it run end-to-end in CI."

- Open a draft PR after M2.7 lands; verify the workflow fires.
- Verify a fork-style PR (e.g., from a personal fork without secrets access) skips integration cleanly without failing the check.
- Verify the workflow passes when all integration tests pass.
- Manually break a test (in a throwaway branch) and confirm CI catches it.

### Docs to update

- `README.md` "Local development" — note that integration CI runs on PRs and that fork PRs will only see hygiene CI (collaborators see both).
- The `tests/README.md` from M2.6 — add a line pointing at the CI workflow file.

### How to review this PR

- Inspect `.github/workflows/integration.yml` for clarity and the secret-availability gate.
- Trigger the workflow by pushing the M2.7 PR; verify it runs end-to-end.
- Check the GitHub Actions logs for the "secrets available" step output and the test results.

### Stop for review after M2.7

Open a PR titled `M2.7: integration CI workflow`. Wait for human review.

After merge, M2 is done. Run the full M2 acceptance flow on a fresh checkout (clone, install both harnesses, follow the README):
1. Create a project.
2. Create a Claude Code agent named `assistant` and a Codex agent named `codex-helper` (or similar).
3. Confirm the hyphen↔underscore collision check works: try to also create `codex_helper` → rejected.
4. Send a message to each agent via the selector; both stream correctly.
5. Confirm per-agent metadata (token usage, context utilization) populates for both.
6. Open the integration CI workflow run for the M2.7 merge — should be green.

---

## Notes for the implementing agent

- **Type hints / signatures.** All function signatures (Rust + TypeScript) fully typed; in TypeScript keep `strict: true`; don't reach for `any`.
- **No imports inside functions** unless absolutely necessary (per global instructions).
- **No commits.** Stage and prepare commits, but **do not commit** — the user commits manually.
- **No comments unless the why is non-obvious** (per CLAUDE.md). Code structure should be self-explanatory.
- **Stop after each sub-milestone.** Hand back to the user with: (1) what landed, (2) what tests pass, (3) any open questions or surprises that came up. Do not start the next sub-milestone until the user signals to proceed.
- **If a sub-milestone surfaces a question this plan didn't anticipate** — pause and ask. Don't pattern-match to "the spec probably says..." — the spec is a few hundred lines; check it.
- **M1 backend abstractions are stable; extend, don't redesign.** If you find yourself wanting to change `HarnessAdapter`, `AdapterEvent`, the dispatcher, the `EventEmitter` trait, or the M1 registry layout — stop and ask first. Those got two review rounds; large changes deserve another. Frontend ownership (state shape, subscription lifecycle) is a different story: M2.5 *requires* a frontend reshuffle for correct multi-agent semantics, and M3+ will reshape it again for multi-pane. That's expected. The "extend, don't redesign" rule scopes to backend.
