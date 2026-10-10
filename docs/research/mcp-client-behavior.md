# Research: How MCP clients handle long calls, progress, and large results

**Captured:** in progress (procedure written 2026-10-10; results pending)
**Versions:** fill in per client below
**Decision:** pending — see "Decisions this note settles"
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

What the docs say, as of 2026-10-10, to check against:

- Claude Code (<https://code.claude.com/docs/en/mcp>): `MCP_TOOL_TIMEOUT` wall-clock limit,
  default about 28 hours; a 5-minute idle limit for HTTP servers that a progress notification
  resets; a main-conversation call still running after two minutes moves to a background task
  and "the result arrives as a task notification when the call settles" (v2.1.212 or later,
  interactive sessions only); results over 50,000 characters, or over `MAX_MCP_OUTPUT_TOKENS`
  (default 25,000 tokens), are saved to a file the model reads.
- Codex: third-party guides citing <https://developers.openai.com/codex/mcp> give `url`,
  `bearer_token_env_var`, `http_headers`, `env_http_headers`, and `tool_timeout_sec`
  (default 60 seconds). Not read first-hand; verify.
- Claude Desktop: URL-based custom connectors are reached from Anthropic's cloud, so they
  cannot reach `127.0.0.1`; local servers run over stdio
  (<https://claude.com/docs/connectors/building/mcpb>). `mcp-remote` bridges stdio to HTTP.

## Setup

On a Mac with Claude Code, Codex, and Claude Desktop installed and signed in, and Rust and
Node installed:

1. `cd tools/mcp-probe && cargo run --release --locked`. It prints the URL, a fresh token,
   and a ready-to-paste configuration for each client, and writes every request and tool event
   to `mcp-probe-log.jsonl` in that folder. Leave it running. To keep the same token across
   restarts, set `MCP_PROBE_TOKEN`.
2. Configure each client from the printed snippets:
   - **Claude Code:** run the printed `claude mcp add …` in a scratch folder (not a project
     folder), and start `claude` in that folder. `/mcp` should list `mcp-probe` as connected.
   - **Codex:** add the printed `[mcp_servers.mcp-probe]` block to `~/.codex/config.toml`,
     after checking the key names against `codex mcp add --help` and Codex's docs, and start
     `codex` with `MCP_PROBE_TOKEN` set to the token. Record which keys worked.
   - **Claude Desktop:** add the printed `mcp-probe` entry under `mcpServers` in
     `claude_desktop_config.json` (Settings → Developer → Edit Config), and restart Claude
     Desktop. If it fails to connect, try without `--transport http-only`, then without
     `--allow-http`, and record what worked.
3. Record each client's version: `claude --version`, `codex --version`, and Claude Desktop's
   About window.

Every experiment below is one prompt typed into the client. The probe's tools tell the model
to call them only when asked, so phrase each prompt as an instruction. After each one, read
the log: `tool_start` and `tool_end` give exact timings, and a `tool_end` with outcome
`dropped (client went away)` means the client gave up at that moment.

## Experiments

Run each for each client unless marked otherwise.

**E1 — What the client declares.** Connect only. In the log, find the `http_request` whose
body has `"method": "initialize"`. Record `params.protocolVersion`, `params.clientInfo`, and
`params.capabilities`. MCP tasks support shows as a `tasks` key in `capabilities`.

**E2 — Limit without progress.** Prompts: "Call slow with seconds 30", then 70, 130, 330.
Record for each: result returned, or the client's error and the `elapsed_s` of the dropped
call. Expected from docs: Codex fails past 60 s; Claude Code fails near 300 s (idle limit).

**E3 — Progress keeps it alive.** "Call slow_with_progress with seconds 330 and
interval_seconds 60." Record whether it completes, and whether the client sent a progress
token (`progress_token_received` in `tool_start`). If Claude Code completes it, repeat with
seconds 1800 to confirm a long wait.

**E4 — Claude Code backgrounding (Claude Code only, interactive).**

1. "Call slow_with_progress with seconds 200 and interval_seconds 60." After two minutes,
   check that `/tasks` lists the call and that you can ask an unrelated question meanwhile.
   Record whether the result reaches the conversation when it finishes.
2. Idle wake: "Call slow_with_progress with seconds 240 and interval_seconds 60. While it
   runs, don't do anything else; when its result arrives, tell me the time it finished." Then
   type nothing. Record whether Claude resumes by itself when the result arrives, or only
   after you send another message.
3. Repeat step 2 in Codex and Claude Desktop with seconds 50, for comparison.

**E5 — The call-again loop.** "Start a job of 180 seconds with start_job, then call check_job
with wait_seconds 20 until it is done, and tell me the secret word." Record from the log the
number of `check_job` calls and the gaps between them, and from the transcript whether the
model finished, gave up, asked you, or started a new job. For Codex also run it with
wait_seconds 50.

**E6 — Large results.** "Call big with chars 20000 and tell me the last line number you can
see and whether you see END." Repeat with 45000, 60000, and 120000. Record for each: whole,
cut (where), saved to a file the model then read, or an error.

**E7 — Configuration checks.** Codex: the configuration keys that worked, and whether Codex
supports MCP servers configured per project folder (so Codex agents launched by Switchboard
need not see the server). Claude Desktop: whether `mcp-remote` worked against
`http://127.0.0.1`, with which flags.

## Results

Fill in one row per experiment per client. "—" means not applicable.

| Exp | Claude Code (version) | Codex (version) | Claude Desktop (version) |
|---|---|---|---|
| E1 protocol version, tasks? | | | |
| E2 30 s | | | |
| E2 70 s | | | |
| E2 130 s | | | |
| E2 330 s | | | |
| E3 330 s with progress | | | |
| E3 1800 s with progress | | — | — |
| E4.1 backgrounded, session usable, result delivered | | — | — |
| E4.2 idle session resumes by itself | | | |
| E5 loop: checks, gaps, outcome | | | |
| E6 20,000 | | | |
| E6 45,000 | | | |
| E6 60,000 | | | |
| E6 120,000 | | | |
| E7 configuration | — | | |

## Decisions this note settles

Record each with the measurement that settles it. The plan's §0.1 lists these as open.

- Default `timeout_seconds` for `wait_*`, and the maximum per client.
- How the server applies per-client limits: chosen from the client's `initialize` name, or one
  conservative default with a high maximum the caller may raise.
- Progress-notification cadence (must be under the shortest idle limit found).
- The result budget, in serialized characters.
- Whether any client needs MCP tasks.
- The Claude Desktop path: `mcp-remote`, or an in-repo stdio bridge (M4).
- The Codex configuration Settings shows, and whether Codex can keep the tools away from the
  agents Switchboard launches.
