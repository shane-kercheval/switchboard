# Research: Antigravity CLI (`agy`) for Switchboard

**Captured:** 2026-05-19
**Tool version:** `agy --version` reports `1.0.0` (CLI). `agy changelog` lists only `1.0.0: Initial release of the Antigravity CLI.` Build channel: stable.
**Status:** Hands-on probing against the installed binary at `/Users/shanekercheval/.local/bin/agy`, authenticated as `shane.kercheval@gmail.com` via macOS Keychain (silent auth — no `agy login` command was invoked during this probe; auth had been established previously through the Antigravity IDE app).

**Companion to:** [`gemini-cli-observed.md`](gemini-cli-observed.md), [`claude-code-cli-observed.md`](claude-code-cli-observed.md), [`codex-cli-observed.md`](codex-cli-observed.md). Antigravity is being evaluated as the headless replacement for Gemini CLI on free / Google AI Pro / Ultra tiers (announced cutover: 2026-06-18).

## Why we're researching it

Google has announced that on 2026-06-18 the free / Google AI Pro / Google AI Ultra tier flow that today routes through `gemini` (`@google/gemini-cli`) will be served by the new Antigravity product instead. `agy` is the CLI to the Antigravity desktop app. Switchboard's Gemini adapter (`crates/harness/src/gemini/`) is built against `gemini` v0.42.0 — the question is whether the existing adapter can swap to `agy`, share parser/session code with it, or needs to be rewritten as a separate adapter.

## Binary & version

```
$ which agy
/Users/shanekercheval/.local/bin/agy
$ agy --version
1.0.0
$ file /Users/shanekercheval/.local/bin/agy
/Users/shanekercheval/.local/bin/agy: Mach-O 64-bit executable arm64
$ ls -la /Users/shanekercheval/.local/bin/agy
-rwxr-xr-x@ 1 shanekercheval  staff  140435408 May 19 12:09 /Users/shanekercheval/.local/bin/agy
```

- **140 MB single static binary, Mach-O arm64.** No wrapper script; not a thin shim.
- **Written in Go.** `strings` shows `go1.27-20260427-RC04 cl/906595525 +5fb2392a6f X:fieldtrack,boringcrypto,simd` and dense `runtime.*` symbols. Internal package paths reference `google3/third_party/jetski/...` (a Google-internal monorepo). Heavy use of `jsonrpc2`, `gogoproto`, `grpc`, and embedded MCP client (`mcp.Connection`, `mcp.ClientSession`, `mcp.streamableClientConn`).
- **Architecture is `agy` (CLI process) → embedded gRPC "language server" in-process → remote Google "Code Assist" backend.** From `~/.gemini/antigravity-cli/log/cli-*.log`:
  ```
  server.go:1295] Starting language server process with pid 13230
  server.go:485] Language server listening on random port at 49463 for HTTPS (gRPC)
  server.go:492] Language server listening on random port at 49464 for HTTP
  http_helpers.go:178] URL: https://daily-cloudcode-pa.googleapis.com/v1internal:loadCodeAssist
  http_helpers.go:178] URL: https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels
  http_helpers.go:178] URL: https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse
  ```
  The gRPC server is in-process (same PID) and the wire calls go to `daily-cloudcode-pa.googleapis.com` — the same Google "Code Assist" backend that `gemini` uses. **Stream model is SSE (`alt=sse`) to Google.**
- **Server-side execution is the model.** The `agy` process orchestrates: it hosts the local MCP client, runs the tool subprocesses (e.g., `run_command`), and proxies tool calls/results to Google's backend, which decides the next step. The model never runs locally.

The accompanying **Antigravity desktop app** lives at `/Applications/Antigravity.app/` (Electron / Chromium app — `app.asar`, `DevToolsActivePort`, `Code Cache` are the tells). Its state directory is `~/Library/Application Support/Antigravity/` (~300 KB of Cookies / Preferences / SingletonLock — a typical Electron profile, not transcript storage). The IDE app and `agy` share auth (Keychain) and a small amount of state (`~/.gemini/antigravity/`) but the CLI's transcripts live elsewhere (see "Session files" below).

## Subcommands and flags

Full `agy --help` output (22 lines total):

```
Usage of agy:
  --add-dir                       Add a directory to the workspace (repeatable) (default [])
  -c                              Short alias for --continue
  --continue                      Continue the most recent conversation
  --conversation                  Resume a previous conversation by ID
  --dangerously-skip-permissions  Auto-approve all tool permission requests without prompting
  -i                              Short alias for --prompt-interactive
  --log-file                      Override CLI log file path
  -p                              Short alias for --print
  --print                         Run a single prompt non-interactively and print the response
  --print-timeout                 Timeout for print mode wait (default 5m0s)
  --prompt                        Alias for --print
  --prompt-interactive            Run an initial prompt interactively and continue the session
  --sandbox                       Run in a sandbox with terminal restrictions enabled

Available subcommands:
  changelog       Show changelog and release notes
  help            Show help for subcommands
  install         Configure environment paths and shell settings
  plugin          Manage plugins (install, uninstall, list, enable, disable)
  plugins         Alias for plugin
  update          Update CLI
```

Switchboard-relevant flag summary:

| Flag | Purpose | Switchboard relevance |
|---|---|---|
| `-p` / `--print` / `--prompt` | Single prompt, non-interactive. **Output is plain text to stdout.** | Required for headless dispatch. |
| `--print-timeout <dur>` | Max wait time for a single `-p` turn. Default `5m0s`. On timeout, prints `Error: timed out waiting for response` to stderr **and exits 0**. | Required to bound a turn. |
| `-c` / `--continue` | Continue the most recent conversation in this cwd. | Useful but per-cwd "most recent" semantics conflict with multi-agent. |
| `--conversation <uuid>` | Resume a conversation by ID. **Unknown UUID warns and starts a fresh conversation, no error.** | The closest analogue to Gemini's `--session-id` / `--resume` pair. |
| `--add-dir <path>` | Add a directory to the workspace (repeatable). | Extends file-tool reach beyond cwd. |
| `--dangerously-skip-permissions` | Auto-approve all tool permission requests. | Equivalent of Claude's `--dangerously-skip-permissions` and Gemini's `--yolo`. |
| `--sandbox` | Run with terminal restrictions enabled. | Not probed; sandboxing not in scope for Switchboard. |
| `--log-file <path>` | Override the default CLI log path. | Default is `~/.gemini/antigravity-cli/log/cli-<YYYYMMDD>_<HHMMSS>.log` (symlinked as `cli.log`). |

**Notable gaps vs. Gemini CLI:**

- **No `--session-id <uuid>` flag.** The caller cannot pre-generate a UUID — the conversation ID is server-assigned and only available after the turn starts. (See "Server-side vs local execution" below; the conversation is created on Google's backend.)
- **No `--resume latest` / `--resume <index>` form.** `-c` / `--continue` plays that role for "most recent"; UUIDs go through `--conversation`.
- **No `--output-format` / `--json` / `--stream-json` flag at all.** Output is plain markdown-ish text. No stream-JSON mode.
- **No `--model` flag.** Model selection lives in the desktop app's settings and is propagated via `model_config_manager`. The transcript shows `Gemini 3.5 Flash (High)` was used; switching models from the CLI is not exposed.
- **No `--skip-trust` flag.** Workspace trust appears to be handled implicitly (a fresh untrusted cwd like `/tmp/agy-probe-fresh-untrusted` ran a `-p` turn successfully without any prompt). Trust lives in `~/.gemini/antigravity-cli/settings.json` (`trustedWorkspaces: ["/Users/shanekercheval/repos/switchboard"]`). Initial trust is presumably granted at the IDE app's onboarding step (`POST_ONBOARDING_STEP_TYPE_ADD_WORKSPACE` in `~/.gemini/antigravity/antigravity_state.pbtxt`).
- **No `--list-sessions` / `--delete-session`.** No CLI-level enumeration of past conversations.
- **No `-i` headless mode.** `--prompt-interactive` opens the interactive TUI seeded with a prompt — not a Switchboard target.

## Authentication

**Auth lives in the macOS Keychain.** Login was already established via the Antigravity desktop app; `agy` reads it through Go's keyring binding:

```
auth.go:114] ChainedAuth: authenticated via keyring (effective: keyring)
```

Keychain entry confirmed: `security dump-keychain login.keychain | grep -i antigrav` returns `svce="gemini" acct="antigravity"`. Storage service name reuses `gemini` (load-bearing for the "shared `~/.gemini/`" theme — see below).

A separate OAuth credential file at `~/.gemini/oauth_creds.json` (mode 0600) carries the Gemini CLI's OAuth token. **Antigravity does not use this file**; it has its own keyring entry. The `oauth_creds.json` here is residue from the Gemini CLI install.

**Auth detection without invoking `-p`:** there is no `agy auth status` or `agy whoami` subcommand. The strongest signal is in the CLI log: every `agy -p` start emits `Failed to get OAuth token: error getting token source from auth provider: You are not logged into Antigravity.` repeatedly during init, then `Print mode: not authenticated, trying silent auth` → on success, `ChainedAuth: authenticated via keyring (effective: keyring)`. If keyring auth fails, the print-mode invocation will surface the failure as a print response (not probed — would require revoking the keyring entry).

**`agy login` does not exist as a subcommand.** Authentication flows through the Antigravity desktop app's onboarding screen, which writes to the Keychain. A headless-only environment (no GUI) is therefore **not a supported config**.

## Headless invocation end-to-end

Test:

```
$ cd /tmp/agy-probe
$ time agy -p "Reply with the single word 'ack'" 2>stderr.txt
ack
exit=0
agy ...  0.30s user 0.16s system 8% cpu 5.215 total
```

- **Stdout**: plain text — `ack` followed by a newline. **No JSON envelope.** No streaming event records. The model's text reply is dripped to stdout as it's generated (`text_drip.go:173] Drip stopped: lastStepIdx=2, charIdx=3, length=3` in the log shows the server-side text-drip mechanism; from the user's POV it looks like normal stdout streaming).
- **Stderr**: empty on success.
- **Exit code**: 0.
- **Wall-clock**: ~5s for a one-word reply (init overhead is ~1.5s of Go startup + `loadCodeAssist` + `fetchAvailableModels` HTTPS roundtrips before the first streamGenerateContent SSE call).

Larger reply (markdown-formatted):

```
$ agy --log-file /tmp/agy-custom.log -p "List 3 colors"
Here are 3 colors:

1.  **Red**
2.  **Green**
3.  **Blue**
```

The output is **markdown** — links are rendered as `[Name](file://...)`, lists use `1. **bold**` syntax. The CLI does not strip formatting. Switchboard would have to accept that "live tool output" for Antigravity is a markdown-decorated string blob with no per-event structure.

## Session files

**Storage root: `~/.gemini/antigravity-cli/`.** (Yes — under `.gemini/`. The CLI explicitly resolves "GeminiDir" and falls back to `~/.gemini/antigravity-cli/`. The shared `~/.gemini/oauth_creds.json`, `~/.gemini/settings.json`, and `~/.gemini/installation_id` are all visible to `agy` but it does not write to them — those remain Gemini-CLI-owned. From the log: `CLI app data directory: /Users/shanekercheval/.gemini/antigravity-cli`.)

Layout observed after a series of headless probes:

```
~/.gemini/antigravity-cli/
├── settings.json              # { colorScheme, enableTelemetry, trustedWorkspaces[] }
├── keybindings.json
├── installation_id
├── history.jsonl              # interactive-mode user input history (display strings, ts, workspace)
├── last_check.timestamp       # auto-update poll marker
├── bin/webm_encoder
├── updater/update.lock
├── log/
│   ├── cli.log -> log/cli-<YYYYMMDD>_<HHMMSS>.log
│   └── cli-<YYYYMMDD>_<HHMMSS>.log         # one Go-glog-style log per invocation
├── cache/
│   ├── onboarding.json
│   └── last_conversations.json             # { "<cwd>": "<conversation-uuid>" }
├── knowledge/
│   └── knowledge.lock
├── implicit/
│   └── <uuid>.pb                           # implicit conversations, encrypted .pb
├── conversations/
│   └── <conversation-uuid>.pb              # primary conversation log — encrypted .pb
├── brain/
│   └── <conversation-uuid>/
│       └── .system_generated/
│           └── logs/
│               └── transcript.jsonl        # plaintext JSONL transcript (load-bearing)
└── scratch/                                # ephemeral working dir
```

**Per-project state** lives at `~/.gemini/config/projects/<project-uuid>.json` with a symlink in the cwd:

```
~/.gemini/config/projects/12ff33a3-3b61-440b-b2ca-25983328ecee.json
/tmp/agy-probe/.antigravitycli/12ff33a3-3b61-440b-b2ca-25983328ecee.json -> <symlink to above>
```

Content:
```json
{
  "id": "12ff33a3-3b61-440b-b2ca-25983328ecee",
  "name": "/tmp/agy-probe",
  "projectResources": {
    "resources": [
      {"gitFolder": {"folderUri": "file:///tmp/agy-probe", "defaultBranch": "main", "allowWrite": true}}
    ]
  }
}
```

**`.antigravitycli/` IS written into the user's cwd as a side effect of the first `agy -p` invocation.** This is comparable to a `.git/` style metadata directory but ungitignored by default. Switchboard would need to advise users to `.gitignore /.antigravitycli/`.

### Conversation file format (the protobuf side)

`~/.gemini/antigravity-cli/conversations/<uuid>.pb` is **not** a vanilla protobuf. First bytes don't match protobuf tag patterns — they're high-entropy random bytes:

```
$ head -c 64 ~/.gemini/antigravity-cli/conversations/<uuid>.pb | xxd
00000000: 3342 3deb 349e 415b de3b 82af 16ea 40bf  3B=.4.A[.;....@.
00000010: 3f07 2243 92db bba2 48b6 f2bf 4dcc 147d  ?."C....H...M..}
00000020: 183c b9b6 35ec 960f 9d8e 5142 e017 b92b  .<..5.....QB...+
```

This is consistent with an AES-CTR / AES-GCM encrypted protobuf — the binary contains `crypto/aes`, `chacha20`, `EdDSA`, OpenPGP, x509 symbols extensively. The encryption key is presumably derived from the Keychain entry. **Switchboard cannot parse `.pb` conversation files without reverse-engineering the encryption.** This file is _not_ usable as a hydration source.

### Transcript file (the parseable side) — JSONL

`~/.gemini/antigravity-cli/brain/<conversation-uuid>/.system_generated/logs/transcript.jsonl` is **plaintext JSONL** — one record per "step". This is the source of truth Switchboard can parse.

Happy-path example (1-word reply):

```jsonl
{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:19:59Z","content":"<USER_REQUEST>\nReply with the single word 'ack'\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-05-19T12:19:59-07:00.\n</ADDITIONAL_METADATA>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.5 Flash (High). ...</USER_SETTINGS_CHANGE>"}
{"step_index":1,"source":"SYSTEM","type":"CONVERSATION_HISTORY","status":"DONE","created_at":"2026-05-19T19:19:59Z"}
{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:19:59Z","content":"ack"}
```

Tool-call example (`Read the file MARKER.txt and reply with its contents`):

```jsonl
{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:23:03Z","content":"<USER_REQUEST>\nRead the file MARKER.txt in current directory and reply with its contents only.\n</USER_REQUEST>..."}
{"step_index":1,"source":"SYSTEM","type":"CONVERSATION_HISTORY","status":"DONE","created_at":"2026-05-19T19:23:03Z"}
{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:23:03Z","thinking":"**Locating the Workspace** ...","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"ls -F\"","Cwd":"\"/Users/shanekercheval/.gemini/antigravity-cli/scratch\"","WaitMsBeforeAsync":"0","toolAction":"\"Listing files in scratch directory\"","toolSummary":"\"Listing files\""}}]}
{"step_index":3,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","created_at":"2026-05-19T19:23:07Z","content":"Created At: ...\nCompleted At: ...\nThe command completed successfully.\nStdout:\n\nStderr:\n"}
{"step_index":5,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:23:07Z","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"pwd\"","Cwd":"\".\"",...}}]}
{"step_index":6,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","created_at":"2026-05-19T19:23:08Z","content":"...Output:\n/tmp/agy-probe\n"}
{"step_index":7,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:23:08Z","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"ls -F\"","Cwd":"\"/tmp/agy-probe\"",...}}]}
{"step_index":8,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","created_at":"2026-05-19T19:23:10Z","content":"...Output:\nMARKER.txt\n"}
{"step_index":9,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:23:10Z","tool_calls":[{"name":"view_file","args":{"AbsolutePath":"\"/tmp/agy-probe/MARKER.txt\"",...}}]}
{"step_index":10,"source":"MODEL","type":"VIEW_FILE","status":"DONE","created_at":"2026-05-19T19:23:12Z","content":"...\nThe following code has been modified to include a line number before every line ... \n1: SWITCHBOARD_AGY_PROBE_42\n2: \nThe above content shows the entire, complete file contents of the requested file.\n"}
{"step_index":11,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:23:12Z","content":"SWITCHBOARD_AGY_PROBE_42"}
```

Record schema:

| Field | Type | Notes |
|---|---|---|
| `step_index` | int | Monotonically increasing per conversation. Note: not always strictly +1 (the tool-call example skips from 3 → 5; index 4 is presumably an internal/system step elided from `.system_generated/logs`). |
| `source` | string enum | `USER_EXPLICIT` (user input), `SYSTEM` (system step), `MODEL` (model output, including tool calls and tool results). |
| `type` | string enum | `USER_INPUT`, `CONVERSATION_HISTORY`, `PLANNER_RESPONSE`, `RUN_COMMAND`, `VIEW_FILE`, etc. The binary references many other types (from `strings`): `CortexStepRunCommand`, `CortexStepCodeAction`, `CortexStepCodeSearch`, `CortexStepFileChange`, `CortexStepGrepSearch`, `CortexStepEditNotebook`, `CortexStepReadNotebook`, `CortexStepMcpTool`, `CortexStepBrowser*` (browser automation), `CortexStepCheckpoint`, `CortexStepNotifyUser`, `CortexStepClipboard`, `CortexStepCloudSQLExecuteSQL`, `CortexStepFindAllReferences`, `CortexStepLintApplet`, `CortexStepCompileDiagnostic`, `CortexStepSuggestedResponses`, `CortexStepKnowledgeArtifacts`. Type vocabulary is large and presumably grows. |
| `status` | string | `DONE` was the only value observed. Likely also `RUNNING` / `FAILED` exist (not probed in completed transcripts). |
| `created_at` | RFC3339 UTC | Timestamp. |
| `content` | string (optional) | Free-form text. For `USER_INPUT`, this is the wrapped user prompt (with `<USER_REQUEST>` / `<ADDITIONAL_METADATA>` / `<USER_SETTINGS_CHANGE>` XML-tag envelope). For `PLANNER_RESPONSE` without tool calls, the model's reply text. For tool-result types (`RUN_COMMAND`, `VIEW_FILE`), a pre-rendered multi-line string with `Created At:`, `Completed At:`, `Stdout:`, `Stderr:`, `Output:`, `File Path:` headers. |
| `thinking` | string (optional) | The model's chain-of-thought, present on some `PLANNER_RESPONSE` records. **Always present in the transcript file**, unlike Gemini where `thoughts` only appear opportunistically. |
| `tool_calls` | array (optional) | Array of `{name, args}` objects. `args` values are pre-stringified — each value is a JSON string containing a JSON literal. Tool names observed: `run_command`, `view_file`. The binary references many more (see `type` above) — adapters must accept arbitrary names. |

**No `session_meta` line.** Unlike Gemini's session-file header (`{sessionId, projectHash, startTime, kind}`) or Codex's `session_meta`, the transcript file has **no top-level metadata record**. The conversation UUID is in the file path, not in the file. Switchboard's parser must derive metadata from the directory name.

**No tokens / usage / cost record.** The transcript file does not carry token counts, cached-tokens, or any usage telemetry. Those (if they exist) live inside the encrypted `.pb` conversation file or on the server.

**No assistant-message streaming records.** Unlike Gemini's per-chunk `{role:"assistant", delta:true}` events, the transcript file records the **completed** `PLANNER_RESPONSE` only. The text dripping seen on stdout is a server-side rendering effect (`text_drip.go`) — the on-disk record is whole.

**Resume creates a new conversation if the UUID is unknown.** Tested: `agy --conversation 00000000-0000-0000-0000-000000000000 -p "hi"` printed `Warning: conversation "00000000-0000-0000-0000-000000000000" not found.` to stderr and exited 0 with a fresh-conversation reply. No error / non-zero exit code. **Adapters cannot rely on exit code to detect missing-conversation.**

**Resume by known UUID appends to the same `transcript.jsonl`.** Both `-c` and `--conversation <known-uuid>` continue the same conversation; new steps are appended to the existing transcript file (verified — `--conversation 1343c404-... -p "Repeat the contents..."` recalled the prior file read correctly).

### `cache/last_conversations.json` — the per-cwd resume target

```json
{
  "/Users/shanekercheval/repos/switchboard": "832de387-660f-489a-9814-1ef8b19312db",
  "/tmp/agy-probe": "1343c404-84be-4274-af97-5e7aa173da4f"
}
```

This is the `-c` / `--continue` target — Antigravity tracks one "most recent conversation per cwd." Switchboard's adapter cannot use `-c` for multi-agent dispatch from the same cwd (the same problem Codex's `--resume last` has) — agents must resume by explicit UUID via `--conversation`.

### `implicit/<uuid>.pb` — separate "implicit" conversations

There's also an `implicit/` directory with separate `.pb` files. Hypothesis: these are short-lived background conversations created by IDE features (the implicit completion tier). Not surfaced in `last_conversations.json`. Not probed.

## Tool / permission model

- **Tools are server-side dispatched, locally executed.** Google's backend decides "call `run_command` with `ls -F`," tells `agy`, `agy` runs the subprocess locally, captures stdout/stderr, sends the result back to Google, Google decides the next step.
- **Built-in tool names observed in transcripts: `run_command`, `view_file`.** From binary symbols, additional native tools include `edit_file`, `replace_file_content`, `code_search` (`semmle.code.cpp.models.interfaces.Taint` is a Semmle reference — likely the code-search backend), `find_all_references`, `grep_search`, `edit_notebook`, `read_notebook`, browser automation (`navigate_page`, `take_snapshot`, `click_browser_pixel`, etc. via the bundled `chrome-devtools-plugin`).
- **Permissions are interactive by default.** `agy --help` lists `--dangerously-skip-permissions` as "Auto-approve all tool permission requests without prompting" — implying that without it, tool calls block on user prompts. Print-mode (`-p`) is silent: in the probe runs, `run_command` and `view_file` executed without prompting even without `--dangerously-skip-permissions` — print-mode appears to be implicitly permissive for the cwd workspace, or the CLI auto-approves when stdin is not a TTY (not pinned down).
- **`toolPermission=request-review` is the default setting**, per `~/.gemini/antigravity-cli/log/cli-*.log`. (`cli_setting_manager.go:65] CLI settings initialized: permissions=<nil>, toolPermission=request-review`.) What "review" means in `-p` mode is unclear after probing.
- **`run_command` runs in a configurable cwd.** The transcript records show `args.Cwd` explicitly set per call (sometimes `"."`, sometimes an absolute path, sometimes `"/Users/.../scratch"` — the model chooses). This matters for Switchboard's sandboxing/safety story.
- **Tool result format in the transcript is a pre-rendered text blob** (`"Created At: ...\nCompleted At: ...\nThe command completed successfully.\nStdout:\n...\nOutput:\n..."`). No structured `exit_code`, no separate stdout/stderr streams in the JSONL — they're concatenated into the `content` string.
- **`view_file` rewrites the file content with line-number prefixes** ("The following code has been modified to include a line number before every line, in the format: `<line_number>: <original_line>`"). Switchboard cannot recover the raw bytes of a file the model viewed from the transcript alone — line numbers are baked in.

## MCP server config

**Path: `~/.gemini/config/mcp_config.json`.** Same path as a freshly-installed Gemini CLI extension config; this is global, not per-project. Verified by the CLI log:

```
discovery.go:335] Failed to load JSON config file /Users/shanekercheval/.gemini/config/mcp_config.json: unexpected end of JSON input
```

The file's schema is the standard `{ "mcpServers": { "<name>": { "command": ..., "args": [...], "env": {...}, "serverUrl": "...", "headers": {...} } } }` shape (consistent with Gemini CLI and Claude Code). Both stdio (`command` / `args`) and HTTP (`serverUrl`) transports are supported per the binary's `mcp.streamableClientConn` references. **Schema parity with Gemini CLI presumed.** Not probed: per-project MCP overrides (`<cwd>/.gemini/config/mcp_config.json` or similar).

## Skills

**Yes — skills exist, under plugin directories.** `~/.gemini/config/plugins/<plugin-name>/skills/<skill-name>/SKILL.md`. A bundled `chrome-devtools-plugin` ships with multiple skills:

```
~/.gemini/config/plugins/chrome-devtools-plugin/skills/
├── debug-optimize-lcp/SKILL.md
├── troubleshooting/SKILL.md
├── chrome-devtools/SKILL.md
├── memory-leak-debugging/SKILL.md
└── a11y-debugging/SKILL.md
```

SKILL.md uses standard YAML-frontmatter + markdown:

```markdown
---
name: chrome-devtools
description: Uses Chrome DevTools via MCP for efficient debugging, troubleshooting and browser automation. Use when ...
---

## Core Concepts

**Browser lifecycle**: Browser starts automatically on first tool call ...
```

**Same convention as Claude Code's `~/.claude/skills/<skill>/SKILL.md`** — though Antigravity nests them under plugins. Plugins are managed via `agy plugin install/uninstall/list/enable/disable`. `agy plugin import` supports importing from existing Gemini or Claude installs (`Error: unknown import source or invalid path` when called without an arg, listing the two valid sources). Plugin install marketplaces are referenced (`plugin link <mp> <target>` — generate link to a marketplace) but not probed.

## Server-side vs local execution

**Load-bearing for Switchboard.** Architecture confirmed from the log:

1. The model lives entirely on Google's backend (`daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse`).
2. `agy` is the orchestrator: receives tool-call requests from the SSE stream, executes locally (`run_command`, `view_file`), and posts results back.
3. The conversation **state** lives server-side. The local `transcript.jsonl` is a **log of steps**, not the source of truth. Restarting `agy` and resuming via `--conversation <uuid>` re-fetches state from the server (the local `.pb` file is presumably a local cache; the JSONL transcript is reconstructed from server-side events).
4. **Cancellation reaches Google's backend through the SSE stream's closure**, not by signaling the model. When you SIGTERM `agy`, the local process exits, the SSE connection closes, and Google's backend stops generating. But the conversation state at the point of cancellation is still on Google's server — the server may have logged a partial step that the local transcript never received.

**Implications for Switchboard:**

- **No "replay from disk" possible.** Unlike Claude Code (whose `~/.claude/projects/.../*.jsonl` is the authoritative transcript), Antigravity's local files cannot reconstruct a conversation without the server. If the user is offline, the existing local transcript can be displayed, but no new steps can be generated.
- **Hydration is read-only and lossy.** Token / cost / usage data is server-side; it's not in the JSONL.
- **Process-group SIGKILL is meaningful** (it severs the SSE connection promptly), but the server may continue executing one more step before noticing the disconnect. Switchboard should not assume cancellation is instantaneous server-side.
- **Concurrent dispatches in the same cwd are not Gemini-style prefix-collision-risky** — there's no filename derived from session-ID prefix; each conversation gets its own directory under `brain/<full-uuid>/`. **No UUID-v4-vs-v7 concern here.**

## SIGTERM behaviour

Tested: `agy -p "Count from 1 to 100 slowly"` backgrounded, then `kill -TERM <pid>` (PID alone, not process group):

- The single `agy` process is a **lone parent** — `ps -o pid,ppid,pgid` shows no child processes (the embedded language server runs as a goroutine in-process). Subprocess `run_command` invocations are spawned transiently and reaped.
- `kill -TERM <pid>` causes immediate exit with **exit code 0**.
- The stdout in this probe was empty (no number was written before exit) because the model had not started streaming yet.
- **Switchboard's existing `Command::process_group(0)` + `killpg` pattern is unnecessary here** — `agy` does not fork persistent children. But it remains correct (a no-op for `agy` is fine).
- **Exit code 0 on cancellation matches Gemini.** The adapter cannot distinguish "completed turn" from "cancelled mid-turn" by exit code alone — it must check for a `PLANNER_RESPONSE` terminal record in the transcript file (or whatever terminal-step heuristic applies — given the JSONL has no explicit "turn complete" record, this is non-trivial).

## Error paths

| Probe | Stdout | Stderr | Exit |
|---|---|---|---|
| Empty prompt `agy -p ""` | (none) | `Error: empty prompt. Usage: agy --print "your prompt here"` | 0 |
| Unknown `--conversation <uuid>` | Fresh-conversation greeting (`Hello! I'm Antigravity, ...`) | `Warning: conversation "..." not found.` | 0 |
| Timeout (5-min default exceeded) | `Error: timed out waiting for response` | (none) | 0 |
| Unauthenticated (not probed; would require revoking Keychain entry) | unclear | unclear | unclear |
| Network failure (not probed) | unclear | unclear | unclear |

**Notable**: `agy` exits 0 on virtually every error condition surveyed. Failure detection by exit code is unreliable. The adapter must parse stdout for `^Error:` or `^Warning:` lines and check transcript-file completeness.

## Comparison to Gemini CLI

For each contract Switchboard's Gemini adapter relies on, here is the Antigravity equivalent:

| Contract | Gemini CLI (`gemini`) | Antigravity CLI (`agy`) | Verdict |
|---|---|---|---|
| Binary name | `gemini` (Node.js, `@google/gemini-cli`) | `agy` (Go, single static Mach-O) | **Different** |
| Language / install method | Node, `npm i -g` / `brew` / `npx` | Go, ships with Antigravity desktop `.app`; PATH installed via `agy install` | **Different** |
| Headless flag | `-p` / `--prompt <text>` | `-p` / `--print` / `--prompt <text>` | **Same shape, same letter** |
| Output format flag | `-o stream-json` / `-o json` / `-o text` | (none) — plain markdown text only | **Different** (no JSON / stream-JSON) |
| Stream event vocabulary | NDJSON: `init` / `message` / `tool_use` / `tool_result` / `result` | None on stdout. JSONL on disk: `USER_INPUT` / `CONVERSATION_HISTORY` / `PLANNER_RESPONSE` / `RUN_COMMAND` / `VIEW_FILE` / many more `CortexStep*` types | **Different** (offline-only, post-hoc vs live stream) |
| Session-ID flag (caller-controlled UUID) | `--session-id <uuid>` (caller mints UUID, first turn creates session) | (none) — conversation UUID is server-assigned | **Different** (regression: caller cannot control ID) |
| Resume flag | `--resume <uuid>` / `-r latest` / `-r <index>` | `--conversation <uuid>` / `-c` (most-recent-in-cwd) | **Renamed but same shape** (UUID-resume works) |
| Behaviour on unknown resume target | Exit 42 with stderr error | Exit 0 with stderr warning, fresh conversation | **Different** (silent fresh-start vs error) |
| Workspace-trust flag | `--skip-trust` (required for fresh cwd) | (none) — implicit / handled via Keychain + IDE onboarding | **Different** (no equivalent flag; trust is GUI-mediated) |
| Auto-approve permissions | `--yolo` / `--approval-mode yolo` | `--dangerously-skip-permissions` | **Renamed but same shape** |
| Session file path | `~/.gemini/tmp/<project-name>/chats/session-<startTime>-<id-prefix>.jsonl` | `~/.gemini/antigravity-cli/brain/<full-uuid>/.system_generated/logs/transcript.jsonl` (plaintext JSONL) + `~/.gemini/antigravity-cli/conversations/<uuid>.pb` (encrypted) | **Different** path and shape |
| Session file format | JSONL: header line + `{type:"user"}` / `{type:"gemini"}` / `{$set:...}` records; toolCalls embedded | JSONL: flat `{step_index, source, type, status, created_at, content, thinking?, tool_calls?}` records; no header, no `$set` | **Different shape** |
| Session file: token / usage data | Per-`gemini`-record `tokens: {input, output, cached, thoughts, tool, total}` | **Absent.** No usage data on disk. | **Different** (regression: no offline cost data) |
| Session file: thoughts / thinking | Opportunistic `thoughts:[{subject, description, ts}]` on some records | First-class `thinking: "..."` string field on `PLANNER_RESPONSE` records | **Different shape** (richer + more frequent) |
| Tool call envelope | `{id, name, args, result:[{functionResponse:...}], status, resultDisplay, ...}` per `toolCalls[]` | `{name, args}` (stringified-JSON values) on `PLANNER_RESPONSE`; tool result on a subsequent step with `type:"RUN_COMMAND"` / `"VIEW_FILE"` / etc., `content` is pre-rendered text blob | **Different** (split across two records; no structured result fields) |
| Tool name vocabulary | `update_topic` auto-fires; `read_file`, `run_shell_command`, etc. | `run_command`, `view_file`, plus 20+ `CortexStep*` types | **Different** (overlapping concept, different names) |
| Auth file path | `~/.gemini/oauth_creds.json` + `~/.gemini/settings.json` (`security.auth.selectedType`) | macOS Keychain (`svce="gemini" acct="antigravity"`); no canonical file | **Different** (Keychain only — no file-only auth detection) |
| MCP config path | `~/.gemini/settings.json` (`mcpServers` key) | `~/.gemini/config/mcp_config.json` (top-level `{mcpServers: {...}}`) | **Renamed but same shape** (same JSON schema, different file) |
| Skills path | `~/.gemini/extensions/<ext>/skills/...` (Gemini extensions) | `~/.gemini/config/plugins/<plugin>/skills/<skill>/SKILL.md` | **Renamed but same shape** (SKILL.md convention identical) |
| Process model | Single Node parent; subprocess SIGTERM works via process-group kill | Single Go parent, no persistent children; SIGTERM to pid works directly | **Different** (simpler — process-group plumbing unneeded) |
| Server-side vs local | Hybrid — model on server, but stream-JSON gives full event visibility locally; session file is fully replayable | Server-side state is authoritative; local transcript is a log of received events; no token data offline | **Different** (more server-dependent) |
| Concurrent-dispatch safety | UUID-v7 first-8-char filename collision risk (use UUID v4) | No filename collision risk — full-UUID directories | **Same** (no concern here) |
| `.gitignore` impact in cwd | None — Gemini doesn't write into the user's cwd | `<cwd>/.antigravitycli/<project-uuid>.json` symlink IS written on first invocation | **Different** (new gitignore consideration) |

## Pending verification / unclear after probing

1. **Auth-failure shape.** Requires revoking the Keychain entry; not probed. Expected to surface as a stdout/stderr error since `agy` has no JSON envelope.
2. **`--sandbox` semantics.** Not probed — would need to find a tool call that surfaces the sandbox restriction.
3. **Permission-prompt behaviour in `-p` mode without `--dangerously-skip-permissions`.** Tool calls (`run_command`, `view_file`) ran without prompts in print-mode without the flag, suggesting print-mode is implicitly permissive — but the underlying setting is `toolPermission=request-review`. The interaction between these is unclear after probing.
4. **Stream-JSON / structured output flag.** None documented or visible in `--help`; no env var equivalent surfaced from `strings`. If a hidden `--output-format json` exists, it wasn't found. Likely truly absent.
5. **Long-reply chunking.** With no `--output-format` flag, the transcript records the whole `PLANNER_RESPONSE` content in one record. Live stdout drips text. No way to observe boundaries.
6. **Encrypted `.pb` conversation file decryption.** Not attempted. The Keychain entry is presumably the key derivation source. Switchboard should not attempt this — the JSONL transcript is sufficient.
7. **Per-project MCP override path.** Not probed. The IDE app may support `<cwd>/.antigravitycli/mcp_config.json` or `<cwd>/.gemini/config/mcp_config.json`, but not verified.
8. **Behaviour when `~/.gemini/antigravity-cli/` is missing or read-only.** Not probed. Presumed to be re-created from defaults.
9. **Step `status` values beyond `DONE`.** Only `DONE` was observed in completed transcripts. The schema presumably includes `RUNNING` / `FAILED` / `CANCELLED` (analogous to Gemini's `status: "success"` / non-success), but cancellation mid-turn was not captured in a transcript probe.
10. **`step_index` gaps.** The tool-use transcript jumped 3 → 5 with no index-4 record. Whether index 4 is suppressed for the user-facing transcript or merely an internal-only step type is unclear.
11. **MCP tool-call shape in the transcript.** Only native tools (`run_command`, `view_file`) were observed in probing. The `MCP_TOOL_BACKGROUND_MODE_*` strings in the binary suggest MCP tool calls get a distinct `type` value, but real MCP wiring wasn't exercised.
12. **`history.jsonl` semantics.** The file at `~/.gemini/antigravity-cli/history.jsonl` carries a single interactive-mode probe entry from a separate `agy -i` run. It is NOT updated by `-p` mode invocations — `agy -p` writes nothing to it. Not a hydration source.

## Known limitations (Switchboard adapter)

- **Expired-resume context loss is silent to the user.** When a resume's `--conversation <uuid>` no longer exists server-side, `agy` prints `Warning: conversation … not found`, forks a *fresh* conversation, and answers without the prior context. The adapter detects this (scanning stdout + stderr), re-captures the new conversation UUID, and heals the sidecar so subsequent turns continue the new conversation — but the turn still completes with a real (context-free) answer, and the user is **not** notified that their prior context was dropped (only a `tracing::warn` breadcrumb is emitted). A user-visible "this conversation expired; continuing fresh" note would require a non-fatal warning event the normalized wire vocabulary doesn't have; widening it for this case is out of scope. If the fork can't be re-correlated (no matching new directory — e.g. the transcript path changed), the turn fails loudly as `AdapterFailure` rather than silently completing an unresumable agent. The fork-and-heal recapture sequence is covered hermetically by the `fake_agy` fixture binary (`crates/harness/tests/antigravity_adapter.rs`) — which simulates the stale-conversation fork, the new-directory mint, and the heal — in addition to the `make test-live` round-trip against a real server.
  - On the fork turn specifically, `agy` streams its fresh-conversation greeting (`Hello! I'm Antigravity…`) to stdout *before* the actual answer, so the user sees the greeting prepended to that one turn's reply. This is `agy`'s own output (can't be suppressed) and only occurs on the rare expiry-fork — not an adapter bug.
- **Conversation UUID capture is correlated by prompt, with a narrow concurrency caveat.** First-turn capture matches the new `brain/<uuid>/` directory whose `USER_INPUT` echoes this dispatch's exact prompt. Two concurrent same-cwd dispatches with *byte-identical* prompts are unbindable and fail loud (`AdapterFailure`, "retry") rather than risk binding the wrong conversation — a deliberate safety stop, not silent corruption.
- **Encrypted-only conversations are silently skipped on project reopen.** A conversation that exists only as the encrypted protobuf store (`conversations/<uuid>.pb`) with no `transcript.jsonl` sidecar — e.g. one created outside Switchboard, or whose `.system_generated/` artifacts were pruned — cannot be hydrated (the `.pb` is unreadable). Hydration degrades to an empty transcript with a single `tracing::debug` breadcrumb rather than blocking project open, so the user sees that agent on reopen as though it had no history. This matches the other harnesses' missing-session-file behavior.
- **MCP / skills registries are user-scope only.** The sidebar's Antigravity MCP-server and skills cells are loaded from `~/.gemini/config/mcp_config.json` and `~/.gemini/config/plugins/*/skills/*/SKILL.md` (user scope). A per-project override (`<cwd>/.gemini/config/mcp_config.json` or workspace-scoped plugins) is plausible but **unverified** in the research probe, so it is not loaded — a project that relies on workspace-scoped MCP servers or skills will show only the user-scope set. These registries are display-only (they do not gate dispatch), so the gap is cosmetic. The loaders' `cwd` parameter is reserved for adding workspace scope once its path and merge direction are confirmed.
