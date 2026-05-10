# Research: Codex non-interactive mode (`codex exec`)

**Captured:** 2026-05-09
**Affects blueprint sections:** §5 (harness integration), open questions 10.11, 10.12, 10.13.

## Summary

Switchboard drives Codex through `codex exec`, the non-interactive subcommand. This note captures what is exposed in headless mode and what is not, paralleling [claude-code-headless.md](claude-code-headless.md).

## Key findings

**Note (added after hands-on probe):** Codex's `--json` stream is a deliberately minimal subset of what's actually recorded. The session file (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`) carries much more — including `model_context_window`, rate-limit info, the full system prompt (`base_instructions`), and encrypted reasoning blocks. See [codex-cli-observed.md](codex-cli-observed.md) for the complete picture. Switchboard's Codex adapter may need to read both the stream and the session file to surface everything we want.

### Token usage is exposed in `codex exec --json`

Per-turn usage is reported in `turn.completed` events:

```json
"turn.completed", "usage": {
  "input_tokens": 24763,
  "cached_input_tokens": 24448,
  "output_tokens": 122,
  "reasoning_output_tokens": 0
}
```

Source: [Non-interactive mode — Codex / OpenAI Developers](https://developers.openai.com/codex/noninteractive)

This gives Switchboard real-time token consumption per turn, including reasoning-token usage. Cost is not exposed as a single field the way Claude Code surfaces `total_cost_usd`; it would need to be derived from token counts and per-model pricing if Switchboard wants to display dollar figures for Codex agents.

### Context-window maximum is not documented

The non-interactive docs do not surface a `tokens_max` or context-window-size field. As with Claude Code, Switchboard would need to maintain its own model→max-context map and compute utilization itself.

### `/compact` programmatic trigger is not documented for `codex exec`

The non-interactive docs describe usage reporting, structured outputs, stdin piping, and CI integration patterns, but make no mention of programmatic compaction triggers. Available evidence suggests `/compact` is interactive-only, mirroring Claude Code.

### Auto-compaction is configurable globally

Codex exposes a configurable token threshold for automatic history compaction (in its config reference; "unset uses model defaults"). Whether this fires inside `codex exec` specifically is not explicitly documented, but it is a config-level setting rather than a TUI feature, so it is reasonable to assume it applies in non-interactive runs as well.

There is at least one reported case of Codex CLI running out of context instead of compacting in long sessions: [openai/codex#19842 — Codex CLI runs out of context instead of compacting/resuming long thread with many tool calls](https://github.com/openai/codex/issues/19842). Worth tracking as a known sharp edge.

## Implications for Switchboard

1. **Token usage is available; cost is derived.** Switchboard can show per-turn token consumption directly from `turn.completed` events. Dollar-cost display for Codex agents requires a per-model pricing table inside Switchboard, and is a separate concern from Claude Code (which surfaces cost natively).
2. **Context utilization is computed, same as Claude Code.** Same model→max-context map mechanism applies; the map should cover both Anthropic and OpenAI models. (Tracked under open question 10.12.)
3. **Programmatic `/compact` is unavailable; rely on auto-compact.** Same posture as Claude Code: monitor utilization, surface warnings, defer to harness auto-compaction. (Tracked under open question 10.11; monitoring under 10.13.)
4. **Codex's auto-compaction has known reliability issues.** [openai/codex#19842](https://github.com/openai/codex/issues/19842) reports cases where long sessions exhaust context rather than compacting. Switchboard should be defensive: surface warnings well before the model's limit, not just before auto-compact would theoretically trigger, since the fallback path is uncertain.

## Sources

- [Non-interactive mode — Codex / OpenAI Developers](https://developers.openai.com/codex/noninteractive) (primary)
- [CLI reference — Codex / OpenAI Developers](https://developers.openai.com/codex/cli/reference)
- [Configuration Reference — Codex / OpenAI Developers](https://developers.openai.com/codex/config-reference)
- [openai/codex#19842 — Codex CLI runs out of context instead of compacting/resuming](https://github.com/openai/codex/issues/19842)
