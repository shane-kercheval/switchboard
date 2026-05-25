# Research: Claude Code CLI hands-on observations

**Captured:** 2026-05-09
**Tool version:** Claude Code 2.1.138
**Companion to:** [claude-code-headless.md](claude-code-headless.md) (the docs-derived note). This file captures what we actually observed by exercising the CLI.

## Method

Probes were run from a clean scratch directory (`/tmp/switchboard-probe/`) against `claude -p` with various flags, and the captured outputs (JSON / stream-json) and on-disk session files were inspected. Real outputs are quoted below.

## CLI surface relevant to Switchboard

`claude --help` reveals these flags relevant to driving the CLI from another process:

| Flag | Purpose |
|---|---|
| `-p, --print` | Non-interactive mode (the only mode Switchboard uses). |
| `--output-format <text\|json\|stream-json>` | Output format. `stream-json` is what Switchboard wants. |
| `--input-format <text\|stream-json>` | Allows streaming user messages back into a running session. |
| `--include-partial-messages` | Token-by-token deltas for stream-json (only with `-p`). |
| `--include-hook-events` | Surfaces hook lifecycle events (only with stream-json). |
| `-r, --resume [value]` | Resume by session UUID. |
| `--session-id <uuid>` | Use a specific session UUID for the session (Switchboard can choose the ID). |
| `--fork-session` | When resuming, branch instead of continuing — **native fork support**. |
| `--continue` | Resume the most-recent session in the cwd. |
| `--no-session-persistence` | Don't persist session to disk (useful for ephemeral one-shots). |
| `--bare` | Strict minimal mode. Skips MCP servers, plugins, user agents/skills, auto-memory, CLAUDE.md. |
| `--mcp-config <files...>` | Explicit MCP config (needed in bare mode to re-enable MCP). |
| `--strict-mcp-config` | Use only the configs from `--mcp-config`, ignore everything else. |
| `--system-prompt <prompt>` | Replace default system prompt. |
| `--append-system-prompt <prompt>` | Add to default system prompt. |
| `--allowedTools` / `--disallowedTools` / `--tools` | Tool allow/deny list. |
| `--permission-mode <mode>` | `acceptEdits`, `auto`, `bypassPermissions`, `default`, `dontAsk`, `plan`. |
| `--dangerously-skip-permissions` | The "yolo" flag. |
| `--add-dir <dirs...>` | Additional directories tools can access. |
| `--max-budget-usd <amount>` | Hard budget cap (only with `-p`). |
| `--model <model>` / `--fallback-model <model>` | Model selection + automatic fallback. |
| `--effort <level>` | `low\|medium\|high\|xhigh\|max`. |
| `--debug [filter]`, `--debug-file <path>` | Debug logging. |

Subcommands of note: `mcp` (configure MCP servers), `agents` (manage agents), `project` (project state), `auth`, `setup-token`.

## Session storage and lifecycle

**Storage location:** `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`

Where `<encoded-cwd>` is the absolute working directory with **both `/` and `.` replaced by `-`**. The rule applies uniformly regardless of dot position within the path. Empirically verified by running `claude -p` in several cwd shapes and inspecting `~/.claude/projects/`:

| Cwd | Encoded |
|---|---|
| `/private/tmp/sw-probe/foo.bar/sub` | `-private-tmp-sw-probe-foo-bar-sub` (mid-component dot) |
| `/private/tmp/sw-probe/.hidden/sub` | `-private-tmp-sw-probe--hidden-sub` (leading dot) |
| `/private/tmp/sw-probe/foo/.bar.baz` | `-private-tmp-sw-probe-foo--bar-baz` (multiple dots, mixed) |
| `/private/tmp/sw-probe/foo/version.1.2.3` | `-private-tmp-sw-probe-foo-version-1-2-3` (version-style) |
| `/Users/x/repo/.switchboard/projects/abc` | `-Users-x-repo--switchboard-projects-abc` (Switchboard's actual layout) |

(Original M1.3 research listed only `/` → `-`; the dot-stripping rule was missed because probe paths happened to contain no dots. Re-verified in M1.5 with the probe shapes above after manual testing surfaced the bug against `.switchboard/`. The rule's behaviour under the `/Users/` path-root is constituted by the original M1.5 bug reproduction itself: claude wrote the session file at `~/.claude/projects/-Users-shanekercheval-repos-temp--switchboard-projects-<uuid>/` for a real cwd at `/Users/shanekercheval/repos/temp/.switchboard/projects/<uuid>` — same `.` → `-` rule applied under `/Users/`. No separate `/Users/`-rooted probe was run; the live bug acted as the verification.)

**Format:** newline-delimited JSON. Each line is one event with a `uuid` and a `parentUuid` chain forming a tree (which is what `--fork-session` branches from).

**Resume works as documented:** `claude -p --resume <session-id> "..."` continues the session. Confirmed: a follow-up "What was the file content I asked you to read?" correctly recalled `PROBE_FILE_CONTENT_42` from the prior turn's tool call.

**Switchboard implication:** the on-disk format is well-defined and inspectable. Switchboard can introspect any session it spawns by reading the corresponding `.jsonl` (e.g. for "show me what happened in this turn" UI). Forking is a single flag, no manual file copying needed.

## Output stream — `stream-json` event types

The `stream-json` output emits one JSON object per line. Observed event types in order across a tool-using turn:

1. **`system` / `subtype: "init"`** — first event. Contains the entire agent environment:
   - `session_id`, `cwd`, `claude_code_version`, `model`, `permissionMode`, `apiKeySource`
   - `tools`: full list of tool names (built-ins + MCP + dynamic)
   - `mcp_servers`: array of `{name, status}` (`status` ∈ `connected`, `needs-auth`, etc.)
   - `slash_commands`: full list of slash commands available (built-ins + plugin/MCP-provided)
   - `agents`: list of agent names (built-in + user)
   - `skills`: list of skill names (built-in + user)
   - `plugins`, `memory_paths`, `output_style`, `analytics_disabled`, `fast_mode_state`
2. **`rate_limit_event`** — `{status, resetsAt, rateLimitType, overageStatus, ...}`. Useful for surfacing "X minutes until quota refreshes."
3. **`assistant`** events — one per assistant message. Body matches the Anthropic API message format with `content` blocks. Block types observed:
   - `thinking` (with a `signature` for redacted thinking; the `thinking` field itself may be empty)
   - `tool_use` (with `id`, `name`, `input`, `caller: {type: "direct"}`)
   - `text` (the actual reply)
   Each `assistant` event includes per-message `usage` (input/output/cache tokens) and `stop_reason: null` while the turn is still progressing.
4. **`user`** events — when the harness emits a tool result, it does so as a synthetic user event with role `user` and `content` = `[{type: "tool_result", tool_use_id, content, is_error}]`. **Switchboard must not confuse these with human-typed user messages** — they are harness-generated.
5. **`result`** — the final event marking the end of the entire user-initiated turn. Subtype `success` or otherwise. Contains:
   - `result`: the final text response (the canonical reply Switchboard would surface)
   - `stop_reason`: `"end_turn"`, etc. (definitive at this point)
   - `num_turns`: count of agent turns (one tool-use cycle = 2)
   - `duration_ms`, `duration_api_ms`
   - `total_cost_usd`
   - `usage`: aggregate token counts
   - `modelUsage`: **per-model breakdown including `contextWindow` and `maxOutputTokens`**
   - `permission_denials`: array of any tool calls denied
   - `terminal_reason`: `"completed"` etc.

### Stop detection: just listen for `result`

`stop_reason` on intermediate `assistant` events is `null` while the cycle continues. The single, definitive end-of-turn signal is the `result` event. Switchboard should treat `result` as "this turn is done" and not try to parse stop signals from individual `assistant` events.

This also resolves part of system-design open question 10.1 (what about tool-call-only responses): there is no such thing as a "tool-call-only response" at the harness level — the harness keeps cycling (model → tool_use → tool_result → model → ...) until the model emits a final text and the `result` event fires. Switchboard always sees a complete turn.

### `contextWindow` IS exposed in the `result` event

This contradicts our earlier docs-derived research (which said `tokens_max` was closed-as-not-planned in #8011). As of v2.1.138, every `result` event carries `modelUsage.<model>.contextWindow`. Example:

```json
"modelUsage": {
  "claude-haiku-4-5-20251001": {
    "inputTokens": 348, "outputTokens": 13,
    "contextWindow": 200000, "maxOutputTokens": 32000, ...
  },
  "claude-opus-4-7[1m]": {
    "inputTokens": 6, "outputTokens": 6,
    "contextWindow": 1000000, "maxOutputTokens": 64000, ...
  }
}
```

**Switchboard implication:** we do not need to maintain a model→max-context map for Claude Code (open question 10.12 partially resolved for Claude Code). We can derive utilization directly from each turn's result event. Codex needs separate verification.

### `total_cost_usd` and per-model cost are native

Confirmed both at the top level (`total_cost_usd`) and per model (`modelUsage.<model>.costUSD`). No derivation needed.

### Tool-error surfaces inline

When a tool call errors (we tested with a non-existent file), the `tool_result` user-event carries `is_error: true` and a textual error message. The model receives this, responds normally (e.g. "the file does not exist"), and the turn completes with `stop_reason: "end_turn"`. Failures inside a single tool call do not bubble up as a turn-level error — only model/API/auth failures do.

## `--bare` mode behavior

Confirmed by inspecting the `system/init` event with vs without `--bare`:

| Field | Default `-p` | `--bare` |
|---|---|---|
| `tools` count | ~80 (incl. MCP tools) | 3 (Bash + file Read + file Edit only) |
| `mcp_servers` | All registered, with status | Empty |
| `agents` | Built-ins + user-defined | Built-ins only |
| `skills` | Built-ins + user-defined | Built-ins only |
| `plugins` | Loaded | Empty |
| `slash_commands` count | ~50 | ~18 (no MCP/plugin slash commands) |
| `memory_paths` | Auto-resolved | `null` |

**Key nuance:** `--bare` strips *user-installed* MCP servers, plugins, skills, and agents. Built-in agents (Explore, general-purpose, Plan, statusline-setup) and built-in skills (debug, simplify, claude-api, etc.) **still load** in bare mode. So "bare" does not mean "no agents/skills" — it means "no user-configured ones."

Implications for our migration plan when `--bare` becomes default: Switchboard must explicitly pass `--mcp-config`, `--plugin-dir`, `--add-dir` (for CLAUDE.md), and `--settings` to restore the user's full environment.

## Initial-prompt mechanism

Plan §4 Primitive 1 described "initial prompt" as just a regular first message. We now have the alternatives confirmed:

- `--system-prompt <prompt>` replaces the default system prompt entirely.
- `--append-system-prompt <prompt>` adds to it.
- A first user message after spawn is also valid (and harness-agnostic).

For Switchboard's initial-prompt feature, the cleanest implementation is "send as first user message" — works in both Claude Code and Codex (assuming Codex has equivalent), keeps the mechanism uniform, and avoids tying initial-prompt semantics to Claude Code's system-prompt knobs. The system-prompt flags can be exposed separately later if a power user wants to use them, but they aren't needed for the basic feature.

## Authentication and credentials

`apiKeySource` in `system/init` reports the credential source (`"none"` in our test, since we use OAuth). Bare mode skips OAuth and keychain reads — Anthropic auth there must be `ANTHROPIC_API_KEY` or `apiKeyHelper` via `--settings`. Switchboard is unlikely to need to manage this directly but should report the source on initialization for debugging.

## Error events

Forced by passing `--model invalid-model-name`:

```json
{
  "type": "result",
  "subtype": "success",            // misleading — see below
  "is_error": true,
  "api_error_status": 404,
  "result": "There's an issue with the selected model (invalid-model-name)...",
  "stop_reason": "stop_sequence",
  "usage": { ... all zeros ... },
  "modelUsage": {},
  "permission_denials": [],
  "terminal_reason": "completed"
}
```

**Switchboard implication:** `subtype` is not a reliable error signal — it stays `"success"` even when the turn errored. Switchboard should detect errors via `is_error: true` and/or `api_error_status != null`. Process exit code is `1` on error, which is a useful out-of-band signal as well.

## Permission denials

Forced by `--permission-mode dontAsk` plus a prompt asking the model to use `Write`:

```json
{
  ...
  "is_error": false,
  "permission_denials": [
    {
      "tool_name": "Write",
      "tool_use_id": "toolu_01C3nYGSu3eEu3f4wCdtvs7g",
      "tool_input": {
        "file_path": "/private/tmp/switchboard-probe/test-write.txt",
        "content": "hello"
      }
    }
  ],
  "result": "The Write tool was denied — Claude Code is running in \"don't ask\" mode and the Write capability is blocked. ..."
}
```

**Switchboard implication:** denials do not error the turn (`is_error: false`) — the model receives the denial as feedback and adapts its response. `permission_denials` contains the full attempted call with arguments. Switchboard can use this to surface "the model tried X but was blocked" affordances without treating the turn as failed.

Three behaviors to keep distinct:

- **Disallowed tools** (e.g. `--disallowedTools Read`): the tool doesn't appear in the model's palette at all; the model often routes around (we saw it use `cat` via `Bash` after `Read` was disallowed). No `permission_denials` fires because no attempt was made.
- **Model self-restraint**: in `--permission-mode dontAsk`, the model frequently *refuses to attempt* destructive operations on its own ("rm is destructive — want me to proceed?") rather than trying. No `permission_denials` fires here either. To force a denial in our probe, we had to explicitly tell the model "just attempt it."
- **Permission denial**: the model attempted, the harness blocked it. This is the case `permission_denials` captures.

Switchboard's "the model tried X but was blocked" UI surface only fires for the third case. Self-restraint shows up as ordinary text output ("I won't do that"); disallowed-tool routing shows up as the model using a different tool.

## Concurrent invocations

Three parallel `claude -p` invocations from the same cwd completed cleanly:

- Three unique session UUIDs (one per process)
- Three separate session JSONL files, no contention
- Different durations (1.5s, 2.1s, 6.4s) — independent timing

Wall-clock for the slowest invocation matched its individual duration, confirming actual parallelism (not queueing). **Implication: fan-out via concurrent process spawn is safe at this scale.** No file locking or session-state contention observed.

## Cancellation (SIGTERM mid-stream)

Probed by spawning `claude -p "Write a 100-line poem..."`, waiting 20 seconds (long enough to be mid-stream-output), then sending SIGTERM to the process.

**Process model:** Claude Code is a **single process** — no child processes spawned. `pgrep -P <pid>` shows nothing. Killing the parent kills the whole thing; no process-tree concerns.

**Exit code:** `143` (= 128 + 15, the standard "killed by SIGTERM"). Switchboard can use this to distinguish "killed externally" from "completed normally" (exit 0 on success, exit 1 on internal error).

**Stream output**: captured everything up to the moment of kill — including a partial `text_delta` event mid-token-stream like `"text":"**The Ocean's Hymn**\n\nBeneath the bruise of d"`. So Switchboard's adapter, if it wants to show the operator "here's what the agent had said before you cancelled," should buffer the streamed events itself and not rely on the session file for partial recovery.

**Session file (`~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`)**: stops at the last completed assistant turn. The partial assistant response that streamed to stdout was **not persisted** to the session file. Event types we saw in the file: `queue-operation`, `attachment` (tool/MCP/skill listings), `user`, `last-prompt`, `ai-title`. No `assistant` entry for the in-flight turn.

**Resume after cancel**: works cleanly. Sending a follow-up via `claude -p --resume <uuid> "Just say 'resumed ok'"` returned `is_error: false`, `result: 'resumed ok'`, `stop_reason: end_turn`. The harness session is in a usable state; the cancelled turn is just absent.

**Switchboard implication**: cancellation works as expected. The adapter:
1. Tracks the spawned PID (returned by the language's subprocess API).
2. On user-initiated cancel, sends `SIGTERM` to the PID.
3. Buffers the stream output independently if it wants to surface partial content (the session file won't have it).
4. After cancel, the agent's harness session is preserved and can be re-sent messages immediately.

## M1.3 implementation findings (2026-05-13)

These observations came from building `ClaudeCodeAdapter` and running the live integration tests (`tests/live.rs`).

### `--verbose` is mandatory with `--include-partial-messages --output-format stream-json`

Without `--verbose`, the `stream_event` delta lines are absent from the output even when `--include-partial-messages` is passed. The required flag combination for streaming text deltas is:

```
claude -p <prompt> \
  --output-format stream-json \
  --include-partial-messages \
  --verbose \
  --dangerously-skip-permissions \
  [--session-id <uuid>]
```

`--verbose` is not optional here — omitting it silently produces a stream without `content_block_delta` events.

### `--session-id` vs `--resume` for session continuity

`--session-id <uuid>` **creates** a new session with the given UUID. It does NOT resume an existing session — passing `--session-id` with a UUID that already has a persisted session file fails the second turn.

`--resume <uuid>` **resumes** an existing session by UUID.

The correct adapter pattern:
- **First turn** (no session file exists): `--session-id <uuid>`
- **Subsequent turns** (session file exists at `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`): `--resume <uuid>`

`ClaudeCodeAdapter` checks `~/.claude/projects/<canonicalized-cwd-with-/-replaced-by-->/<uuid>.jsonl` at dispatch time to pick the right flag. This was confirmed by the `live_session_id_idempotency_confirmed` test: two sequential turns sharing the same `session_id` both complete when the first uses `--session-id` and the second uses `--resume`.

### Exact `stream_event` shape with `--include-partial-messages`

```json
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}}
```

Tool input streaming emits `input_json_delta` instead of `text_delta` — these must be skipped (tool input is not displayed as content):

```json
{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}}
```

The terminal `assistant` message carries the complete assembled text in its `content` blocks. With `--include-partial-messages`, this arrives *after* the delta stream and must be explicitly skipped to avoid double-emitting the text as `ContentChunk`s. The parser skips top-level `type: "assistant"` events for this reason.

## Findings during M2.4-prep (2026-05-15)

While probing Codex session-file shapes for M2.4 (see `codex-cli-observed.md` §"Findings during M2.4-prep"), the same question was asked of Claude Code's on-disk session file. Result: a **cross-harness symmetry** worth documenting on the Claude side too — the gap is structural, not Codex-specific.

### Claude session-file record types (claude-code 2.1.x era)

Inspected `~/.claude/projects/-Users-shanekercheval-repos-switchboard/<uuid>.jsonl` from active M2.3-era work. Record types observed:

```
agent-name              (84)
ai-title                (241)
assistant               (1472)
attachment              (180)
file-history-snapshot   (180)
last-prompt             (242)
permission-mode         (232)
queue-operation         (18)
system                  (123)
user                    (1051)
```

`system` subtypes: `local_command`, `api_error`, `away_summary`, `compact_boundary`, `turn_duration`. **No `system/init`-equivalent**, no `tools` field, no `mcp_servers` field anywhere in the file (`grep -cE '"tools":\s*\[|"mcp_servers":\s*\['` returns `0`).

### The registry is stream-only

The `system/init` event — which the M1 parser reads to populate `SessionMeta.tools` / `SessionMeta.mcp_servers` — is emitted **only on the live stream-json output**, never written to the session file. The session file records the conversation tree (assistant/user messages, tool calls, file snapshots) but not the available-tools / MCP-servers registry that was in effect when the session started.

→ **No impact on live dispatch.** M1's Claude adapter consumes `system/init` from the stream directly; `SessionMeta` events fire correctly on every dispatch. The wire-format contract is unchanged.

→ **M2.6 (disk rehydration) implication.** When loading a Claude transcript from disk, the parser cannot reconstruct `SessionMeta.tools` / `SessionMeta.mcp_servers` from the session file itself — that data is genuinely lost on disk. M2.6 reconstructs `SessionMeta` from a **combination of sources**: `model` from the first `assistant.message.model` in the session file; `harness_version` as `String::new()` (the empty-string-means-absent convention `parse_system_event` already uses); `mcp_servers` from the Claude config-loader that reads the three MCP scopes (`~/.claude.json` user-level + nested local under cwd path, `<cwd>/.mcp.json` project-level) and merges them; `skills` from a directory scan of `~/.claude/skills/` + `<cwd>/.claude/skills/`; `tools: vec![]` (the stream-side `system/init.tools` listing is the only populator, and that's not on disk). See §"MCP and skills registry sourcing" below for full file shapes.

→ **Cross-harness symmetry.** Codex has the identical session-file gap (its on-disk `session_meta.payload` carries `cli_version` and `model_provider` but no model/tools/mcp_servers; `model` lives in per-turn `turn_context`; the available-MCP-servers registry isn't snapshotted at all). Codex's rehydration follows the same pattern: source per-turn records for `model` + `harness_version`, call the Codex config-loader (`~/.codex/config.toml` + `<cwd>/.codex/config.toml`) for `mcp_servers`, scan `~/.agents/skills/` + `<cwd>/.agents/skills/` for `skills`. Both harnesses thus end up with a populated `mcp_servers` / `skills` from rehydration — sourced from config files, not the session file.

### What IS recoverable from disk (preview for M2.6)

For both harnesses, the conversation **content** rehydrates fine — `assistant`/`user` records (Claude) and `response_item/message` records (Codex) carry the turn text + tool calls. The on-disk gap is specifically the **registry of available capabilities**, which the harnesses treat as a runtime/configuration concern rather than a per-session artifact. This is M2.6's design problem, not a regression in shipped code.

## Subagent (`Agent` tool) representation — stream vs disk (2026-05-24, claude-code 2.1.149/2.1.150)

Default `claude -p` (which Switchboard uses) loads the user's full environment, so the model can **delegate to subagents** mid-turn — the built-in `Explore`/`Plan`/`general-purpose` agents or any user-defined `.claude/agents/*.md`. This is auto-invoked behavior (the model decides to delegate); it is *not* exotic. Two hands-on probes (a trivial "reply pong" subagent and a tool-using "run `echo hello-from-subagent` and report" subagent) established exactly how this surfaces. **It surfaces differently on the live stream vs. on disk, and the live shape mis-attributes subagent work to the parent turn.** This is the ground truth behind the follow-up work item [`../implementation_plans/2026-05-24-subagent-rendering-fidelity.md`](../implementation_plans/2026-05-24-subagent-rendering-fidelity.md).

**The tool is named `Agent`, not `Task`** in this CLI era. It appears as a normal `tool_use` block: `{ "name": "Agent", "input": { "description", "subagent_type", "prompt" } }`. (System-design §9 calls it "the `Task` tool" — stale; update to `Agent`.)

**Live stream — subagent-internal events are emitted, tagged with `parent_tool_use_id`.** Each top-level stream record carries a `parent_tool_use_id` field: `null` for the parent agent's own events, and set to the `Agent` tool_use's id for any event produced *by* the subagent. A tool-using subagent produces this sequence (parent agent's `Agent` call has id `toolu_017e…`):

```
assistant  parent_tool_use_id=null        content: tool_use  name=Agent      (toolu_017e…)  ← parent
assistant  parent_tool_use_id=toolu_017e… content: tool_use  name=Bash       (toolu_01UE…)  ← SUBAGENT's own call
user       parent_tool_use_id=toolu_017e… content: tool_result            (Bash output)     ← SUBAGENT's own result
user       parent_tool_use_id=null        content: tool_result            (Agent aggregate) ← subagent's report to parent
```

New `system` subtypes also appear during a subagent run: `task_started`, `task_notification`, `status` (alongside the existing set). `parse_system_event` only acts on `subtype == "init"` and skips all others, so these are handled gracefully (no error) — confirmed by the existing `system_non_init_subtype_is_skipped` test.

**On disk — the subagent's internals live in a separate sidecar file, NOT inline.** The main session file (`~/.claude/projects/<encoded-cwd>/<session>.jsonl`) records only the parent's `Agent` tool_use + its aggregate `tool_result`. The subagent's own `Bash` call is written to a **separate** transcript at:

```
~/.claude/projects/<encoded-cwd>/<session-id>/subagents/agent-<id>.jsonl
```

There are **no `isSidechain:true` records** in the main file in this era (the older inline-sidechain layout is gone). New top-level record types observed in the 2.1.149/150 main file (beyond the M2.4-prep list): `ai-title`, `attachment`, `last-prompt`, `queue-operation` — all non-conversation metadata the rehydration parser should skip.

**The load-bearing consequence (for the parser and M4.6 rehydration):** the parser (`crates/harness/src/parser.rs`) **does not read `parent_tool_use_id`**. So:

- **Live:** `parse_assistant_envelope` / `parse_user_envelope` emit `ToolStarted{name:"Bash"}` / `ToolCompleted` for the subagent's internal call **with the parent's `turn_id`** — rendering the subagent's work as the *parent agent's own* tool calls, interleaved between the `Agent` `ToolStarted` and its `ToolCompleted`. A subagent doing N tool calls floods the parent turn with N mis-attributed calls.
- **Disk/rehydration:** `session_file.rs` reads only the main file (it does not descend into `subagents/`), so the rehydrated turn shows just the `Agent` call + its aggregate result — the nested calls are absent.
- **Net:** the *same turn renders differently live vs. after restart* — live over-shows (nested calls, mis-attributed); rehydrate under-shows (only the `Agent` summary). Neither corrupts data; it's a rendering-fidelity + consistency gap. The disk view is the *correct* abstraction level ("a delegation is one tool call from the parent's view"), and matches how Gemini's `invoke_agent` already surfaces (one `ToolStarted`/`ToolCompleted` pair — though whether Gemini *also* leaks a tool-using subagent's internals into its stream is not yet probed; see the follow-up item).

Probe commands (reproducible; `--include-partial-messages --verbose --dangerously-skip-permissions`, fixed `--session-id`, throwaway cwd) are recorded in the follow-up work item.

## Things still worth probing

- **`--input-format stream-json`** — for sending follow-up messages mid-stream without restarting the process. Could matter for the "long-lived agent process" deferred decision.
- **Compaction events in the stream** — does auto-compact emit observable events? Not seen in any of our short tests because we never approached the threshold.
- **`--include-hook-events`** — what hook events flow through, and do they let Switchboard observe pre/post-tool-call lifecycle?
- **`--debug` filtering** — what categories exist, and does any of it give us harness-level observability we'd want?

These can be picked up later; they are not blocking for §5 design.

## Resolutions / updates for the system-design

1. **Open question 10.12 (model→max-context map)** — partially resolved for Claude Code: `contextWindow` is in `result.modelUsage.<model>`. Switchboard does not need to maintain a Claude Code map. (Codex still TBD.)
2. **Open question 10.1 (tool-call response handling)** — effectively resolved: the harness handles the loop internally; Switchboard sees one `result` event per user-initiated turn regardless of how many tool cycles happened inside.
3. **§5 "Required harness commands" — `Fork a session from a checkpoint`** — confirmed native via `--fork-session`. Can mark as native, not "needs verification."
4. **§5 "Process model"** — confirmed default `-p` loads the user's full environment exactly as the prior research note claimed. Confirmed `--bare` strips user-installed extensions but keeps built-ins.

## Sources

- Hands-on probes captured in `/tmp/switchboard-probe/` (`hello-json.out`, `hello-stream.out`, `tool-call.out`).
- Session files at `~/.claude/projects/-private-tmp-switchboard-probe/*.jsonl`.
- `claude --help` (v2.1.138).

## MCP and skills registry sourcing

For the user-facing sidebar listing of MCP servers and skills, Switchboard reads Claude Code's config files directly when stream-side `system/init` data is unavailable (disk rehydration after restart). The scope tables below are the authoritative in-repo summary; see [Claude Code MCP docs](https://code.claude.com/docs/en/mcp) and [Claude Code Settings docs](https://code.claude.com/docs/en/settings) for upstream documentation.

### MCP scopes (three levels)

| Scope     | Storage location                                              | Description                                |
| --------- | ------------------------------------------------------------- | ------------------------------------------ |
| `user`    | `~/.claude.json` top-level `mcpServers` table                 | All projects, current user                 |
| `local`   | `~/.claude.json` nested under the project's path entry        | Current project only, private to user (default for `claude mcp add`) |
| `project` | `<cwd>/.mcp.json` (separate file at the project root)         | Shared with the team via version control   |

Format: JSON. Switchboard's loader reads all three locations and merges by entry name; the resolution order matches Claude's own runtime behavior (project > local > user, with project winning when the same name is defined at multiple scopes).

For live dispatch, Switchboard does **not** read the files — Claude itself has already merged them and emits the result in the stream's `system/init` event. The config-loader is only used for disk rehydration (M2.6), where the session file lacks any `system/init`-equivalent record.

### Skills (directory-based, not config file)

Two locations scanned:

- `~/.claude/skills/<name>/SKILL.md` — user-scope skills, all projects.
- `<cwd>/.claude/skills/<name>/SKILL.md` — project-scope skills.

Each immediate subdirectory containing a `SKILL.md` counts as one skill; the directory name is the skill name. Merge by name; project-scope wins.

### Why this matters

Live Claude dispatch already provides the registry via `system/init`. The config-loader exists only to fill the gap for **rehydrated** transcripts (sessions loaded from disk after a Switchboard restart) — without it, the per-agent sidebar would show empty MCP / skills lists until the user dispatches a new turn and a fresh `system/init` arrives.

This is display-only information; dispatch is unaffected.
