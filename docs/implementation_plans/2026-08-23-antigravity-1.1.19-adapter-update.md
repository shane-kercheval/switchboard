# Antigravity `agy` 1.0.14 → 1.1.19: fix the breakages, adopt the new capabilities

**Status: complete.** Branch: `harness-review/antigravity-1.1.19`.

**Version note:** planned against `agy` 1.1.19; `agy` auto-updated to **1.1.20** mid-review,
and all final verification was run against 1.1.20 (18/18 live). The 1.1.20 delta was probed
and is benign — see `docs/harness-behavior.md` §6.

This plan remediates the findings of the 2026-08-23 harness-update review of Antigravity
(`agy` 1.0.14 → 1.1.20, 35 releases). The review found four breaking changes and three new
capabilities (model/effort selection, a structured output stream, and a quota query). The
decision (with the project owner) was to **address everything that broke and implement
everything that opened up** — not to patch minimally and defer.

**Outcome: five of six milestones shipped. M5 (quota) was built and then cut** — the
`/usage` poll works, but it costs 3.7–6.6s of phantom-busy time on every turn and the ways
around that cost more than the gauge is worth on the least-used harness. The capability is
recorded in `harness-behavior.md` §5.1 so it isn't rediscovered as an easy win.

## Required reading before implementing

- `AGENTS.md` — especially **"Agents: run long commands in the FOREGROUND, and wait"** (live
  tests get SIGTERMed if backgrounded) and the **live-test naming convention**
  (`live_antigravity_*` prefix is load-bearing for `make test-live-antigravity`).
- `docs/harness-behavior.md` §0, §1.1–1.4, §3, §3.3, §3.4, §4 (G1/G11/G15/G16/G18/G21/G24),
  §6 (the Antigravity 1.1.2 invalid-tool-call entry — this plan supersedes it), §7.3.
- `docs/harness-update-review.md` — the playbook this review ran under; M6 updates it.
- `docs/research/archive/antigravity-cli-observed.md` — frozen provenance for the old
  contract. Cite it; do not edit it.
- `crates/harness/src/antigravity/mod.rs` module doc — the current five-bullet contract
  statement. Three of its five bullets are now stale; M2 rewrites it.
- There is no public web documentation for `agy`. The authoritative changelog is the CLI
  itself: `agy changelog < /dev/null`.

Process: follow the user's standing rules — **never commit or push unless explicitly told**;
prepare work in milestone-sized units and stop for review. Run `make fmt` / `make lint` /
`make test` per milestone and `make check` + `make test-live-antigravity` at the end.

---

## Probe evidence (2026-08-23, agy 1.1.19)

The scratch findings file from the review lived in `/tmp` and may be gone; the load-bearing
evidence is reproduced here. All probes used the adapter's exact flag set
(`-p <prompt> --add-dir <cwd> --dangerously-skip-permissions --print-timeout 24h --log-file <tmp>`)
unless noted.

### Verified unchanged (do not touch)

- Transcript path `~/.gemini/antigravity-cli/brain/<uuid>/.system_generated/logs/transcript.jsonl`
  and record shape (`step_index, source, type, status, created_at, content[, thinking]`);
  terminal `MODEL`/`PLANNER_RESPONSE` with `status:"DONE"`. New sibling `chunks/` dirs and a
  `transcript_full.jsonl` exist; both were byte-identical to `transcript.jsonl` in probes.
- `USER_SETTINGS_CHANGE` model announcement still rides `USER_INPUT.content` on turn 1 and on
  model change (`from None to Gemini 3.1 Pro (High)`); carry-forward readback (§3.3) intact.
- The CLI-log `Created conversation <uuid>` line still exists (the `server.go:` line number
  moved 755 → 1074; our matcher anchors on the text, not the number).
- Unknown `--conversation <uuid>` → exit 0 + stderr `warning: conversation "…" not found` +
  fresh conversation minted. Fork-and-heal contract intact.
- Dash-leading *sentence* prompts (`"- Reply …"`) still dispatch fine (G18's Antigravity arm).
- Live suite before any fix: **14/15 pass**. The one failure is real (breakage 1 below).

### Breakage 1 — failed tool calls write no transcript record (fails a live test)

`live_antigravity_invalid_file_read_completes_as_tool_error` fails @ 1.1.19. A tool call
rejected during argument validation writes **no record at all** to any transcript file — the
`step_index` sequence skips it (`0 USER_INPUT · 1 CHECKPOINT · 2 PLANNER_RESPONSE(tool_calls)
· 4 PLANNER_RESPONSE("ack")`; step 3 absent from `transcript.jsonl`, `transcript_full.jsonl`,
and `chunks/*`). Reproduced twice. **Successful** tool results still land on disk (as
`MODEL`/`GENERIC` records) — the loss is failed calls only. This supersedes the agy 1.1.2
`SYSTEM`/`ERROR_MESSAGE` contract in `harness-behavior.md` §6: that record no longer exists.
Symptom without a fix: the tool row spins forever, and FIFO pairing mis-attributes the *next*
tool's result to the failed call.

The information still exists on the structured stream (see "stream-json" below): the tool's
`step_update` is emitted twice, `state:"ACTIVE"` then **`state:"ERROR"`** — a third terminal
state alongside `ACTIVE`/`DONE`.

### Breakage 2 — errors moved stdout → stderr; exit codes became meaningful

| Condition | old (documented) | now @ 1.1.19 |
|---|---|---|
| print timeout | exit 0, `Error: timed out waiting for response` on **stdout** | exit **1**, `Error: timeout waiting for response` on **stderr** (wording changed too) |
| empty prompt | exit 0 | exit **1**, stderr `Error: Error: empty prompt.` |
| invalid `--model`/`--effort` | n/a | exit **1**, stderr `Error: invalid model selection (…)` |
| prompt exactly equal to a known flag name | n/a | exit **2**, stderr `Error: -p took "--sandbox" as its prompt…` |

`classify_outcome` scans **stdout only** (`first_error_line(stdout_lines)`), so a timeout now
misclassifies as `AdapterFailure` ("agy exited without producing an answer; stderr: …")
instead of `HarnessError` with the real message. **Unverified:** whether the
`Authentication required` line also moved to stderr — the probe that would answer this
(isolated `$HOME`) disrupts the developer's real auth session and must **not** be run.
Scanning both streams covers either answer.

### Breakage 3 — print mode intercepts slash commands

- `-p "/model"` → exit 0, prints a TSV row, **no conversation created**, no quota spent. Our
  adapter would see stdout content but capture no UUID → `AdapterFailure`.
- `-p "/clear"` → exit 2, stderr names the fix: `pass --disable-slash-commands`.
- Path-shaped (`/tmp/...`) and unknown-command (`/notarealcommand …`) prompts fall through to
  the model normally.
- With `--disable-slash-commands`, `/model` and `/clear` were verified to reach the model and
  mint a conversation.

`harness-behavior.md` §0's claim that Antigravity treats slash prompts as model text is stale.

### Breakage 4 — a prompt that is *exactly* a flag name is rejected pre-dispatch

Rejected (exit 2) iff the `-p` value exactly matches a known `agy` flag token: `--sandbox`,
`--add-dir`, `--model`, `-p`, `-c` all rejected; `--notaflag` and `--sandbox looks like a
flag, reply 'ack'` reach the model; `--help` is intercepted (prints usage, exit 0). The
attached form `-p=<value>` does **not** avoid it (the check is on the value). A leading
transport-only ASCII space **does**: `" --sandbox"` and `" -p"` both reached the model.

### Capability 1 — model and effort are now selectable headlessly

- `agy models` (needs auth + network; prints "Fetching available models…" then TSV
  `slug<TAB>display name`). Note: the 1.1.12 changelog claims `--output-format json` on this
  subcommand, but 1.1.19 **rejects the flag** — TSV is all there is.
- `--model <slug>` dispatched headlessly in 5.5 s (the old ">60 s hang" report in §3.3 is
  stale) and — decisive — **did not mutate `~/.gemini/antigravity-cli/settings.json`**
  (byte-identical before/after), which removes §3.3's core objection (the only lever used to
  be harness-owned global config).
- Validation is client-side, pre-dispatch, exit 1, quota-free, and self-describing.

Probe matrix (each row live-verified @ 1.1.19):

| Invocation | Result |
|---|---|
| `--model gemini-3.5-flash-low` | ✅ runs; transcript reads back `Gemini 3.5 Flash (Low)` |
| `--effort low` (no `--model`) | ✅ re-selects the effort variant of the active model (`Gemini 3.1 Pro (Low)`) |
| `--model gemini-3.1-pro` (bare) | ❌ `requires --effort (available: low, high)` |
| `--model gemini-3.1-pro-high --effort low` | ❌ folded slug `conflicts with --effort=low` |
| `--model claude-sonnet-4-6 --effort high` | ❌ `--effort is not supported for model "claude-sonnet-4-6"` |
| `--model claude-opus-4-6-thinking --effort low` | ❌ `--effort is not supported …` |
| `--model gpt-oss-120b-medium --effort low` | ❌ folded slug conflicts |
| `--model gpt-oss-120b` (bare) | ✅ runs |
| `--model gpt-oss-120b --effort medium` | ✅ runs; reads back `GPT-OSS 120B (Medium)` |
| `--effort bogus` | ❌ `invalid --effort "bogus" (valid: low, medium, high)` |
| `--model no-such-model-xyz` | ❌ enumerates the available models |

Account catalog @ 1.1.19 (from `agy models`): `gemini-3.7-flash-{high,medium,low}`,
`gemini-3.6-flash-{…}`, `gemini-3.5-flash-{…}`, `gemini-3.1-pro-{high,low}`,
`claude-sonnet-4-6`, `claude-opus-4-6-thinking`, `gpt-oss-120b-medium`.

### Capability 2 — `--output-format stream-json` (added 1.1.8)

NDJSON on stdout. Observed vocabulary (probe, 1.1.19):

```
{"event":"init","conversation_id":"<uuid>","init":{"cwd":…,"permission_mode":…,"tools":[…]}}
{"event":"step_update","step_update":{"conversation_id":…,"step_index":N,"state":"ACTIVE|DONE|ERROR",
  "step_type":"user_input|checkpoint|agent_response|tool",
  "tool_name":…, "tool_info":{"name","parameters","output"},   // tool steps
  "text_delta":…, "usage":{…}, "duration_seconds":…}}          // agent_response steps
{"event":"result","result":{"conversation_id":…,"status":"SUCCESS|ERROR","response":…,
  "error":…,"duration_seconds":…,"num_turns":…,
  "usage":{"input_tokens","output_tokens","thinking_tokens","cache_read_tokens","total_tokens"}}}
```

- Failure shape verified via a forced 3 s timeout: `status:"ERROR"`,
  `error:"timeout waiting for response"`, exit 1 — the error rides the `result` event on
  stdout.
- Stream `step_index` values match the transcript's.
- `agent_response` steps carried `text_delta` + `usage` but **no thinking text** in probes —
  the transcript's `thinking` field has no observed stream equivalent. This is why the stream
  is adopted as a **control channel only** (below), not as the content source.

### Not probeable (record as unverified, design around it)

- The auth-failure shape under 1.1.19 (either output format) — probing requires logging out.
- The quota `RESOURCE_EXHAUSTED` shape under 1.1.19 — cannot be forced.
- Whether either appears in the `stream-json` `result.error` — same reason. The existing
  stdout/log detectors are therefore **kept as fallbacks**, not deleted.

---

## Architecture decision (applies to M2–M3; establish once, reuse)

**The stream is the control channel; the transcript stays the content source.** Adopting
`stream-json` is an *addition*, not a swap:

- From the stream: the conversation id (`init`), tool lifecycle including the `ERROR` state
  that disk drops, the terminal result + error message, and token usage.
- From the transcript tail (unchanged): assistant text, thinking, tool starts/results — the
  same records hydration replays from disk, preserving the live/disk parity invariant the
  adapter already tests.
- The pre-existing detectors (stdout `Error:`/auth line scan, CLI-log RPC scan, transcript
  terminal answer) remain as fallbacks beneath the stream, because the two failure shapes we
  cannot probe (auth, quota) are exactly the ones we must not cut over blind on, and because
  `stream-json` is young (added 1.1.8; 1.1.18 was still fixing a dropped-stream
  false-success bug in it).

This rationale must survive into the `antigravity/mod.rs` module doc, not just this plan.

A second cross-cutting decision: **outcome classification stays signal-based; do not gate on
exit codes.** Exit codes became non-zero on failures (breakage 2), but their full map is
unprobed (SIGTERM, every error class), and the stderr/stream signals are sufficient. Adding a
Claude-G22-style exit gate here would be speculative.

---

## Milestone 1 — Dispatch armoring and stderr failure signals

### Goal & Outcome

Fix the three breakages that don't need the stream: slash interception, stderr-migrated
errors, and flag-shaped prompt rejection. After this milestone:

- A user can send `/model`, `/clear`, or any slash-leading message to an Antigravity agent
  and it reaches the model as text (no silent TSV turn, no exit-2 failure).
- A user can send a message that happens to be exactly `--sandbox` (or any other `agy` flag
  token) and it reaches the model.
- A timed-out or otherwise failed `agy` turn shows the harness's real `Error:` message as a
  `HarnessError` again, instead of the generic "exited without producing an answer"
  `AdapterFailure`.
- A logged-out turn is still fast-failed and force-killed even if `agy` now prints the
  `Authentication required` line on stderr instead of stdout.

### Implementation Outline

All in `crates/harness/src/antigravity/`.

1. **`build_args`: add `--disable-slash-commands` unconditionally.** Rationale (to a code
   comment): print-mode slash interception (agy 1.1.9–1.1.12) either answers locally without
   creating a conversation (`/model`) or exits 2 (`/clear`); Switchboard sends always target
   the model. This mirrors the tradeoff Claude took in `harness-behavior.md` §0 — native
   slash commands/skills are deliberately bypassed from the ordinary composer.
2. **`build_args`: transport-only leading space for `-`-leading prompts.** Rule: if the
   prompt's first character is `-`, prefix one ASCII space. The precise upstream rejection
   rule is "value exactly equals a known flag token", but matching that would mean
   maintaining agy's flag list (drift); a uniform space on all dash-leading prompts is
   drift-proof, and probes confirm space-prefixed prompts dispatch and answer normally.
   The journal/transcript keep the exact user text (same posture as Claude's slash prefix —
   check how `claude_code` documents its space and match that framing). Note G18's "Antigravity
   unchanged" cell becomes "space-prefixed" in the docs pass (M6).
3. **Failure scan covers stderr.** `classify_outcome` already receives the stderr tail;
   extend it so `first_error_line` / `is_auth_failure_line` run over the stderr lines as well
   as stdout (stdout first — order preserves existing precedence). Keep the existing "no
   answer; stderr: …" fallback for stderr content that matches neither.
4. **Auth fast-fail watches stderr too.** The producer's dispatch loop currently applies
   `is_auth_failure_line` only to stdout lines and force-kills on match, bounding the OAuth
   hang. The same bounding must trigger from a stderr line. How to plumb it (a channel/callback
   from the stderr drain vs. reading stderr lines in the select loop) is the implementer's
   call after reading the producer — the contract is: an auth line on *either* stream
   force-kills promptly, same as today's stdout path.
5. **Timeout wording:** the message changed (`timed out waiting for response` →
   `timeout waiting for response`). `first_error_line` matches the `Error:` prefix, so no
   matcher change is expected — verify nothing else substring-matches the old wording
   (`is_auth_failure_line`'s `authentication timed out` is a different string; leave it, it
   still covers older CLIs and costs nothing).

### Definition of Done

- Unit tests: `build_args` asserts the new flag and the space-prefix (dash-leading prompt,
  flag-token prompt, and a normal prompt left untouched); `classify_outcome` cases for an
  `Error:` line arriving via stderr only (→ `HarnessError`, verbatim message) and an auth
  line via stderr only (→ `AuthFailure`, authored message).
- `fake_agy` fixture: extend so a scripted run can emit the error on stderr with exit 1, and
  cover the end-to-end classification through the adapter.
- **End-to-end auth fast-fail test (required — the classification test above does not cover
  it).** stderr is drained by a separate task that is only awaited after the child exits, so
  a passing classification test proves nothing about the mid-run force-kill. Add a `fake_agy`
  mode that prints the auth line on **stderr** and then blocks well past any test bound, and
  assert through the adapter that the turn ends `Failed{AuthFailure}` with the authored
  message and the child is reaped — wrapped in an explicit `tokio::time::timeout` (generous,
  but far below the ~30 s OAuth window this bounds) so a regression fails fast and legibly
  instead of riding the fixture's sleep or the suite's global timeout. Mirror the existing
  stdout-auth fixture test's structure.
- Live tests (naming: `live_antigravity_*`): a slash-leading prompt (`/model …`-shaped)
  completes as a model turn; a prompt exactly equal to a flag token completes. Update the
  existing dash-leading live test only if its assertion breaks.
- Known limitation recorded (code comment + M6 docs): the auth-line stream location @ 1.1.19
  is unverified (probe is destructive); scanning both streams makes the answer moot.

---

## Milestone 2 — `stream-json` control channel

### Goal & Outcome

Adopt `--output-format stream-json` as the control channel per the architecture decision.
After this milestone:

- The conversation id is captured from the stream's `init` event — contractual, immediate —
  instead of grepping a Google-internal log line (which becomes fallback #1; the filesystem
  watch stays fallback #2).
- A failed tool call renders as a failed tool in the live transcript (`ToolCompleted`
  `is_error:true`) instead of spinning forever — this makes the failing live test's *live*
  path correct (hydration is M3).
- Turn outcome classification prefers the stream's `result` event (`SUCCESS`/`ERROR` + error
  message); the transcript-terminal-answer / stdout / log-scan classification chain remains
  beneath it as fallback.
- Antigravity turns report token usage (input / output / thinking / cache-read) on `TurnEnd`,
  filling metadata cells that are ❌ today. `total_tokens` is **dropped** — the shared wire
  type retains component fields only and the UI does not render raw totals. No
  context-window bar (agy exposes no window).

### Implementation Outline

1. **Pre-work probes (cheap, non-destructive — run before coding):**
   - `stream-json` + `--conversation <existing>`: does a resume replay prior steps as
     `step_update` events (the way text mode replays prior answers on stdout)? This decides
     whether stream events need the same resume-cursor gating the transcript tail has. Design
     for gating by `step_index` against the existing resume cursor either way — it is correct
     if replay happens and harmless if not.
   - `stream-json` + `--disable-slash-commands` together (expected fine; confirm).
2. **Stdout handling becomes format-dispatching.** Add `--output-format stream-json` to
   `build_args`. In the producer's stdout loop, try to parse each line as a stream event;
   a line that is not JSON falls through to the **existing** line handlers (the `Error:`/auth
   scan, the "produced output" liveness signal). Rationale to comment: unknown whether
   plain-text control lines (auth prompt) still appear under stream-json; the fallthrough
   keeps them covered.
3. **New stream-event parser** (small; alongside the transcript parser in `parser.rs` or a
   sibling module — implementer's call). Contract:
   - `init` → the conversation UUID. Feed the existing capture path so
     `SessionLocatorCaptured` and fork-and-heal work unchanged; demote
     `conversation_id_from_log` to fallback (keep its live guard test — the log line is now
     the fallback's contract).
   - `step_update` with `step_type:"tool"`, `state:"ERROR"` → mark the corresponding pending
     tool failed: emit `ToolCompleted { is_error: true }` with an output built from what the
     event carries (probes showed `tool_info.output` null on ERROR; author a clear message,
     e.g. naming that agy rejected the call, rather than an empty string). Pairing: the
     pending-tool FIFO in `AntigravityParserState` already pairs results by
     `step_index > planner_step`; an ERROR event pairs the same way. Apply the resume-cursor
     gate from probe (1). `ACTIVE`/`DONE` tool events are **ignored** — `DONE` results keep
     coming from the transcript (content source), and consuming them from both places would
     double-complete.
   - `result` → hold status + error + usage for terminal classification (next item). Do
     **not** emit `response` text — content stays transcript-sourced (parity; and on resume
     the semantics of `response` are unverified).
   - Unknown `event` values and unknown `step_type`s are skipped silently (additive-tolerant,
     same posture as every other adapter parser).
4. **Terminal classification.** `classify_outcome` gains the stream result as the
   highest-precedence signal *for failures*: a `result` with `status:"ERROR"` and a non-empty
   `error` → `HarnessError` (auth-matched text → `AuthFailure`) with that message. A
   `status:"SUCCESS"` result corroborates but does **not** replace the transcript
   terminal-answer requirement for `Completed` — the 1.1.18 changelog shows a dropped stream
   once produced false success, and the transcript answer is what the UI renders; keep
   "Completed requires a transcript terminal answer". No-result-event runs fall through the
   existing chain untouched.
5. **Usage on `TurnEnd`.** Map `result.usage` into `TurnUsage` (`events.rs`) with this exact
   mapping — do **not** extend the shared type for one harness:

   | agy `result.usage` | `TurnUsage` |
   |---|---|
   | `input_tokens` | `input_tokens` |
   | `output_tokens` | `output_tokens` |
   | `cache_read_tokens` | `cached_input_tokens` |
   | `thinking_tokens` | `reasoning_output_tokens` |
   | `total_tokens` | *dropped* (no such field; totals are not rendered) |

   Everything else (`cache_creation_input_tokens`, `context_input_tokens`,
   `context_tokens_after_turn`, `context_window`, `total_cost_usd`) stays `None`. Note
   `context_input_tokens: None` means Antigravity never enters the Claude/Codex cached-token
   reconciliation described in that field's doc comment — `cached_input_tokens` is a plain
   passthrough here. No context window → the context bar stays absent; nothing to do.
6. **Liveness signal.** `saw_stdout_content` currently means "agy printed answer text".
   Under stream-json define it as "any stream event parsed" (init counts) OR any non-JSON
   stdout line — preserving its role in `classify_outcome`'s fail-loud branches.
7. **`fake_agy` gains a stream-json mode** so all of the above is fixture-testable: scripted
   emission of `init` / tool-ERROR `step_update` / `result` (success and error), plus a mode
   omitting the `result` event (fallback chain) and one emitting garbage lines (fallthrough).
8. **Rewrite the `antigravity/mod.rs` module doc.** Bullets "No structured stream protocol",
   "exit code is useless", and the stdout-as-control-channel description are stale. State the
   new contract: stream = control, transcript = content, fallback chain, and why (this plan's
   architecture-decision rationale, condensed).

### Definition of Done

- Fixture-driven tests through the adapter for: id capture from `init` (log line absent);
  fallback capture when no stream events parse (legacy text mode script); tool-ERROR →
  `ToolCompleted{is_error:true}` with the authored output; result-ERROR → `HarnessError`
  verbatim; result-SUCCESS **without** a transcript answer → still fail-loud (the
  no-answer branch), proving the corroboration rule; usage mapped onto `TurnEnd`.
- Unit tests on the stream parser: unknown event/step_type skipped; non-JSON line ignored by
  the stream parser (and observed by the legacy scan); resume-cursor gating of stale
  `step_index` events.
- Live: existing suite green except the tool-error test (fixed fully in M3);
  a new `live_antigravity_stream_json_init_and_result_shapes` asserting the three event
  types, the id in `init`, and non-empty usage on `result` — the drift tripwire for the new
  primary contract.
- Module doc rewritten.

---

## Milestone 3 — Dangling-tool backstop (the disk data-loss mitigation)

### Goal & Outcome

Disk transcripts no longer record failed tool calls at all (breakage 1) — that history is
upstream data loss and cannot be recovered. Mitigate so the loss never renders as a stuck UI:

**M3 is the completion gate for the dangling-tool fix.** M2 delivered the live half only;
until this milestone lands, a user watches a tool get correctly marked failed at turn end and
then sees it revert to a spinner on reopen, because the failure was never written to disk and
the hydrator has no equivalent close-out. M2 must not be described as having fixed dangling
tools on its own, and the two ship together.

- On **reopen/hydration**, a turn whose planner requested a tool that has no result record
  shows that tool as failed, not as pending forever — and later tools in the same turn pair
  to their own results (no FIFO shift). Output text comes from
  `parser.rs`'s `MISSING_TOOL_RESULT_OUTPUT`; **reuse that constant, do not author a second
  copy**, so the live and reopened renderings of the same failure cannot drift apart.
- **Already delivered by M2 — do not rebuild:** the live close-out backstop.
  `AntigravityParserState::close_pending_tools` already closes *any* tool still pending at
  the final post-exit drain, not only stream-flagged ones, which is what this section
  originally scoped as M3's live half.
- **Still M3's on the live side:** the capture-gated question of whether richer live
  attribution can ever be made safely. M2 deliberately resolves at turn end and attributes a
  harness message only in the unambiguous one-tool/one-failure case; if the captures below
  establish a real step→call invariant, this milestone may tighten that. If they don't, M2's
  behavior stands — this is investigation, not a commitment.
- `live_antigravity_invalid_file_read_completes_as_tool_error` passes (M2 already restored
  its live path; M3 must keep it passing).

### Implementation Outline

**This milestone is gated on a capture probe. Do not implement the pairing rule before
running it.** The mispairing risk is the real defect here; closing pending tools is the easy
half. Both live parsing (`parser.rs`) and hydration (`session_file.rs`, its own
`pending_tools` FIFO around lines 325–386) pair a result to the **oldest** pending tool whose
`step_index` is lower (`pop_plausible_result`). For "planner(2) calls A → A's record dropped
→ planner(4) calls B → result(5)", result 5 satisfies `> 2` and pairs to **A**. On disk that
result is indistinguishable from a delayed result for A unless a transcript invariant says
otherwise. The review captured only the `0,1,2,4` single-tool shape, which does **not**
establish any A/B rule — "require adjacency" was an unverified guess and must not be
implemented as though it were evidence.

1. **Pre-work capture probe (required).** Force, live, a turn where an invisible tool failure
   is followed by another tool that succeeds, in **both** shapes:
   (a) the failing and succeeding calls in **separate** `PLANNER_RESPONSE` records;
   (b) both calls in a **single** `PLANNER_RESPONSE` record (the `tool_calls` array allows it —
   `record_to_live_events` already indexes calls within one record).
   Capture the resulting transcripts as fixtures. A reliable way to force the failure is the
   existing live test's shape (`view_file` on a path that does not exist) paired with a
   trivially-succeeding `run_command`.
2. **Derive the pairing rule from those captures, not from assumption.** The primary
   candidate — to confirm or refute, not to assume — is that a pending tool's result always
   arrives before the **next `PLANNER_RESPONSE` record**, which would make "on a new planner
   record, close still-pending tools from earlier planner steps as failed" both the
   mispairing fix and a superset of the turn-end backstop for case (a). If the captures
   support it, implement that and record the evidence in a comment at the pairing site.
3. **Conservative fallback for genuine ambiguity.** If case (b) shows no way to tell which
   call a result belongs to, do **not** invent a heuristic: leave the ambiguous result
   unattached and close that planner record's pending tools as "no result recorded". Wrong
   provenance (showing B's output under A) is worse for the user than an explicit unknown.
   Note the residual asymmetry honestly in the code comment and in M6's docs: the **live**
   path is exact for this case (M2's stream `ERROR` events carry `step_index`); **hydration**
   is only as exact as the disk invariant allows.
4. **Hydration** (`session_file.rs`): at each turn boundary (terminal answer record / next
   `USER_INPUT` / EOF — read how the hydrator currently bounds turns), close every
   still-pending tool with `is_error: true` and the authored output.
5. **Live** (`parser.rs` / producer turn-end synthesis): same closure at `TurnEnd` emission
   for any `pending_tool_ids` remnant. The M2 stream-ERROR path will normally have emptied it.
6. The authored output text is a user-facing string — plain language, no internals
   (`docs/harness-behavior.md` §1.3's authored-message style).

### Definition of Done

- Fixture tests (hydration + live) built from the **probe captures**, not hand-authored
  guesses: the already-captured single-tool shape (indices 0-1-2-4); case (a) across planner
  records — B's result pairs to B, A closes failed; case (b) within one planner record —
  whichever behavior the capture proved (exact pairing, or the conservative unattached
  policy); a normal all-success multi-tool turn unchanged; a turn with zero tool calls
  unaffected.
- **Required: a live-vs-hydrated parity test** over the same fixture turn. The defect this
  milestone closes is not "hydration leaves tools open" so much as "the live and reopened
  views of one turn disagree", and only a paired assertion catches that class directly.
  Assert **structural** parity — both renderings terminal, `is_error: true`, and non-empty
  output — **not identical text**: in the unambiguous single-tool case the live path
  legitimately shows agy's own per-tool message, which is absent from disk and therefore
  unrecoverable on reopen. Text equality *is* assertable in the ambiguous (N>1) case, where
  both paths use `MISSING_TOOL_RESULT_OUTPUT`.
- `make test-live-antigravity` fully green (all 15 pre-existing tests plus any added by
  M1/M2) — this milestone closes the review's
  failing test.
- README "Harness support and limitations" gains the user-facing entry (write it in M6 with
  the rest of the docs; note it here so it isn't lost): reopening a project cannot show
  Antigravity tool calls that failed — the CLI doesn't save them — they appear as
  "no result recorded".

---

## Milestone 4 — Model and effort selection for Antigravity

### Goal & Outcome

Antigravity agents gain the same Primary/Secondary model+effort profile experience Claude and
Codex have (§3.3/§3.4 "Status: shipped" machinery):

- The Add-Agent / profile editor shows a **model dropdown** (~7 entries, Google's display
  names) and an **effort dropdown whose options are that model's actual levels**, hidden
  entirely for models with no effort axis. This was an explicit design decision — see
  rationale below; do not implement a folded `gemini-3.1-pro-high`-style single list.
- A selected model/effort is dispatched every turn (`--model <base-slug> [--effort <level>]`)
  and reads back through the existing per-turn carry-forward display.
- "Default" (no selection) remains valid and passes neither flag — existing Antigravity
  agents keep working untouched; agy runs its global-settings model as today.
- A stale catalog entry (model retired server-side) fails the turn pre-dispatch with agy's
  own self-describing error, verbatim — cheap, loud, quota-free.

### Design rationale (must survive into code comments / the M6 docs)

- **Why a per-model effort matrix here when Claude deliberately has none:** Claude's CLI
  accepts every level for every model and silently degrades — a matrix would drift silently
  and its ground truth is doc-derived. Antigravity's CLI validates client-side, pre-dispatch,
  and enumerates the valid set in its error; a wrong matrix entry fails loudly, costs
  nothing, and self-describes the fix. The per-model gating pattern also already exists in
  the picker (`CODEX_MAX_ULTRA_MODELS` in `src/lib/agentSelection.ts`) — reuse that pattern,
  don't invent a parallel one. Antigravity's own TUI (`/model` since 1.1.5) groups by base
  model with an effort selector, so this matches the harness's native mental model.
- **Why a curated static catalog, not runtime `agy models`:** the subcommand needs auth +
  network at picker-open time (spinner/failure states for one harness), and its
  `--output-format json` doesn't actually work @ 1.1.19 (TSV only). Staleness degrades to a
  self-describing pre-dispatch error. Provenance comment on the catalog: derived from
  `agy models` TSV + client-side validation errors @ 1.1.19, 2026-08-23; refresh on harness
  reviews.

### The catalog

| value (dispatch slug) | label | effort levels |
|---|---|---|
| `gemini-3.7-flash` | Gemini 3.7 Flash | low, medium, high |
| `gemini-3.6-flash` | Gemini 3.6 Flash | low, medium, high |
| `gemini-3.5-flash` | Gemini 3.5 Flash | low, medium, high |
| `gemini-3.1-pro` | Gemini 3.1 Pro | low, high |
| `claude-sonnet-4-6` | Claude Sonnet 4.6 (Thinking) | — (no axis) |
| `claude-opus-4-6-thinking` | Claude Opus 4.6 (Thinking) | — (no axis) |
| `gpt-oss-120b` | GPT-OSS 120B (Medium) | — (single variant; dispatch bare) |

"(Thinking)"/"(Medium)" in labels are Google's own display names, not effort labels we add.

### Implementation Outline

1. **Pre-work probes (required — gaps the review left):**
   - Bare `--model claude-sonnet-4-6` (no `--effort`) dispatches. (Its *rejection with*
     `--effort` is verified; bare acceptance is not.)
   - Bare `--model gemini-3.7-flash` is expected to be **rejected** like 3.1-pro (axis
     models require `--effort`); confirm with one probe.
   - `--model`/`--effort` on a `--conversation` resume: works, overrides the conversation's
     prior model, and the `USER_SETTINGS_CHANGE` change-announcement fires. This validates
     the "send every turn" uniform dispatch decision (§3.3) for Antigravity. If resume +
     `--model` misbehaves, stop and report — don't design around it unilaterally.
   - If any probe contradicts the catalog above, fix the catalog and record the correction.
2. **Effort-required rule:** for a model **with** effort levels, an effort selection is
   required — there is no "Default" effort option for those models (bare axis-model dispatch
   is an agy error). Pre-select the highest level when the user picks the model (matches
   Antigravity's own default flavor, `… (High)`). For no-axis models the effort control is
   hidden and no `--effort` is ever passed. For a fully-default profile (no model), neither
   flag is passed.
3. **Capability flips:** `HarnessKind::supports_model_selection` and
   `supports_effort_selection` → true for Antigravity. Registration currently **forbids**
   model/effort on Antigravity agents at the `SelectionUnsupported` checks in
   `Project::register_agent_inner` and `Project::set_agent_profiles` (`crates/core/src/project.rs`
   — the checks are duplicated across both functions and cover both the primary and secondary
   slots); flipping the flags lifts it. The narrow attach path
   (`register_attached_antigravity_agent`) structurally omits model/effort and is correct to
   leave alone — an attached agent gets a profile afterward through `set_agent_profiles`,
   which unlocks automatically once the flags flip.
4. **Where the model↔effort invariant is enforced — deliberately asymmetric; document both
   halves.** Two combinations are invalid, and they are caught by different mechanisms on
   purpose:
   - **Effort without a model** → rejected at the **persistence boundary**. Add a
     catalog-free rule alongside the existing capability checks in *both*
     `register_agent_inner` and `set_agent_profiles`, for *both* profile slots: for
     Antigravity, an effort requires a model. (Needs a new `CoreError` variant — the existing
     `SelectionUnsupported` is a statement about *harness capability* and would misdescribe
     this.) Rejecting at write time keeps the stored record truthful; do **not** paper over
     it by dropping the flag at dispatch, which would persist a profile that says one thing
     while dispatch does another. No legacy-data hazard: pre-M4 Antigravity records cannot
     carry an effort at all, because today it is forbidden outright.
   - **An axis model without an effort** → left to **agy's own pre-dispatch rejection**,
     surfaced verbatim as a `HarnessError` through M1's stderr scan
     (`--model gemini-3.5-flash requires --effort (available: low, medium, high)`; exit 1,
     quota-free). Enforcing this in core would require duplicating the model catalog into
     Rust — two sources of truth that drift — against the codebase's keep-dispatch-dumb
     posture (Codex forwards any model/effort verbatim and surfaces the server's 400).
   State this split in the plan-derived code comments at the validation site and in
   `build_args`, naming which mechanism catches which case, so the asymmetry reads as
   designed rather than as an omission.
4. **`build_args`:** append `--model <slug>` / `--effort <level>` when the profile carries
   them, every dispatch (uniform per-turn posture, §3.3). The function grows an
   `&AgentRecord`-shaped input like the other adapters' arg builders — match their signature
   pattern.
5. **Frontend:** catalog + per-model effort sets extend `effortOptionsFor`
   (`src/lib/agentSelection.ts`), which is Codex-only today — that is the established
   per-model gating pattern; reuse it rather than adding a parallel one. In
   `AgentProfileEditor.svelte` the effort control is currently rendered on a **harness-level**
   `{#if effortSupported}` and there is **no hide-on-empty-options mechanism to reuse** — it
   must be added: hide the effort control when the model-scoped option set is empty (no-axis
   model) and when no model is selected. For an Antigravity axis model, suppress the
   `allowUnset` "Default" injection and apply the pre-select; note `setModel`'s existing
   fallback-to-a-valid-effort branch already does most of this — the change is not preserving
   a null effort for that case. Read `docs/ui-conventions.md` before touching components.
6. **Per-turn model *and effort* readback (new work — the earlier "no new rendering work"
   assumption was wrong).** `extract_model_from_record` (`antigravity/mod.rs:1131`) splits the
   announcement at `" ("` and **discards the parenthetical**, and `effort` is hard-coded
   `None` at three sites (`mod.rs:779` live `TurnEnd`, `mod.rs:1639`, `session_file.rs:437`
   hydration — the last with a now-stale comment claiming the model name embeds the tier so
   there is no separate axis). So a turn run as "Gemini 3.5 Flash (Low)" would display as a
   bare model name with no effort, live and on reopen, making selected-vs-observed
   misleading. Extend the parse to return model **plus** `Option<effort>`: split the
   parenthetical only when it case-insensitively maps to a catalogued level
   (`Low`/`Medium`/`High` → `low`/`medium`/`high`); otherwise keep the display name intact —
   which also stops today's stripping of meaningful names like "Claude Sonnet 4.6 (Thinking)"
   and "GPT-OSS 120B (Medium)". Thread the effort through the same carry-forward state the
   model already uses, into live `TurnEnd` and hydration, replacing the three `None`s, and
   fix the stale comment. Prefer this over echoing the dispatched value (Claude's approach):
   the announcement is on disk, so hydration and live agree for free and unchanged resumes
   are already covered by carry-forward. The `model (effort)` transcript footer then renders
   it with no further frontend work.
   **Scope:** `extract_model_from_record` is private to `antigravity/mod.rs` and parses
   Antigravity's own announcement sentence; the Claude/GPT-OSS names it handles are models
   *offered inside Antigravity*, unrelated to the Claude Code adapter. This behavior change
   has no blast radius outside Antigravity.
7. **Resume command parity:** the interactive "Resume in terminal" command
   (`resume.rs`) — mirror whatever Claude/Codex do about including the model flag; don't
   invent a divergent shape.

### Definition of Done

- Unit tests: `build_args` with model+effort, model-only (no-axis), neither; registration
  accepts model/effort for Antigravity now (and the old rejection test is inverted, not
  deleted); the new effort-requires-a-model rule rejects at **both** `register_agent_inner`
  and `set_agent_profiles`, for **both** primary and secondary slots.
- Readback tests: announcement parse for each catalog shape — an axis label
  (`Gemini 3.5 Flash (Low)` → model + `low`), a non-catalog parenthetical preserved intact
  (`Claude Sonnet 4.6 (Thinking)`, `GPT-OSS 120B (Medium)`), and a bare name; carry-forward
  across fresh turn / unchanged resume / mid-conversation model change; a hydration fixture
  asserting the effort survives reopen.
- Frontend tests: effort options track the selected model; no-axis model hides effort;
  no model selected hides effort; required-effort default applied for an axis model with no
  "Default" option offered. Component-level per the AGENTS.md testing note if the
  editor wiring changes.
- One fixture test proving a model-without-required-effort dispatch surfaces agy's verbatim
  rejection as a `HarnessError` (this is the documented enforcement path for that half of the
  invariant, and doubles as M1 stderr-scan coverage on a second message shape).
- Live tests: `live_antigravity_model_flag_selects_model` (dispatch `gemini-3.5-flash-low`'s
  base+effort, assert the readback via the existing model-announcement machinery);
  a resume-with-model-change variant (or extend the existing
  `live_antigravity_model_change_announced_on_resume` to drive the change via flags instead
  of its current mechanism — implementer's choice after reading it; keep cost tiny per the
  live-test cost discipline).
- `make check` green.

---

## Milestone 5 — Rate-limit / quota surface via `/usage` — **BUILT, THEN CUT**

> **Not shipped.** This milestone was implemented to green tests (adapter poll, normalizer,
> `fake_agy` support, 21 tests, 2 live tests) and then removed at review. The poll is a second
> `agy` invocation that spawns its own language server: **3.7–6.6s** and **~150–250 MB RSS**,
> measured. Because an agent is marked idle only when its adapter event channel closes, awaiting
> the poll after `TurnEnd` leaves the agent visibly "processing" for that window — with an
> already-inert Stop button — on every successful turn. The alternatives cost more than the gauge
> is worth on Switchboard's least-used harness: polling concurrently doubles peak `agy` processes
> per agent *and* gives up freshness (quota updates at turn end — verified), while moving the poll
> off the turn lifecycle means the dispatcher's per-agent actor must own it.
>
> The payload shape, the two-pool model-family split, the measurements, and the cheap future
> shape (poll once at project open, from the app layer) are recorded in `harness-behavior.md`
> §5.1. The code is recoverable from the branch's stash entry. **Everything below is the
> as-designed plan, kept for provenance — it does not describe shipped behavior.**

### Goal & Outcome

Antigravity 1.1.11/1.1.12 added non-interactive print-mode answers for read-only slash
commands. `-p "/usage"` (alias `/quota`) with `--output-format json` returns a structured
quota payload **without starting an agent turn, spending quota, or creating a conversation**
(`num_turns: 0`, empty `conversation_id`, runs in a couple of seconds). This fills the
rate-limit sidebar cell that is ❌ for Antigravity today. After this milestone:

- After each completed Antigravity turn, the agent's sidebar shows two quota gauge lines —
  5-hour and weekly window, with percent used and reset time — exactly like Codex's existing
  two-window rendering.
- The gauges show the bucket for **the agent's model group** ("Gemini Models" vs "Claude and
  GPT models"): selected model first (M4), carry-forward observed model otherwise, clean-hide
  if the model is unknown (§3 sidebar absent-field convention).
- The values are restart-durable via the existing metadata sidecar, with the existing
  "snapshot from …" tooltip semantics.
- A quota fetch failure changes nothing visible and never affects the turn.

Probe evidence (2026-08-23 @ 1.1.19), `agy -p "/usage" --output-format json`:

```
"command": {"name": "usage", "data": {"description": "…", "groups": [
  {"name": "Gemini Models", "description": "Models within this group: Gemini Flash, Gemini Pro",
   "buckets": [
     {"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly",
      "remaining_fraction":0.9857,"reset_time":"2026-08-30T23:57:21Z","description":"…"},
     {"id":"gemini-5h","name":"Five Hour Limit Remaining","window":"5h",
      "remaining_fraction":0.9500,"reset_time":"2026-08-24T04:57:21Z","description":"…"}]},
  {"name": "Claude and GPT models", "buckets": [
     {"id":"3p-weekly",…}, {"id":"3p-5h",…}]}]}}
```

`/credits` also answers structurally (`{"remaining_credits":0,"upgrade_uri":…}`) —
**deliberately out of scope**: the overage-credits product story is a separate question;
record it as a known extension in the M6 docs, don't build it. `/context` is
interactive-only in print mode (exit 2) — the context bar stays absent for Antigravity.

### Implementation Outline

1. **Fetch lives in the adapter** (harness-specific logic never leaks to the dispatcher/app).
   After a turn classifies **`Completed`** — and only then — the producer runs
   `agy -p "/usage" --output-format json` (stdin closed, per-dispatch `--log-file`, no
   `--add-dir` needed) and parses `command.data.groups`. The completed-turns-only gate is
   load-bearing: an auth-dead agent fails its turn before any fetch, so the fetch can't be
   the thing that trips `agy`'s interactive OAuth fallback. Apply the same defensive guards
   as dispatch anyway (bounded timeout of a few seconds, auth-line force-kill from M1's
   shared matchers).
2. **Emission reuses the existing normalized rate-limit event** (the type Claude's
   `rate_limit_event` and Codex's `rate_limits` already map into — read `events.rs` and both
   emitters before shaping this). Mapping: `used_percent = (1 − remaining_fraction) × 100`;
   `resets_at` from `reset_time`; 5-hour bucket → primary window, weekly → secondary
   (mirroring Codex's `window_minutes` labeling — 300 / 10080). Both groups are carried on
   the event (or the event is emitted per-group with a discriminator — choose whichever the
   existing type accommodates without extension); the **frontend** picks the group by the
   agent's model, since the model can change per turn while the sidecar snapshot persists.
   If the existing type cannot carry the group split cleanly, stop and report rather than
   extending the shared wire type unilaterally.
3. **Timing constraint:** the event must land on the turn's own stream so the dispatcher's
   existing sidecar persistence picks it up (that is what makes it restart-durable for
   free). Whether it is emitted just before `TurnEnd` (bounded delay, like the post-turn
   enrichment Codex already does) or the dispatcher tolerates it after — read the dispatcher
   sink first; the constraint is: **never delay turn-end unboundedly, never fail the turn**.
   On fetch/parse failure, emit nothing.
4. **Model → group mapping** is display-time: Gemini-family model names → "Gemini Models"
   buckets; Claude/GPT names → "Claude and GPT models" buckets; unknown → render nothing.
   Keep the mapping in the frontend next to the Antigravity catalog (M4) so the two stay in
   one place.
5. **Honest-scope note for the M6 docs:** this is point-in-time, account-scoped polling —
   concurrent Antigravity use outside Switchboard moves the numbers. Identical semantics to
   Claude/Codex account-window quotas; say so rather than implying per-agent attribution.

### Definition of Done

- Fixture tests: `fake_agy` answers the `/usage` invocation with the captured JSON; adapter
  emits the normalized event with correct percent/reset mapping; malformed/absent payload →
  no event, turn unaffected; a failed turn → no fetch at all.
- Frontend tests: gauge lines render for a Gemini-model agent from the Gemini buckets and
  for a Claude-model agent from the 3p buckets; unknown model renders nothing; snapshot
  restores from the sidecar.
- Live test `live_antigravity_usage_payload_shape` asserting the contract we now render:
  `command.data.groups[].buckets[]` with `remaining_fraction` ∈ [0,1] and a parseable
  `reset_time` — the drift tripwire for a payload Google can reshape silently.
- §3 metadata table update and the polling-semantics note are queued for M6.

---

## Milestone 6 — Documentation and review close-out

### Goal & Outcome

Every durable fact from this review lands in its designated home; the review is recorded per
the playbook. Someone reading `harness-behavior.md` afterward sees the *current* contract
with no stale claims.

### Implementation Outline

Small and mechanical, but wide. Follow `harness-update-review.md` §6's placement rules
(durable behavior → `harness-behavior.md`; reasoning → PR description; one-line verdict →
snapshot row).

1. **`docs/harness-behavior.md`:**
   - §0: Antigravity now intercepts print-mode slash commands; we opt out with
     `--disable-slash-commands` (replaces "Codex and Antigravity treat the tested inputs as
     model text" for the Antigravity half).
   - §1.1/§1.2/§1.4 Antigravity rows: stream-`result` signal, stderr `Error:` lines,
     non-zero exits (and the decision not to gate on them), timeout wording change.
   - §3 metadata table: Antigravity tokens cell ❌ → ✅ (no window); rate-limit/quota cell
     ❌ → ✅ (M5's `/usage` polling — note the point-in-time account-scoped semantics, and
     record `/credits` as a known unbuilt extension).
   - §3.3: rewrite the Antigravity paragraph — model selection is now shipped via
     `--model` (the "no usable per-call model control — and we won't build one" decision is
     reversed with the evidence: no settings-file mutation, stable slugs, fast dispatch);
     keep the carry-forward display mechanism description.
   - §3.4: Antigravity row — `--effort` per-call, per-model level sets, client-side
     validation; the "mirror images" closing line about Gemini/Antigravity needs updating.
   - §4 gap register: new entry for the failed-tool-record disk loss (mitigated, upstream,
     README-visible) — record the live-vs-hydration asymmetry M3 lands on (live exact via
     stream `ERROR`; hydration only as exact as the proven disk invariant); G11 (model
     metadata fragile) — update status in light of selection *and* the effort readback;
     G18 cell for Antigravity (space-prefix); note G15's capture is now fallback.
   - §6 version notes: one consolidated Antigravity 1.1.x entry covering the four breaking
     changes + the two capabilities (supersede/annotate the 1.1.2 invalid-tool-call entry —
     that shape no longer exists).
   - §7.3 hidden-dir limitation: re-check the rejection string on this bump (one cheap
     probe) and update the "verified @" stamp.
2. **`README.md` "Harness support and limitations":** user-facing entries — failed tool
   calls absent from reopened history (plain-language, symptom-first); model/effort
   selection now available for Antigravity (remove/update any entry saying it isn't).
3. **`docs/system-design.md` §9** capability matrix: Antigravity model selection
   native-now; effort likewise.
4. **`docs/harness-update-review.md`:** "Last reviewed" Antigravity row → `1.1.19` /
   2026-08-23 / one-line verdict (breaking ×4, fixed; model+effort+stream-json+quota
   adopted; live count per the final suite); prepend the Review-history entry;
   **fix the stale §2 sources row** — `agy` now
   has `agy changelog` (the "no public changelog — review = re-probe" premise is obsolete);
   update §5's framing accordingly (changelog first, probes to verify, since the changelog
   was demonstrably incomplete — the tool-record loss appears in no changelog entry).
5. Final gate: `make check` and `make test-live-antigravity` (foreground, generous timeout),
   then stop and report per the user's review-before-commit process.

### Definition of Done

- All listed docs updated; no stale Antigravity claim remains in §0/§3.3/§3.4.
- Playbook row + history entry present; changelog-source row fixed.
- `make check` exit 0; the full live antigravity suite green (the original 15 plus every
  live test this plan adds), run in the foreground.
