# Research: Claude Code headless mode (`claude -p`)

**Captured:** 2026-05-09
**Affects plan sections:** §5 (harness integration), §6 (prompt providers / cross-agent normalization), open questions 10.9 and 10.10.

## Summary

Switchboard drives Claude Code through `claude -p` (the headless / programmatic CLI mode, also surfaced as the Agent SDK CLI). This note captures what loads, what doesn't, and the future migration that will affect Switchboard.

## Key findings

### Default `claude -p` loads the full user environment

> Without it [`--bare`], `claude -p` loads the same context an interactive session would, including anything configured in the working directory or `~/.claude`.

Source: [Run Claude Code programmatically — Claude Code Docs](https://code.claude.com/docs/en/headless)

This means skills, hooks, plugins, MCP servers, auto-memory, and CLAUDE.md all load by default in headless mode. **Auto-invoked skills work normally** — the model discovers them at session start (via the YAML frontmatter description) and decides whether to load the full SKILL.md body mid-turn, exactly as in an interactive session.

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

## Implications for Switchboard

1. **Default mode is the right choice for v1.** Switchboard wants the user's full environment to load — skills, hooks, MCP tools, plugins, CLAUDE.md, auto-memory all participate normally. Using `--bare` would amputate the very behavior we want to orchestrate. (Reflected in §5 "Process model".)
2. **Cross-agent prompt normalization (§6) does not extend to user-invoked slash commands.** It *does* extend to model-invoked behavior (auto-invoked skills, MCP tools called by the model mid-turn) because the user's environment is loaded.
3. **Passthrough mechanism (§5) is constrained to what can be implemented out-of-band today.** `/cost`-equivalents can be derived from `--output-format json` metadata; `/model`-equivalents can be implemented by re-spawning with a different `--model` flag. A blanket slash-command passthrough is not achievable until the upstream issues land.
4. **Isolate harness command-line construction.** When `--bare` becomes the `-p` default, Switchboard will need to pass `--mcp-config`, `--agents`, `--plugin-dir`, `--settings`, etc. to preserve current behavior. Keep this in one helper. (Tracked as open question 10.9.)

## Sources

- [Run Claude Code programmatically — Claude Code Docs](https://code.claude.com/docs/en/headless) (primary)
- [Extend Claude with skills — Claude Code Docs](https://code.claude.com/docs/en/skills)
- [Agent Skills — Claude API Docs](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview)
- [anthropics/claude-code#837 — use slash commands in print/headless/non-interactive mode](https://github.com/anthropics/claude-code/issues/837)
- [anthropics/claude-code#38505 — \[FEATURE\] CLI flag to invoke a skill `/<cmd>` like interactive mode](https://github.com/anthropics/claude-code/issues/38505)
