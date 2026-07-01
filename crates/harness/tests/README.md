# Harness live tests

The **live-harness test suite** — integration tests that spawn the real
`claude`, `codex`, `gemini`, and `agy` (Antigravity) CLIs and assert on the
events the adapters emit. Developer-local; CI does not run them.

Why they exist: adapter correctness depends on behavior we don't control —
event vocabularies, exit codes, stream timing, session-file layout — and
upstream CLI vendors change it, sometimes silently. Fixture-driven tests lock
in our _current_ understanding and keep passing after upstream drift; live
tests are how we notice. Full rationale in `AGENTS.md` "Live testing against
real harnesses".

## When to run

- Before merging any adapter-touching PR.
- After a new release of any harness CLI — to catch upstream drift early.
- Periodically as a sanity check; vendors can change server-side behavior with
  no client version bump.

## How to run

One-time: install and authenticate each harness with subscription credentials
(no API keys) — `claude auth login`, `codex login`, an interactive `gemini`
sign-in, and the Antigravity desktop-app sign-in for `agy`.

```sh
make test-live              # all harnesses
make test-live-claude       # one harness — spend quota only on what you changed
make test-live-codex        #   (e.g. after that harness ships a new version)
make test-live-gemini
make test-live-antigravity
```

Every prompt is constrained to a tiny response (e.g. "reply with ack"), so the
whole suite costs cents and finishes in a few minutes. The constraint is
per-test response size, not test count.

## Conventions

- **Test naming is load-bearing.** Every live test name starts with
  `live_<harness>_` (`claude` / `codex` / `gemini` / `antigravity`); the
  per-harness `make` targets filter on it. See `AGENTS.md` for the rule. The
  authoritative inventory of what exists is
  `cargo test … -- --ignored --list`, **not this file** — so this file
  deliberately does not enumerate individual tests (a hand-maintained list
  just rots).
- **One file per coverage focus.** Adapter-level live tests are flat files
  here: `live.rs` (happy-path event vocabulary), `tool_use.rs` (tool
  lifecycle), `transcript_load.rs` (hydration round-trip). Dispatcher-layer
  end-to-end lives in `crates/dispatcher/tests/live_end_to_end.rs`; the
  binary/auth availability probes live inline in `crates/app/src/commands.rs`.
  Add a file when a category has a distinct focus; extend `live.rs` for new
  event types on the happy path.
- **Tiny, deterministic prompts.** Assert on structure and contracts, never on
  exact model wording (which varies run to run).

## What's covered

- **Happy-path event vocabulary** — `ContentChunk`, `TurnEnd { Completed }`,
  `SessionMeta`, per-harness `usage` / `RateLimitEvent`, and session resume.
- **Tool lifecycle** — a prompt forces a real tool call; assert `ToolStarted`
  pairs with a non-error `ToolCompleted`.
- **Transcript hydration** — dispatch a live turn, then reload it through the
  same `load_*_transcript` path the app uses on project open, and assert the
  reconstructed turns + sidebar contract match what the live stream emitted.
- **Dispatcher end-to-end** — the full `Directory → project → agent →
  send_message → real subprocess → events` slice, asserting the
  `turn_start → content → turn_end → agent_idle` ordering holds _identically_
  through every harness (proves the dispatcher is harness-neutral, not secretly
  coupled to one harness's behavior).
- **Availability probes** — assert the binary/auth probes find the real CLI,
  auth file, or Keychain entry where the adapter expects them; drift-detection
  for a harness relocating its auth or renaming its binary.

Two per-harness subtleties worth knowing (the _why_, since they aren't obvious
from a test name):

- **Gemini's live tool output is empty** for read-like tools
  (`tool_result.output = ""`) — the real content lives in the session file. So
  Gemini's tool-use live test pairs by id only, and the output-content
  assertion lives in the hydration test instead. This is the "live =
  best-effort, hydration = authoritative" contract.
- **Antigravity has no structured stream** — answer text and tool lifecycle are
  tailed from `transcript.jsonl`, and a turn is `Completed` only when a terminal
  answer is read from it. Its tool-use live test therefore doubles as the guard
  that an agentic (tool-using) turn still yields a readable terminal answer. See
  `docs/harness-behavior.md` (raw probe: `docs/research/archive/antigravity-cli-observed.md`).

## What's intentionally NOT covered live

Deliberate gaps — covered fixture-driven instead, because they can't be
triggered reliably (or non-destructively) against a real CLI:

- **`TurnEnd { Failed { … } }`** (`HarnessError` / `AdapterFailure` /
  `AuthFailure`) — the adapters expose no way to force a model error,
  subprocess failure, or auth failure from outside, and triggering an auth
  failure would break the developer's logged-in state. The parser side is
  covered by the fixture-driven adapter tests (`claude_adapter.rs`,
  `codex_adapter.rs`, `gemini_adapter.rs`, `antigravity_adapter.rs`). If a
  production user reports a misclassified failure, capture the payload and add
  a fixture test from it.
- **Auth-failure stream/output shapes** — same reason (can't break OAuth /
  Keychain state in a test). The substring-matching classifiers are
  best-effort until a production report sharpens them.
