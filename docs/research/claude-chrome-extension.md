# Claude in Chrome — CLI integration (probe notes)

Ground-truth probe of how the Claude in Chrome browser extension is driven from
`claude` non-interactively, and why Codex needs no equivalent flag. Probed
2026-08-31 against **Claude Code 2.1.251** / **codex-cli 0.151.0** on macOS.

Probes ran from `/tmp/sbprobe`, first with the extension **installed and Chrome
running**, then repeated with the extension **removed** from Chrome.

## Summary

| | Claude Code | Codex |
| --- | --- | --- |
| Opt-in mechanism | `--chrome` CLI flag (per session) | none — global plugin |
| Where enablement lives | `~/.claude.json` → `claudeInChromeDefaultEnabled` | `~/.codex/config.toml` → `[plugins."chrome@openai-bundled"] enabled = true` |
| Who writes it | Claude Code (`/chrome` → "Enabled by default") | the ChatGPT desktop app |
| Works in headless `-p` | yes | yes |

Codex "just works" because the ChatGPT desktop app enables a **global** plugin that
`codex exec` inherits. Claude Code gates the same capability behind a per-session
flag, so an orchestrator has to opt in explicitly.

## Claude Code

### Flags

- `--chrome` — "Enable Claude in Chrome integration"
- `--no-chrome` — "Disable Claude in Chrome integration"

`claude --chrome --no-chrome` yields **0** browser tools, i.e. the disable wins in
that order. (Only that order was probed.)

### What `--chrome` adds

`--chrome` attaches an MCP server named `claude-in-chrome` with **22 tools**:

```
browser_batch, computer, file_upload, find, form_input, get_page_text,
gif_creator, javascript_tool, list_connected_browsers, navigate,
read_console_messages, read_network_requests, read_page, resize_window,
select_browser, shortcuts_execute, shortcuts_list, switch_browser,
tabs_close_mcp, tabs_context_mcp, tabs_create_mcp, upload_image
```

The `stream-json` `init` event reports it as
`{"name": "claude-in-chrome", "status": "connected"}` — the same shape as any
other MCP server, so it needs no special handling to surface.

Measured init cost on one machine: 20,434 → 22,863 context tokens (**+2,429**).
Small because this Claude Code version defers tool schemas behind `ToolSearch`;
the agent pays the rest only when it actually reaches for the browser.

### It works headlessly

Full end-to-end automation succeeded under Switchboard's exact invocation shape
(`-p --output-format stream-json --verbose --dangerously-skip-permissions`), plus
`--chrome`: the agent created a tab group, navigated to a page, read the
accessibility tree, and closed the tab. Exit code 0, `is_error: false`.

The one-time introductory dialog the docs describe is **interactive-only** — it
does not appear or block in `-p`.

### Blocker: browser selection when more than one is connected

With two browsers connected and none paired, **every** browser tool call fails with:

> Multiple Chrome browsers are connected to this account and none has been
> selected for this session. Before any browser action, you MUST call the
> AskUserQuestion tool …

`AskUserQuestion` does not exist in `-p`, so the turn dead-ends: the agent
reports the problem as prose and does no browser work. Exit code is still 0 and
`is_error` is still `false` — **the failure is invisible at the process level**.

Two escapes:

- The agent calls `mcp__claude-in-chrome__select_browser` with a `deviceId` from
  `list_connected_browsers`. Verified to work in `-p`.
- The user pairs once (interactively via `/chrome` → "Select browser…", or via any
  successful `select_browser`).

Pairing **persists user-globally** in `~/.claude.json`:

```json
"chromeExtension": { "pairedDeviceId": "…", "pairedDeviceName": "Browser 1" }
```

Once paired, later `-p` sessions drive the browser with no selection step —
verified with a fresh session that went straight to `tabs_context_mcp`.

Note that "browsers" here are extension connections, not browser applications: two
were listed (`isLocal: true`, macOS) with only one Chrome running.

### Native messaging host is installed on first use

Before any `--chrome` run this machine had only the **Claude desktop app's** host:

- `com.anthropic.claude_browser_extension.json` → `/Applications/Claude.app/Contents/Helpers/chrome-native-host`

The first `claude --chrome` wrote Claude Code's own:

- `com.anthropic.claude_code_browser_extension.json` → `~/.claude/chrome/chrome-native-host`

under `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/`. It
connected immediately, with no Chrome restart, contrary to the docs' warning that
Chrome reads the manifest at startup.

Moving that manifest aside and re-running still reported `status: "connected"` —
once Chrome has the connection, the manifest is not re-consulted per launch. So
manifest removal is **not** a valid simulation of an absent extension.

### Extension not installed: `--chrome` degrades silently

Probed with the extension actually removed from Chrome. **Passing `--chrome` with no
extension installed produces no error at any level a supervising process can see:**

- exit code **0**
- stderr **empty**
- `init` still reports all 22 tools and `{"name": "claude-in-chrome", "status": "connected"}`
- the final `result` event has `is_error: false`, `subtype: "success"`

The `connected` status describes the **local MCP server process**, not the browser.
It is not a liveness signal for the extension.

The failure surfaces only when the agent actually calls a browser tool:

> Browser extension is not connected. Please ensure the Claude browser extension is
> installed and running (https://claude.ai/chrome), and that you are logged into
> claude.ai with the same account as Claude Code. If this is your first time
> connecting to Chrome, you may need to restart Chrome for the installation to take
> effect. …

and even that arrives as an **ordinary successful tool result** — the `tool_result`
carries no `is_error` key at all. (Contrast the multi-browser case above, which does
set `is_error: true`.) The agent reads the prose, gives up, and reports it in its
answer text. So the whole failure mode is only ever visible as natural language.

The docs' interactive "Claude wants to use your browser" install prompt does not
appear in `-p`.

### There is no reliable pre-flight detection

- `~/.claude.json` → `cachedChromeExtensionInstalled` stayed **`true`** after the
  extension was removed. It is a stale cache; do not trust it.
- `claude mcp list` does not list `claude-in-chrome` at all (with or without
  `--chrome`) — it only reports user-configured servers, so it offers no health check.
- The native messaging manifest persists after uninstall, and is not re-consulted
  once Chrome holds a connection.
- The only true signal found is the Chrome profile directory
  (`~/Library/Application Support/Google/Chrome/<Profile>/Extensions/fcoeoabgfenejglbffodgkkbkcdhcgfn`),
  which was present when installed and absent after removal. Brittle: it varies per
  profile and per Chromium browser, so it is a heuristic at best.

The practical consequence: Switchboard cannot cheaply know whether the browser is
usable before dispatching a turn. The realistic options are to always pass `--chrome`
and accept an occasional wasted turn, or to make it an explicit per-agent opt-in the
user turns on when they know the extension is there.

### Other constraints (from docs, not probed)

- Requires a direct Anthropic plan and `/login` auth. With an API key or a
  `claude setup-token` token, Claude Code **keeps Chrome integration off even when
  `--chrome` is passed**. Switchboard is subscription-auth-only, so this is fine.
- Not available through Bedrock / Vertex / Foundry.
- Extension v1.0.36+.
- Not supported under WSL (not relevant — Switchboard is macOS).
- An org can block the `claude-in-chrome` MCP server via the `deniedMcpServers`
  managed setting.

## Where Switchboard would hook in

`build_args` in `crates/harness/src/claude_code/mod.rs` — a single push alongside the
existing `--model` / `--effort` options.

The setting belongs in the user-global personal `config.yaml`, not project config and
not per-agent: extension installation and browser pairing are both machine-level
facts, and Claude Code's own affordance (`/chrome` → "Enabled by default") is global
too. Whether an individual agent uses the browser is decided by the task, not by
configuration — the tools simply go unused otherwise, at a cost of ~2.4k context
tokens.

Emit the flag explicitly in both directions — `--chrome` when on, `--no-chrome` when
off — rather than omitting it when off. Switchboard is a supervisor whose displayed
state should match what the agent can actually do, and a user who set Claude Code's
own global default would otherwise get browser tools while Switchboard's UI claims
they are off. Being explicit also makes dispatch independent of
`claudeInChromeDefaultEnabled`, a key in a file Switchboard does not own.

**The compatibility objection to that was checked and does not hold.** Emitting a flag
unconditionally makes every ordinary Claude turn — not just browser ones — depend on
the CLI recognizing it, which would be a poor trade for a feature almost nobody
enables. But **both flags shipped together in 2.0.72 (2025-12-17)**, the release that
introduced Claude in Chrome as a beta: `--no-chrome` is present in that build's
`cli.js` and in 2.1.0's, verified by pulling both tarballs from npm. Chrome went GA at
2.1.198 and the current line is 2.1.251. So there is no version window in which
`--chrome` is understood and `--no-chrome` is not, and no realistic Switchboard user
is on a pre-2.0.72 CLI (auto-update is on by default). Alternatives considered and
dropped as machinery against a non-existent risk: omitting the flag when off, and
gating emission on reading `claudeInChromeDefaultEnabled`.

**Unverified, deliberately:** that `--no-chrome` actually overrides
`claudeInChromeDefaultEnabled: true`. Near-certain — an explicit flag beating persisted
config is near-universal CLI behavior, and `--no-chrome` exists for exactly this — but
never probed, because probing requires mutating the developer's real `~/.claude.json`
(auth is Keychain-bound, so an isolated `CLAUDE_CONFIG_DIR` copy fails to log in). The
only evidence in hand is `claude --chrome --no-chrome` → 0 tools, which is flag-vs-flag
precedence, a different question. If the assumption is wrong, the consequence is bounded
and cosmetic-to-mild: a user who enabled Chrome by default inside Claude Code sees
browser tools while Switchboard's Settings says off. To settle it, set the key `true`
and run two arms in one window — no flag (expect the server **present**, which proves
the key reaches `-p` at all) then `--no-chrome` (expect **absent**). Arm 1 is required:
without it a negative result cannot distinguish "the flag wins" from "the key is inert
in headless mode."

Note the asymmetry when presenting this in Settings: the toggle governs Claude agents
only. Codex's browser capability is controlled in the ChatGPT desktop app and cannot
be driven from Switchboard.

Browser pairing needs **no** Switchboard affordance. When the agent hits the
unpaired-multi-browser case it explains the problem in its reply and lists the
connected browsers by name; because Switchboard resumes sessions, the user answers in
the next message and the agent calls `select_browser` itself. The pairing then
persists user-globally forever. Total cost is one round-trip, once. Do not put
`/chrome` instructions in Settings — it is an interactive-only slash command that
does not exist in `-p`, so it would tell the user to go operate a different app.

A Settings-side pairing flow is possible (a scripted `claude -p` calling
`list_connected_browsers`, then `select_browser` on the user's pick — both verified to
work headlessly), but it spends a model turn and real UI to replace one sentence the
user types once. Not worth building unless the conversational path proves to confuse
people in practice.

## Sources

- <https://code.claude.com/docs/en/chrome>
- <https://code.claude.com/docs/en/cli-reference>
