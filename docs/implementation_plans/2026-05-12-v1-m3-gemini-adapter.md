# M3 — Gemini CLI as third harness

**Branch:** `m3` (off `main` post-M2).
**Status:** M3.1 complete (probing + fixture capture). M3.2 onward unstarted.

## How to read this plan

1. Read [`2026-05-12-v1.md`](./2026-05-12-v1.md) §M3 first — the milestone's role in the v1 roadmap and the "abstraction load-bearing test" rationale for inserting it here.
2. Then read [`docs/research/gemini-cli-observed.md`](../research/gemini-cli-observed.md) **end-to-end**, especially the "Findings during M3.1 (2026-05-17)" appendix. The findings supersede the pre-probe planning body above them. **Every CORRECTED / NEW heading in that appendix is load-bearing for this plan.** If a planning-doc claim and a findings-appendix claim conflict, the findings are correct.
3. Read [`AGENTS.md`](../../AGENTS.md) — project conventions, test-type vocabulary, live-test policy.
4. Read the M2 plan ([`2026-05-12-v1-m2-agent-adaptors.md`](./2026-05-12-v1-m2-agent-adaptors.md)) for the per-harness adapter pattern, sidecar/non-sidecar choices, and the M2.7 live-harness test scaffolding that M3 extends.
5. Implement sub-milestones M3.2 → M3.5 in order. Each is self-contained: code + tests + doc updates. **Commit after each sub-milestone**; user reviews, then signals to proceed. **Single PR** at the end.

Source-of-truth crate documentation lives in the source. This plan is "what to build and why"; the *how* lives in the existing M2 patterns the new code mirrors.

## Critical premise

**M2 established the per-harness adapter abstraction.** M3 is the load-bearing test that the abstraction is genuinely harness-neutral — not "Claude-shaped" or "Codex-shaped." From the M3.1 findings, **M2's abstraction holds**: no new `AdapterEvent` / `NormalizedEvent` / `HarnessAdapter` variants are required. All Gemini stream events map onto the existing vocabulary. The reserved `ContentKind::Thinking` variant (added defensively in M2.2) gets its first concrete consumer here.

If implementation requires non-additive changes to `AdapterEvent` / `NormalizedEvent` / `HarnessAdapter` / `FailureKind`, **or** harness-specific branches in `Dispatcher` / `EventEmitter` / `AppState` (beyond the per-harness adapter slot), **stop and escalate** as a milestone-level review point. The dispatcher staying harness-agnostic is just as load-bearing as the wire-format types staying additive — both are the M2 abstraction. Capture what surfaced for future harness additions.

## Resolved design decisions

These were resolved during M3.1 and are not up for relitigation during M3.2+. Each is documented in the research doc's findings appendix with the empirical evidence; the rationale must survive into the M3.2 adapter module's docstring.

1. **No Codex-style sidecar.** Gemini supports `--session-id <uuid>` (caller-controlled). The adapter follows the **Claude Code pattern**: pre-generate a UUID at agent registration, pass on every dispatch (`--session-id` first turn, `--resume <uuid>` subsequent turns). Detect first-vs-subsequent by checking session-file existence at the expected path.

2. **Gemini session IDs are UUID v4, not UUID v7.** Switchboard mints UUID v7 everywhere else (time-ordered). Gemini's session-file naming uses the first 8 hex chars of the session ID as a filename suffix; two UUID v7s minted in the same millisecond share their first 8 chars, causing on-disk session-file interleave (verified empirically — see findings doc, "CRITICAL — NEW" section). Localized fix: `GeminiAdapter` generates UUID v4 for session IDs only. `AgentId` / `TurnId` / `ProjectId` remain UUID v7 elsewhere.

3. **Filter Gemini-internal builtin tool events.** Gemini auto-fires an `update_topic` tool on most non-trivial headless turns to manage its own internal topic context. For v1, the adapter does **not** emit `ToolStarted` / `ToolCompleted` for any name in a shared `GEMINI_INTERNAL_TOOL_NAMES: &[&str]` deny-list (currently `&["update_topic"]`). Rationale: these tools carry no information the user needs (model's own conversation-state metadata, not project state); surfacing them on every turn pollutes the unified transcript. The constant is consumed by both the live adapter parser and the hydrator (M3.3), so a future addition lands in lockstep across both surfaces.

   **Implication for live tests** (per decision #5 below): the bifurcation is **live tests assert tool lifecycle only; sentinel-in-output assertion lives in the hydration test** (where the session file carries the content). See decision #5.

4. **Pass `--skip-trust` on every spawn.** Gemini's workspace-trust gate blocks headless dispatches by default with exit 0, empty stdout, and an error on stderr. Switchboard's bound cwd is by definition the user's working directory, which is what the gate is asking about; the adapter asserts this unconditionally via `--skip-trust`.

5. **Live tool output is empty for read-like tools; hydration is authoritative.** Gemini's stream `tool_result.output` is empty for `read_file` (and likely other user-data tools) even on success. The session file's `toolCalls[].result[].functionResponse.response.output` carries the full content. Live UI shows the tool *lifecycle* (`ToolStarted` / `ToolCompleted { is_error: false, output: "" }`); transcript hydration via `load_gemini_transcript` surfaces the real output. This matches the "live = best-effort, hydration = authoritative" pattern Switchboard already uses for Codex's `context_window` enrichment.

   **UX implication for M3.4**: `UnifiedTranscript.svelte`'s tool-item renderer suppresses the `output` body when `output === ""` — *general rule, not Gemini-specific*. The tool lifecycle (started → completed badge) still renders; the body just collapses when empty. On project reopen, hydration fills the body in from the session file. This avoids "the live view shows nothing then the reopened view shows content" looking like a regression — both views show *something coherent*, with hydration adding detail.

   **Implication for live tests** (M3.5): Gemini's live tool-use test asserts **lifecycle only** — `ToolStarted` + matching `ToolCompleted` with `is_error: false`, **not** sentinel-in-output. The sentinel-in-output assertion moves to the transcript-hydration test (where the session file does carry it).

6. **No new wire-format variants required.** Gemini stream events map onto the existing `AdapterEvent` vocabulary:

   | Gemini stream | Switchboard `AdapterEvent` |
   |---|---|
   | `init` | `SessionMeta { model, harness_version: "<gemini-cli-version>" or "", tools: [], mcp_servers: [], skills: [], ... }` |
   | `message` role=user | **Ignore** (echo of prompt). |
   | `message` role=assistant `delta:true` | `ContentChunk { kind: Text, text: content }` |
   | `tool_use` (non-`update_topic`) | `ToolStarted { tool_use_id: tool_id, kind, name: tool_name, input: parameters }` |
   | `tool_result` (non-`update_topic`) | `ToolCompleted { tool_use_id: tool_id, output, is_error: status != "success" }` |
   | `result` status="success" | `TurnEnd { outcome: Completed, usage: <derived from stats> }` |
   | `result` status="error" | `TurnEnd { Failed { kind: classify_gemini_error(error.message), message: error.message } }` — returns `AuthFailure` if auth substring matches, else `HarnessError`. See decision #8. |
   | EOF without `result` (cancelled) | `TurnEnd { outcome: Failed { kind: AdapterFailure, message: "subprocess exited without terminal event" } }` |
   | Exit 42 + empty stdout | `TurnEnd { Failed { kind: classify_gemini_error(actionable_stderr_line), message: actionable_stderr_line } }` — returns `AuthFailure` if auth substring matches, else `AdapterFailure`. See decision #8 + M3.2 step 10. |

7. **Auth-mode detection from `~/.gemini/settings.json`**: `security.auth.selectedType` is a single string (`"oauth-personal"` / `"gemini-api-key"` / `"vertex-ai"` / Workspace-equivalent). The presence-check parallels the existing Codex auth probe (`~/.codex/auth.json`).

8. **Auth-failure stream shape: deferred to M3.2 implementation.** Not probed in M3.1 because triggering it would break the developer's OAuth state. Expected to surface in `result.status:"error"` with a recognizable `error.message` substring. M3.2 implements a best-effort substring match in a **shared `classify_gemini_error(message: &str) -> FailureKind` helper** consumed by both the in-stream `result.status:"error"` path *and* the exit-42 stderr path. Substrings (case-insensitive): `"401 Unauthorized"`, `"PERMISSION_DENIED"`, `"authentication"`. If none match, fall back to `HarnessError`. Tightening the rule later only touches the one helper; both surfaces stay in lockstep.

## Documentation the implementing agent must read before coding

- `docs/research/gemini-cli-observed.md` — full doc, with findings appendix as ground-truth.
- `docs/system-design.md` §9 (normalized event vocabulary), §7 (unified-stream model).
- `crates/harness/src/claude_code/mod.rs` — closest pattern parallel for Gemini's adapter (caller-controlled session ID, `--session-id` first / `--resume` subsequent, process-group spawn).
- `crates/harness/src/codex/mod.rs` — secondary pattern parallel (process-group + `Stdio::null()` discipline, post-terminal stream behavior, EOF-without-terminal-event handling).
- `crates/harness/src/claude_code/session_file.rs` — for hydrator pattern parallel (`load_*_transcript`).
- `crates/harness/src/codex/session_file.rs` — for the `parse_*_transcript_content` separation (parser logic isolated from FS access for test ergonomics).
- `crates/harness/src/events.rs` — wire-format types, especially `AdapterEvent`, `TurnOutcome`, `FailureKind`.
- `crates/harness/tests/README.md` — live-test policy + "intentionally not covered" deferral pattern.
- `https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/headless.md` — official Gemini headless docs. Note: findings appendix supersedes any conflict with the docs.
- `https://github.com/google-gemini/gemini-cli` — source, in case behavior questions arise that fixtures don't answer.

---

## Sub-milestone M3.1 — Gemini CLI probing + fixture capture

**Status: DONE.** Completed 2026-05-17 against `gemini` v0.42.0 with OAuth-personal auth.

### Outcome (completed)

- Hands-on verification of all 8 "Pending verification (M3 fixture capture)" items in the pre-probe section of `gemini-cli-observed.md`.
- 7 small live invocations (~1% of 1,000/day free OAuth tier) producing captured fixtures under `crates/harness/tests/fixtures/gemini/`:
  - `happy-path.stream.jsonl` + `happy-path.session.jsonl`
  - `tool-use.stream.jsonl` + `tool-use.session.jsonl`
  - `resume.stream.jsonl`
  - `error-invalid-model.stream.jsonl` + `error-invalid-model.stderr.txt`
  - `error-unknown-resume.stderr.txt`
  - `interleaved-collision.session.jsonl` (the UUID-prefix-collision demonstration)
- "Findings during M3.1 (2026-05-17)" appendix added to `gemini-cli-observed.md` documenting CONFIRMED / CORRECTED / NEW vs. the pre-probe planning body.

Nothing for the implementing agent to do here — read the findings appendix and proceed to M3.2.

---

## Sub-milestone M3.2 — Gemini adapter implementation

### Goal & outcome

Spawn `gemini -p` as a per-turn subprocess, map its stream-json output onto the existing `AdapterEvent` vocabulary, and integrate into the existing per-harness adapter framework. After this sub-milestone:

- `cargo test --workspace` passes with new fixture-driven integration tests covering all M3.1-captured fixtures plus the auth-failure inline-JSON test.
- `crates/harness/src/gemini/` module exists with `GeminiAdapter` implementing `HarnessAdapter` end-to-end.
- A dispatch through `GeminiAdapter` against a recorded fixture (replayed via a new `fake_gemini` test binary) emits the right normalized events in the right order, with the right terminal `TurnEnd` outcome.
- `HarnessKind::Gemini` exists in `crates/core::harness` and is accepted by `Project::register_agent`.
- The Codex-style sidecar pattern is **not** introduced for Gemini; session continuity uses the Claude-Code pattern (`--session-id` first, `--resume` subsequent, session-file existence detection).
- No new `AdapterEvent` / `NormalizedEvent` / `HarnessAdapter` variants. If implementation requires one, **stop and escalate** per the critical premise.

### Implementation outline

1. **`HarnessKind::Gemini` variant.** Add to `crates/core/src/harness.rs` enum + the wire-format serde rename. The enum is `#[non_exhaustive]`, but match-site updates are still required. **Explicit match-site list** (so the implementor doesn't have to sweep):
   - `crates/app/src/commands.rs::send_message_impl` — adapter selection.
   - `crates/app/src/commands.rs::attach_agent_impl` — new Gemini arm (per M3.4 step 4), not catch-all fallthrough.
   - `crates/app/src/commands.rs::load_transcript_impl` — new Gemini arm dispatches to `load_gemini_transcript` (M3.3).
   - `crates/app/src/lib.rs` — adapter instantiation (mock vs. real).
   - `src/lib/types.ts` — `HarnessKind` union widens to `"gemini"`.
   - Frontend discriminated-union narrowing in `CreateAgentForm.svelte`, `Sidebar.svelte`, `UnifiedTranscript.svelte`, `harnessAvailability.ts`.

2. **Module layout.** New crate-module `crates/harness/src/gemini/` mirroring the `codex/` structure:
   - `mod.rs` — the `GeminiAdapter` impl, `HarnessAdapter` impl, `build_args`, spawn glue. Module docstring names the deferred MCP / skills surface explicitly: "MCP server loading and skills discovery for Gemini are deferred; corresponding modules will land alongside their UI surface in a later milestone."
   - `parser.rs` — stream-JSON line-by-line parser converting Gemini events to `AdapterEvent`s. Hosts `GEMINI_INTERNAL_TOOL_NAMES` and `classify_gemini_error`.
   - `session_file.rs` — path-lookup helpers introduced here (`gemini_session_file_glob`, `resolve_gemini_project_name`, full-sessionId collision-safety filter). M3.3 layers `load_gemini_transcript` on top of these helpers; M3.4's attach arm reuses them. **No helper duplication across sub-milestones** — each one calls the M3.2 helper, doesn't re-derive it.
   - **No `config.rs` / `skills.rs` placeholder modules.** For v1, the adapter emits `SessionMeta { mcp_servers: vec![], skills: vec![] }` directly. Empty stub modules would be dead code with a high risk of being treated as authoritative empty registries by future callers. The deferral is documented in the `mod.rs` docstring; a future contributor adds the modules when they're actually implemented.

3. **`GeminiAdapter` struct.** Plain struct holding optional binary path override (for `fake_gemini` in tests, mirroring the Claude / Codex pattern). Implements `HarnessAdapter::dispatch` and `HarnessAdapter::probe`. Constructor `new()` uses `gemini` from `PATH`; `with_binary_path(path)` overrides.

4. **Session-ID generation: UUID v4, not v7.** Add a `crates/core` helper `AgentRecord::register_with_session_id_v4` (or extend the existing `register_agent` to take a session-ID kind parameter — implementation decides). The Gemini agent registration flow uses v4; everything else stays v7. Rationale (must survive in code comment): "Gemini's session-file naming uses the first 8 hex chars of the session ID; UUID v7s minted in the same millisecond share their first 8 chars, causing on-disk session-file interleave under concurrent dispatch."

5. **`build_args(agent, prompt, cwd, home_override) -> Vec<String>`** — pattern parallel to `claude_code::build_args`. Args (verified order-insensitive against captured fixtures, but spec the order for stability):
   ```
   gemini
     -p <prompt>
     --output-format stream-json
     --session-id <uuid>           ← first turn only (session-file doesn't exist)
     --resume <uuid>               ← subsequent turns (session-file exists)
     --yolo
     --skip-trust
   ```
   Detection of "first-vs-subsequent" mirrors the Claude pattern: check whether a session file exists at the expected path under `~/.gemini/tmp/<project-name>/chats/session-*-<session-id-first-8-chars>.jsonl`. Project-name lookup via `~/.gemini/projects.json`. Helper functions in `session_file.rs`:
   ```rust
   pub fn gemini_session_file_glob(home_dir: &Path, cwd: &Path, session_id: Uuid) -> Result<Option<PathBuf>, ...>;
   pub fn resolve_gemini_project_name(home_dir: &Path, cwd: &Path) -> Option<String>;
   ```
   Pre-validate empty / whitespace-only prompts at the adapter level (Gemini accepts whitespace-only but rejects empty with exit 42; trim and reject in the adapter for consistent error messaging).

6. **Subprocess spawn.** Follow the existing M2 pattern verbatim:
   - `tokio::process::Command::new(&binary).args(&args).current_dir(cwd)`.
   - `process_group(0)` on Unix (load-bearing for SIGTERM via `killpg`).
   - `Stdio::null()` for stdin (we never write to it).
   - `Stdio::piped()` for stdout and stderr.
   - Tokio `BufReader` (not std) on the async pipe.

7. **Stream parser (`parser.rs`).** Line-by-line JSON deserialization. Each line is a record matched by `"type"` field. Handle:
   - `init` → `AdapterEvent::SessionMeta { model, harness_version: <lazy-cached `gemini --version` output, or `""` if unavailable; see M3.2 step 11>, tools: vec![], mcp_servers: vec![], skills: vec![], raw: the_init_object }`.
   - `message` role=user → skip (the prompt echo).
   - `message` role=assistant `delta:true` → `AdapterEvent::ContentChunk { kind: ContentKind::Text, text }`.
   - `tool_use` where `tool_name == "update_topic"` → skip (per resolved decision #3).
   - `tool_use` other → `AdapterEvent::ToolStarted { tool_use_id, kind: <Builtin or Mcp based on name>, name, input: parameters }`. Use `ToolKind::Mcp` if `tool_name` starts with `mcp__` (Switchboard's existing MCP-naming convention), else `ToolKind::Builtin`.
   - `tool_result` where the matching `tool_use_id` was filtered (it was `update_topic`) → skip.
   - `tool_result` other → `AdapterEvent::ToolCompleted { tool_use_id, output, is_error: status != "success" }`.
   - `result` → terminal `TurnEnd`. See step 8.
   - Unknown `"type"` → warn-and-skip with a `tracing::warn!`; don't fail the stream. Forward-compat for upstream CLI additions.

   Track which `tool_use_id`s were filtered (`update_topic`) so the matching `tool_result` skips too. Track parser state across lines: `seen_init: bool`, `filtered_tool_ids: HashSet<String>`, possibly the model from `init` if it's needed for later.

8. **`result` event → `TurnEnd`**. Parse `result.status`:
   - `"success"` → `TurnEnd { outcome: TurnOutcome::Completed, usage: <derived from stats>, ... }`.
   - `"error"` → pass `result.error.message` through `classify_gemini_error(msg)` (the shared helper from resolved decision #8). Returned `FailureKind` is `AuthFailure` if any auth substring matches, else `HarnessError`. Wrap into `Failed { kind, message: error.message }`. Capture a fixture if the auth-failure substring match fails in production; this is the M3.2 best-effort rule we tighten reactively.

   `usage` derivation from `result.stats`:
   ```
   TurnUsage {
     input_tokens: stats.input_tokens,
     output_tokens: stats.output_tokens,
     cached_input_tokens: Some(stats.cached),
     reasoning_output_tokens: None,         // Gemini doesn't expose; thoughts tokens come via session-file
     context_window: None,                  // Not in stream; could be enriched from session file post-terminal but defer (no v1 sidebar requirement)
     total_cost_usd: None,                  // OAuth/free-tier model: no per-turn dollar cost
   }
   ```

   On `Failed { ... }`, surface `usage: Some(TurnUsage { all-zeros, total_cost_usd: None })` matching what the stream actually emits (zero-filled stats).

9. **EOF / cancellation handling.** Mirror the existing M2 pattern: if the stream EOFs without a terminal `result` event, synthesize `TurnEnd { outcome: Failed { kind: AdapterFailure, message: "subprocess exited without terminal event" } }`. This handles both real subprocess crashes AND SIGTERM cancellation (which exits 0 with no terminal event — verified in M3.1).

10. **Exit-42 handling.** If the subprocess exits with code 42 and stdout was empty, extract the first stderr line that isn't a known noise token (`"YOLO mode is enabled"`, `"Ripgrep is not available"`, `"Approval mode overridden"`). Pass that line through `classify_gemini_error` (the same helper used by step 8). If it returns `AuthFailure`, synthesize `Failed { kind: AuthFailure, message: <line> }`; otherwise `Failed { kind: AdapterFailure, message: <line> }`. Keeps the auth-detection rule symmetric across both failure surfaces (stream + exit-code).

11. **`probe()` implementation.** `GeminiAdapter::new()` is **cheap and never fails** (does not shell out) — matches the existing Claude / Codex constructor pattern. `probe()` is a separate method invoked by the frontend availability check; it shells out to `gemini --version` and returns `Ok(())` on exit 0, else `DispatchError::BinaryNotFound`. The cached version string for `SessionMeta.harness_version` is fetched **lazily on first `dispatch()`** via `OnceLock<String>` on the adapter; if the lazy fetch fails (binary disappeared between probe and dispatch), `harness_version: ""` is acceptable (other adapters already tolerate the empty case).

12. **`AppState` wiring** (`crates/app/src/state.rs` + `crates/app/src/lib.rs`). Add `gemini_adapter: Arc<dyn HarnessAdapter>` field to `AppState`; thread through the existing adapter-selection logic (mock vs. real). On startup, instantiate `GeminiAdapter::new()` unless `SWITCHBOARD_HARNESS=mock` is set, in which case all three harnesses use `MockHarnessAdapter`.

13. **`fake_gemini` test binary.** New `crates/harness/src/bin/fake_gemini.rs` mirroring `fake_codex.rs`: reads a fixture path from argv, replays the fixture's stream content to stdout, exits 0 (or with the exit code embedded in the fixture filename via a convention if needed, e.g., `fixture.exit42.jsonl`). Tests use `GeminiAdapter::with_binary_path(env!("CARGO_BIN_EXE_fake_gemini"))` to substitute.

14. **Fixture-driven integration tests** (`crates/harness/tests/gemini_adapter.rs`). One test per:
   - Happy path (`happy-path.stream.jsonl`) → asserts `SessionMeta`, single `ContentChunk` with `"ack"`, `TurnEnd Completed`.
   - Tool use (`tool-use.stream.jsonl`) → asserts `update_topic` filtered; `read_file` `ToolStarted` + `ToolCompleted` pair emitted; final `ContentChunk` arrives; `TurnEnd Completed`.
   - Resume (`resume.stream.jsonl`) → asserts `SessionMeta` carries the resumed session ID.
   - Error / invalid model (`error-invalid-model.stream.jsonl`) → asserts `TurnEnd Failed { HarnessError }` with the surfaced error message.
   - Auth failure (inline JSON in test body, since M3.1 didn't capture this fixture) → asserts `TurnEnd Failed { AuthFailure }`.
   - EOF without terminal → asserts synthesized `Failed { AdapterFailure }`.
   - Unknown event type → asserts warn-and-skip (use `tracing-subscriber` test machinery if practical, else assert downstream events parse correctly).
   - Multi-chunk content (`message` events with `delta:true` arriving as two records) → asserts both `ContentChunk` events emitted in order.
   - `build_args` unit tests: first turn (no session file → `--session-id`), subsequent turn (file exists → `--resume`).
   - Process-group spawn assertion (mirror the Claude / Codex existing test that checks `pgid` matches the child PID).

### Definition of done

- `make check` is green. New fixture-driven tests pass. No clippy warnings.
- All M3.1 fixtures replay cleanly through the parser.
- The auth-failure detection substring match has at least three cases tested (one positive, two negatives) so future tightening can target the rule explicitly.
- `crates/harness/src/gemini/mod.rs` carries a module docstring summarizing the four resolved decisions from this plan that must survive in code (UUID-v4-for-session-id, `--skip-trust`-always, `update_topic`-filtered, live-tool-output-may-be-empty).
- `HarnessKind::Gemini` round-trips through `register_agent`, `list_agents`, JSONL persistence.
- `crates/harness/src/lib.rs` re-exports `GeminiAdapter` at the crate root, matching the Claude / Codex pattern.

### Stop and escalate if

- Any Gemini stream behavior surfaces that requires a non-additive change to `AdapterEvent` / `NormalizedEvent` / `HarnessAdapter` / `FailureKind`. That's the M3 abstraction-load-bearing test failing; capture what it is and surface as a milestone-level review point.
- The session-file path lookup logic surfaces an edge case the M3.1 findings didn't capture (e.g., `projects.json` missing the cwd entry under some condition). Capture and revisit.

---

## Sub-milestone M3.3 — Session-file transcript hydration

### Goal & outcome

Implement `load_gemini_transcript` so project re-open rehydrates Gemini agent transcripts the same way Claude and Codex do. After this sub-milestone:

- `load_transcript_impl` in `crates/app/src/commands.rs` dispatches to `load_gemini_transcript` for `HarnessKind::Gemini` agents.
- A captured `tool-use.session.jsonl` fixture round-trips into a `LoadedTranscript` whose `Turn::Agent.items` contains a `TurnItem::Tool` with `output: Some("SWITCHBOARD_GEMINI_PROBE_TOOL_5F8A21\n")` and `is_error: Some(false)` — the real tool output from the session file, not the empty stream output.
- The `$set` mutation records and the second header (collision case) are filtered safely.
- `thoughts` arrays surface as `TurnItem::Text { kind: ContentKind::Thinking }` items, interleaved by timestamp with the main text.

### Implementation outline

1. **`load_gemini_transcript(home_dir, cwd, session_id, agent_id) -> Result<LoadedTranscript, LoadTranscriptError>`** in `crates/harness/src/gemini/session_file.rs`. Signature parallel to `load_claude_transcript`. `home_dir` is injected for testability.

2. **Path resolution.** Two-step:
   - Look up cwd → project name via `~/.gemini/projects.json`. Returns `Ok(LoadedTranscript::default())` (with meta loaded from configs if applicable) when the project entry is missing — the "never-dispatched-yet" case.
   - Glob `~/.gemini/tmp/<project-name>/chats/session-*-<session-id-first-8-chars>.jsonl`. If exactly one match, use it. If multiple matches (filename collision: more than one session shared the first-8-char prefix), find the file whose first record's `sessionId` matches the full UUID. If zero matches, `Ok(LoadedTranscript::default())`.

3. **`parse_gemini_transcript_content(content, agent_id, target_session_id)`** — pure function, no FS access, mirroring `parse_codex_transcript_content`. Takes the **full requested session UUID** so collision filtering is correctness, not heuristic. Walks records in order:
   - **Skip** lines that are `{"$set": ...}` mutation records (always — they're header mutations, not events).
   - **Track `current_session_id`** across the file. On every header record (line with `kind: "main"`), update `current_session_id` from the header's `sessionId` field. Records *only contribute to the output transcript* when `current_session_id == target_session_id`. This is how prefix-collision files are correctly demixed: a second header switches `current_session_id`, the records after it are filtered until/unless another header switches back.
   - On `type: "user"` (filtered to current session) → open a `Turn::User { agent_id, turn_id: <fresh UUID v7>, started_at: timestamp, text: content[0].text }`.
   - On `type: "gemini"` records (filtered to current session): **dedupe by `id`, last-wins**. The same `id` may appear multiple times as the record accrues data (verified empirically). Buffer per-`id`, replace on every observed copy, finalize when the record is "done" (heuristically: when a new gemini-record `id` appears, or end-of-file).
   - **Aggregate** the user record + all subsequent gemini records (up to but not including the next user record) into a `Turn::Agent { items: Vec<TurnItem>, usage, status }`. Each gemini-record contributes:
     - Its `content` string as a `TurnItem::Text { kind: Text, text }` (skip if empty).
     - Each `thoughts[i]` as a `TurnItem::Text { kind: Thinking, text: thought.subject + "\n" + thought.description }` (or surface only `description` — implementation decides; document).
     - Each `toolCalls[i]` as a `TurnItem::Tool` — see step 4.
   - `Turn::Agent.usage`: take from the **last** gemini-record's `tokens` field. Map to `TurnUsage` (input/output/cached map directly; `total` is informational only; `thoughts` and `tool` token buckets don't have a `TurnUsage` field today and are dropped silently — capture as a follow-up question for v2 if cost UI evolves).
   - `Turn::Agent.status`: **default `TurnStatus::Complete`**. Mark `TurnStatus::Failed` *only* when the file is clearly truncated — the final non-blank line is partial JSON that fails to parse. Parser warnings emitted via the M2.6 warnings channel are surfaced to the UI but **never change the turn status** — keeping M2.6's non-blocking warning model intact. Heuristic "looks unfinished" markers (empty content + no toolCalls + no following user record) are **not** sufficient grounds to mark Failed — Gemini legitimately persists sparse records mid-turn.

4. **`TurnItem::Tool` reconstruction.** From the gemini-record's `toolCalls[i]`:
   ```
   TurnItem::Tool {
     tool_use_id: tc.id,
     kind: if tc.name.starts_with("mcp__") { Mcp } else { Builtin },
     name: tc.name,
     input: tc.args,
     output: tc.result[0].functionResponse.response.output  // real output, not empty
       (or fall back to tc.result[0].functionResponse.response stringified if `output` field is missing),
     is_error: Some(tc.status != "success"),
     started_at: tc.timestamp,
     completed_at: tc.timestamp,  // session-file form doesn't separate start/completed
   }
   ```
   Filter against the **shared `GEMINI_INTERNAL_TOOL_NAMES` constant** from M3.2's `parser.rs` — same deny-list as the live adapter, so any future internal-tool addition stays in lockstep across both surfaces.

5. **`commands::load_transcript_impl` dispatch.** New `HarnessKind::Gemini` arm: call `load_gemini_transcript(home_dir, &directory.path, session_id, agent.id)`. The session ID is read from `AgentRecord.session_id` (Gemini agents have `Some(uuid)` like Claude, unlike Codex which has `None` + sidecar).

6. **Re-exports.** `crates/harness/src/lib.rs` exports `load_gemini_transcript` and `gemini_session_file_path` (helper for tests).

7. **Fixture-driven integration tests** (in `crates/harness/src/gemini/session_file.rs` or a new tests module — implementation decides):
   - Happy-path session round-trip → asserts user/agent turn pair, model populated, no warnings.
   - Tool-use session round-trip → asserts `TurnItem::Tool` carries the real `output` (the sentinel string from the captured fixture).
   - `$set` filtering — assert mutation records produce no phantom turns.
   - Dedupe-by-id — assert a gemini-record appearing twice (synthetic test case) is collapsed into one turn item.
   - Collision file (`interleaved-collision.session.jsonl`) → load with target `session_id = ...009`, assert the `"red"`-prompt user/agent records are present and the `"blue"`-prompt records are **absent**. Load again with target `session_id = ...00A`, assert the inverse. The contract is "records belonging to the requested session, *only*."
   - `update_topic` filtered from `TurnItem::Tool`.
   - `thoughts` surface as `Thinking` items.

### Definition of done

- `make check` green. New tests pass. Auth probe and harness availability (M3.4) not required yet.
- A captured `tool-use.session.jsonl` round-trips into a `LoadedTranscript` with the sentinel string in the agent turn's tool item.
- `load_transcript_impl` Tauri command works end-to-end against a real Gemini agent (manual: dispatch a turn, restart, reopen project, observe hydrated transcript).
- Frontend `hydrate` reducer input populates correctly for Gemini agents (M3.4 verifies in the UI).

---

## Sub-milestone M3.4 — Frontend wiring + `HarnessKind::Gemini`

### Goal & outcome

The unified-stream UI accepts Gemini as a third equally-first-class harness, including both create-new and attach-existing flows. After this sub-milestone:

- The add-agent / create-agent form lists Gemini alongside Claude Code and Codex; both create and attach modes work for all three.
- The per-harness availability banner detects Gemini binary missing / Gemini auth not configured, with the same shape as the existing Claude and Codex banners. The `HarnessBanner` type is widened so auth-detectable harnesses are no longer hard-coded to Codex.
- The unified transcript shows Gemini agents with a per-harness icon / badge, distinct from Claude and Codex.
- The empty-tool-output render rule (resolved decision #5) lands here: `UnifiedTranscript.svelte`'s tool-item renderer suppresses the `output` body when `output === ""`. General rule, not Gemini-specific.
- Auth-mode detection surfaces the current Gemini auth method (`oauth-personal` / `gemini-api-key` / `vertex-ai` / Workspace-equivalent) — informational; UI affordance design is M5+.

### Implementation outline

1. **`HarnessKind` in TS** (`src/lib/types.ts`). Add `"gemini"` to the discriminated union. The wire format is `snake_case` so the TS literal matches Rust's `#[serde(rename_all = "snake_case")]` output.

2. **Widen `HarnessBanner` for auth-detectable harnesses.** Today (`src/lib/types.ts:240-248`) `auth_missing` is typed as `harness: "codex"` literal — a v1-era hardcoding for the single auth-detectable harness. Widen to `harness: "codex" | "gemini"`. Update `bannerCopy` and `harnessUnavailableReason` (`src/lib/harnessAvailability.ts:18-39`) to switch on `banner.harness` for the auth-missing copy: Codex says "run `codex login`", Gemini says "run `gemini auth login`" (verify exact CLI command during implementation). Claude `auth_missing` remains unsupported — the v1 invariant that Claude auth detection is unavailable survives.

3. **Add-agent form** (`src/lib/components/CreateAgentForm.svelte`). Add Gemini as a third option in the harness selector. The existing mode toggle (`"create" | "attach"`) stays **harness-generic** — Gemini supports both create and attach identically to Claude and Codex. **Do not add per-harness disable logic** for the attach mode (an earlier review iteration suggested it; the decision was reversed in favor of implementing Gemini attach as a first-class flow per step 4).

4. **`attach_agent_impl` Gemini arm** (`crates/app/src/commands.rs`). Add a `HarnessKind::Gemini` arm parallel to Claude's:
   - Parse the user-supplied UUID via `parse_uuid`.
   - Locate the session file using the **shared helpers from M3.3** (`gemini_session_file_glob`, `resolve_gemini_project_name`). The glob matches by first-8-char prefix; on multi-match, inspect each candidate's header `sessionId` field and pick the file whose **full** UUID matches the user-supplied one (collision-safe per resolved decision #3 in M3.3).
   - If no candidate exists or no candidate's header matches the full UUID → `AppError::SessionFileNotFound { harness: Gemini, expected_path: <glob path> }`. **Never** fall back to "latest" — attaching to the wrong conversation is worse than asking the user to retry.
   - Cross-project session-id uniqueness check (mirror `check_codex_session_id_unique`'s pattern; new `check_gemini_session_id_unique` helper). Reject with `AppError::SessionAlreadyAttached` if found.
   - Register with `session_id: Some(uuid)` (Claude shape, not Codex's `None`-plus-sidecar). Add `Project::register_attached_gemini_agent(name, session_uuid)` to `crates/core` paralleling `register_attached_claude_agent`.

5. **Per-harness availability banner** (`src/lib/harnessAvailability.ts` + corresponding Svelte component). New `check_gemini_binary` Tauri command (parallels `check_claude_binary` / `check_codex_binary`). New `check_gemini_auth` Tauri command (see step 6). Banner copy follows the existing pattern: "Gemini CLI not on PATH — install via `npm i -g @google/gemini-cli`."

6. **`check_gemini_auth_impl`** in `crates/app/src/commands.rs`. Read `~/.gemini/settings.json` and parse the `security.auth.selectedType` string. Return `Ok(())` if any supported method is set (all four are supported for v1 — Switchboard doesn't care which auth pool the user is on, it just needs *some* auth). Return `Err(AppError::AuthNotConfigured { harness: Gemini, ... })` if the file is missing or `selectedType` is absent. Pattern parallels `check_codex_auth_impl`.

7. **Unified transcript per-harness badge** (`src/lib/components/UnifiedTranscript.svelte`). Add Gemini icon / color. Don't introduce a global color-coding refactor — additive only.

8. **Empty-tool-output suppression** (`src/lib/components/UnifiedTranscript.svelte`). When rendering a `TurnItem::Tool` in either live or hydrated state, suppress the `output` body when `output === ""`. The lifecycle badge (`started → completed`, error indicator) still renders; the body block just collapses. **Harness-agnostic rule** — apply to all tool items regardless of harness. Rationale (must survive in comment): "Gemini's live stream emits empty `output` for read-like tools; the session file carries the real content. Suppressing empty bodies avoids 'the live view shows nothing then the reopened view shows content' looking like a regression — both views show *something coherent*, with hydration adding detail."

9. **Sidebar per-agent overview** (`src/lib/components/Sidebar.svelte`). Gemini agents render the same shape as Claude / Codex with their per-agent status. For v1: no Gemini-specific cost/quota surface in the sidebar (defer to M5; informational auth-mode is the limit of M3 scope).

10. **App tests** (`src/lib/state/index.test.ts`, `App.test.ts`, `CreateAgentForm.test.ts`, `harnessAvailability.test.ts`). Add Gemini cases mirroring the Claude / Codex existing coverage. Specifically:
    - `HarnessBanner` widened-type cases (auth-missing variant for both Codex and Gemini).
    - `CreateAgentForm` attach-mode validation for Gemini.
    - Empty-output suppression render assertion (component-level test against `UnifiedTranscript`).

### Definition of done

- Frontend `make check` green.
- Creating a Gemini agent in the UI, sending a message, and seeing the response in the unified transcript works end-to-end (manual).
- **Attaching** an existing Gemini session via UUID works end-to-end (manual): create a session via `gemini -p ... --session-id <uuid>` in a shell, then attach to it in Switchboard, dispatch a follow-up turn, verify resume worked.
- Restart-and-reopen project rehydrates Gemini transcripts correctly (manual).
- Per-harness availability banner correctly reports "Gemini binary missing" if you `mv` the gemini binary off PATH (manual).
- Empty-output suppression: an `update_topic` (or any zero-output tool) renders without a blank body block in the live view.

### Attach test coverage (fixture-driven, in `crates/app/src/commands.rs` tests module)

Add a Gemini attach test block parallel to the existing Codex / Claude attach tests:
- Happy path → registers the agent with the supplied UUID.
- Session-file-not-found → returns `AppError::SessionFileNotFound`.
- Multi-candidate (prefix collision) where the full UUID **does** match one candidate → returns success.
- Multi-candidate where the full UUID **doesn't** match any candidate → returns `SessionFileNotFound` (never picks a wrong one).
- Duplicate-name → returns the existing name-conflict error.
- Same-project session collision → returns `SessionAlreadyAttached`.
- Cross-project session collision → returns `SessionAlreadyAttached`.
- Missing `projects.json` entry → returns `SessionFileNotFound`.

---

## Sub-milestone M3.5 — Live-harness test suite extension

### Goal & outcome

The live-harness suite covers Gemini's happy path, tool use, transcript round-trip, and error path. After this sub-milestone:

- `make test-live` exercises Gemini in addition to Claude and Codex.
- The per-event-type live coverage rule established in M2.7 applies to Gemini: at least one reliably-triggerable test per major event surface.
- The "intentionally not covered live" section of `crates/harness/tests/README.md` is updated for Gemini's specific deferrals (auth-failure, MCP tool path, content of read-like tool output in stream).

### Implementation outline

1. **Layout.** Add Gemini cases to the existing flat-file live-test layout under `crates/harness/tests/`. Pattern parallel to M2.7's additive approach (don't split into a 14-file matrix):
   - Extend `tests/live.rs` with `live_gemini_basic_turn_completes` and `live_gemini_resume_reuses_session` (happy-path + resume).
   - Extend `tests/tool_use.rs` with `live_gemini_emits_tool_started_and_tool_completed_for_file_read`. **Asserts lifecycle only** (not sentinel-in-output) per resolved decision #5 — adapter live test cannot rely on stream tool output content for Gemini.
   - Extend `tests/transcript_load.rs` with `live_gemini_transcript_load_via_session_file_round_trips` (text round-trip) and `live_gemini_transcript_load_hydrates_tool_items` (the sentinel-driven tool-output assertion lives here, against the session file, where the content actually exists).
   - **Required**: extend `crates/dispatcher/tests/live_end_to_end.rs` with a Gemini variant of the three-event-ordering check (`turn_start → content_chunk → turn_end → agent_idle`). This is the canonical empirical assertion of M3's headline claim — that the M2 dispatcher abstraction is genuinely harness-neutral — proved through the actual dispatcher code path, not just through adapter-layer fixtures. Mirror the existing Claude / Codex variants (~60 LOC).

2. **`check_gemini_auth` live test.** Inline in `crates/app/src/commands.rs` `#[cfg(test)] mod tests`, marked `#[ignore]`, parallel to the existing `live_check_codex_auth_finds_real_auth_file`.

3. **Cost discipline.** Every live prompt constrained to a small response (e.g., `"Reply with the single word 'ack'"`). Total Gemini-side cost per `make test-live` run: ~5 small turns × ~10k tokens-each ≈ 5 requests / 50k tokens. Well within the 1,000/day OAuth tier.

4. **README updates** (`crates/harness/tests/README.md`):
   - Extend "What's covered" to mention Gemini tests.
   - Extend "What's intentionally not covered live" with two new bullets:
     - "Gemini tool-output content in the stream (elided for read-like tools per `gemini-cli-observed.md`; covered by `live_gemini_transcript_load_hydrates_tool_items` against the session file)."
     - "Gemini auth-failure (cannot trigger without breaking the developer's OAuth state; covered fixture-driven via the inline-JSON test in `gemini_adapter.rs`)."

### Definition of done

- `make check` green (live tests reported as ignored).
- `make test-live` green with `claude`, `codex`, and `gemini` all installed and authenticated.
- Total `make test-live` wall-clock: still ≲ 3 minutes.
- Manual: pass the acceptance flow below.

### Acceptance — three harnesses, one project

After M3.5 lands, on a clean checkout:

1. Create a project.
2. Add three agents: one Claude (`claude-helper`), one Codex (`codex-helper`), one Gemini (`gemini-helper`).
3. Send a message to each via the recipient picker.
4. **Attach test**: outside Switchboard, run `gemini -p "..." --session-id <known-uuid> --yolo --skip-trust` in some directory. In Switchboard's add-agent modal, attach to that UUID under a new agent name. Confirm the prior turn rehydrates correctly and that a follow-up dispatch resumes the session. Then create a second project and try attaching the same UUID under another agent name there — must be **rejected** with the cross-project session-uniqueness error.
5. All three agents' turns (plus the attached agent) appear in the unified transcript in chronological order, attributed by agent name + harness badge.
6. Per-agent sidebar populates for each (status, $ for Claude, % for Codex, no per-turn cost surface for Gemini in v1).
7. Restart the app, reopen the project — all transcripts rehydrate from their respective harness session files. New turns continue cleanly.
8. The `AdapterEvent` / `NormalizedEvent` / `HarnessAdapter` vocabulary is unchanged from M2 (additive changes only). If a non-additive change was needed, document the surprise prominently in the M3 PR body.

---

## After M3.5 — open the M3 PR

Once M3.2–M3.5 are committed and `make check` is green on `m3`:

1. Push the branch (it already tracks `origin/m3`).
2. Open a PR titled `M3: Gemini CLI as third harness`.
3. PR body should call out:
   - Whether the M2 abstraction held (additive-only wire-format) — expected yes; surface explicitly if no.
   - The localized UUID-v4-for-Gemini-session-IDs deviation and its rationale.
   - The "intentionally not covered live" deferrals (auth-failure, MCP, stream tool-output content).
   - A pointer to `docs/research/gemini-cli-observed.md` for the empirical ground truth.
4. CI runs `make check` automatically; live tests stay developer-local per the AGENTS.md policy.

---

## Out of scope for M3

The following are real, but not M3 deliverables:

- **MCP server / skills loading for Gemini.** Switchboard reads MCP server configs and skills directories for Claude and Codex (the `config.rs` / `skills.rs` modules). Gemini has parallel concepts (`gemini mcp` subcommand; `gemini skills` subcommand; `~/.gemini/settings.json` carries MCP server config). **M3 does not create placeholder `config.rs` / `skills.rs` modules** for Gemini — the adapter emits `SessionMeta { mcp_servers: vec![], skills: vec![] }` directly. A future milestone (likely M4 or M5) adds the modules when the loaders are actually implemented; the deferral is documented in `gemini/mod.rs`'s docstring.
- **Per-agent cost / credit / quota surface for Gemini.** The 1,000/day OAuth tier is per-account, not per-turn. Surfacing this in the UI is M5/M8 territory.
- **Auth-failure stream-shape verification.** Deferred to M3.2's "best-effort substring match" approach, with a fixture captured reactively if a production user reports a misclassification.
- **Gemini's `invoke_agent` sub-agent affordance.** Gemini's builtin sub-agent dispatch fires for substantive prompts and can take 60+ seconds wall-clock for a single user-perceived turn (verified empirically). M3 surfaces it as a single `ToolStarted` / `ToolCompleted` pair; the longer wall-clock is left for M4's per-turn stall / cancellation policy to address.
- **Workspace-trust UX beyond `--skip-trust`.** The adapter unconditionally passes `--skip-trust`. If Gemini deprecates that flag in a future release, the adapter breaks; track upstream release notes (M3.1's known known-unknown).
- **`thoughts`-tokens / `tool`-tokens in `TurnUsage`.** Gemini's session-file telemetry has `thoughts` and `tool` token buckets the existing `TurnUsage` struct doesn't carry. Drop silently for v1; revisit if M5 cost UI evolves to need them.

## Notes for the implementing agent

- **Type hints / signatures.** All function signatures (Rust + TypeScript) fully typed; TypeScript stays `strict: true`; don't reach for `any`.
- **No imports inside functions** unless absolutely necessary.
- **No commits without explicit user instruction.** Stage and prepare; the user commits manually after reviewing each sub-milestone.
- **No comments unless the why is non-obvious.** Code structure should be self-explanatory. *Do* preserve the rationale for these resolved decisions in the relevant module / function docstrings — each one defends against a class of "simplifying" refactor that would undo the decision:
  - **UUID v4 for Gemini session IDs** (`gemini/mod.rs`): Gemini's 8-char-prefix filename collision under UUID v7's millisecond-shared prefixes.
  - **`--skip-trust` always** (`gemini/mod.rs`): the workspace-trust gate blocks headless dispatches without it; Switchboard's bound cwd is by definition the user's working directory.
  - **`GEMINI_INTERNAL_TOOL_NAMES` shared constant** (`gemini/parser.rs`): deny-list pattern; future internal tools may need adding here. Constant consumed by both the adapter and the hydrator so they stay in lockstep.
  - **`classify_gemini_error` shared helper** (`gemini/parser.rs`): both the in-stream `result.status:"error"` path and the exit-42 stderr path call it so auth-failure detection is symmetric across failure surfaces.
  - **`UnifiedTranscript`'s empty-output suppression** (component docstring or inline comment): the harness-agnostic rule exists because Gemini's stream emits empty `output` for read-like tools — without suppression, the live view shows blank tool bodies that "fill in" only after reopen.
  - **Full-sessionId collision-safety filter in the hydrator** (`gemini/session_file.rs`): the parser's `current_session_id`-tracking rule defends against Gemini's 8-char-prefix filename collision; do not "simplify" by removing the full-UUID check.
  - **Lazy version fetch via `OnceLock`** (`gemini/mod.rs`): the constructor stays cheap and non-failing per the M2 adapter pattern; version is fetched on first dispatch.
- **Stop after each sub-milestone.** Summarize: (1) what landed, (2) what tests pass, (3) any open questions or surprises. Wait for the user to commit + signal before continuing.
- **If a sub-milestone surfaces something M3.1's findings appendix didn't anticipate** — pause and ask. Don't pattern-match to "the spec probably says..." — check it against the captured fixtures or, if the answer is genuinely empirical, run one more small probe (cost: ~10k tokens) and append to the findings appendix.
- **M2 backend abstractions are stable.** The whole *point* of M3 is to validate this. The expected extensions are (a) `HarnessKind::Gemini` variant on the existing enum (additive), (b) new `crates/harness/src/gemini/` module, (c) `gemini_adapter` field on `AppState`. Outside those listed changes, if you find yourself wanting to change `HarnessAdapter`, the dispatcher, the `EventEmitter` trait, or the wire-format types — **stop and ask** per the critical premise. That's exactly what M3 is supposed to detect.
- **Resolved decisions are committed.** The "Resolved design decisions" section at the top is not up for relitigation during implementation. If implementation evidence makes a resolved decision look wrong, surface it explicitly — don't quietly drift.
