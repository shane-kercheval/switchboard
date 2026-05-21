# Note — Claude context-utilization formula ignores prompt-cache tokens

**Status**: investigation needed; not an implementation plan.
**Date**: 2026-05-18.
**Out of scope of M3**; surfaced during M3 testing.

## The user-visible problem

For Claude agents, the sidebar's `context after last turn: X%` bar barely moves as the conversation grows. A user can ask Claude to write multiple long poems (each thousands of tokens), and the bar might still read 1–2% afterward — when the *true* context occupancy is much higher.

The bar isn't broken — it's showing exactly what the formula computes. The formula is what's wrong.

## Root cause — Anthropic's prompt caching

Anthropic's API does aggressive prompt caching on multi-turn conversations. Each turn's `usage` reports:

- `input_tokens` — the **marginal new uncached input** for this turn (typically just the user's new prompt, not the prior conversation).
- `cache_read_input_tokens` — the cached prefix (the entire prior conversation history) replayed from cache.
- `cache_creation_input_tokens` — input being written to the cache this turn.
- `output_tokens` — the assistant's reply.

The current `contextUtilization` formula in `src/lib/components/Sidebar.svelte`:

```ts
(input_tokens + output_tokens) / context_window
```

…sees only the marginal new uncached input + the output. The cached prefix — which **is** in the model's context window and **is** part of what's loaded for inference — is missing. On a turn 10 of an active conversation:

- Real context occupancy: ~30k tokens (full prior history) + ~2k (new prompt) + ~3k (reply) = ~35k of 200k = ~17%.
- What the formula sees: 2k + 3k = 5k of 200k = ~2%.

The bar underestimates true occupancy roughly by an order of magnitude on long conversations.

## Why we capture `cached_input_tokens` but don't use it

The Claude session-file parser **does** extract this field:

```rust
// crates/harness/src/claude_code/session_file.rs:parse_usage
cached_input_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_u64),
```

It rides through `TurnUsage::cached_input_tokens` to the frontend. The sidebar just doesn't include it in the math. The fix is mechanical — change the formula.

## Proposed fix (high-level)

Update `contextUtilization` in `src/lib/components/Sidebar.svelte` to include cached input tokens:

```ts
const inputTokens = turn.usage.input_tokens ?? 0;
const cachedInputTokens = turn.usage.cached_input_tokens ?? 0;
const outputTokens = turn.usage.output_tokens ?? 0;
const window = turn.usage.context_window;
return (inputTokens + cachedInputTokens + outputTokens) / window;
```

Same denominator; the numerator now includes the cached prefix.

## Open questions for the implementing agent

1. **Per-harness applicability**. Does this formula change also apply correctly to Codex and Gemini? Codex's `cached_input_tokens` semantics may differ from Claude's (the OpenAI / Codex API does its own caching, but the wire shape might or might not match). Verify by looking at what Codex's session-file parser puts in `cached_input_tokens`, what fixture data shows, and whether the resulting math under-counts or over-counts Codex's real occupancy. If Codex's `cached_input_tokens` is already cumulative-conversation-input (which would be the safe inclusive value), the formula is right for both. If it's something else, the fix may need to branch on harness.

2. **`cache_creation_input_tokens`**. Anthropic also reports cache creation tokens — input that's being **written** to the cache this turn (paid at a higher rate). Are these distinct from `cache_read_input_tokens` for context-occupancy purposes? Likely they're part of the same context window — so include them too if the parser exposes them. Verify against Anthropic's published usage docs.

3. **The bar's interpretation**. "Context after last turn" today means "for the most recent agent turn, what fraction of the context window is occupied by input + output of that turn." With the fix, it means "input + cached prefix + output," which is closer to "how full is the conversation's context right now." Same label, more accurate interpretation. Consider whether the label needs to change (probably not — the new meaning is what users intuitively expect).

4. **`reasoning_output_tokens`**. Claude's extended-thinking models report reasoning tokens separately. These are part of the response but counted on the output side. The current formula's `output_tokens` already includes them (Anthropic's `output_tokens` is the total — verify). If `reasoning_output_tokens` is a subset of `output_tokens` (the typical shape), no change needed. If it's separate and additive, include it.

5. **What `context_window` value to use**. Already a separate field on `TurnUsage`, populated from `result.modelUsage.<model>.contextWindow` for Claude. No change here unless the per-model context window varies by extended-thinking mode (worth verifying).

## What this is **not**

- Not a fix for the "context % missing on project reopen" issue — that's persistence, covered separately in `2026-05-18-note-claude-cost-context-persistence.md`. The formula fix applies once data is available; the persistence note handles the "no data after reopen" gap.
- Not relevant for Gemini — Gemini doesn't report a context window at all, so the bar hides regardless of formula.
- Not a change to the underlying `TurnUsage` shape — the data is already there, the consumer just doesn't read all of it.

## Pointer to existing code

- `src/lib/components/Sidebar.svelte::contextUtilization` — the current formula.
- `src/lib/state/types.ts::TurnUsage` — the field shape.
- `crates/harness/src/claude_code/session_file.rs::parse_usage` — proves `cached_input_tokens` is captured from `cache_read_input_tokens`.
- `crates/harness/tests/fixtures/claude/*.stream.jsonl` — captured fixtures showing what Anthropic's `usage` block looks like with prompt caching active. Reading one with `cache_read_input_tokens > 0` would let the investigator confirm the field semantics empirically before touching code.
