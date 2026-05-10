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

Where `<encoded-cwd>` is the absolute working directory with `/` replaced by `-`. For example, `/private/tmp/switchboard-probe` becomes `-private-tmp-switchboard-probe`.

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

This also resolves part of plan open question 10.1 (what about tool-call-only responses): there is no such thing as a "tool-call-only response" at the harness level — the harness keeps cycling (model → tool_use → tool_result → model → ...) until the model emits a final text and the `result` event fires. Switchboard always sees a complete turn.

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

**Switchboard implication:** denials do not error the turn (`is_error: false`) — the model receives the denial as feedback and adapts its response. `permission_denials` contains the full attempted call with arguments. Switchboard can use this to surface "the model tried X but was blocked" affordances without treating the turn as failed. Note that with disallowed *tools* (e.g. `--disallowedTools Read`), the model often routes around (we saw it use `cat` via `Bash` after `Read` was disallowed) — disallowance prevents the tool from appearing, denial happens after attempt.

## Concurrent invocations

Three parallel `claude -p` invocations from the same cwd completed cleanly:

- Three unique session UUIDs (one per process)
- Three separate session JSONL files, no contention
- Different durations (1.5s, 2.1s, 6.4s) — independent timing

Wall-clock for the slowest invocation matched its individual duration, confirming actual parallelism (not queueing). **Implication: fan-out via concurrent process spawn is safe at this scale.** No file locking or session-state contention observed.

## Things still worth probing

- **`--input-format stream-json`** — for sending follow-up messages mid-stream without restarting the process. Could matter for the "long-lived agent process" deferred decision.
- **Compaction events in the stream** — does auto-compact emit observable events? Not seen in any of our short tests because we never approached the threshold.
- **`--include-hook-events`** — what hook events flow through, and do they let Switchboard observe pre/post-tool-call lifecycle?
- **`--debug` filtering** — what categories exist, and does any of it give us harness-level observability we'd want?

These can be picked up later; they are not blocking for §5 design.

## Resolutions / updates for the plan

1. **Open question 10.12 (model→max-context map)** — partially resolved for Claude Code: `contextWindow` is in `result.modelUsage.<model>`. Switchboard does not need to maintain a Claude Code map. (Codex still TBD.)
2. **Open question 10.1 (tool-call response handling)** — effectively resolved: the harness handles the loop internally; Switchboard sees one `result` event per user-initiated turn regardless of how many tool cycles happened inside.
3. **§5 "Required harness commands" — `Fork a session from a checkpoint`** — confirmed native via `--fork-session`. Can mark as native, not "needs verification."
4. **§5 "Process model"** — confirmed default `-p` loads the user's full environment exactly as the prior research note claimed. Confirmed `--bare` strips user-installed extensions but keeps built-ins.

## Sources

- Hands-on probes captured in `/tmp/switchboard-probe/` (`hello-json.out`, `hello-stream.out`, `tool-call.out`).
- Session files at `~/.claude/projects/-private-tmp-switchboard-probe/*.jsonl`.
- `claude --help` (v2.1.138).
