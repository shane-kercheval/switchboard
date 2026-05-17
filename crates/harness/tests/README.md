# Harness live tests

This directory holds the **live-harness test suite** — integration tests that
spawn the real `claude` and `codex` CLIs and assert on the events the
adapters emit. They are developer-local; CI does not run them.

See `AGENTS.md` "Live testing against real harnesses" for the rationale
(upstream CLI vendors silently change behavior we depend on; fixture-driven
tests can't catch that).

## When to run

- Before merging any adapter-touching PR.
- After a Claude Code or Codex release.
- Periodically as a sanity check even when nothing has changed locally.

## How to run

One-time setup — make sure both harnesses are installed and authenticated
with subscription credentials (no API keys):

```sh
claude login
codex login
```

Then from the repo root:

```sh
make test-live
```

Runs `cargo test --locked -p switchboard-harness -p switchboard-dispatcher
-p switchboard-app -- --ignored`. Should complete in 1–3 minutes. Every
test prompt is constrained to a tiny response (e.g., "reply with ack") so
total subscription cost is negligible.

## What's covered

Live tests live in flat files directly under `crates/harness/tests/`.
**Convention**: add a new file when the category has a distinct coverage
focus (e.g., `tool_use.rs`, `transcript_load.rs`); extend `live.rs` for
happy-path coverage of new event types.

Coverage today:

- `live.rs` — happy-path event vocabulary for both adapters: `ContentChunk`,
  `TurnEnd { Completed }`, `SessionMeta`, `RateLimitEvent` (Codex), enriched
  `TurnEnd.usage.context_window`, sidecar shape (Codex), session resume
  (both harnesses).
- `tool_use.rs` — per harness, plant a sentinel file, prompt for a
  file-read / shell-cat tool, locate the `ToolCompleted` whose `output`
  contains the sentinel (`is_error: false`), and assert a matching
  `ToolStarted` was emitted for the same `tool_use_id`. Sentinel-driven
  pairing keeps the test robust against the CLI emitting preliminary tools
  (e.g., Claude using `TodoWrite` before the real Read).
- `transcript_load.rs` — full transcript-hydration round-trip:
  - Per harness, dispatch a live "ack" turn and assert the reconstructed
    `Turn::User` + `Turn::Agent` match the live stream (text, terminal
    status, no warnings) plus the sidebar contract: Claude hydrated
    `usage.is_some()` with `context_window.is_none()` (stream-only field);
    Codex hydrated `usage.context_window.is_some()` (enriched from
    `task_started.model_context_window`) and `last_rate_limit.is_some()`
    (from `token_count.rate_limits`); `meta.mcp_servers` / `skills` /
    `tools` deserialize structurally. The Codex test exercises the
    sidecar-driven lookup path (`commands::load_transcript_impl`'s
    production path).
  - Claude `live_claude_transcript_load_hydrates_tool_items`: dispatch a
    Read-tool turn, load the transcript, assert the agent turn contains a
    `TurnItem::Tool` with `is_error: Some(false)` and `output` carrying
    the staged sentinel. Drift-detection for tool persistence — neither
    `tool_use.rs` (live events only) nor the ack round-trip catches a CLI
    bump that changes how tool calls are written to the session file.

Dispatcher-layer live coverage (turn lifecycle ordering, session-id path
encoding, cwd routing) lives alongside the dispatcher in
`crates/dispatcher/tests/live_end_to_end.rs`.

A live auth probe (`live_check_codex_auth_finds_real_auth_file`) lives
inline in `crates/app/src/commands.rs` — it asserts that the real Codex
auth file is at the path `check_codex_auth_impl` expects.

## What's intentionally not covered live

- **`TurnEnd { Failed { kind: HarnessError } }`** — the adapters don't
  expose a model-override flag, so there's no reliable way to trigger
  `HarnessError` against a real CLI from outside the adapter. The
  fixture-driven tests (`claude_adapter.rs`, `codex_adapter.rs`) cover the
  parser side. If a production user reports an `HarnessError`-shaped
  failure that gets misclassified, capture the payload and write a fixture
  test from it.
- **`TurnEnd { Failed { kind: AdapterFailure } }`** and
  **`TurnEnd { Failed { kind: AuthFailure } }`** — same rationale: hard to
  trigger reliably against a real subprocess. Fixture coverage exists.
