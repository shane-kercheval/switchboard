# Harness live tests

This directory holds the **live-harness test suite** — integration tests that
spawn the real `claude`, `codex`, and `gemini` CLIs and assert on the events
the adapters emit. They are developer-local; CI does not run them.

See `AGENTS.md` "Live testing against real harnesses" for the rationale
(upstream CLI vendors silently change behavior we depend on; fixture-driven
tests can't catch that).

## When to run

- Before merging any adapter-touching PR.
- After a Claude Code or Codex release.
- Periodically as a sanity check even when nothing has changed locally.

## How to run

One-time setup — make sure all three harnesses are installed and
authenticated with subscription credentials (no API keys):

```sh
claude login
codex login
gemini   # interactive sign-in (no `gemini auth login` subcommand)
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

- `live.rs` — happy-path event vocabulary for all three adapters:
  `ContentChunk`, `TurnEnd { Completed }`, `SessionMeta`, `RateLimitEvent`
  (Codex), enriched `TurnEnd.usage.context_window` (Claude / Codex),
  sidecar shape (Codex), session resume (all three). Gemini's
  `SessionMeta` carries `tools: vec![]` (its `init` doesn't list tools);
  the test asserts the structural contract without requiring a populated
  tools list.
- `tool_use.rs` — per harness, plant a sentinel file, prompt for a
  file-read / shell-cat tool, assert `ToolStarted` + matching
  `ToolCompleted` (`is_error: false`).
  - Claude / Codex pair by sentinel-in-output: `ToolCompleted.output`
    contains the staged sentinel. Robust against the CLI emitting
    preliminary tools (e.g., Claude using `TodoWrite` before the real
    Read).
  - Gemini pairs by `tool_use_id` + `is_error: false` **without checking
    output content**. Gemini's stream emits `tool_result.output = ""` for
    read-like tools (real content lives in the session file). The
    sentinel-in-output assertion for Gemini moves to `transcript_load.rs`
    where the session file does carry it.
- `transcript_load.rs` — full transcript-hydration round-trip:
  - Per harness, dispatch a live "ack" turn and assert the reconstructed
    `Turn::User` + `Turn::Agent` match the live stream (text, terminal
    status, no warnings) plus the sidebar contract. Per-harness
    `usage` / `last_rate_limit` shape:
    - Claude: `usage.is_some()` with `context_window.is_none()`
      (stream-only field).
    - Codex: `usage.context_window.is_some()` (enriched from
      `task_started.model_context_window`) and `last_rate_limit.is_some()`
      (from `token_count.rate_limits`). Exercises the sidecar-driven
      lookup path (`commands::load_transcript_impl`'s production path).
    - Gemini: `usage.is_some()` with `context_window.is_none()` (Gemini's
      session file has no context-window field) and `last_rate_limit` is
      `None` (no rate-limit telemetry in the session file).
  - `live_claude_transcript_load_hydrates_tool_items` + the Gemini
    counterpart: dispatch a Read-tool turn, load the transcript, assert
    the agent turn contains a `TurnItem::Tool` with `is_error: Some(false)`
    and `output` carrying the staged sentinel. For Gemini this is **load-
    bearing** for the "live = best-effort, hydration = authoritative"
    contract — the live stream emits empty tool output, hydration fills
    it in. For both: drift-detection against a CLI bump renaming
    tool-record fields in the session file.

Dispatcher-layer live coverage (turn lifecycle ordering, session-id path
encoding, cwd routing) lives alongside the dispatcher in
`crates/dispatcher/tests/live_end_to_end.rs`. Includes a Gemini variant
of the event-ordering check (`turn_start → content_chunk → turn_end →
agent_idle`) that proves the dispatcher abstraction is genuinely
harness-neutral through the real subprocess code path, not just through
adapter-layer fixtures.

Live auth probes (`live_check_codex_auth_finds_real_auth_file`,
`live_check_gemini_auth_finds_real_settings_file`) live inline in
`crates/app/src/commands.rs` — they assert the real auth files live at
the paths `check_codex_auth_impl` and `check_gemini_auth_impl` expect.

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
- **Gemini tool-output content in the stream** — elided for read-like
  tools (`tool_result.output = ""`), per
  `docs/research/gemini-cli-observed.md`. Covered by
  `live_gemini_transcript_load_hydrates_tool_items` against the session
  file, which carries the real output.
- **Gemini auth-failure stream shape** — cannot trigger without breaking
  the developer's OAuth state. Covered fixture-driven via the inline-JSON
  test in `gemini_adapter.rs`; the substring-matching rule
  (`is_gemini_auth_failure_message`) is best-effort until a production
  user reports a misclassification.
