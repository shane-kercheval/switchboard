# Research: Claude Code vs Codex (headless comparison) — ARCHIVED

> **Status: archived (2026-05-19).** This is a captured-in-time pre-M2 research artifact that informed the initial per-harness adapter design. It compares only Claude Code and Codex (Gemini was added later in M3) and was never intended to be a living document. The "Updates the system-design needs" section at the bottom describes changes that have already flowed into `docs/system-design.md`.
>
> **Current per-harness ground truth lives in:**
> - [`claude-code-cli-observed.md`](../claude-code-cli-observed.md)
> - [`codex-cli-observed.md`](../codex-cli-observed.md)
> - [`gemini-cli-observed.md`](../gemini-cli-observed.md)
>
> Don't update this file. If you need a cross-harness side-by-side, the §9 *Harness capabilities Switchboard depends on* and *Per-harness adapter and normalized event stream* tables in `docs/system-design.md` are the living source of truth.

**Captured:** 2026-05-09
**Tool versions:** Claude Code 2.1.138, codex-cli 0.128.0
**Companion to:** [claude-code-cli-observed.md](../claude-code-cli-observed.md), [codex-cli-observed.md](../codex-cli-observed.md)

This note distills the per-harness observations into a side-by-side, focused on the design decisions Switchboard's §5 (harness integration) needs to make. Where the harnesses behave the same, we note it; where they diverge, we name the consequence.

## Process invocation

| Concern | Claude Code | Codex |
|---|---|---|
| Non-interactive entry | `claude -p "..."` | `codex exec "..."` |
| Structured output | `--output-format stream-json --verbose` | `--json` |
| Working directory | Inherits cwd; `--add-dir` for extras | `-C, --cd <DIR>`; `--add-dir <DIR>` |
| Skip permissions ("yolo") | `--dangerously-skip-permissions` | `--dangerously-bypass-approvals-and-sandbox` |
| Skip git-repo requirement | n/a (not required) | `--skip-git-repo-check` (Codex requires a git repo by default) |
| Disable persistence | `--no-session-persistence` | `--ephemeral` |
| Configuration override | One flag per knob | `-c key=value` (TOML) — single override mechanism for everything |

**Implication:** Switchboard's "harness invoker" helper needs harness-specific command-line construction. The two harnesses share the *concept* of "spawn a non-interactive process," but the flag vocabularies are not interchangeable.

## Session storage

| Concern | Claude Code | Codex |
|---|---|---|
| Path format | `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl` | `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<session-uuid>.jsonl` |
| Encoded cwd in path? | Yes — `/path/to/dir` → `-path-to-dir` | No — date-partitioned instead |
| Session ID assignable? | Yes — `--session-id <uuid>` | No — Codex assigns it |
| Resume by ID | `--resume <uuid>` | `codex exec resume <uuid>` |
| Native fork | Yes — `--fork-session` (with `--resume`) | **No non-interactive fork.** Top-level `codex fork` is interactive only. |

**Implication:** Switchboard can ask Claude Code to use a specific session ID at spawn (useful for predictable file paths). Codex assigns its own and Switchboard must capture it from the first stream event. Forking is a real asymmetry — Claude Code is fork-native, Codex requires a workaround (start a new session and re-feed context, or copy session-file state manually). This needs a §5 acknowledgement and possibly an open question.

## Output stream — event vocabularies

The two streams emit very different shapes. A side-by-side of the events Switchboard needs to handle:

| Concept | Claude Code | Codex |
|---|---|---|
| Session metadata announcement | `system` / `subtype: "init"` (rich: tools, MCP servers, slash commands, agents, skills, plugins, model, version, memory) | None in stream. (Session-file `session_meta` has it.) |
| Rate-limit / quota | `rate_limit_event` (in stream) | `token_count.rate_limits` (session file only) |
| Turn start | First `assistant` event | `turn.started` |
| Model assistant content | `assistant` event with `content` blocks (`thinking`, `tool_use`, `text`) | `item.completed` with `item.type: "agent_message"` |
| Tool call (started) | n/a (tool_use is part of `assistant` content) | `item.started` with `item.type: "command_execution"` |
| Tool call (result) | `user` event with `tool_result` content | `item.completed` with `command_execution` (full output, exit_code) |
| Turn end | `result` (subtype: success / error) | `turn.completed` |
| Per-turn cost | `result.total_cost_usd` + `modelUsage.<model>.costUSD` | Not exposed (derive from token counts) |
| Per-turn token usage | `result.usage` + `result.modelUsage.<model>` (incl. context window) | `turn.completed.usage` |
| Context window max | `result.modelUsage.<model>.contextWindow` | Not in stream; in session-file `task_started.model_context_window` |

**Key observation:** Switchboard needs a translation layer from each harness's native events into a normalized internal event vocabulary. A reasonable normalized shape:

```
TurnStart { agent, session_id }
ContentChunk { agent, kind: thinking | text | tool_use, data }
ToolResult { agent, tool_use_id, output, is_error }
TurnEnd { agent, stop_reason, usage: { input, output, cached, reasoning, context_window? }, cost_usd?, raw_event }
RateLimitEvent { agent, info }
```

Each harness adapter translates its native events into this shape. Switchboard's workflow engine, UI, and persistence layer consume the normalized form.

## Streaming mechanism

Both harnesses stream over stdout in real time. The implementation pattern is the same:

1. Switchboard spawns the harness process (`child_process.spawn` in Node, `subprocess.Popen` in Python, etc.) with the structured-output flag (`claude -p --output-format stream-json --verbose` or `codex exec --json`).
2. The harness writes one JSON object per line to stdout as events occur.
3. Switchboard reads stdout line-by-line, parses each line as one event, dispatches to the normalized event stream.
4. The process exits or emits its terminal event and the stream ends.

Standard Unix pipe-and-readline. **No file-watching required for the basic case.**

### Granularity: per-event, not per-token

Both harnesses emit events at **content-block granularity**, not character-by-character. When Claude Code generates a `thinking` block, you get one event for the whole block; when it generates a `tool_use`, one event; when it generates the final text, one event. Codex is the same — one `item.completed` per `agent_message` or `command_execution`.

The events do arrive in real time, not buffered to the end — during the file-read probe, we observed the thinking → tool_use → tool_result → final text events stream in over a few seconds. So the user gets visible feedback throughout, even though each chunk is a whole content block rather than a stream of tokens.

### Token-by-token streaming (Claude Code only, untested)

Claude Code's `--include-partial-messages` flag (only works with `-p` and `--output-format=stream-json`) emits partial message chunks as they arrive — true token-level streaming. We did not probe this; the v1 design assumes content-block granularity is sufficient.

Codex has no equivalent flag observed. For Codex, content-block streaming is the ceiling.

For v1, content-block streaming is almost certainly enough — text appears as the model generates it; the user sees progress. Token-by-token is a polish item if the typing animation feels too chunky.

### Terminal event is the stop signal

Do not try to detect end-of-turn by absence of events. Wait for the explicit terminal event:

- Claude Code: `result` (always — check `is_error` for success vs failure).
- Codex: `turn.completed` (success) or `turn.failed` (error).

Process exit also signals terminal-or-crash; useful as an out-of-band cross-check.

### When file-watching enters the picture (Codex only)

The Codex `--json` stream is a deliberately minimal subset of what gets recorded in the session file (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`). The session file carries extras the stream doesn't: rate limits, `model_context_window`, full reasoning blocks (encrypted), Codex's internal developer messages.

If Switchboard's Codex adapter wants those fields, it can **read the session file on `turn.completed`** to enrich the normalized event with the missing data. Two notes:

- This is a **read-after-completion** pattern, not a **tail-follow** pattern. Much simpler — no file-watcher infrastructure, just an `fs.readFile` after the terminal event.
- Claude Code does not need this — its stream and session file carry roughly the same information.

Tracked under open question 10.15 in the system-design.

## Stop detection

Both harnesses provide a single, definitive end-of-turn event, but Codex has a separate failure terminus:

- Claude Code: `result` event always — check `is_error: true` and/or `api_error_status != null` to detect errors. (Subtype stays `"success"` even on error — misleading; do not use it as the error signal.)
- Codex: `turn.completed` on success, **`turn.failed`** on error. Switchboard's adapter must wait for **either**.

Process exit code is `1` on error in both harnesses, useful as an out-of-band cross-check.

In both cases, intermediate events do not signal stop reliably. **Switchboard should always wait for the explicit end event** — never try to derive stop from individual content events.

This also means the system-design's open question 10.1 (what if the next assistant response is a tool call?) is not a real concern at the harness level. Both harnesses run the model → tool_use → tool_result → model loop internally and only emit the end event when the cycle finally produces a final answer. From Switchboard's POV, every user-initiated turn produces exactly one terminal event regardless of internal tool cycles.

## Cancellation (SIGTERM mid-stream)

Probed both harnesses by spawning a long-prompt invocation, waiting until the model was producing output (or reasoning, in Codex's case), then sending SIGTERM.

| Concern | Claude Code | Codex |
|---|---|---|
| Process model | Single process | Two-process tree (Node wrapper + codex binary) |
| SIGTERM propagation | Just kill the PID; nothing else to clean up | Killing the parent kills the child too (verified); but **process groups are safer cross-platform** — spawn in a new group, kill the group |
| Exit code on SIGTERM | `143` (128 + 15) — distinguishable from completion | **`0`** (parent catches signal, exits gracefully) — **not distinguishable from completion via exit code alone** |
| Detecting cancellation | Exit code 143 OR absent `result` event | Absent `turn.completed` AND absent `turn.failed` (must rely on the stream, not the exit code) |
| Session file behavior | Stops at last completed turn; partial assistant response **not** persisted | Captures more than the stream did (reasoning, token_count, rate limits) but no `agent_message` for the in-flight turn |
| Recovering partial content | From the streamed events Switchboard buffered itself | Same — session file doesn't carry partial responses either |
| Resume after cancel | Works cleanly; session is in a usable state | Works cleanly; session is in a usable state |

**Switchboard cancellation design (per-harness adapter):**

1. **Track the spawned PID** — every subprocess API returns it. Trivial.
2. **Spawn in a new process group** so a single signal to the group cleans up both single-process (Claude Code) and tree (Codex) harnesses uniformly. Rust: `Command::process_group(0)`, then `nix::sys::signal::killpg`. Python: `os.setsid()`, then `os.killpg`.
3. **Buffer the stream** if Switchboard wants to surface "here's what the agent had said before you cancelled." The session files do not carry partial assistant responses for either harness — only the stream does.
4. **Detect cancellation from the absence of a terminal event** in the stream rather than the exit code. Codex's exit-0-on-SIGTERM behavior makes the exit code unreliable for this purpose, even though Claude Code's exit 143 would have worked alone.
5. **Resume is fine** — after cancellation the session is usable. The cancelled turn is just absent from the transcript; the next message proceeds normally.

## Permission denials

Tested only on Claude Code (`--permission-mode dontAsk` + a Write attempt). Findings: denials do not error the turn (`is_error: false`); the model receives the denial as feedback and adapts. The full attempted call is captured in `result.permission_denials`. Codex denial behavior is presumed similar but not directly probed — verification deferred to implementation.

## Concurrent invocations

Tested only on Claude Code: three parallel `claude -p` from the same cwd completed cleanly with three unique session UUIDs, three separate session JSONL files, no file-locking or contention observed. Wall-clock matched the slowest invocation, confirming actual parallelism. Codex concurrent runs not directly probed; presumed similar based on the harness design (each `codex exec` writes its own date-partitioned session file).

## Tool-call surface

| Aspect | Claude Code | Codex |
|---|---|---|
| Tool palette | Wide, typed (Bash, Read, Edit, Glob, Grep, Write, MCP-provided tools, etc.) | Single primary surface: shell (`command_execution`). Edits done via `apply_patch` shell calls. |
| Tool-name visibility in stream | Yes — every `tool_use` has a `name` field | Less semantic — everything looks like a shell command, you read the command string to know what's happening |
| Tool result format | Structured `tool_result` content with `is_error` boolean | `command_execution` item with `aggregated_output` and `exit_code` |
| MCP tools | First-class — appear in `tools` list and as `tool_use` events with `mcp__server__name` | Configurable but practically narrower; observed only one MCP server registered in the user's config |

**Implication:** Switchboard's tool-call rendering will look different per harness. For Claude Code, render by tool name with structured input. For Codex, render the literal command and exit code. Both are valid — Switchboard should not try to force them into a unified rendering.

## Cost and usage

| Aspect | Claude Code | Codex |
|---|---|---|
| Cost per turn | Native (`total_cost_usd`) | Not exposed; Switchboard derives from token counts × per-model pricing table |
| Tokens per turn | `usage.input_tokens` / `output_tokens` / `cache_*` | `usage.input_tokens` / `cached_input_tokens` / `output_tokens` / `reasoning_output_tokens` |
| Per-model breakdown | Yes (`modelUsage.<model>`) | No (Codex uses one model per session) |
| Context window max | Yes in stream (`modelUsage.<model>.contextWindow`) | Yes in session file only (`task_started.model_context_window`) |

**Implication for §5:**
- Switchboard ships and maintains a per-model **pricing** table (for Codex cost derivation). This was already implicit; confirmed needed.
- Switchboard ships and maintains a per-model **context-window** table (or reads the session file for Codex). Optional for Claude Code, required for Codex if we don't want to read session files. This is open question 10.12; the answer is **maintain the table; treat any harness-provided value as authoritative override when available**.

## Auto-compaction

Both harnesses do auto-compact on their own. Neither exposes a programmatic `/compact` trigger in non-interactive mode. We did not observe an auto-compact event firing in either harness during these short probes (turns were tiny). The behavior described in the docs-derived notes still stands: rely on auto-compact, surface warnings as utilization climbs, do not implement Switchboard-side compaction.

## What `--bare` (Claude Code) vs `--ignore-user-config` (Codex) actually skip

Both harnesses have a "minimal" or "ignore user config" mode useful for reproducibility:

| Aspect | Claude Code `--bare` | Codex `--ignore-user-config` |
|---|---|---|
| Effect on user MCP servers | Skipped | Skipped (per docs; not exhaustively probed) |
| Effect on user skills | Skipped | n/a — skills are passed inline regardless |
| Effect on user agents | Skipped (built-ins remain) | Codex doesn't have agents in the same sense |
| Effect on `CLAUDE.md` / project-level instructions | Skipped | n/a (Codex has `.rules` files, controlled by `--ignore-rules`) |

These are not symmetric features but serve similar purposes. For Switchboard's "ride defaults" stance (§5), we use neither. For the future when Claude Code's `--bare` becomes default, Switchboard explicitly opts back into the user environment via `--mcp-config`, `--plugin-dir`, `--add-dir`, `--settings`. Codex doesn't have a similar default-flip announced.

## Summary of asymmetries Switchboard needs to handle

1. **Stream vocabulary**: completely different event types. Need per-harness adapters to a normalized event stream.
2. **Cost reporting**: native in Claude Code, derived in Codex. Pricing table needed for Codex.
3. **Context window max**: in Claude Code's stream, in Codex's session file only. Maintain a fallback table either way.
4. **Fork**: native in Claude Code (`--fork-session`); requires workaround in Codex (no non-interactive fork). Worth tracking as a constraint.
5. **Session ID assignment**: Switchboard can specify it for Claude Code; cannot for Codex — must capture from first event.
6. **Session file richness vs stream**: in Claude Code the stream is roughly equivalent to the session file. In Codex the stream is a deliberately minimal subset; the session file is the full story. **Switchboard may want to read Codex session files for some operations** (rate limits, context window, full reasoning) that aren't in the stream.
7. **Tool-call semantics**: typed tools (Claude Code) vs shell commands (Codex). UI rendering differs.

## Updates the system-design needs

After this round of research, the changes for §5 and the open questions are:

1. **§5 "Required harness commands for MVP"** — needs reorganization to call out per-harness gaps explicitly. Specifically:
   - "Fork a session from a checkpoint" — native Claude Code, **gap in Codex**. Either drop from v1 or document the workaround.
   - "Read context utilization" — note that Claude Code provides `contextWindow` in the stream as of v2.1.138 (resolves part of 10.12); Codex requires session file or maintained table.
   - Cost reporting asymmetry (native vs derived).
2. **Open question 10.1 (tool-call response handling)** — can be **closed**. The harnesses handle the loop internally; Switchboard always sees a single end-of-turn event.
3. **Open question 10.12 (model→max-context map)** — partially resolved but stays open: bundled table is still needed for Codex; Claude Code provides `contextWindow` natively now and we can use it as authoritative.
4. **New open question candidate: Codex non-interactive fork.** Should we manually copy session files? Should we drop fork from v1's Codex agent capability? Should we wait on upstream?
5. **New open question candidate: Should Switchboard read Codex session files** in addition to the `--json` stream to access the richer event data (rate limits, context window, reasoning)? Tradeoff: more complete information vs more file-watching plumbing.

These updates flow into the next plan revision pass.

## Sources

- Hands-on probes in `/tmp/switchboard-probe/` (Claude Code: `hello-json.out`, `hello-stream.out`, `tool-call.out`; Codex: `codex-hello.out`, `codex-tool-call.out`).
- Session files at `~/.claude/projects/-private-tmp-switchboard-probe/*.jsonl` and `~/.codex/sessions/2026/05/09/rollout-*.jsonl`.
- `claude --help` (v2.1.138), `codex --help` / `codex exec --help` / `codex exec resume --help` / `codex mcp list` (v0.128.0).
