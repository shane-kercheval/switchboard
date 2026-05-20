# M3B — Antigravity CLI adapter

## Why this milestone exists

Google announced on 2026-05-18 that the Gemini CLI stops serving free / Google AI Pro / Ultra tiers on **2026-06-18**. The replacement is Antigravity CLI (binary `agy`), a Go-based client for a server-side agent. Switchboard's v1 supports subscription/tier auth only (no API keys), so the Gemini adapter shipped in M3 stops working for ~100% of our target audience in ~30 days.

Antigravity is **not** a renamed Gemini CLI. The research doc enumerates the contract differences; the load-bearing ones for adapter design:

- **No structured stream output.** `agy -p` writes plain markdown to stdout. There is no `--output-format stream-json` equivalent. The transcript file (`transcript.jsonl`) only records **completed** records (no per-chunk assistant text streaming). Switchboard's live stream must therefore use **two sources in parallel**: stdout for live assistant text (`ContentChunk { kind: Text }`), and `transcript.jsonl` tail for tool lifecycle + terminal outcome.
- **No caller-controlled session ID.** The conversation UUID is assigned server-side. The adapter must capture it post-spawn (closer to Codex's sidecar pattern than to Gemini's caller-mints-UUID pattern). `AgentRecord.session_id` is `None` for Antigravity at creation time; a per-agent sidecar carries the captured UUID for downstream consumers (resume, hydration, attach, uniqueness checks).
- **Primary store is encrypted protobuf** (`~/.gemini/antigravity-cli/conversations/<uuid>.pb`). The only parseable transcript is a sidecar JSONL at `~/.gemini/antigravity-cli/brain/<uuid>/.system_generated/logs/transcript.jsonl`. The `.system_generated/` segment is flagged by Google as an internal artifact — treat the path as a single load-bearing constant with a documented "this may change" comment.
- **Auth is macOS-Keychain-only.** No file-based auth probe; detection via `security find-generic-password -s <service>`. Service name pinned during M3B.1.
- **No usage / token / cost data on disk.** All server-side. Sidebar cost / quota / context cells degrade to "—" for Antigravity.

Reusable from the Gemini adapter: the JSON schema for MCP server configs (paths differ); the `SKILL.md` plugin convention (paths differ). Roughly 10% of the surface; the rest is genuinely separate work.

## What we do NOT do

- **Do not delete or modify the Gemini adapter.** Paid Workspace users keep Gemini CLI past 2026-06-18; we keep their support working. When Gemini CLI is fully discontinued (or our analytics show no users on it), removal is a separate, mechanical PR.
- **Do not factor a shared `google_shared/` module preemptively.** The research showed only loaders are reusable, and each loader is small. Two parallel loaders is cheaper than one parameterized loader plus the cognitive cost of a shared module that exists for ~30 days of overlap. If a fourth Google-flavored harness appears, factor then — not now. *(This decision belongs in a comment in each Antigravity loader file so the reader knows duplication is deliberate.)*
- **Do not introduce a new event variant for "plain-text assistant turn"** or any other Antigravity-specific bend in the normalized event vocabulary (`ContentChunk` / `ToolStarted` / `ToolCompleted` / `TurnEnd` / `SessionMeta` / `RateLimitEvent`). The wire-format contract is harness-neutral; if Antigravity's transcript format can't fit, we either map to the existing vocabulary or change the vocabulary as a separate, deliberate design pass.
- **Do not implement turn cancellation in this milestone.** `TurnOutcome` has only `Completed` and `Failed` today; there is no dispatcher cancellation token; the UI has no cancel button. Cancellation is a cross-cutting design pass across all harnesses, not an Antigravity-specific feature. `process_group(0)` spawn is retained for future-proofing only.

## Required reading before implementation

- `docs/research/antigravity-cli-observed.md` — full ground-truth probe results. Every claim in this plan derives from there.
- `docs/research/gemini-cli-observed.md` — the comparison baseline; the contract-diff table is the quickest way to internalize what's different.
- `docs/system-design.md` §7 (sidebar matrix), §9 (per-harness adapter and normalized event stream), §11 (deferred decisions). The Antigravity rows in §7 and §9 must be added in this milestone — they don't exist yet.
- `crates/harness/src/gemini/mod.rs`, `gemini/session_file.rs`, `gemini/config.rs`, `gemini/skills.rs` — the structural template for the new `antigravity/` module. Don't copy line-for-line; the contracts differ. Use it for shape and naming conventions.
- `crates/harness/src/codex/` — Codex's post-spawn session-ID capture and sidecar persistence is the closest existing pattern to Antigravity's needs. Read it before writing the sidecar.
- `crates/harness/src/events.rs` — the authoritative wire-format vocabulary. **All event emissions in this milestone map into the existing `AdapterEvent` and `TurnOutcome` types; the plan that follows references those types by their real names.**

## Cross-cutting rules

- **No milestone references in code.** Per AGENTS.md: comments describe the rule, not the chronology. Rationale lives in commit messages and this plan; code says "Antigravity's transcript path is `…/.system_generated/…` which is flagged by Google as internal," not "// per M3B.2."
- **No unwrap/expect outside `main` or test code.** Standard project rule.
- **Live tests are developer-local only, never CI.** Marked `#[ignore = "requires agy installed — run with: make test-live"]`. See AGENTS.md "Live testing against real harnesses."
- **The `transcript.jsonl` path is a single constant in one place.** When Google renames `.system_generated/` (and they will), we want to flip exactly one line.
- **Live tests must not require destructive state changes to the developer's machine.** No "delete the Keychain entry before running the auth-failure test"; auth-failure paths are covered by fixture-driven unit tests against captured stderr patterns.

---

## Milestone M3B.1 — Foundation: HarnessKind variant, sidecar contract, auth detection

### Goal & Outcome

Plumb `HarnessKind::Antigravity` through every place harness kinds are named — Rust enums, TS unions, registries, wire-format mappings. Pin the sidecar contract (per-agent persistence of the server-assigned conversation UUID) before any milestone depends on it. Implement binary + auth detection so the picker can correctly classify availability.

Functional outcomes:

- `HarnessKind::Antigravity` exists as a non-exhaustive enum variant in `switchboard-core`.
- `Project::register_agent(_, HarnessKind::Antigravity)` produces an `AgentRecord` with `session_id: None` (matching Codex's pattern).
- A new per-agent Antigravity sidecar shape exists at the storage path used by Codex's sidecar (match exactly — same directory, same naming convention). Sidecar fields: `conversation_id: Uuid`, `captured_at: DateTime<Utc>`, `transcript_path: PathBuf`. Read/write helpers are tested in isolation.
- `check_antigravity_binary_impl` and `check_antigravity_auth_impl` exist and produce the standard `HarnessAvailability` discriminated union. The picker hides Antigravity entirely if the binary is missing; shows a clear "subscription auth required" banner if the binary is present but Keychain auth is missing.
- `HarnessBanner.auth_missing` extends to include `"antigravity"`.
- An attempt to *dispatch* to an Antigravity agent fails fast with a typed `AppError::HarnessNotYetSupported { harness: HarnessKind::Antigravity }` — no panic, no `unimplemented!()`, no milestone tag. This error is removed in M3B.2 when dispatch lands.

### Implementation Outline

1. **`HarnessKind` enum.** Add the variant in `crates/core/src/…`. Every `match` on `HarnessKind` in the workspace gains an `Antigravity` arm. For arms not yet meaningful (sidebar rendering, dispatch, hydration), route through the existing typed-error patterns the codebase already uses for "harness understood but not yet wired" — read the Codex arms for the precedent. **Do not use `unimplemented!()` in any runtime code path.** A user-triggerable panic is the wrong failure mode and milestone-tagged comments violate the AGENTS.md rule.

2. **`Project::register_agent` for Antigravity.** `session_id = None` at creation. Add a unit test that mirrors `register_codex_agent_leaves_session_id_none` for Antigravity.

3. **Antigravity sidecar.** Pin the shape:
   ```rust
   pub struct AntigravitySidecar {
       pub conversation_id: Uuid,           // server-assigned, captured post-spawn
       pub captured_at: DateTime<Utc>,
       pub transcript_path: PathBuf,        // resolved absolute path; lets hydration skip path recomputation
   }
   ```
   Storage path: match Codex's sidecar location exactly (don't invent a parallel scheme). Reader returns `Option<AntigravitySidecar>` — `None` means "not yet captured" and is a valid state for an agent that has never dispatched. Writer is atomic (write-temp-then-rename, same pattern Codex uses).

4. **`crates/harness/src/antigravity/mod.rs`.** Create the module with `pub struct AntigravityAdapter` implementing `HarnessAdapter`. The `dispatch` method returns the typed `AppError::HarnessNotYetSupported` (or the closest existing variant — read the Codex skeleton for what was used before its dispatch was wired). Register the adapter in the harness factory. Comment on `dispatch`: describe the rule ("Antigravity dispatch is implemented in `antigravity::dispatch` and wired through in a subsequent change") — no milestone tag.

5. **`check_antigravity_binary_impl`.** Mirrors the existing Gemini probe. `which::which("agy")`. Returns the standard `HarnessAvailability` discriminated union.

6. **`check_antigravity_auth_impl`.** Probe the Keychain via `security find-generic-password -s <service>`. The service name must be pinned during M3B.1 — discover it by inspecting the Keychain on the implementer's authed dev machine, by running `strings` on the `agy` binary against likely candidates, or by triggering an authed `agy` invocation under `dtruss`/`fs_usage`. Document the discovered service name as a constant with a comment naming the discovery method. If the service name proves unstable across `agy` versions, fall back to a cheap `agy -p "ping"` probe and pattern-match the unauthed stderr signature documented in research doc line 106; structure the code so the fallback is a single function swap.

7. **TS types.** Extend `HarnessKind` to include `"antigravity"`. Extend `HarnessAvailability` discriminated union with an Antigravity variant matching the existing Gemini/Codex shape. Extend `HarnessBanner.auth_missing = "codex" | "gemini"` to include `"antigravity"`. Reducer default branches handle unknown discriminants gracefully (existing convention).

8. **Sidebar / harness-picker UI.** Antigravity row renders with the same shape as the other three. Status / banners are driven by the binary + auth probes from steps 5-6. Don't populate model / MCP / skills / cost / context columns yet — those land in later milestones; empty cells are fine.

### Definition of Done

- `cargo check --workspace` clean; `pnpm tsc --noEmit` clean; `make lint` clean.
- Unit tests:
  - `check_antigravity_binary_impl`: binary-present and binary-missing paths.
  - `check_antigravity_auth_impl`: authed and unauthed paths against a mock `security` invocation (or `agy -p ping` if the fallback was chosen).
  - Antigravity sidecar: round-trip read/write, `None` for missing file, atomic write (write-then-rename observed by an interrupted test if practical, otherwise pin via code review).
  - `Project::register_agent(_, Antigravity)`: `session_id` is `None`.
- Existing test suite passes unchanged.
- Comment in `antigravity::dispatch` describes the "not yet wired" rule without naming a milestone.

---

## Milestone M3B.2 — Adapter: dual-source streaming + event mapping + terminal outcomes

### Goal & Outcome

The Antigravity adapter spawns `agy -p "<prompt>"`, captures the server-assigned conversation UUID, persists it to the sidecar, and streams events to the dispatcher via two parallel sources: stdout (live assistant text) and `transcript.jsonl` tail (tool lifecycle + terminal outcome).

Functional outcomes:

- Sending a prompt to an Antigravity agent results in a live transcript flowing into Switchboard's UI: text appears as the model produces it (from stdout drip), tool calls and tool results surface as they happen (from transcript tail), the turn terminates cleanly with `TurnOutcome::Completed` or `TurnOutcome::Failed { kind, message }`.
- A resumed dispatch (same agent, second prompt) only emits events for records newly appended to the transcript; prior turn records are not re-emitted.
- An unauthenticated invocation produces `TurnEnd { outcome: Failed { kind: AuthFailure, message } }` (assuming M3B.1's auth probe was bypassed somehow — usually the auth banner catches this first).
- A round-trip "Reply with 'ack'" live test against the developer's real `agy` install passes.

### Implementation Outline

This is the load-bearing milestone. Order is important.

**1. Spawn.** Standard adapter pattern:

- `tokio::process::Command::new("agy")`, args `["-p", &prompt]`.
- Working directory = the agent's bound cwd.
- `Stdio::piped()` for stdout (we read it for live text).
- `Stdio::null()` for stdin.
- `Stdio::piped()` for stderr; buffer for auth-failure forensics and error attribution.
- `process_group(0)` on Unix. Retained for consistency with other adapters and future cancellation support; no cancellation logic in this milestone.

**2. Capture the server-assigned UUID.** Poll `~/.gemini/antigravity-cli/brain/` for a new subdirectory whose mtime is newer than spawn-time. The directory name is the UUID. Polling interval: 100ms. Timeout: 5 seconds.

Each poll tick **also** calls `child.try_wait()?`. If the child exits before the directory appears, terminate immediately with `TurnEnd { outcome: Failed { kind: HarnessError, message: format!("agy exited (code {N}) before creating a session directory; stderr: {captured}") } }`. This avoids a 5-second user-visible "starting" hang on early-crash failures.

On success, persist the sidecar (`conversation_id`, `captured_at`, `transcript_path`) atomically before continuing.

Fallback to `~/.gemini/antigravity-cli/cache/last_conversations.json` is not implemented in this milestone — added only if filesystem-watch proves unreliable in practice.

**3. Establish the transcript cursor.** Before tailing, capture the current `transcript.jsonl` length (or last `step_index`) for the captured UUID. If the file doesn't exist yet, cursor = 0. Tail emits events only for records appended past the cursor. This is what makes resume safe — prior turn records are not re-emitted.

**4. Dual-source streaming.** Two tasks run concurrently:

- **stdout reader.** Reads bytes from `agy`'s stdout, emits `AdapterEvent::ContentChunk { kind: ContentKind::Text, text }` for each line or each flush boundary. Newlines are preserved so the UI renders the same Markdown structure `agy` writes. Strip ANSI escape codes if present (probe during implementation — if absent, no-op the stripper). **stdout is the only source of live assistant text**; do not emit `ContentChunk { Text }` from the transcript tail.

- **transcript tail.** Opens `~/.gemini/antigravity-cli/brain/<uuid>/.system_generated/logs/transcript.jsonl`. The path lives as a single function `antigravity::paths::transcript_path(uuid: Uuid) -> PathBuf` with a doc comment naming `.system_generated/` as a Google-internal segment that may change between Antigravity versions.

  Tail mechanics:
  - File may not exist immediately. Poll for it with the same 100ms / 5s budget; on each tick, call `child.try_wait()?` and terminate early if `agy` has exited. If both the file is missing AND the child has exited, emit `Failed { kind: HarnessError, message: "transcript file never created" }`.
  - Once open, read in append-tracking mode. Buffer partial lines (a line without a trailing newline must be held, not parsed-and-discarded).
  - Each completed line past the cursor is parsed to a typed `TranscriptRecord`. Records on or before the cursor are silently skipped.

**5. Map `TranscriptRecord` → `AdapterEvent`.** Records from the research doc §5 use uppercase enum-style strings. `id` for tool calls is synthesized as `format!("{}:{}", record.step_index, tool_call.name)` — deterministic and unique within a turn.

| Record shape | `AdapterEvent` emission |
|---|---|
| `source: "USER_EXPLICIT"`, `type: "USER_INPUT"` | None during live dispatch (UI already shows the user prompt). Hydration emits `ContentChunk { kind: Text }` with the un-wrapped user request (strip `<USER_REQUEST>` / `<ADDITIONAL_METADATA>` / `<USER_SETTINGS_CHANGE>` envelopes). |
| `source: "SYSTEM"`, `type: "CONVERSATION_HISTORY"` | None. Internal step. |
| `source: "MODEL"`, `type: "PLANNER_RESPONSE"`, `thinking: <text>` present | `ContentChunk { kind: ContentKind::Thinking, text }`. Emitted *before* the same record's text content, if any. Matches Gemini's surfacing of thoughts. |
| `source: "MODEL"`, `type: "PLANNER_RESPONSE"`, `content: <markdown>`, no `tool_calls` | **Live path:** None (stdout already streamed this text). **Hydration path:** `ContentChunk { kind: Text, text: content }`. Document the live-path skip with a comment so a future maintainer doesn't "fix" the perceived missing text. |
| `source: "MODEL"`, `type: "PLANNER_RESPONSE"`, `tool_calls: [{name, args}, ...]` | One `ToolStarted { tool_use_id: synthesized, name }` per tool_call, in order. |
| `source: "MODEL"`, `type: "RUN_COMMAND" \| "VIEW_FILE" \| <any CortexStep* tool result>`, `content: <pre-rendered text>` | `ToolCompleted { tool_use_id: matched-to-prior-ToolStarted, output: content, is_error: status == "FAILED" }`. Tool result ID matching: associate by order — the i-th tool result in the turn matches the i-th `ToolStarted`. Document this assumption; verify with a multi-tool-call probe during implementation. |
| Any record with `status: "FAILED"` and an auth-flavored error message | `TurnEnd { outcome: Failed { kind: AuthFailure, message } }`. Auth-flavored = matches the unauthed pattern from research doc line 106. |
| Any record with `status: "FAILED"` that is not auth-flavored | `TurnEnd { outcome: Failed { kind: HarnessError, message } }`. |
| Terminal `PLANNER_RESPONSE` with `status: "DONE"` and no `tool_calls` (model has nothing more to say) | `TurnEnd { outcome: Completed }`. The transcript-derived terminal is the source of truth; `agy`'s exit code is corroborating evidence. |
| Unknown record shape (new `CortexStep*` type, new status, etc.) | Log a debug warning and continue. Do not error. Antigravity's type vocabulary is large and growing (research doc line 244). |

If a record cannot fit any of the above (e.g., a `CortexStepMcpTool` with novel result shape), **stop and raise it before introducing a new `AdapterEvent` variant.** The wire-format vocabulary is harness-neutral; widening it is a separate design pass.

**6. Terminator semantics.** The tail loop reads until one of:
- A terminal `PLANNER_RESPONSE` with `status: "DONE"` and no `tool_calls` arrives → `Completed`.
- Any record with `status: "FAILED"` arrives → `Failed`.
- `child.try_wait()?` shows the child has exited AND 5 seconds pass with no new transcript records → `Failed { kind: HarnessError, message: "transcript missing terminal record after agy exit" }`.

Trusting `status: "DONE"` as a load-bearing terminator is a deliberate choice (and a documented brittleness — research doc line 245 notes other status values likely exist). The alternative — pure time-based termination — risks emitting a `TurnEnd` before reading the terminal record if the OS journal lags.

**7. Process-exit reconciliation.** `agy` is a single Go process. Exit code 0 = success; non-zero = error. Use exit code only to corroborate the transcript-derived outcome, not to override it. If exit code disagrees with the transcript (e.g., exit 0 but no terminal record), log a debug warning and prefer the transcript-derived outcome.

**8. Sidebar updates and `SessionMeta` emission.** Post-`TurnEnd`, emit a `SessionMeta` event carrying `model` (parsed from the `USER_INPUT` record's `<USER_SETTINGS_CHANGE>` envelope, or unknown if absent), `mcp_servers` (from M3B.4's loader, stubbed empty in this milestone), `skills` (same). The full sidebar wiring lands in M3B.4 and M3B.5; M3B.2 only needs to emit the event with the fields it can populate today.

### Definition of Done

- Unit tests covering each row of the record-to-event mapping table, with inline-constructed JSON records (no fixture chasing for shapes a live test will also exercise).
- Unit tests for the dual-source coordination:
  - Stdout produces `ContentChunk { Text }`; transcript tail produces no duplicate text emission for `PLANNER_RESPONSE.content` during live dispatch.
  - Transcript cursor: a tail invocation with cursor=N skips the first N records correctly.
- Unit test for the path-watch + tail loop using a `tempdir` that simulates `agy`'s file-creation timing: directory appears after 200ms, file appears after 300ms, lines append at 100ms intervals.
- Unit test for the early-child-exit path: child exits at 100ms while still polling for the conversation directory → emits `Failed { HarnessError }` immediately, not after 5s.
- Unit test for the auth-failure pattern match — both positive and a "looks similar but isn't auth" negative case.
- Live test: `agy -p "Reply with the single word 'ack'"` round-trips through the adapter and produces a `TurnEnd { Completed }` with at least one `ContentChunk { Text }` containing "ack" emitted from stdout.
- Live test: a follow-up prompt against the same agent (resume path) emits only the new turn's events, not the prior turn's.
- Live test: a `agy -p` invocation that triggers a tool call (e.g., "list files in current directory") produces `ToolStarted` + `ToolCompleted` events with matching `tool_use_id`s in correct order.
- The `transcript.jsonl` path constant has a comment naming `.system_generated/` as a Google-internal path that may change between Antigravity versions, with one line to update on a breakage.
- After this milestone, the `AppError::HarnessNotYetSupported` stub from M3B.1 is removed from the dispatch path.

---

## Milestone M3B.3 — Transcript hydration on project reopen

### Goal & Outcome

When a user reopens a project that has prior Antigravity agent activity, Switchboard reads the relevant `transcript.jsonl` files and reconstructs the per-agent turn history into the same normalized `Turn` shape the UI consumes from the other harnesses.

Functional outcomes:

- Reopening a project with past Antigravity agents shows the previous conversation in the unified transcript stream, attributed to the right agent.
- Antigravity turns interleave chronologically with Claude / Codex / Gemini turns from the same project session.
- A conversation that exists only as encrypted protobuf (no `transcript.jsonl` sidecar) is skipped silently — degrading display, not blocking project open. Log a single debug-level warning per skipped conversation.

### Implementation Outline

1. **Locate the transcript via the sidecar.** Read `AntigravitySidecar` for the agent. If sidecar is missing (agent never dispatched), return an empty `LoadedTranscript`. If present, use `sidecar.transcript_path` directly — no path recomputation.

2. **Read and parse `transcript.jsonl`.** Reuse the parser from M3B.2 — same record types. If the file is missing or empty (encrypted-only conversation, or sidecar pointing to a path that no longer exists), return an empty `LoadedTranscript` with a `SessionMeta` carrying loader-derived fields (MCP / skills from M3B.4) but no turns. Log debug warning; do not error.

3. **Normalize to `Turn`.** Group records by turn. Turn boundary = a new `USER_INPUT` record (`source: "USER_EXPLICIT"`, `type: "USER_INPUT"`). Each turn consists of one `USER_INPUT` followed by the `MODEL` and `SYSTEM` records until the next `USER_INPUT`. The terminal `PLANNER_RESPONSE` (no `tool_calls`, `status: "DONE"`) closes the turn.

   The cursor / boundary logic lives in **one function** used by both M3B.2 (live cursor advancement) and M3B.3 (hydration segmentation). If a multi-turn probe during M3B.2 surfaces a different boundary marker, update both call sites.

4. **Emit hydration events.** Each turn produces:
   - `ContentChunk { Text }` for the un-wrapped user request from `USER_INPUT.content` (strip XML envelopes).
   - `ContentChunk { Thinking }` for any present `thinking` fields.
   - `ContentChunk { Text }` for `PLANNER_RESPONSE.content` (unlike live dispatch — hydration has no stdout to replay).
   - `ToolStarted` / `ToolCompleted` for tool-call sequences.
   - `TurnEnd { Completed }` or `TurnEnd { Failed }` at the turn boundary.

### Definition of Done

- Unit tests: empty transcript file, single-turn transcript, multi-turn transcript, transcript with an unknown record type interleaved (must skip and continue), transcript that ends mid-record (truncated file), encrypted-only conversation (no transcript file — empty hydration, debug warning logged).
- Integration test using a real `transcript.jsonl` captured during M3B.2 live testing as a fixture under `crates/harness/tests/fixtures/antigravity/`.
- Live hydration test: run two prompts against a single agent, reopen the project, verify both turns hydrate in the right order with correct attribution.
- Document any known limitation (e.g., "encrypted-only conversations are silently skipped; users see them on project reopen as if they never happened") in a `Known limitations` section of `docs/research/antigravity-cli-observed.md`.
- **`fake_agy` fixture binary + producer-orchestration hardening (carried over from M3B.2).** Build a `fake_agy` test binary (mirrors `fake_codex`/`fake_gemini`) that creates `brain/<uuid>/`, writes `transcript.jsonl` with timed appends, and drips stdout — so the dual-source producer's orchestration becomes hermetically testable (UUID-capture-then-tail ordering, cursor advancement across drains, partial-line handling, the stdout-EOF-plus-exit terminator). The pure pieces (`correlate_conversation_dir`, `is_conversation_not_found`, `user_request_body`, `classify_outcome`) are already unit-tested; the fixture covers the end-to-end sequences that are currently `make test-live`-only.

  **Explicit acceptance criteria (not just "the fixture exists"):**
  - **Fork-and-heal** is the load-bearing one. `fake_agy` simulates a stale `--conversation <uuid>`: it prints `Warning: conversation … not found`, mints a *new* `brain/<new-uuid>/` whose `USER_INPUT` echoes the prompt, drips an answer, and exits. The test must assert: (a) the sidecar heals — its latest record is `<new-uuid>`, not the stale one; (b) the forked turn's tool/thinking events surface (the new transcript is tailed); (c) a *subsequent* dispatch resumes the healed `<new-uuid>` (passes `--conversation <new-uuid>`).
  - **Fast first-turn capture**: `fake_agy` exits immediately after dripping stdout but before the first poll tick could run; assert the post-exit final capture still persists the UUID.
  - **Unresumable**: `fake_agy` drips an answer but creates *no* matching `brain/` dir (simulating a transcript-path break); assert the turn fails `AdapterFailure` (unresumable), not a silent `Completed`.

  Also consider whether the expired-resume context loss warrants a user-visible signal (it currently only logs) — but do not widen the wire-format event vocabulary for it without a deliberate cross-harness design pass.

---

## Milestone M3B.4 — MCP + skills loaders + sidebar registry surfacing

### Goal & Outcome

The Antigravity sidebar row displays MCP servers and skills the same way Gemini's does today. Loader failure is non-load-bearing: missing config → empty list with no warning; unreadable config → empty list with a warning.

Functional outcomes:

- Sidebar's Antigravity row shows real MCP server names from `~/.gemini/config/mcp_config.json`.
- Sidebar's Antigravity row shows real skill names from `~/.gemini/config/plugins/<plugin>/skills/<skill>/SKILL.md`, displayed as `<plugin>/<skill>` (qualified).
- Workspace scope (project-scoped `.gemini/config/`) is **not** implemented in this milestone — research doc treats this as unverified. Document the limitation in `docs/research/antigravity-cli-observed.md`.

### Implementation Outline

1. **MCP loader** (`antigravity/config.rs`). Reads `~/.gemini/config/mcp_config.json` (user scope only in this milestone). JSON schema matches Gemini's `mcpServers` object. Comment: "Path is `~/.gemini/config/`, not `~/.gemini/`. Workspace scope is intentionally not implemented — not verified in research. Add and align merge direction (project-wins) if probed. Not factored into a shared module with Gemini because the schemas are identical but the paths and scopes differ; two short loaders is simpler than one parameterized one for ~30 days of overlap."

2. **Skills loader** (`antigravity/skills.rs`). Scans `~/.gemini/config/plugins/<plugin>/skills/<skill>/SKILL.md`. Skill display name is `<plugin>/<skill>` — disambiguates plugins that ship overlapping skill names. User scope only in this milestone (workspace plugins not verified).

3. **Inject into `SessionMeta`.** Same pattern as Gemini's `inject_session_meta_fields` — populate `harness_version`, `mcp_servers`, `skills` on the `SessionMeta` event emitted post-`TurnEnd` (introduced in M3B.2 as stubbed-empty).

4. **Frontend sidebar.** Antigravity row reads the same `SessionMeta` event shape; no Antigravity-specific frontend code beyond the harness-kind switch added in M3B.1.

### Definition of Done

- Unit tests covering: missing directory, unreadable directory, empty list, multiple plugins, plugin with multiple skills.
- Live tests assert structural `SessionMeta` field presence and types only — not specific values dependent on the developer's local config. Loader correctness is covered by tempdir-staged unit tests.
- `docs/research/antigravity-cli-observed.md` "Known limitations" section gains an entry for unverified workspace scope.

---

## Milestone M3B.5 — Frontend wiring + attach flow + final live tests + docs

### Goal & Outcome

Antigravity is a fully first-class harness in the UI: pick "Antigravity" in the harness picker, an agent spawns, transcript flows live, sidebar populates, project reopen hydrates, attach flow works for a pre-existing `agy` conversation, all banners render correctly.

Functional outcomes:

- A user with `agy` installed and authenticated can run an Antigravity agent end-to-end from the UI with no developer intervention.
- A user without `agy` sees a clear "Install Antigravity CLI from antigravity.google/download" banner.
- A user with `agy` but no Keychain entry sees the standard "subscription auth required" banner (already wired in M3B.1; verify end-to-end here).
- Attaching to an existing Antigravity conversation (by UUID) works — the sidecar is pre-populated; first dispatch resumes against the captured UUID.

### Implementation Outline

1. **Tauri command implementations.** Add `locate_antigravity_candidate`, `check_antigravity_session_id_unique`, plus the Antigravity arm in `attach_agent_impl` and `load_transcript_impl`. Each mirrors the Gemini equivalent with the path / probe-command differences from M3B.1–M3B.4. Auth and binary probes were already added in M3B.1; verify they're wired through to the Tauri command surface here.

2. **Attach flow.** Reads an existing Antigravity conversation UUID from the user, validates the conversation directory exists, pre-writes the Antigravity sidecar so the next dispatch resumes correctly.

3. **Sidebar parity.** The matrix from `docs/system-design.md` §7 needs an Antigravity row. Cost, quota, and context-window utilization cells render as "—" (Antigravity gives us no usage data). All three cells degrade gracefully in the existing sidebar layout (existing harnesses already mix availability).

4. **Live tests.** Cover the Tauri-command paths added in this milestone:
   - Attach to a real existing `agy` conversation by UUID; assert hydration + continued dispatch via the resumed UUID.
   - End-to-end dispatch via `dispatch_to_agent_impl` (not just the adapter directly); assert streamed events arrive on the Tauri event channel.
   - Auth-failure live test: **fixture-driven**, not destructive. Capture the unauthed stderr signature from the research doc (line 106 has the literal string) into a fixture; assert the auth-failure classifier emits `AuthFailure` against the fixture. No Keychain mutation.

5. **System-design doc updates.**
   - §7 sidebar matrix: add Antigravity column; cost / quota / context cells documented as "—".
   - §9 per-harness adapter table: add Antigravity row covering binary, headless flag, session-ID model (server-assigned + sidecar), resume mechanism, workspace-trust handling, permission flag (`--dangerously-skip-permissions`), session-file path, session-file format, stream event source (**dual: stdout + transcript tail**), auth path (Keychain), MCP config path, skills path.
   - §9 normalized event stream block: add a note that Antigravity is the first adapter where the live stream comes partially from a file tail (tool lifecycle) and partially from stdout (assistant text), with a one-line pointer to `antigravity-cli-observed.md` for why.
   - §11 deferred decisions: close the "Gemini → Antigravity transition" note as resolved, pointer to this plan.

6. **AGENTS.md update.** The "Authoritative docs" / `docs/research/` list gains an `antigravity-cli-observed.md` entry alongside the Gemini, Claude, and Codex entries.

### Definition of Done

- `make check` clean.
- `make test-live` includes the new Antigravity live tests and they pass on the developer's machine. No tests require destructive state changes.
- Manual smoke test: open Switchboard, create a project, spawn an Antigravity agent, send a prompt with a tool call (e.g., "list files in the current directory"), confirm tool start/end render, close and reopen the project, confirm the transcript hydrates correctly.
- PR description covers: the architectural difference between Antigravity and the other three harnesses (dual-source streaming, server-assigned UUID via sidecar), why we kept Gemini alongside, the known limitations (no cost surface, no quota surface, `.system_generated/` path brittleness, workspace MCP/skills scope unverified), and the live-test results.

---

## Open questions / known unknowns

Issues the research doc flagged as "unclear after probing" that this plan cannot fully resolve and may surface during implementation:

- **Permission behavior in `-p` mode without `--dangerously-skip-permissions`.** Print-mode appeared implicitly permissive in probing. If a tool call gets prompted-for-permission, the adapter needs to handle that — likely by always passing `--dangerously-skip-permissions` and documenting the risk-acceptance, matching Gemini's `--yolo`.
- **MCP tool-call envelope in the transcript.** Probing exercised only native tools. The MCP envelope (likely `CortexStepMcpTool` step type per research doc line 244) may have a slightly different shape than the native tool-call records; cover during M3B.2 implementation by testing with one real MCP server.
- **Tool-result ordering vs ID matching.** M3B.2's mapping assumes the i-th tool result matches the i-th `ToolStarted`. Verify with a multi-tool-call probe; if Antigravity ever interleaves out-of-order results, switch to a more robust matching scheme.
- **Workspace MCP / skills scope.** Whether Antigravity reads `<cwd>/.gemini/config/mcp_config.json` and `<cwd>/.gemini/config/plugins/...` analogously to Gemini's workspace configs. Not implemented in M3B.4; documented as a known limitation.
- **Keychain service name stability.** Pinned during M3B.1; fallback to `agy -p ping` probe documented.
- **Other `status` values beyond `DONE` / `FAILED`.** Research doc speculates `RUNNING` / `CANCELLED` exist. Unknown record types are logged and skipped; if `RUNNING` turns out to be a meaningful intermediate state, M3B.2's terminator logic may need refinement.

If any of these resolves to "we need to extend the wire-format event vocabulary," **stop and raise it.** That's a design decision for a separate pass, not a milestone fix.
