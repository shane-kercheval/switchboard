# Research: Claude Code headless mode (`claude -p`)

**Captured:** 2026-05-09
**Affects system-design sections:** §5 (harness integration), §6 (prompt providers / cross-agent normalization), open questions 10.9, 10.10, 10.11, 10.12, 10.13.

## Summary

Switchboard drives Claude Code through `claude -p` (the headless / programmatic CLI mode, also surfaced as the Agent SDK CLI). This note captures what loads, what doesn't, and the future migration that will affect Switchboard.

## Key findings

### Default `claude -p` loads the full user environment

> Without it [`--bare`], `claude -p` loads the same context an interactive session would, including anything configured in the working directory or `~/.claude`.

Source: [Run Claude Code programmatically — Claude Code Docs](https://code.claude.com/docs/en/headless)

This means skills, hooks, plugins, MCP servers, auto-memory, and CLAUDE.md all load by default in headless mode. **Auto-invoked skills work normally** — the model discovers them at session start (via the YAML frontmatter description) and decides whether to load the full SKILL.md body mid-turn, exactly as in an interactive session.

**Subagents (the `Agent` tool) also work** — the model delegates to built-in or `.claude/agents/*.md` subagents mid-turn just as interactively. But "work" here means *execute*; how subagent activity is *represented* on the stream vs. on disk is a separate, load-bearing concern — Switchboard currently **mis-attributes** a subagent's internal tool calls to the parent turn (the stream tags them with `parent_tool_use_id`, which our parser ignores), and the live view diverges from the rehydrated one. Ground truth in [`claude-code-cli-observed.md` §"Subagent (`Agent` tool) representation"](claude-code-cli-observed.md); the fix is tracked in [`../implementation_plans/2026-05-24-subagent-rendering-fidelity.md`](../implementation_plans/2026-05-24-subagent-rendering-fidelity.md). (And note: when `--bare` becomes the `-p` default, subagent loading must be preserved with `--agents` — see below.)

### `--bare` mode skips auto-discovery

> Add `--bare` to reduce startup time by skipping auto-discovery of hooks, skills, plugins, MCP servers, auto memory, and CLAUDE.md.

Source: [Run Claude Code programmatically — Claude Code Docs](https://code.claude.com/docs/en/headless)

In `--bare` mode you opt context back in via explicit flags:

| To load                 | Use                                                     |
| ----------------------- | ------------------------------------------------------- |
| System prompt additions | `--append-system-prompt`, `--append-system-prompt-file` |
| Settings                | `--settings <file-or-json>`                             |
| MCP servers             | `--mcp-config <file-or-json>`                           |
| Custom agents           | `--agents <json>`                                       |
| A plugin                | `--plugin-dir <path>`, `--plugin-url <url>`             |

### `--bare` will become the `-p` default in a future release

> `--bare` is the recommended mode for scripted and SDK calls, and will become the default for `-p` in a future release.

Source: [Run Claude Code programmatically — Claude Code Docs](https://code.claude.com/docs/en/headless)

When this flips, Switchboard's "ride the default" strategy stops loading the user's environment automatically. To preserve current behavior we will need to pass the equivalent flags from the table above. Switchboard's design therefore isolates harness command-line construction in a single helper so this becomes a one-place change rather than a refactor.

### User-invoked slash commands are not supported in `-p` mode

> User-invoked skills like `/commit` and built-in commands are only available in interactive mode. In `-p` mode, describe the task you want to accomplish instead.

Source: [Run Claude Code programmatically — Claude Code Docs](https://code.claude.com/docs/en/headless)

Implication: Switchboard's "passthrough" model (let the user type any harness slash command and forward it verbatim) does not work in headless mode today. Built-in commands (`/cost`, `/model`, `/clear`) and user-invoked skills (`/skill-name`) cannot be invoked this way. The auto-invoke side of skills, however, is unaffected.

Open Anthropic issues tracking this gap:

- [anthropics/claude-code#837 — use slash commands in print/headless/non-interactive mode](https://github.com/anthropics/claude-code/issues/837)
- [anthropics/claude-code#38505 — \[FEATURE\] CLI flag to invoke a skill `/<cmd>` like interactive mode](https://github.com/anthropics/claude-code/issues/38505)

### Token usage, context utilization, and compaction

**Token usage** is exposed in `--output-format json` per assistant message:

```json
"usage": {
  "input_tokens": 4,
  "cache_creation_input_tokens": 12582,
  "cache_read_input_tokens": 4802,
  "output_tokens": 12
}
```

`total_cost_usd` and a per-model cost breakdown are also in the JSON output.

Source: [Run Claude Code programmatically — Claude Code Docs](https://code.claude.com/docs/en/headless), [\[FEATURE\] Better context window handling in SDK headless mode (anthropics/claude-code#8011)](https://github.com/anthropics/claude-code/issues/8011)

**Context-window maximum.** Issue [#8011](https://github.com/anthropics/claude-code/issues/8011) requested a `tokens_max` field in `usage` and was **closed as not planned** — but **a `contextWindow` field IS exposed elsewhere**, under `result.modelUsage.<model>.contextWindow`, as of v2.1.138. This was confirmed by hands-on probe (see [claude-code-cli-observed.md](claude-code-cli-observed.md)). The "not planned" was specifically about adding it to `usage`; it ended up surfaced via a different path. Switchboard can read it directly per turn — no model→max-context map needed for Claude Code.

**`/compact` cannot be triggered programmatically.** It is REPL-only. Multiple open feature requests exist:

- [\[FEATURE REQUEST\] Add "Compact" tool for programmatic conversation compaction (#5643)](https://github.com/anthropics/claude-code/issues/5643)
- [Allow skills to trigger /compact programmatically (#39275)](https://github.com/anthropics/claude-code/issues/39275)
- [Compact tool for programmatic context compaction (#39574)](https://github.com/anthropics/claude-code/issues/39574)
- [Expose partial compaction as /compact --from parameter for non-interactive use (#26488)](https://github.com/anthropics/claude-code/issues/26488)

None have shipped.

**Auto-compaction does run.** Claude Code performs automatic summarization when the context window approaches ~95% capacity, driven by headroom accounting (output headroom + compaction headroom). This is a baseline Claude Code behavior tied to the model's headroom, not specifically the TUI, so it applies in headless mode as well.

Sources: [How Claude Code works — Claude Code Docs](https://code.claude.com/docs/en/how-claude-code-works), [Inside Claude Code's Compaction System — Decode Claude](https://decodeclaude.com/compaction-deep-dive/), [Auto-compact FAQ — ClaudeLog](https://claudelog.com/faqs/what-is-claude-code-auto-compact/)

## Implications for Switchboard

1. **Default mode is the right choice for v1.** Switchboard wants the user's full environment to load — skills, hooks, MCP tools, plugins, CLAUDE.md, auto-memory all participate normally. Using `--bare` would amputate the very behavior we want to orchestrate. (Reflected in §5 "Process model".)
2. **Cross-agent prompt normalization (§6) does not extend to user-invoked slash commands.** It *does* extend to model-invoked behavior (auto-invoked skills, MCP tools called by the model mid-turn) because the user's environment is loaded.
3. **Passthrough mechanism (§5) is constrained to what can be implemented out-of-band today.** `/cost`-equivalents can be derived from `--output-format json` metadata; `/model`-equivalents can be implemented by re-spawning with a different `--model` flag. A blanket slash-command passthrough is not achievable until the upstream issues land.
4. **Isolate harness command-line construction.** When `--bare` becomes the `-p` default, Switchboard will need to pass `--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, etc. to preserve current behavior. Keep this in one helper. (Tracked as open question 10.9.)
5. **Context utilization is computed, not read.** Switchboard derives utilization % from raw token counts in the JSON output combined with its own model→max-context map. The map needs maintenance whenever Anthropic ships a new model. Reflected in §5 "Required harness commands" and tracked under open question 10.12.
6. **Programmatic compaction is unavailable; rely on auto-compact.** Switchboard cannot trigger `/compact` in either harness today. It can monitor token usage, surface warnings as the auto-compact threshold (~95%) approaches, and tell the user when interactive intervention is needed if they want explicit control. Implementing a Switchboard-side compaction would underperform the harness's tuned summarization; not worth the effort. Tracked under open questions 10.11 and 10.13 (monitoring for upstream).

## Sources

- [Run Claude Code programmatically — Claude Code Docs](https://code.claude.com/docs/en/headless) (primary)
- [Extend Claude with skills — Claude Code Docs](https://code.claude.com/docs/en/skills)
- [Agent Skills — Claude API Docs](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview)
- [How Claude Code works — Claude Code Docs](https://code.claude.com/docs/en/how-claude-code-works)
- [anthropics/claude-code#837 — use slash commands in print/headless/non-interactive mode](https://github.com/anthropics/claude-code/issues/837)
- [anthropics/claude-code#5643 — Add "Compact" tool for programmatic conversation compaction](https://github.com/anthropics/claude-code/issues/5643)
- [anthropics/claude-code#8011 — Better context window handling in SDK headless mode (closed: not planned)](https://github.com/anthropics/claude-code/issues/8011)
- [anthropics/claude-code#26488 — Expose partial compaction as /compact --from for non-interactive use](https://github.com/anthropics/claude-code/issues/26488)
- [anthropics/claude-code#38505 — \[FEATURE\] CLI flag to invoke a skill `/<cmd>` like interactive mode](https://github.com/anthropics/claude-code/issues/38505)
- [anthropics/claude-code#39275 — Allow skills to trigger /compact programmatically](https://github.com/anthropics/claude-code/issues/39275)
- [anthropics/claude-code#39574 — Compact tool for programmatic context compaction](https://github.com/anthropics/claude-code/issues/39574)
- [Inside Claude Code's Compaction System — Decode Claude](https://decodeclaude.com/compaction-deep-dive/)
- [Auto-compact FAQ — ClaudeLog](https://claudelog.com/faqs/what-is-claude-code-auto-compact/)
