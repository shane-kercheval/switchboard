# Research: `codex app-server` vs `codex exec --json` — migration assessment

**Captured:** 2026-05-16
**Upstream version inspected:** codex `main` branch (≈ post-0.130.0, alphas through `0.131.0-alpha.22` on 2026-05-15). README pulled from `codex-rs/app-server/README.md@main` (~1950 lines), supporting sources from `codex-rs/protocol/src/protocol.rs`, `codex-rs/app-server-protocol/src/protocol/event_mapping.rs`, `codex-rs/exec/src/event_processor_with_jsonl_output.rs`, `codex-rs/app-server/tests/suite/v2/*.rs`, `codex-rs/app-server/tests/common/rollout.rs`, `codex-rs/app-server/src/main.rs`.
**Companion to:** [codex-cli-observed.md](codex-cli-observed.md) (current `exec --json` behavior), [harness-comparison.md (archived)](archive/harness-comparison.md) (normalization contract).
**Switchboard adapter under review:** `crates/harness/src/codex/` (mod 818 LOC, parser 936, session_file 969, sidecar 322, config 373, skills 219; events.rs ~679 — total ≈ 4.3k LOC including tests).

## TL;DR

**What is `codex app-server`?** A new mode (added in codex-cli v0.130, May 2026) where Codex runs as a **long-lived JSON-RPC server** instead of a one-shot subprocess per turn. One server process hosts N threads; clients send JSON-RPC requests over stdio (or a Unix socket) and get back a stream of notifications.

**Why does it matter for Switchboard?** The current `exec --json` adapter spawns a fresh subprocess for every turn and reads JSONL from stdout. That mode structurally cannot emit token-level streaming — the model's reply arrives as one big blob at the end. `app-server` was built specifically to fix this and emits live delta events for agent text, reasoning, and shell-command output.

**What else does app-server fix?** Beyond streaming, it eliminates several workarounds the current adapter pays for:

- `context_window` and `rate_limits` arrive in-stream → no more post-terminal session-file re-reads with retry loops.
- Cancellation is a typed `turn/interrupt` request → no more `killpg` + EOF-synthesis hacks.
- Auth failures are a typed `Unauthorized` enum variant → no more substring-matching `"401 Unauthorized"`.
- Reasoning / thinking blocks become visible → today they're encrypted in the session file.
- Native `thread/fork` and `thread/compact/start` → exec mode has neither.

**What does it cost?** A structurally different adapter shape: instead of spawning a subprocess per turn, Switchboard would supervise one long-lived server process, speak JSON-RPC (id allocator, response correlator, notification dispatch, server-initiated request handling for approvals), and demux notifications per `(threadId, turnId)`. Roughly the same total LOC inside `crates/harness/src/codex/`, but a different shape. The blast radius is contained to the codex adapter crate — dispatcher, app shell, frontend, and the `AdapterEvent` boundary do not change.

**Why wait?** Three reasons, in order of weight:

1. **The upstream API is moving fast.** 20+ PRs merged to `app-server` in the 9 days before this assessment; breaking changes landing weekly (permission-id rename, thread-permission-profile removal, `sessionId` moved onto `Thread`). About 20% of the surface is explicitly marked `[UNSTABLE]` / `experimental` / "do not call from production clients yet."
2. **The on-disk session-file format is identical between the two modes.** Sidecar, hydration, and the existing `~/.codex/sessions/YYYY/MM/DD/rollout-*-<thread_id>.jsonl` path layout all carry forward unchanged — meaning the migration can be deferred without locking us into anything. M2 isn't a dead end; it's the same store the future adapter would read.
3. **Two design assumptions are unverified.** Whether `~/.codex/auth.json` is shared cleanly between exec and app-server processes, and whether thread IDs created by `exec` can be resumed via `app-server`'s `thread/resume`. Both *should* work (shared `codex_home`, shared `RolloutRecorder`) but the public test suite doesn't certify either.

**When to revisit?** Either when a UX complaint about chunky long Codex turns makes streaming load-bearing, or when a second app-server-only capability is needed (typed `fileChange` rendering, `thread/fork`, programmatic compaction).

**Bottom line:** `app-server` is the right destination; today is the wrong day to leave. Wait for the upstream API churn to settle (1–2 minor Codex releases), then migrate.

---

## 1. Summary

**Recommendation: migrate later, not now.** The decision rests on a single factor: **streaming itself is real and is the one capability `exec --json` structurally cannot deliver** (see §2 — `exec`'s JSON processor has no match arm for the new delta events and discards them at the source), but **everything else in M2's adapter still works**, and `app-server` brings substantial migration weight (long-lived server lifecycle, JSON-RPC handshake, per-thread subscription accounting, an experimental surface where ~20% of the API is `[UNSTABLE]` / `experimental` / "**do not call from production clients yet**", and a `--listen ws://` transport that is explicitly "experimental and unsupported"). The on-disk rollout layout, sidecar-based session-id capture, MCP/skills config-loader, and `AdapterEvent` vocabulary all carry over unchanged — the migration is structural inside the spawn/dispatch loop, not at the event boundary. Right time to migrate: when Switchboard's UX is bottlenecked by chunky message arrival on long Codex turns, or when a second `app-server`-only capability (typed `fileChange` items, native `thread/fork`, `thread/compact/start`) becomes load-bearing. Until then, the M2 adapter is fit for purpose and the upstream API is still moving.

## 2. Event vocabulary diff

`exec --json` ground truth: §"Findings during M2.1" / §"Findings during M2.3" of [codex-cli-observed.md](codex-cli-observed.md). `app-server` ground truth: `codex-rs/app-server/README.md` §"Turn events" + §"Items" (lines 1209–1272), `codex-rs/protocol/src/protocol.rs` lines 1820–2070 (typed shapes), `codex-rs/app-server-protocol/src/protocol/event_mapping.rs` (the `item_event_to_server_notification` mapper that confirms which `EventMsg`s become app-server notifications).

**Critical structural finding (verified in source).** `codex-rs/protocol/src/protocol.rs:1838` and `:1863` define `impl HasLegacyEvent for AgentMessageContentDeltaEvent { fn as_legacy_events(..) -> Vec<EventMsg> { Vec::new() } }` (and the same for `ReasoningContentDelta`, `ReasoningRawContentDelta`). The bridge from new typed events to the legacy event vocabulary returns empty. `codex-rs/exec/src/event_processor_with_jsonl_output.rs` matches on `ServerNotification::ItemStarted | ItemCompleted | TurnStarted | TurnCompleted | TurnDiffUpdated | TurnPlanUpdated | ThreadTokenUsageUpdated | ConfigWarning | Error | DeprecationNotice | HookStarted | HookCompleted | ModelRerouted | ModelVerification` — and has **no arm for `ItemAgentMessageContentDelta`, `ItemReasoningContentDelta`, `ItemReasoningRawContentDelta`, `CommandExecutionOutputDelta`, `PlanDelta`, `FileChangePatchUpdated`**. So `exec --json` literally cannot emit token-level streaming today; the upstream design discards deltas before the JSONL writer sees them.

| Concept | `exec --json` shape (today) | `app-server` shape | Switchboard `AdapterEvent` mapping |
|---|---|---|---|
| Thread lifecycle | `thread.started {thread_id}` (one-shot per `codex exec` invocation) | `thread/started {thread: {...}}` notification after `thread/start`/`thread/resume`/`thread/fork` JSON-RPC response | Sidecar capture (current pattern); same role |
| Turn start | `turn.started {}` | `turn/started {turn: {id, status: "inProgress", items: [], ...}}` notification, **after** `turn/start` request returns | Dispatcher-owned `TurnStart` (synthesized; unchanged) |
| Agent text — full | `item.completed {item: {id, type: "agent_message", text: "<full>"}}` once per message | `item/started` (empty text) → 0..N `item/agentMessage/delta {thread_id, turn_id, item_id, delta}` → `item/completed {item: {type: "agentMessage", id, text: "<full>"}}` | Today: 1 `ContentChunk{Text, text=full}` from `item.completed`. With app-server: many `ContentChunk{Text, text=delta}` from delta notifications; the `item/completed` becomes idempotent / dropped (otherwise the full text would duplicate) |
| Reasoning summary | **No equivalent** in stream (encrypted in session file only) | `item/reasoning/summaryTextDelta {item_id, summary_index, delta}` + `item/reasoning/summaryPartAdded` | New mapping: `ContentChunk{Thinking, text=delta}` (the `ContentKind::Thinking` variant exists in `events.rs:21` reserved for exactly this) |
| Reasoning raw text | **No equivalent** | `item/reasoning/textDelta {item_id, content_index, delta}` (open-source models only) | `ContentChunk{Thinking, text=delta}` |
| Shell command — start | `item.started {item: {id, type: "command_execution", command, aggregated_output: "", exit_code: null, status: "in_progress"}}` | `item/started {item: {type: "commandExecution", id, command, cwd, status: "inProgress", commandActions}}` | `ToolStarted{Builtin, name: "command_execution", input: command}` — same |
| Shell command — stdout/stderr stream | **No equivalent** (only the final aggregated output) | `item/commandExecution/outputDelta {item_id, stream: "stdout"\|"stderr", chunk}` (base64-encoded) | New: emit interim `ToolCompleted`-style chunks, or extend `AdapterEvent` with a `ToolOutputDelta` variant; current `AdapterEvent` has no streaming-tool-output variant |
| Shell command — completion | `item.completed {item: {id, type: "command_execution", command, aggregated_output, exit_code, status}}` | `item/completed {item: {type: "commandExecution", id, aggregatedOutput, exitCode, durationMs, status}}` | `ToolCompleted{output, is_error}` — same; new fields `durationMs`/`commandActions` are extra metadata |
| File edit | Surfaces as `command_execution` (Codex invokes `apply_patch` via shell) | First-class `item/started`/`item/completed` with `item.type: "fileChange"`, `changes: [{path, kind, diff}]`, plus experimental `item/fileChange/patchUpdated` snapshots | New mapping; today these collapse into `ToolCompleted{command_execution}`. Could synthesize a `ToolStarted{name: "file_change"}` to preserve current rendering |
| MCP tool call — start | `item.started {item: {id, type: "mcp_tool_call", server, tool, arguments, result: null, error: null, status: "in_progress"}}` | `item/started {item: {type: "mcpToolCall", id, server, tool, status: "inProgress", arguments}}` | `ToolStarted{Mcp, name: "<server>.<tool>", input: arguments}` — same |
| MCP tool call — completion | `item.completed {item: {id, type: "mcp_tool_call", ..., result: {content, structured_content}, error, status}}` | `item/completed {item: {type: "mcpToolCall", id, ..., result, error, status}}` | `ToolCompleted{output, is_error}` — same (the camelCase/snake_case difference is the only wire change) |
| Plan updates | **No equivalent** | `turn/plan/updated {turnId, plan: [{step, status}]}` + per-item `item/plan/delta` (experimental) | No mapping today; could surface as a new `AdapterEvent::PlanUpdate` variant or drop |
| Token usage | `turn.completed {usage: {input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens}}` (no `context_window`, no cost) | `thread/tokenUsage/updated {info: {total_token_usage, last_token_usage, model_context_window}}` (separate notification stream, fires repeatedly during/after each turn; includes `model_context_window` directly — see `protocol.rs:2008` `TokenUsageInfo` def) | `TurnEnd.usage` (or a new `TokenUsageUpdate` event); `context_window` arrives in-stream so **post-terminal session-file enrichment for context_window becomes obsolete** |
| Rate limits | **No equivalent in stream** (session-file `token_count.rate_limits` only — see [codex-cli-observed.md](codex-cli-observed.md) §"Findings during M2.1") | `account/rateLimits/read` (request) + `account/rateLimits/updated` (push notification) — a separate, account-scoped channel, not turn-scoped | `RateLimitEvent` — fired by **account-scoped subscription**, not synthesized from session-file enrichment |
| Turn — success | `turn.completed {usage: {...}}` | `turn/completed {turn: {status: "completed", ...}}` | `TurnEnd{Completed}` — same |
| Turn — failure | `turn.failed {error: {message}}` (sometimes preceded by one or more `error` events with retry/reconnect text) | `turn/completed {turn: {status: "failed", error: {message, codexErrorInfo?, additionalDetails?}}}` plus pre-terminal `error` notifications with the same payload | `TurnEnd{Failed{...}}` — same, with a far richer `codexErrorInfo` enum (`ContextWindowExceeded`, `UsageLimitExceeded`, `HttpConnectionFailed{httpStatusCode}`, `Unauthorized`, `BadRequest`, `SandboxError`, etc. — see README §"Errors"). `Unauthorized` is the cleaner replacement for today's `401 Unauthorized`-substring match for `FailureKind::AuthFailure` |
| Turn — interrupted | **No first-class signal** (today: kill process group + observe "no terminal event before EOF" — see `mod.rs:617` `synthesize_truncation_turn_end`) | `turn/completed {turn: {status: "interrupted"}}` after `turn/interrupt` request | `TurnEnd{Completed}` with a third outcome state, OR `TurnEnd{Failed{kind: Cancelled, ...}}` — requires either a new `TurnOutcome::Interrupted` variant or reuse |
| Approval request (command/file) | Not surfaced (approvals disabled via `--dangerously-bypass-approvals-and-sandbox` per [codex-cli-observed.md](codex-cli-observed.md) §"Findings during M2.1") | `item/commandExecution/requestApproval` / `item/fileChange/requestApproval` server-initiated JSON-RPC requests; client responds `{decision: ...}` | No mapping needed in v1 (yolo flag still applies); future feature surface |
| Session metadata (init) | **No equivalent in stream** (read post-terminal from session-file `session_meta` + `turn_context` per [codex-cli-observed.md](codex-cli-observed.md) §"Findings during M2.4-prep") | `initialize` response carries `{user_agent, codex_home, platform_family, platform_os}`; `thread/start` response carries `{thread: {id, modelProvider, path, ephemeral, status, ...}}` | Today's `SessionMeta` event still requires config-loader for `mcp_servers`/`skills` (neither stream nor app-server carries that registry — see [codex-cli-observed.md](codex-cli-observed.md) §"MCP and skills registry sourcing"). `model` comes from `turn/start` overrides or `thread/started`'s persisted-default; `harness_version` no longer needs session-file enrichment because the `initialize` response carries `user_agent` (`"codex_vscode/0.130.0"` format per `initialize.rs` test). Post-terminal enrichment can shrink to "just config registry loading" |
| Compaction | **Not surfaced** (auto-only, no stream event) | `item/started`/`item/completed` with `item.type: "contextCompaction"`, plus deprecated `compacted {threadId, turnId}` | No mapping today; could surface as `AdapterEvent::Compaction{started, completed}` |
| MCP server startup | **No equivalent** | `mcpServer/startupStatus/updated {name, status: "starting"\|"ready"\|"failed"\|"cancelled", error}` | Could feed `SessionMeta.mcp_servers[].status` live instead of computing it from config files (today the status is always `"unknown"`) |
| Warnings | **No equivalent** (stderr or `Reading additional input from stdin...` line) | `configWarning {summary, details?, path?, range?}` and `warning {threadId?, message}` notifications | Could surface as `AdapterEvent::Warning` or log-only |

## 3. Architectural shape of app-server

**Process model.** `codex app-server` is a single long-lived subprocess that owns ALL threads, ALL turns, the MCP-server registry, and the configured authentication. It is the same binary as `codex` (`codex-rs/app-server/src/main.rs` — `arg0_dispatch_or_else` routes `argv[0]` and the subcommand selects app-server mode). One process per Codex installation, not one per agent.

**Transport** (`codex-rs/app-server/src/main.rs` `--listen` flag + README §"Protocol"):

- **`stdio://`** (default): newline-delimited JSON, one JSON-RPC message per line. Same shape Switchboard already reads from `exec --json`'s stdout — Tokio `BufReader::lines()` works identically.
- **`unix://[PATH]`**: websocket frames over `$CODEX_HOME/app-server-control/app-server-control.sock` (or a custom path). Uses an HTTP Upgrade handshake. `codex app-server proxy` is a built-in stdio<->unix-socket adapter, so a Tauri host could route either way without re-implementing the framing.
- **`ws://IP:PORT`**: WebSocket with HTTP `/readyz` and `/healthz` probes. **README says verbatim: "Websocket transport is currently experimental and unsupported. Do not rely on it for production workloads."** Avoid in v1.
- **`off`**: no local listener (for clients that embed the server in-process).

**JSON-RPC framing.** JSON-RPC 2.0 **without the `"jsonrpc": "2.0"` envelope field on the wire** (README §"Protocol"). Requests have `{method, id, params}`; responses have `{id, result}` or `{id, error}`; notifications have `{method, params}` and no `id`. The combination of requests-with-response, server-initiated requests (approvals, attestation, dynamic tool calls, user-input prompts — README §"Approvals"), and notifications means the transport is **fully bidirectional** — not just a server-push stream. A client implementation needs an id allocator, a response correlator (id → pending oneshot), and a notification dispatch table, exactly like an MCP client.

**Lifecycle.** Per-connection: `initialize` request (required first) → `initialized` notification → any other method. The `initialize.params.clientInfo.{name, title, version}` is recorded for OpenAI's Compliance Logs Platform (README §"Initialization" — "If you are developing a new Codex integration that is intended for enterprise use, please contact us to get it added to a known clients list"); `initialize.params.capabilities.optOutNotificationMethods` accepts an array of exact-match notification methods to suppress. Subsequent calls before `initialize` return `"Not initialized"`; repeated `initialize` returns `"Already initialized"`. Per-thread: `thread/start` (or `thread/resume`/`thread/fork`) → response auto-subscribes the connection → `turn/start` → stream of notifications → `turn/completed`. Threads remain loaded after their owning connection drops; `thread/unsubscribe` removes the connection's subscription but the server keeps the thread in memory for 30 minutes after the last subscriber drops, then emits `thread/closed`.

**Expected supervision pattern.** One server process per Switchboard install, supervised by the Tauri host. Reconnect on disconnect (the listener accepts new connections with the same `initialize` handshake; existing threads survive). README §"Protocol" backpressure: bounded internal queues; saturated ingress is rejected with JSON-RPC error code `-32001` ("Server overloaded; retry later.") which clients should retry with exponential backoff.

**Crash behavior.** Not documented in detail in the README. The "remote control" + "daemon-safe restart" PRs (#21831, #22877) on `main` indicate restart-with-state-recovery is an active area but not stable. The server stores thread state in sqlite (`thread/metadata/update` "patch sqlite-backed metadata"; `thread/list` `useStateDbOnly` "return from the state DB without scanning JSONL rollouts"), so process restart doesn't lose `thread.id` mappings — but in-flight turns die with the process.

## 4. Session/thread mapping

The good news: **on-disk rollout file format and path layout are identical between `exec` and `app-server`**. `codex-rs/app-server/tests/common/rollout.rs` defines `pub fn rollout_path(codex_home: &Path, filename_ts: &str, thread_id: &str) -> PathBuf` that returns `codex_home/sessions/YYYY/MM/DD/rollout-{filename_ts}-{thread_id}.jsonl` — verbatim the same shape M2.4's enrichment reads (`session_file.rs`). `codex-rs/core/src/rollout.rs` re-exports the same `RolloutRecorder` and `SESSIONS_SUBDIR` used by both modes, confirming a single writer. The README confirms `thread.path` on `thread/start` responses points at this file (null only if `ephemeral: true`).

**Thread-ID compatibility.** Thread IDs are UUIDs assigned by Codex on `thread/start` (same as `exec`'s first `thread.started` event). The schema is shared: the integration test helpers in `codex-rs/app-server/tests/suite/v2/thread_resume.rs` use `create_fake_rollout(codex_home, filename_ts, ...)` to write rollout JSONL files that `thread/resume` then reads back. **Cross-mode resume — exec-started session resumed via `thread/resume` and vice versa — is plausible because both modes share the rollout writer and reader, but is not exercised in the public test suite I read.** The README does not explicitly say "you may mix exec and app-server thread IDs"; the v0.130 release notes don't address it. **Would need to test directly** before relying on it.

**Mapping to Switchboard's sidecar.** Today's `SessionLinkRecord {session_id: String, session_partition_date: NaiveDate, started_at: DateTime<Utc>}` (`crates/harness/src/codex/sidecar.rs`) translates one-to-one:

- `session_id` = `thread.id` from `thread/start` / `thread/resume` response (replaces the first `thread.started` stream event)
- `session_partition_date` = `chrono::Local::now().date_naive()` on first dispatch (unchanged — partition is still local-date per [codex-cli-observed.md](codex-cli-observed.md) §"NEW: Codex partitions session files by local date, not UTC"); OR derive from `thread.path` (`.../sessions/2026/05/16/...`) which is cleaner because the path is canonical
- `started_at` = `Utc::now()` on first dispatch (unchanged)

The sidecar's role does NOT shrink, because the long-lived app-server doesn't persist Switchboard's per-agent → thread mapping for us. The sidecar is still the durable link between an `AgentId` and a Codex `thread_id` across Switchboard restarts.

**Switchboard's same-session-uniqueness invariant.** App-server allows multiple simultaneous client connections subscribed to the same `threadId` (each gets the same notifications). But Codex's underlying rollout-writer invariant is unchanged: one writer per session file, enforced upstream. To preserve Switchboard's "no two agents → same thread_id" rule, the agent registry stays as-is; the rule is enforced at agent-registration time and is independent of transport.

## 5. Per-question answers (Q1–Q10)

### Q1 — Full event vocabulary comparison

See table in §2 for the full diff. Summary:

- **app-server has, exec lacks:** `item/agentMessage/delta`, `item/reasoning/summaryTextDelta`, `item/reasoning/summaryPartAdded`, `item/reasoning/textDelta`, `item/commandExecution/outputDelta`, `item/plan/delta`, `item/fileChange/patchUpdated`, `turn/plan/updated`, `turn/diff/updated`, `thread/tokenUsage/updated` (with `model_context_window` in-stream), `account/rateLimits/updated`, `mcpServer/startupStatus/updated`, `model/rerouted`, `model/verification`, `configWarning`, `warning`, `deprecationNotice`, typed `fileChange` and `webSearch` and `imageView` and `contextCompaction` item types, server-initiated approvals (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `mcpServer/elicitation/request`, `item/permissions/requestApproval`, `attestation/generate`, `item/tool/call`, `item/tool/requestUserInput`), thread-lifecycle (`thread/started`, `thread/closed`, `thread/archived`, `thread/unarchived`, `thread/status/changed`, `thread/name/updated`, `thread/goal/*`), realtime audio events (`thread/realtime/*`), fuzzy file search events, hook lifecycle events. The pre-terminal `error` notification with `codexErrorInfo` discriminator is also new — today's adapter has to substring-match the message text (see [codex-cli-observed.md](codex-cli-observed.md) §"NEW: `turn.failed.error.message` has variable shape"); app-server replaces that with `Unauthorized` / `ContextWindowExceeded` / `UsageLimitExceeded` / `HttpConnectionFailed` enum variants.
- **exec has, app-server lacks:** **nothing of substance.** `exec`'s `thread.started`/`turn.started`/`turn.completed`/`turn.failed`/`item.started`/`item.completed`/`error` set is a strict subset of app-server's vocabulary. The `aggregated_output` and `exit_code` fields on `command_execution` `item.completed` are present in both (just camelCase in app-server: `aggregatedOutput`, `exitCode`).
- **No-equivalent / drop:** `exec`'s `Reading additional input from stdin...` stderr preamble (cosmetic only, not in app-server because no stdin involvement); `exec`'s exit code semantics (app-server runs as a server, exit codes are no longer per-turn).

### Q2 — Process model / supervision / transport

Process model: **long-lived single subprocess** (`codex app-server`) that owns N threads and N concurrent turns. NOT per-turn. NOT per-agent. The `codex-rs/app-server/src/main.rs` listener loop accepts multiple client connections and keeps threads alive for 30 minutes after the last subscriber unsubscribes.

Supervision expectations: README is silent on detailed crash/restart guarantees. The presence of PR #21831 "[exec-server] serve websocket listener via HTTP upgrade" and PR #21963 "[exec-server] serve websocket listener via HTTP upgrade" and the `codex remote-control` entrypoint (added in v0.130, see WebSearch — [Codex CLI v0.130 with remote-control](https://jls42.org/en/news/ia-actualites-10-may-2026)) tell us upstream is actively working on this layer, which means the supervision contract is **not stable yet**.

Transport: stdio (default), Unix domain socket (websocket framing over a socket; the recommended local control-plane), or ws://IP:PORT (explicitly experimental, do not rely on it). For Switchboard's "ride defaults" stance, **stdio is the right pick** — same posture as today, no socket lifecycle management, no port conflicts, no auth-on-the-wire.

### Q3 — Session/thread management

**Same thread abstraction as exec.** Threads are UUIDs; rollouts are written to `~/.codex/sessions/YYYY/MM/DD/rollout-<filename_ts>-<thread_id>.jsonl` by the same `RolloutRecorder` (`codex-rs/core/src/rollout.rs` re-exports `codex_rollout::RolloutRecorder` used by both modes; the test helper `codex-rs/app-server/tests/common/rollout.rs` `rollout_path()` writes to the identical path the existing M2.4 adapter reads).

Create new: `{"method": "thread/start", "id": N, "params": {model?, cwd?, sandboxPolicy?, ...}}` → response carries `{thread: {id, path, ephemeral, status, ...}}`. New `thread/started` notification fires after the response.

Resume existing: `{"method": "thread/resume", "id": N, "params": {threadId, excludeTurns?}}`. Response shape matches `thread/start`; rollout history reconstructed from the on-disk JSONL.

**Cross-mode resume (exec ↔ app-server) is plausible but unverified.** Both modes write/read the same rollout files, but the integration test suite I read (`thread_resume.rs`) only exercises app-server → app-server. **Recommend testing this directly before depending on it** for migration (e.g., resume a thread Switchboard created with M2's `codex exec` from a v1.x release in the new `app-server`-based adapter — should work, but the source doesn't certify it).

Same `~/.codex/sessions/YYYY/MM/DD/rollout-*-<thread_id>.jsonl` writes: yes (`thread.path` is set to that exact path on `thread/start` responses, null only when `ephemeral: true`). So Switchboard's hydration-on-project-open path keeps working unchanged; the file shape (`session_meta` line 1, `event_msg/*`, `response_item`, `turn_context`) is the same writer.

### Q4 — Cancellation

Explicit cancel request: `{"method": "turn/interrupt", "id": N, "params": {threadId, turnId}}` → `{id: N, result: {}}` immediately, then the server eventually emits `turn/completed {turn: {status: "interrupted"}}`. README §"Example: Interrupt an active turn": "Rely on the `turn/completed` event to know when turn interruption has finished." Verified in `codex-rs/app-server/tests/suite/v2/turn_interrupt.rs:126`: `assert_eq!(completed.turn.status, TurnStatus::Interrupted);`.

This is **much cleaner** than today's "kill process group + watch for missing terminal event" pattern (`crates/harness/src/codex/mod.rs:617` `synthesize_truncation_turn_end`). Cancelling one turn does NOT kill the server (the server stays up for the other N-1 threads and any other client). README also notes: "This does not terminate background terminals; use `thread/backgroundTerminals/clean` when you explicitly want to stop those shells" — important for tools the model launched in the background.

Partial output disposition: per README §"Example: Steer an active turn" and §"Example: Interrupt an active turn", any deltas emitted before the cancel still arrive; the final `turn/completed` carries `status: "interrupted"` and whatever `items` accumulated. The integration test `turn_interrupt_resolves_pending_command_approval_request` (`turn_interrupt.rs:208`) confirms that pending approval requests are resolved (cancelled) when the turn is interrupted — server cleans up the server-initiated request half too.

### Q5 — Concurrency

**One app-server can handle N concurrent in-flight turns across N different threads.** README §"API Overview" describes each thread as independently subscribable; `thread/loaded/list` returns "thread ids currently loaded in memory"; `turn/interrupt` is scoped to `(threadId, turnId)`. Each thread has at most one active turn at a time — `turn/steer` "rejects... if there is no active turn, `expectedTurnId` does not match the active turn, or the active turn kind does not accept same-turn steering"; `turn/start` on a thread that already has an active turn would return an error (`ActiveTurnNotSteerable`). So the constraint is **one in-flight turn per thread, N threads in parallel** — exactly Switchboard's per-agent concurrency model. No structural problem.

Backpressure: bounded queues, JSON-RPC error `-32001` "Server overloaded; retry later" with exponential-backoff guidance. README §"Protocol".

### Q6 — Auth

`app-server` supports both auth modes:

- **`apikey`**: caller posts an API key via `{"method": "account/login/start", "params": {"type": "apiKey", "apiKey": "sk-..."}}`. Persisted by app-server.
- **`chatgpt`** (subscription, the v1 mode): `account/login/start` with `type: "chatgpt"` (browser OAuth flow, opens a `chatgpt.com/...` URL with localhost callback) or `type: "chatgptDeviceCode"` (display verification URL + code, poll for completion). Tokens persisted to disk and refreshed automatically.

**Compatibility with Switchboard's existing `~/.codex/auth.json`.** Not directly stated in the README, but the design strongly implies it: `codex_home` is shared across modes (`initialize` response returns the same `codexHome` the exec mode uses; `codex-rs/app-server/src/main.rs` uses the same `Arg0DispatchPaths` and `codex_config::LoaderOverrides` as `codex exec`). A user already logged in via `codex login` from the terminal should have their `chatgpt` session immediately visible to the app-server. **Verify with a live probe** before depending on it: spawn `codex app-server`, send `{"method": "account/read"}`, expect `{"account": {"type": "chatgpt", "email": "...", "planType": "..."}}` for an already-logged-in user, with no `account/login/start` flow triggered.

If the verify confirms shared auth: subscription auth is fully supported, no API-key requirement, no Switchboard-orchestrated login UX needed. v1's "subscription only" stance survives the migration cleanly.

If the verify fails (app-server requires its own login flow): Switchboard would need to either pre-instrument the OAuth handshake or fall back to "user runs `codex login` first" with a banner — but still no API-key requirement is introduced.

### Q7 — Stability signals

**Mixed.** The core thread/turn/item API is the production interface for OpenAI's own VS Code extension and is mostly stable. But the README is dotted with `[UNSTABLE]`, `experimental`, and "do not call from production clients yet" markers:

- `item/autoApprovalReview/started`, `item/autoApprovalReview/completed`: explicitly `[UNSTABLE]` — "This shape is expected to change soon" (README §"Events" / §"Items").
- `review` field shape: `[UNSTABLE]`.
- `plugin/list`: "**under development; do not call from production clients yet**" (README §"API Overview", line ~204).
- `plugin/uninstall`: "**under development; do not call from production clients yet**".
- `thread/turns/items/list`: "experimental; reserved for paging full items for one turn. The API shape is present, but app-server currently returns an unsupported-method JSON-RPC error" — defined but not implemented.
- `thread/realtime/*` (WebRTC, audio): all marked experimental.
- `process/spawn` family: all marked experimental.
- WebSocket transport: "experimental and unsupported. Do not rely on it for production workloads."
- `dynamicTools`, `collaborationMode`, `memoryMode`, `environments`, `goal/*`: experimental, gated by `initialize.params.capabilities.experimentalApi = true`.
- Many `experimentalApi`-gated fields throughout `thread/start`/`turn/start`.

The events Switchboard would actually consume (`thread/started`, `turn/started`, `turn/completed`, `item/started`, `item/completed`, `item/agentMessage/delta`, `item/reasoning/*`, `item/commandExecution/outputDelta`, `thread/tokenUsage/updated`, `account/rateLimits/updated`) are NOT marked experimental. So the surface Switchboard would touch is in the stable subset — but the surface is **evolving fast**: 20+ app-server PRs merged in the 9 days between v0.130.0 (2026-05-08) and the date this doc was captured (2026-05-16). PR titles like "feat(app-server): update remote control APIs for better UX" (#22877, 2026-05-15), "app-server: stop returning thread permission profiles" (#22792, 2026-05-15), "app-server: use permission ids and runtime workspace roots" (#22611, 2026-05-15) show real ongoing shape changes. **Version-pinning is critical** — and the README's first-class versioning support (`codex app-server generate-ts --out DIR` / `generate-json-schema --out DIR` "each output is specific to the version of Codex you used to run the command, so the generated artifacts are guaranteed to match that version") tells you OpenAI assumes clients pin per version.

Recent breaking changes: PR #22792 ("stop returning thread permission profiles" — 2026-05-15), PR #22611 ("use permission ids and runtime workspace roots" — 2026-05-15), PR #21336 ("move v2 `sessionId` onto `Thread`" — 2026-05-06). All within ~10 days of this assessment.

The README and source do not specify a deprecation policy. Pin to a tested version, rebuild test fixtures on each Codex CLI bump, run `make test-live` after every Codex upgrade.

### Q8 — Session-file enrichment becomes obsolete?

**Mostly yes. Substantially obsolete; not fully obsolete.**

- **`context_window` (today: enriched from session-file `task_started.model_context_window`)** — `app-server`'s `thread/tokenUsage/updated` notification carries `info.model_context_window` directly (`protocol.rs:2013`). **Enrichment for context_window can be deleted.**
- **`cli_version` / `harness_version` (today: read from session-file `session_meta.cli_version`)** — `app-server`'s `initialize` response returns a `user_agent` string of the form `"codex_vscode/0.130.0"` (per `tests/suite/v2/initialize.rs:55` `assert!(user_agent.starts_with("codex_vscode/"));`). **Enrichment for harness_version can be deleted** (parse the user-agent suffix). Or read the platform fields from the same response.
- **`rate_limits` (today: read from session-file `token_count.rate_limits`)** — `app-server`'s `account/rateLimits/updated` notification carries the same shape (`primary`, `secondary`, `usedPercent`, `windowDurationMins`, `resetsAt`, `rateLimitReachedType` per README §"7) Rate limits (ChatGPT)"). **Enrichment for rate_limits can be deleted.**
- **`model` (today: read from session-file `turn_context.model` per [codex-cli-observed.md](codex-cli-observed.md) §"NEW: `model` lives in per-turn `turn_context`")** — `thread/start` accepts `model` in params; the persisted default is reflected in `thread/started` notification. **Enrichment for model can be deleted.**
- **`mcp_servers` and `skills` (today: read from `~/.codex/config.toml` + `<cwd>/.codex/config.toml` + `~/.agents/skills/` + `<cwd>/.agents/skills/`)** — `app-server` exposes `skills/list` (read-only, with `forceReload` option) and the `mcpServer/startupStatus/updated` notification (per-server status). The config-file loader **could** stay (display-only registry display works as-is) OR Switchboard could call `skills/list` and listen to `mcpServer/startupStatus/updated`. The latter is richer (live status, not just "configured") but adds protocol surface. Pragmatic choice for v1 migration: keep `crates/harness/src/codex/config.rs` and `skills.rs` unchanged — they don't depend on `app-server` and continue to work whether the dispatch path uses `exec` or `app-server`.

Net: **`session_file.rs` (969 LOC) can shrink to nearly zero** — only the disk-rehydration path for hydrating prior transcripts on project open still needs the session-file parser. The post-terminal enrichment cycle (`mod.rs:434-542` `emit_terminal_with_enrichment`) collapses to ~5 lines that consume in-stream notifications instead of re-reading disk.

### Q9 — MCP tool behavior

Same shape, different case convention. `exec --json` emits `item.completed {item: {id, type: "mcp_tool_call", server, tool, arguments, result, error, status}}` (snake_case `item.type`, snake_case fields). `app-server` emits `item/completed {item: {type: "mcpToolCall", id, server, tool, arguments, result, error, status}}` (camelCase tag, same field names because `server`/`tool`/`arguments`/`result`/`error`/`status` are already lowercase one-word). The semantic content is identical:

- `server` = MCP server name
- `tool` = tool name within that server
- `arguments` = JSON object of inputs
- `result` = `{content: [{type, text}], structured_content?}` on success
- `error` = error blob on failure
- `status` = `"inProgress"` (app-server) / `"in_progress"` (exec) / `"completed"` / `"failed"`

Today's parser produces `name: format!("{server}.{tool}")` (`crates/harness/src/codex/parser.rs:130`) — no change needed. The only adapter-side change is the case convention (`item.type: "mcp_tool_call"` vs `item.type: "mcpToolCall"`, status `"in_progress"` vs `"inProgress"`).

`app-server` also adds a separate, server-initiated `mcpServer/elicitation/request` flow when an MCP server needs the user to fill out a form or open a URL mid-call (README §"MCP server elicitations"). v1 doesn't surface this; the adapter would decline these requests by default (`{action: "decline"}`) until M-something adds the UI.

### Q10 — Migration cost estimate

**Medium rewrite**, not "same shape with different transport." The structural changes:

1. **Replace the per-turn subprocess spawn with a connection manager** to a singleton long-lived `codex app-server` process. Spawn the server lazily on first dispatch; keep it alive across all turns; reconnect on disconnect; supervise via the Tauri host. New module: `crates/harness/src/codex/app_server.rs` (~300–500 LOC for the JSON-RPC framing + reconnect + id allocator + per-connection state). This module owns the `initialize` handshake once per process lifetime.
2. **Replace `build_args()` + `tokio::process::Command` per dispatch with `turn/start` JSON-RPC requests** demuxed by `(threadId, turnId)`. The dispatch function becomes "send `turn/start`, register a notification listener for this thread, forward events into the `EventStream` channel, await `turn/completed` notification". New struct: a per-thread notification subscriber that the connection manager routes notifications into.
3. **`sidecar.rs` stays, with one field change**: `session_id` is still the `thread_id`; `session_partition_date` is now derived from `thread.path` (the rollout-file path the server returns on `thread/start`) rather than `chrono::Local::now()` — cleaner because the path is canonical. `started_at` unchanged. The fail-loud "missing or non-string thread_id" path (`mod.rs:287-301`) becomes "thread/start response missing thread.id field" — same semantics. **Sidecar code: ~30 LOC of change, not deletable.**
4. **`session_file.rs` shrinks dramatically**. Post-terminal enrichment (`emit_terminal_with_enrichment`, `load_with_retry`, the 200ms+200ms retry, the path-construction helpers) all delete. The disk-rehydration parser (for project-open transcript loading — M2.6 territory) stays because it's reading the same on-disk format. **~700 LOC deletable from `session_file.rs`**.
5. **`parser.rs` rewrites against the new notification vocabulary**. The line-oriented `serde_json::from_str` plus `match value.get("type").and_then(Value::as_str)` shape becomes `match notification.method.as_str()` over `"thread/started" | "turn/started" | "turn/completed" | "item/started" | "item/completed" | "item/agentMessage/delta" | "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" | "item/commandExecution/outputDelta" | "thread/tokenUsage/updated" | "account/rateLimits/updated"`. Per-notification structs deserialize directly via `codex-app-server-protocol`'s exported TS-aligned types (or hand-rolled, depending on whether Switchboard wants the upstream Rust dep). **~600 LOC rewritten, mostly mechanical**. Plus a new buffer-per-item accumulator for delta concatenation (`item_id -> StringBuilder`) so `ContentChunk` events can be either per-delta or coalesced.
6. **`config.rs` and `skills.rs` stay unchanged** — display-only config-file reading, no Codex CLI involvement.
7. **`events.rs` extensions** (or not, depending on streaming UX). To preserve today's UI fidelity: minimum-change is to keep emitting one `ContentChunk` per delta (UI accumulates by `turn_id` — already implied by `text: String` carrying a chunk). If Switchboard wants tool-output streaming, add `AdapterEvent::ToolOutputDelta {turn_id, tool_use_id, chunk}` and the UI updates the in-progress tool card. `ContentKind::Thinking` (`events.rs:21`) finally fires for real (reasoning deltas), no new variant needed.
8. **Cancellation rewrites**. `cancel_turn(turn_id)` becomes a JSON-RPC `turn/interrupt` request rather than process-group kill. The `synthesize_truncation_turn_end` path (`mod.rs:617`) — the "stream ended without terminal event" synthesis — almost entirely deletes: connection-loss is now a connection-manager concern, not a per-turn concern; turn timeouts are observed by waiting for `turn/completed` with `status: "interrupted"` after sending `turn/interrupt`.

Rough sizing: **−600 LOC from `session_file.rs`, −500 LOC of subprocess/sidecar mechanics from `mod.rs`, +400 LOC for the app-server connection module, +600 LOC of new parser code, +200 LOC for the per-turn JSON-RPC request/notification correlator. Net: roughly the same total LOC (~3500 → ~3500), but structurally different shape.** Tests must be reworked end-to-end (the existing `fake_codex` fixture binary that streams JSONL would be replaced with a `fake_app_server` fixture binary that speaks JSON-RPC over stdio, or a mock connection that the connection manager talks to in-process).

Live-test cost: the existing `tests/live.rs` suite (small prompts → "ack") translates 1:1 — spawn `codex app-server`, initialize, `thread/start`, `turn/start`, await `turn/completed`. Per-test cost unchanged.

## 6. Migration sketch

Structural changes only (no code):

- **`CodexAdapter::dispatch` (`mod.rs:110`)** changes shape from "spawn `tokio::process::Command::new(codex_binary_path).args(build_args(...))` per turn" to "acquire a connection to the singleton `app-server` (creating it if absent), send `turn/start` JSON-RPC, register a per-turn notification subscriber, return the EventStream backed by that subscriber". The `force_session_meta` / `is_first_dispatch_after_attach` parameter survives because the SessionMeta emission gate (sidebar registry) is independent of transport.
- **`build_args()` (`mod.rs:200`)** deletes entirely. Replaced by a struct that produces `ThreadStartParams` / `TurnStartParams` JSON-RPC payloads.
- **`run_producer()` (`mod.rs:237`)** restructures: no more stdout reader; instead a notification-pump task that demuxes server notifications onto per-turn channels by `(threadId, turnId)`. The `lines.next_line().await` loop becomes "await next notification matching our `(threadId, turnId)` subscription".
- **`try_persist_sidecar()` (`mod.rs:577`)** survives, called once after the `thread/start` response yields `thread.id`. The `chrono::Local::now().date_naive()` heuristic can be retired in favour of parsing the date out of `thread.path` (the rollout file path the server returns).
- **`synthesize_truncation_turn_end()` (`mod.rs:619`)** mostly deletes. Connection-loss is a connection-manager concern, not per-turn synthesis. The only remnant: a "connection dropped mid-turn" failure path that emits `TurnEnd{Failed{AdapterFailure, "app-server connection lost"}}` — much simpler than today's stderr-tail-tracking machinery.
- **`emit_terminal_with_enrichment()` (`mod.rs:458`)** collapses. `usage` arrives on `turn/completed` with `model_context_window` already in-band; `RateLimitEvent` is fed by an independent `account/rateLimits/updated` subscription that the connection manager owns (not per-turn); `SessionMeta` emission for first-dispatch-after-attach is unchanged (config-loader stays the same source of truth for `mcp_servers`/`skills`); `model` and `harness_version` come from the `thread/started`/`initialize` responses respectively, not from disk.
- **`stderr_task` (`mod.rs:254`)** deletes — there's no per-turn stderr drain. The app-server itself runs with `LOG_FORMAT=json` (README §"Tracing/log output") which can be teed to the Switchboard log writer if useful.
- **`process_group(0)` / `kill_subprocess_group`** delete — no per-turn subprocess to kill. The single long-lived server process the Tauri host supervises has its own lifecycle.
- **Per-agent concurrency invariant** preserved trivially: each agent maps to one thread; one in-flight turn per thread is enforced by the server (`ActiveTurnNotSteerable`); N parallel agents = N parallel `(threadId, turnId)` subscriptions on one connection. No per-agent process accounting needed any more.
- **Sidecar field semantics**: `session_id` and `session_partition_date` unchanged. Add (optionally) `thread_path: Option<PathBuf>` cached from the `thread/start` response so the enrichment path doesn't need to reconstruct it.
- **`AdapterEvent::TurnEnd.usage`** no longer needs the post-terminal enrichment overlay because `model_context_window` is in-band. The `apply_context_window` helper (`mod.rs:557`) deletes.
- **Hydration path (M2.6)** unchanged. The session-file parser keeps reading the same on-disk JSONL files. Live dispatch routes through the new app-server connection; disk rehydration routes through the existing parser.

Magnitude: **medium rewrite**. The mechanical line count cancels out (deletions in enrichment + spawn loop roughly balance the new connection-manager code), but the design shift (per-turn subprocess → singleton server connection) requires getting reconnect semantics, backpressure, and crash-recovery right. The blast radius is contained inside `crates/harness/src/codex/` — `crates/dispatcher/`, `crates/app/`, frontend, and the `AdapterEvent` boundary do not change shape. Fixture rework + live-testing are part of the scope.

## 7. Pros / cons / open questions

### Pros (vs. today's `exec --json`)

- **Token-level streaming** of agent text and reasoning — the one thing `exec --json` structurally cannot do because `exec`'s JSONL processor has no match arm for `AgentMessageContentDelta` (verified in `codex-rs/exec/src/event_processor_with_jsonl_output.rs`).
- **`context_window` in-stream** via `thread/tokenUsage/updated` → eliminates post-terminal session-file enrichment for the field, eliminates the 200ms+200ms retry that today's adapter does to wait for the file flush.
- **`account/rateLimits/updated` push notifications** → no need to scan `token_count.rate_limits` in the session file; rate-limit display updates live.
- **Native cancellation** via `turn/interrupt` with `turn/completed{status: "interrupted"}` — replaces "kill process group + observe missing terminal event" workaround, including the EOF synthesis path.
- **Structured `codexErrorInfo` discriminator** (`Unauthorized`, `ContextWindowExceeded`, `UsageLimitExceeded`, `HttpConnectionFailed{httpStatusCode}`, etc.) → replaces today's fragile `"401 Unauthorized"`-substring matcher for `FailureKind::AuthFailure` and the variable-shape `turn.failed.error.message` unwrap. Each Codex error case becomes typed.
- **Per-tool-output streaming** for shell commands (`item/commandExecution/outputDelta`) — long-running commands surface progress live.
- **Reasoning visibility** for the first time — Switchboard can render thinking blocks via `ContentKind::Thinking` (already reserved). Today they're encrypted in the session file and invisible.
- **Native `thread/fork`** — Codex's M2-known gap (no `codex exec fork`, see [codex-cli-observed.md](codex-cli-observed.md) §"Things still worth probing") is solved at the JSON-RPC level.
- **Native `thread/compact/start`** — programmatic compaction trigger that `exec` lacks entirely.
- **No per-turn subprocess spawn cost** — for short turns this is a real saving (Node-startup latency for the parent + Rust child startup); for long turns it's noise.
- **Typed `fileChange` items** (with diff content) — better UI rendering than today's "everything looks like a shell command running apply_patch".
- **The `~/.codex/sessions/*` on-disk layout is unchanged** — hydration-on-open path keeps working, and a Switchboard install can theoretically run with M2 (`exec`-mode) for some users and a future `app-server`-mode for others without diverging the session-file store.

### Cons

- **Substantially larger upstream surface area.** README is ~1950 lines; the v2 protocol crate exposes 100+ JSON-RPC methods. Switchboard would consume maybe 10 of them, but the dependency footprint grows.
- **Singleton process supervision** — Switchboard now owns a server-lifecycle problem (start, restart-on-crash, shutdown-on-app-quit, version-mismatch detection) that today doesn't exist. The Tauri host gains a daemon-supervision responsibility.
- **Active rewrite of upstream surface** — 20+ app-server PRs in the 9 days before this assessment; recent breakers like #22792 (permission profiles), #22611 (permission ids), #21336 (`sessionId` on Thread). Version-pinning is mandatory and the test-fixture-bump cost on every Codex bump becomes real.
- **~20% of the API is explicitly experimental or `[UNSTABLE]`**, including realtime, dynamic tools, collaboration modes, memory mode, plugins, marketplaces. Switchboard would have to be careful not to leak experimental fields into the v1 surface inadvertently.
- **JSON-RPC framing complexity** — id allocator, response correlator, server-initiated request handling (approvals, attestation, dynamic-tool calls), notification dispatch, exponential backoff on `-32001` backpressure. Today's "read JSONL from stdout" is markedly simpler.
- **Cross-mode resume not certified** — exec-started thread IDs ought to be resumable in app-server (and vice versa) because both modes share the rollout writer, but the public test suite doesn't exercise it. Switchboard users with M2-era persisted sessions would be in undefined territory until verified.
- **WebSocket transport experimental/unsupported** — the only way to safely run the server is stdio or unix socket. ws://IP:PORT is off the table for v1.
- **Subscription-auth path not explicitly verified** to share `~/.codex/auth.json` with the exec mode. Strong design implication that it does (shared `codex_home`, same `arg0_dispatch`), but uncertain until probed.
- **Server-initiated approval requests** (command, file change, MCP elicitation, dynamic tools, permissions) — Switchboard would have to either decline-by-default (preserve today's yolo-flag posture) or build the approval UI. Adds protocol surface even if v1 declines everything.

### Open questions

- **Does `account/read` against a freshly-spawned `codex app-server` immediately reflect a `~/.codex/auth.json` session created by `codex login` from the terminal?** README is silent on cross-process sharing of auth state; the design strongly implies yes (shared `codex_home`), but not verified. Single small live probe answers it.
- **Cross-mode resume** — can a thread_id captured by an M2 (`exec`) Switchboard install be resumed by a future (`app-server`) install via `thread/resume`? Source design says yes (same `RolloutRecorder`); public test suite doesn't certify it. Single live probe answers it.
- **`turn/completed.turn.error`'s exact shape and the `codexErrorInfo` variant set in 0.130.0** — protocol.rs lists the variants but the README's `codexErrorInfo` documentation is short. Need to capture failure-path fixtures (auth failure, invalid model, context-exceeded) to confirm the wire shapes before relying on enum discriminants.
- **App-server restart-with-state-recovery semantics** — PRs #21831 ("daemon-safe restart handling") and #22877 ("update remote control APIs") are landing; what does Switchboard see if the server crashes mid-turn? README is silent. Important for supervision design.
- **Backpressure under N concurrent agents** — README documents `-32001` "Server overloaded; retry later" with exponential-backoff guidance, but no observed thresholds. Need to live-probe with N=5+ concurrent turns to verify Switchboard's dispatcher behaves sensibly when the server pushes back.
- **Notification ordering across threads** — README says notifications are per-connection. If two threads' deltas arrive interleaved on the same connection, the connection manager has to demux fairly. Source isn't explicit about fairness guarantees; need to verify under live load that one busy thread can't starve another.
- **`session_meta` writer changes between exec and app-server** — both modes use the same `RolloutRecorder`, but the `session_meta.payload.originator` field is `"codex_exec"` for `exec` and presumably `"codex_vscode"` (or the `clientInfo.name`) for app-server. Switchboard's `--session-source` argument in app-server defaults to `"vscode"`; need to verify hydration doesn't trip on the changed originator (it shouldn't — the field is informational, not load-bearing in [codex-cli-observed.md](codex-cli-observed.md) §"NEW: `session_meta` does NOT carry model / tools / mcp_servers").
- **`thread.path` vs sidecar `session_partition_date`** — `thread.path` would be the canonical source going forward; should the sidecar add a `thread_path: PathBuf` field, or should `session_partition_date` be derived on-demand from path? Affects migration of existing sidecars.
- **Stability of `app-server`'s `thread/tokenUsage/updated` vs `turn/completed.usage`** — both carry usage. Which one is canonical for the per-turn accounting Switchboard already displays? README implies `thread/tokenUsage/updated` is the live source and `turn/completed` carries the final snapshot; need to confirm by capture which fires last.
- **Will OpenAI deprecate `exec --json` once app-server is the recommended path?** No deprecation signal in README or release notes today. If yes, the migration window matters; if no, the M2 adapter can live indefinitely. The fact that PR #22343 ("feat(exec-server): use protobuf relay frames", 2026-05-13) is still adding features to the exec server suggests no near-term deprecation.

## Sources

- `codex-rs/app-server/README.md@main` — primary protocol reference (~1950 lines): https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md
- PR #5546 "Add item streaming events" (merged): https://github.com/openai/codex/pull/5546 — introduces `AgentMessageContentDeltaEvent`, `ReasoningContentDeltaEvent`, `ReasoningRawContentDeltaEvent`; their `HasLegacyEvent` impls return empty vecs (verified in `codex-rs/protocol/src/protocol.rs:1838-1872`), confirming `exec --json` cannot bridge to deltas.
- `codex-rs/protocol/src/protocol.rs@main` lines 1820–2070 — typed event definitions, `TokenUsage`, `TokenUsageInfo` with `model_context_window: Option<i64>`: https://github.com/openai/codex/blob/main/codex-rs/protocol/src/protocol.rs
- `codex-rs/app-server-protocol/src/protocol/event_mapping.rs@main` — `item_event_to_server_notification` mapping that confirms `AgentMessageContentDelta`/`ReasoningContentDelta`/`ExecCommandOutputDelta` become app-server notifications: https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/event_mapping.rs
- `codex-rs/exec/src/event_processor_with_jsonl_output.rs@main` — the `exec --json` event router; the `ServerNotification` match expression that excludes all `*Delta` notifications: https://github.com/openai/codex/blob/main/codex-rs/exec/src/event_processor_with_jsonl_output.rs
- `codex-rs/app-server/src/main.rs@main` — `--listen stdio:// | unix:// | ws:// | off`, `--session-source vscode` default, `--strict-config`, `--remote-control` flags: https://github.com/openai/codex/blob/main/codex-rs/app-server/src/main.rs
- `codex-rs/app-server/tests/common/rollout.rs@main` — `rollout_path()` defining `codex_home/sessions/YYYY/MM/DD/rollout-{filename_ts}-{thread_id}.jsonl`: https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/common/rollout.rs
- `codex-rs/app-server/tests/suite/v2/initialize.rs@main` — `user_agent` shape (`"codex_vscode/..."`), `codex_home`, `platform_family`, `platform_os`: https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/suite/v2/initialize.rs
- `codex-rs/app-server/tests/suite/v2/thread_start.rs@main` — `ThreadStartParams`, `ThreadStartResponse`, `ThreadStartedNotification`, `ThreadStatus`: https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/suite/v2/thread_start.rs
- `codex-rs/app-server/tests/suite/v2/thread_resume.rs@main` — `create_fake_rollout`, `rollout_path`, `create_fake_rollout_with_token_usage`; resume reads the same on-disk rollouts: https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/suite/v2/thread_resume.rs
- `codex-rs/app-server/tests/suite/v2/turn_interrupt.rs@main` — `turn/interrupt` request → `turn/completed{status: Interrupted}`: https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/suite/v2/turn_interrupt.rs
- `codex-rs/app-server/tests/suite/v2/turn_start.rs@main` — turn-start notification sequence; `turn/started` → `item/*` → `turn/completed`: https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/suite/v2/turn_start.rs
- `codex-rs/app-server/tests/suite/v2/rate_limits.rs@main` — `account/rateLimits/read` shape, primary/secondary windows, `usedPercent`, `resetsAt`: https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/suite/v2/rate_limits.rs
- Codex GitHub releases: https://github.com/openai/codex/releases — 0.130.0 on 2026-05-08, 0.131.0-alpha line through 2026-05-15 confirming active development pace.
- Recent app-server PRs (search results, 2026-05-06 → 2026-05-16): #22877 (remote control UX), #22841 (memory prompt injection), #22792 (stop returning thread permission profiles), #22611 (permission ids + runtime workspace roots), #22404 (websocket listener restored with auth guard), #22338 (login issuer override gating), #21963 (websocket via HTTP upgrade), #21843 (TCP websocket removed), #21831 (daemon-safe restart), #21336 (sessionId moved onto Thread). Confirms upstream is actively reshaping the surface.
- v0.130 release context — [Codex CLI v0.130 with remote-control](https://jls42.org/en/news/ia-actualites-10-may-2026): `codex remote-control` entrypoint, large-thread pagination, hot config reload.
- Companion in-repo docs: [codex-cli-observed.md](codex-cli-observed.md), [codex-noninteractive.md](codex-noninteractive.md), [harness-comparison.md (archived)](archive/harness-comparison.md).
- Switchboard adapter code under review: `/Users/shanekercheval/repos/switchboard/crates/harness/src/codex/{mod.rs, parser.rs, session_file.rs, sidecar.rs, config.rs, skills.rs}`, `/Users/shanekercheval/repos/switchboard/crates/harness/src/events.rs`.
