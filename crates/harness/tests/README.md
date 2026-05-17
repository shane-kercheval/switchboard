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
- `tool_use.rs` — `ToolStarted` and matching `ToolCompleted` (correlated by
  `tool_use_id`, `is_error: false`) for a successful file-read / shell tool
  call per harness.
- `transcript_load.rs` — full M2.6 round-trip: dispatch a live turn, then
  call `load_*_transcript` and assert the reconstructed `Turn::User` and
  `Turn::Agent` match the live stream (text content, terminal status, no
  parser warnings, model populated). The Codex test exercises the
  sidecar-driven lookup path (`commands::load_transcript_impl`'s production
  path).

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
