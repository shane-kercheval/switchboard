# mcp-probe

A throwaway MCP server for M0 of
[`docs/implementation_plans/2026-10-09-mcp.md`](../../docs/implementation_plans/2026-10-09-mcp.md):
it measures how Claude Code, Codex, and Claude Desktop handle long tool calls, progress
notifications, large results, and "call again" loops. The procedure and the results table are
in [`docs/research/mcp-client-behavior.md`](../../docs/research/mcp-client-behavior.md). This
folder is removed once that note is recorded.

It is a Cargo workspace of its own, so `make check` never builds it, and it uses the same
`rmcp` version as the app.

```bash
cd tools/mcp-probe
cargo run --release --locked
```

It listens on `http://127.0.0.1:8765/mcp`, prints a fresh bearer token and a configuration
snippet for each client, and appends every request and tool event to `mcp-probe-log.jsonl`
here. The token is never written to the log.

Environment variables, all optional: `MCP_PROBE_PORT` (default 8765), `MCP_PROBE_TOKEN`
(default: random), `MCP_PROBE_LOG` (default `mcp-probe-log.jsonl`).

Tools: `slow`, `slow_with_progress`, `big`, `start_job`, `check_job`. Each tool's description
says what it does and tells the model to call it only when asked.
