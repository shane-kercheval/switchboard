# Note — Persist Claude cost and context-window across project reopen

**Status**: investigation needed; not an implementation plan.
**Date**: 2026-05-18.
**Out of scope of M3**; surfaced during M3 testing.

## The user-visible problem

On project reopen, the Claude agent's sidebar shows:

- No `$ cost` figure (line hidden).
- No `context after last turn` bar (hidden).

These appear only after the user sends a fresh turn in this session. Codex shows both pre-dispatch because Codex's session file carries them; Claude doesn't.

This is the difference the user noticed: reopen a project with a Claude agent that has past conversation history → sidebar's cost / context lines are blank until a new turn happens, even though we obviously had this information at the time of the last dispatch.

## Why we can't fix it by parsing alone

Anthropic's session file (`~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`) is the system of record for the conversation transcript but **doesn't carry two fields we depend on**:

- `total_cost_usd` — lives in the stream's `result` event (`result.total_cost_usd`), not in any per-message record.
- `context_window` — lives in `result.modelUsage.<model>.contextWindow`, same stream-only fate.

The Claude session-file parser correctly returns `total_cost_usd: None` and `context_window: None` on every hydrated turn. This is documented behavior — the live test `assert_claude_agent_usage` pins it explicitly.

The sidebar's renderers gate on these:

- `sessionTotalCost` sums `turn.usage?.total_cost_usd ?? 0` across completed turns; if every hydrated turn has `None`, the sum is `0` and the `cost > 0` check hides the line.
- `contextUtilization` walks turns from latest to earliest looking for one with a non-null `context_window`; if every hydrated turn has `None`, returns `undefined` and the bar hides.

The Anthropic CLI gives us these values once, when the live `result` event arrives. They're not in the session file. **If we want them on hydration, we have to persist them ourselves.**

## Proposed approach (high-level — investigate the details)

Switchboard already maintains a per-agent **session-link sidecar** for Codex at `<directory>/.switchboard/projects/<project-id>/sessions/<agent-id>.jsonl`. The Codex sidecar holds `{session_id, session_partition_date, started_at}` records — one append per dispatch.

The proposal: **extend the sidecar concept to record per-turn cost / context-window for harnesses that emit them.** For Claude specifically:

1. **Adapter-side**: on every Claude `result` event, append a record to the Claude agent's sidecar with the turn's cost + context_window + turn_id (and probably the same fields for other harnesses' turns when those harnesses emit them).
2. **Hydrator-side**: `load_claude_transcript` reads the sidecar in addition to the session file, joins records by turn_id (or whatever stable key proves correct), and overlays the persisted fields onto the parsed `TurnUsage`. Sidebar then renders cost / context immediately on reopen.

Codex doesn't need this for context_window (already in its session file) and doesn't expose cost. Gemini has neither in its session file but the free OAuth tier means cost isn't a thing; context-window would need a per-model lookup table since Gemini doesn't emit it. Sidecar-driven persistence makes sense for Claude first.

## Open questions for the implementing agent

These need real investigation, not guessed-at answers:

1. **Sidecar shape**. Today's Codex sidecar is one-record-per-dispatch with a fixed schema. The new use case is multiple turns per dispatch (a Claude conversation accumulates turns over time). Does the sidecar grow to one record per turn, or one record per dispatch holding aggregate-this-turn data? Probably per-turn — read the Codex sidecar's existing structure and AGENTS.md's "Switchboard-owned JSONL" loud-fail-on-corrupt invariant before deciding.

2. **Join key on hydration**. Claude's session file has its own record IDs (`requestId`, `parentUuid`, etc.). The dispatcher generates its own `TurnId` per dispatch. Which key joins reliably across the session-file ↔ sidecar boundary? The naïve "Nth sidecar record matches Nth agent turn in the parsed transcript" only works under append-only-everywhere; verify before relying on it.

3. **Write timing**. The Claude adapter emits `TurnEnd` carrying `usage` with `total_cost_usd` and `context_window` populated. Where in the adapter / dispatcher / app flow does the sidecar write happen — adapter-internal (mirror Codex's `sidecar::append_record` call site) or dispatcher-level? Affects testability.

4. **Failure mode**. AGENTS.md "Switchboard-owned JSONL = loud-fail on corruption." Apply that here — a corrupt cost sidecar shouldn't be silently skipped; it should surface so the user knows the data is wrong rather than misleadingly showing a partial figure. But the missing-sidecar case (older agent, never had a sidecar) is "fresh state," not corruption.

5. **Should this generalize beyond Claude?** A sidecar that records `{turn_id, cost?, context_window?}` could serve any harness that emits these stream-only. Codex doesn't need it for context (session file has it) and doesn't have cost. Gemini might want context_window via a hardcoded per-model table — but that's a different scope (no live source). Worth thinking about whether the schema should be per-harness or harness-agnostic.

6. **Backfill story**. Existing Claude agents have no sidecar; their past turns' cost / context are lost. On the first dispatch after this change ships, the sidecar starts accumulating. Prior turns stay blank in the sidebar. Document this as expected, or build a one-time backfill (probably not worth it for the cost field — old turns wouldn't be re-billed).

## What this is **not**

- Not a context-window UX redesign — keep the existing context-utilization bar; just populate it from disk.
- Not Anthropic prompt-cache modeling — orthogonal. The current formula uses `input_tokens + output_tokens / context_window`; if the user later notices the bar barely moves because Anthropic's caching makes `input_tokens` count only marginal new input, that's a separate bug (worth filing too — currently the formula ignores `cache_read_input_tokens`, which on a long cached conversation makes the bar underestimate true context occupancy).
- Not a fix for Gemini's blank context-utilization — Gemini's session file genuinely has no analog. Sidecar-driven persistence doesn't help unless we also accept a hardcoded per-model context-window table.

## Pointer to the existing pattern

`crates/harness/src/codex/sidecar.rs` is the closest live example. Read it end-to-end before designing the new schema. The test infrastructure (`commands::tests::attach_codex_succeeds_and_writes_sidecar`, etc.) is also worth grepping for the establish patterns around sidecar-vs-registry write ordering and corruption handling.
