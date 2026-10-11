# Research: How MCP clients handle long calls, progress, and large results

**Captured:** 2026-10-10
**Versions:** Claude Code 2.1.289 (CLI, interactive and `-p`); Codex CLI 0.161.0 (model GPT-6-Astra, medium effort); Claude Desktop 2.31226.1 (Chat, and Code in Local mode) through `mcp-remote` 0.14.3; probe server on `rmcp` 1.7.0.
**Decision:** Every `wait_*` call defaults to 50 seconds, and a call that carries no progress token is capped there. A call with a progress token may ask for up to an hour, with progress every 60 seconds. Results stay under 25,000 serialized characters (superseded: the MCP plan's revision 10 sets 100,000 for every client). No client needs MCP tasks. Claude Desktop uses `mcp-remote`. Codex is configured per folder, in a trusted orchestration folder. Details and the measurements behind each are in "Decisions this note settles".
**Consumed by:** [implementation_plans/2026-10-09-mcp.md](../implementation_plans/2026-10-09-mcp.md) (M0; the numbers M1, M3, and M4 use)

## Question

Switchboard's MCP server will offer `wait_send`, `wait_agent`, and `wait_workflow`: tools that
block until agent work finishes or a timeout passes. Agent turns take minutes. Before any
product code depends on it, we need to know, for Claude Code, Codex, and Claude Desktop:

1. How long a tool call may run before the client gives up, and whether progress
   notifications extend that.
2. Whether a long call blocks the session, or (Claude Code) moves to the background and
   delivers its result to the model when it settles, and whether that result starts the
   model working if the session is idle.
3. Whether a model told "not done, call again" keeps calling, gives up, or re-sends.
4. How a large tool result reaches the model: whole, cut, or saved to a file.
5. What each client declares at `initialize`, in particular MCP tasks support.
6. Whether Claude Desktop can reach a local HTTP server through `mcp-remote`, and which
   Codex configuration keys the installed version accepts.

What the docs said, as of 2026-10-10, and what the measurements found:

| Claim | Source | Measured |
|---|---|---|
| Claude Code: 5-minute idle limit for HTTP servers, reset by a progress notification | <https://code.claude.com/docs/en/mcp> | Confirmed. Overrides named in its error: a per-server `timeout` (ms) or `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT` |
| Claude Code: a call still running after 2 minutes moves to the background; its result arrives as a task notification | same | Confirmed, including in an idle session |
| Claude Code: results over 50,000 characters, or over `MAX_MCP_OUTPUT_TOKENS` (25,000 tokens), are saved to a file | same | Confirmed: the cut is between 49,500 and 50,500 characters of text |
| Codex: `tool_timeout_sec` defaults to 60 seconds | Codex MCP docs (<https://learn.chatgpt.com/docs/extend/mcp?surface=cli>, formerly developers.openai.com/codex/mcp) | **Wrong for 0.161.0: the default is 300 seconds** ("timed out awaiting tools/call after 300s") |
| Claude Desktop: URL connectors are reached from Anthropic's cloud, so local servers run over stdio | <https://claude.com/docs/connectors/building/mcpb> | Consistent: `mcp-remote` over stdio works in Chat and in Code → Local; Code → Cloud cannot see the server |

## Setup

What was run, on a Mac with all three clients signed in.

1. `cd tools/mcp-probe && cargo run --release --locked`, with `MCP_PROBE_TOKEN` set so the
   token survives restarts. The probe logs every request and tool event to
   `mcp-probe-log.jsonl` in that folder (never the token).
2. Clients, configured so that no user configuration file was changed except Claude
   Desktop's:
   - **Claude Code:** `claude --mcp-config mcp.json --strict-mcp-config --allowedTools
     'mcp__mcp-probe__*'` in a scratch folder, where `mcp.json` holds
     `{"mcpServers":{"mcp-probe":{"type":"http","url":"http://127.0.0.1:8765/mcp","headers":{"Authorization":"Bearer <token>"}}}}`.
     Interactive sessions ran in tmux; `-p` runs used `--output-format stream-json`.
     `claude mcp add` was not exercised; it writes the same server entry.
   - **Codex:** `codex exec` (and once the interactive TUI) with
     `-c 'mcp_servers.mcp_probe.url="http://127.0.0.1:8765/mcp"'`,
     `-c 'mcp_servers.mcp_probe.bearer_token_env_var="MCP_PROBE_TOKEN"'`, and
     `-c 'mcp_servers.mcp_probe.default_tools_approval_mode="approve"'`. Without the last key,
     `codex exec` refuses every MCP call: "MCP tool call requires approval, but approval policy
     is never".
   - **Claude Desktop:** an `mcpServers` entry in `claude_desktop_config.json`:
     `"command": "<absolute path to npx>"`, `"args": ["-y", "mcp-remote@0.14.3",
     "http://127.0.0.1:8765/mcp", "--header", "Authorization:${AUTH_HEADER}"]`, and `"env":
     {"AUTH_HEADER": "Bearer <token>", "PATH": "<node's bin folder>:/usr/bin:/bin"}`. The
     absolute path and `PATH` are needed when Node comes from nvm, because Desktop does not
     read the shell profile. `--allow-http` and `--transport http-only` are not needed for
     `127.0.0.1` (tested from the command line with and without each). Measured in Chat and in
     Code with "Local" chosen; Code with "Cloud" does not get the server's tools.
3. The probe's printed setup snippets predate these findings: its Codex block lacks
   `default_tools_approval_mode`, and its Desktop entry uses a bare `npx` plus two flags that
   are not needed.

## How to read the log

Each experiment is one prompt typed into the client. `tool_start` and `tool_end` give exact
timings. A client that gives up shows in one of two ways:

- **It sends `notifications/cancelled`,** or ends the session. The tool's `tool_end` then
  reads `cancelled by client` at that moment. Claude Code, Codex, and Desktop all did this.
- **It hangs up without cancelling.** `rmcp` does not drop a tool call's future when the
  HTTP connection closes, so the call runs on and logs `completed` even though nobody
  received the result. The transcript, not the log, is the evidence in that case. The
  `dropped (client went away)` outcome only appears when the probe itself stops.

The probe's own `rmcp` server also ends a session after 300 seconds without any session
event (`SessionConfig::keep_alive`, default 300 s, in
`rmcp-1.7.0/src/transport/streamable_http_server/session/local.rs`). A progress notification
counts as an event; a silent tool call does not. A silent call longer than 300 seconds is
therefore cancelled by the server, whatever the client allows (Codex run with
`tool_timeout_sec = 900`, below).

## Results

Claude Code is the CLI (2.1.289). Desktop is Claude Desktop 2.31226.1 in Code → Local mode
unless the row says Chat. Codex is 0.161.0 in `codex exec` unless the row says TUI. "Inline"
means the result reached the model whole, in the conversation.

| Exp | Claude Code | Codex | Claude Desktop |
|---|---|---|---|
| E1 protocol version, tasks? | Tries `server/discover` at `2026-07-28` first (the probe answers 422), then `initialize` at `2025-11-25`. Capabilities `elicitation`, `roots`. **No tasks.** | `initialize` at `2025-06-18`. Capabilities `elicitation`. **No tasks.** | `initialize` at `2025-11-25` as `claude-ai (via mcp-remote 0.14.3)` (Chat) or `local-agent-mode-mcp-probe (via mcp-remote 0.14.3)` (Code → Local). Capabilities `extensions.io.modelcontextprotocol/ui`, `roots` (Local only). **No tasks.** |
| Progress token sent with `tools/call`? | Yes | Yes | **No, in either mode** |
| E2 30 s | Inline | Inline (31 s) | Inline (32 s, Chat and Local) |
| E2 70 s | Inline | Inline (71 s). The model's code cell returns "still running" after about 30 s and the model polls it with a `wait` tool until the result arrives Local: **fails at 60.0 s**: Desktop sends `notifications/cancelled` with reason `SdkError: Request timed out`; the model sees "Error: Request timed out". Chat: **inline** (72 s), no cancel |
| E2 130 s | Moved to the background at 120 s; result delivered as a task notification at 130 s | Inline (131 s), after polling | Not run in Local: fails at 60 s. Not run in Chat |
| E2 330 s | Background at 120 s, then **aborted at 300 s**: "sent no response or progress for 300s; aborting". Same in `-p` (332 s) | **Fails at 300 s**: "timed out awaiting tools/call after 300s" | Not run in Local: fails at 60 s. Not run in Chat |
| E3 330 s with progress (60 s interval) | Completes (interactive, backgrounded; and `-p`, 334 s, inline) | **Fails at 300 s** (333 s): progress does not extend Codex's limit | Not run: no progress token, so no progress can be sent |
| E3 1800 s with progress | Completes; 29 progress notifications; delivered as a task notification | — | — |
| Codex, `tool_timeout_sec = 900` | — | 400 s with progress: **completes**. 335 s without progress: the probe's server cancels the session at 305 s (its 300-s keep-alive), and Codex waits until 900 s before failing | — |
| E4.1 backgrounded, session usable, result delivered | Yes, yes, yes. `/tasks` lists the call with "status working · 90% · 3 progress notifications so far" | — | — |
| E4.2 idle session resumes by itself | **Yes.** The turn ended at 120 s; when the result arrived about two minutes later, Claude started a new turn on its own and reported it | No backgrounding: the turn stays open while the model polls the call, so the result is always seen in the same turn (50 s run) | No backgrounding below 60 s: a 52-s call returned in the same turn |
| E5 loop, `wait_seconds` 20 | 9 checks, 21.7–22.7 s apart; finished with the word | 9 checks, 21.3–21.8 s apart; finished | 8 checks, 22.1–22.4 s apart (one 27.0 s); finished |
| E5 loop, `wait_seconds` 50 | not run | 4 checks, 51.7–52.1 s apart; finished | 4 checks, 52.2–52.5 s apart; finished, no timeout |
| E6 20,000 | Inline | Inline (20,001) | Inline (20,002) |
| E6 45,000 | Inline; also inline at 49,500 | **Cut in the middle** (45,001): the model gets about 10,000 tokens (40,150 characters), head and tail kept, with "Warning: truncated output (original token count: 11251)" and "…N tokens truncated…" | Inline (45,002) |
| E6 60,000 | Saved to a JSON file; the model sees a 2 KB preview and the path. The cut is between 49,500 (inline) and 50,500 (file) characters of text | Cut in the middle to about 10,000 tokens Local: saved to a file with a 2 KB preview, as Claude Code (60,002). Chat: **inline**, whole (60,002) |
| E6 120,000 | Over the token limit: saved to a text file with **no preview**, only an error and the path. Asked to, the model read the file with shell tools | Cut in the middle to about 10,000 tokens | Same as Claude Code (120,002) |
| Idle session expired by the server, next call | Got 404, re-initialized, re-sent the call; ran once; nothing visible to the model | TUI, six idle minutes: the same (404, re-initialize, re-send, ran once) | `mcp-remote` got 404, re-initialized (`mcp-remote-reinit-1`), re-sent; ran once (Chat and Local) |
| E7 configuration | Not exercised (used `--mcp-config`) | See "Codex configuration" | `mcp-remote` 0.14.3 works against `http://127.0.0.1` without extra flags; Chat and Code → Local only |

Chat's upper limits were not measured past 72 seconds and 60,002 characters, because the
decisions below already stay inside Code → Local's tighter ones.

### Codex configuration

Verified against 0.161.0:

- `url` and `bearer_token_env_var` connect; the docs also list `http_headers`,
  `env_http_headers`, and `http_headers_helper` (not exercised).
- `default_tools_approval_mode = "approve"` is required for unattended use. The docs list the
  values `auto`, `prompt`, `writes`, and `approve`, and per-tool overrides under
  `tools.<tool>.approval_mode`.
- `tool_timeout_sec` raises the 300-second default (900 worked).
- **Per-folder configuration exists, for trusted folders only.** A `[mcp_servers.…]` block in
  `<folder>/.codex/config.toml` connected when the folder was trusted (a git repository, trust
  set through `-c projects=…`) and was not loaded at all when it was not trusted (the probe
  saw no request).
- With this model, Codex calls MCP tools from inside a JavaScript code cell
  (`await tools.mcp__mcp_probe__slow(...)`), which returns "still running" after about 30
  seconds and is then polled with a `wait` tool. Both the polling and the 10,000-token cut
  in E6 come from that cell, not from the MCP client. The cut matches the size of Codex's
  `tool_output_token_limit` setting, if that is what applies (a guess, from the setting name
  in the binary).

## Decisions this note settles

Each with the measurement that settles it.

- **Default `timeout_seconds` for `wait_*`: 50 seconds.** Claude Desktop's Code → Local mode
  cancels any call at 60.0 s (E2 70 s) and sends no progress token, so nothing can extend
  that. Chat tolerated 72 seconds but also sends no token. 50-second waits completed in
  Desktop, Codex, and Claude Code (E5).
- **How the server applies per-client limits: by the progress token, not the client's
  name.** A call without a progress token is capped at 50 seconds, because the server cannot
  keep it alive (Desktop, and anything built on the same SDK default). A call with a token may
  ask for up to 3,600 seconds. The tool descriptions say who should ask for more: in Claude
  Code a long wait moves to the background and its result wakes the session (E4), so it is
  the better choice there; in Codex stay under 300 seconds unless the user raised
  `tool_timeout_sec`, because progress does not extend Codex's limit (E3). Client names were
  rejected because Desktop's embeds the user's server name
  (`local-agent-mode-<name>`) and a wrong guess fails silently; a wait that outlives Codex's
  limit fails loudly and the caller retries with a shorter one.
- **Progress cadence: 60 seconds,** whenever the client sent a token. Claude Code's idle
  limit is 300 seconds (E2), and `rmcp`'s session keep-alive is 300 seconds; 60-second
  progress kept a call alive for 1,800 seconds (E3). Progress is also what keeps the
  server's own session alive, so a wait with a token must never go silent.
- **Server session keep-alive: keep `rmcp`'s 300-second default, and add a test.** Clients
  whose session expired while idle reconnect and re-send the call without the model noticing
  (Claude Code, Codex's TUI, `mcp-remote`). The server holds no per-session state of its
  own, so losing a session loses nothing. What the default does constrain is a silent call
  over 300 seconds, which the two rules above rule out. M3 should test that a wait with a
  progress token survives past 300 seconds.
- **Result budget: 25,000 serialized characters** (superseded by the MCP plan's revision 10:
  100,000 for every client, with each client's own limit applying on top). Codex keeps only
  about 10,000 tokens of a
  result, cutting the middle (E6), and this numbered text ran 4.0 characters per token; JSON
  with escapes and code runs denser, so 25,000 characters stays under 10,000 tokens at 2.5
  characters per token. Claude Code's file cut at 50,000 characters is looser. The cost is
  more `read_send_output` pages for long replies. Placeholder was 40,000, which Codex would cut.
- **MCP tasks: not needed.** No client declares support (E1).
- **Claude Desktop path: `mcp-remote`,** with an absolute `npx` path and a `PATH` entry in
  the snippet, in Chat or Code → Local. No in-repo bridge. The README must say Code → Cloud
  cannot use it.
- **Codex configuration shown in Settings:** a `[mcp_servers.switchboard]` block with `url`,
  `bearer_token_env_var` (or `http_headers`), and `default_tools_approval_mode = "approve"`,
  placed in `.codex/config.toml` of a trusted folder that is not any project's folder. Codex
  agents Switchboard launches then do not see the tools, the same folder rule as Claude Code.
  Putting it in `~/.codex/config.toml` would give every launched Codex agent the tools.

### Other findings the plan should absorb

- A tool call keeps running on the server after its client hangs up without cancelling (see
  "How to read the log"). For `wait_*` this is harmless, since waits end at their timeout;
  the M3 test that disabling the server ends in-flight calls checks the cancellation path.
- Claude Code's backgrounded call "does not survive exiting this session" (its own message).
  A `wait_send` lost that way loses nothing; the send keeps running in the app.
- `rmcp` 1.7 answers Claude Code's `server/discover` with 422, and Claude Code falls back to
  `initialize`. Harmless today; worth rechecking when `rmcp` is upgraded.
