# Research: Kimi Code CLI (`kimi`) for Switchboard

> **Captured-in-time provenance.** Raw probes of the installed binary. If Kimi becomes a supported harness, [`harness-behavior.md`](../harness-behavior.md) holds the current behavior and this doc keeps the evidence behind it.

**Captured:** 2026-11-07.
**Tool version:** `kimi --version` reports `2.5.2`.
**Status:** Hands-on probing of `~/.kimi-code/bin/kimi` on macOS, logged in to a Kimi Code subscription through the TUI's `/login`. Every prompt asked for a one-word reply or one `echo`, so the whole probe cost a few cents of quota and left 14 throwaway sessions under `~/.kimi-code/sessions/` for scratch working directories.

**Command surface:** [`cli-surface/kimi.txt`](cli-surface/kimi.txt), regenerated with `make cli-surface HARNESS=kimi`.

**Docs:** https://moonshotai.github.io/kimi-code/en/ (index at `/llms.txt`). Facts below marked "docs" come from there and were not checked by hand.

## Names

- **Kimi Code CLI** is the current product: binary `kimi`, repo `MoonshotAI/kimi-code`, MIT. Its ACP server calls itself `Kimi Code CLI`.
- **Kimi CLI** (`MoonshotAI/kimi-cli`, Python, PyPI `kimi-cli`) is archived. Its README says existing installs will stop working. `kimi migrate` imports its data.
- **Kimi Code** is also the desktop app (`kimi app`) and the subscription plan (kimi.com/code).

## Binary

```
$ ls -la ~/.kimi-code/bin/
-rwxr-xr-x  2953808  fd
-rwxr-xr-x  182590912  kimi
$ file ~/.kimi-code/bin/kimi
Mach-O 64-bit executable arm64
```

- One 183 MB arm64 executable, installed by the vendor's `install.sh` into `~/.kimi-code/bin/` with a bundled `fd`. Docs also list `brew install kimi-code` and `npm install -g @moonshot-ai/kimi-code`.
- The process shows up in `ps` as `kimi-code`. It loads native modules from `~/Library/Caches/kimi-code/native/2.5.2/darwin-arm64/<hash>/node_modules/`, so it is a packaged JavaScript runtime, not a Go or Rust binary.
- `kimi upgrade` is the self-update path. No auto-update was observed during the probe.
- `kimi -p` with stdout redirected to a file flushes only at exit; through a pipe, lines arrive as they are produced. `kimi acp` behaves the same way (see "ACP").

## Headless invocation

```
kimi -p "<prompt>" --output-format stream-json </dev/null
```

| Flag | Observed |
|---|---|
| `-p, --prompt <prompt>` | One turn, no TUI. Exit 0 on success. |
| `--output-format text\|stream-json` | Only with `-p`. Without `-p`: `error: Output format is only supported in prompt mode.`, exit 1. |
| `-m, --model <alias>` | Per-call model. An alias missing from `config.toml`: `error: failed to run prompt: Model "kimi-code/does-not-exist" is not configured in config.toml.`, exit 1. |
| `-S, --session <id>`, `-r <id>` | Resume by id, works under `-p`. `-r` is an undocumented alias printed by the CLI's own resume hint. |
| `-c, --continue` | Resume the most recent session for the cwd, works under `-p`. |
| `-y, --yolo`, `--auto`, `--plan` | Rejected with `-p`: `error: Cannot combine --prompt with --yolo.`, exit 1. |
| `--add-dir <dir>`, `--skills-dir <dir>`, `--agent <name>` | Not probed. |

There is no `--cwd` flag: the working directory is the process cwd. There is no effort flag: effort comes from the model's `default_effort` in `config.toml` (see "Models").

**Permissions.** Under `-p` the session runs in permission mode `auto` (`permission.set_mode` in the session file) and the model receives a system reminder saying approvals are automatic and `AskUserQuestion` must not be called. A plain `echo` ran without any prompt. The docs say static deny rules still apply; not probed.

**stdin.** `kimi -p` waits for EOF on stdin before it starts. With stdin held open by `sleep 40 |`, the turn took 40 s. Content on stdin was ignored (a reply of `ack` to a prompt asking for `ack`, with a contradicting instruction on stdin). A spawner must close stdin or redirect it from `/dev/null`.

**Exit codes observed:** 0 success; 1 for every configuration, auth, session and provider failure; 130 on SIGINT; 143 on SIGTERM.

**Latency:** a one-word turn took 4 to 5 s wall clock; `system.version` appears on stdout within 0.4 s.

## stream-json

One JSON object per line on stdout. Every line has `role`; `meta` lines also have `type`. Everything the probe saw, verbatim apart from ids:

```jsonl
{"role":"meta","type":"system.version","version":"2.5.2"}
{"role":"assistant","content":"ack"}
{"role":"meta","type":"session.resume_hint","session_id":"session_5bfaaab8-2b37-453c-bcc0-9a1d6e09bacf","command":"kimi -r session_5bfaaab8-2b37-453c-bcc0-9a1d6e09bacf","content":"To resume this session: kimi -r session_5bfaaab8-2b37-453c-bcc0-9a1d6e09bacf"}
```

A tool call:

```jsonl
{"role":"meta","type":"system.version","version":"2.5.2"}
{"role":"assistant","tool_calls":[{"type":"function","id":"tool_5hQ8PNWVcgf1WZUO0PXcdEo7","function":{"name":"Bash","arguments":"{\"command\":\"echo hello-from-probe\"}"}}]}
{"role":"tool","tool_call_id":"tool_5hQ8PNWVcgf1WZUO0PXcdEo7","content":"hello-from-probe\n"}
{"role":"assistant","content":"hello-from-probe"}
{"role":"meta","type":"session.resume_hint",...}
```

A provider that cannot be reached (temporary provider via `KIMI_MODEL_*` env vars pointing at a closed local port):

```jsonl
{"role":"meta","type":"system.version","version":"2.5.2"}
{"role":"meta","type":"turn.step.retrying","failed_attempt":1,"next_attempt":2,"max_attempts":10,"delay_ms":597.8581795944432,"error_name":"APIConnectionError","error_message":"Connection error."}
... attempts 2 to 9, delays growing to about 37 s ...
```

then on stderr `error: failed to run prompt: provider.connection_error: Connection error.` and exit 1, after 147 s. No error line reaches stdout.

**Vocabulary observed:** `meta/system.version`, `meta/session.resume_hint`, `meta/turn.step.retrying`, `assistant` with `content`, `assistant` with `tool_calls` (OpenAI function-call shape, `arguments` is a JSON string), `tool` with `tool_call_id` and `content`. The docs list no schema for this format.

**Not in the stream:** thinking (it is in the session file), token usage, the model name, turn start or end markers, the permission mode, and errors. The session id arrives only in the final `session.resume_hint`; a turn that fails or is cancelled prints none, so the id must then come from disk (see "Sessions").

**Timing.** Lines are whole messages written when a step completes, not deltas. In a turn whose tool ran `sleep 10`:

| Time | Line |
|---|---|
| +0.4 s | `system.version` |
| +15.9 s | `assistant` with `tool_calls` |
| +15.9 s | `tool` result |
| +18.6 s | `assistant` final text |
| +18.7 s | `session.resume_hint` |

The tool call is reported only together with its result, so nothing is written while a tool runs. Tool progress goes to stderr instead: the `echo` output appeared there as a bare `hello-from-probe` line.

## Text mode

`--output-format text` (the default) writes `• ack` to stdout. stderr carries `kimi version 2.5.2`, the model's thinking as `• ...` lines, and `To resume this session: kimi -r <id>`.

## Sessions

**Ids** are `session_` plus a lowercase UUID, for example `session_8bd4d92c-ce2a-4d83-96bb-25dcd0aef302`. The docs show ULID-style examples; the binary does not.

**Layout** under `~/.kimi-code/` (`$KIMI_CODE_HOME`):

```
session_index.jsonl                       one line per session: {"sessionId","sessionDir","workDir"}
sessions/.index-cache/scan.json
sessions/<workDirKey>/<sessionId>/
  state.json                              id, version 2, cwd, archived, title, forkedFrom, lastTurnReason, createdAt, updatedAt, agents
  agents/main/wire.jsonl                  the event record (protocol_version 1.5)
  agents/<subagent>/wire.jsonl            subagents (docs; none seen)
  logs/kimi-code.log                      per-session log with per-request timings
  notify/state.json                       {"enabled":false}
```

`<workDirKey>` is `wd_<lowercased basename>_<12 hex>` of the working directory, for example `wd_cwd_d8c864cf91f7` for `.../kimi-probe/cwd`. The hash input was not reverse-engineered; resolve a cwd to its sessions through `session_index.jsonl` or `kimi session list --cwd <dir> --json` instead.

**Paths are stored resolved.** `workDir` and `cwd` hold the real path (`/private/var/...`). `kimi session list --cwd /var/...` (the symlinked form on macOS) returned nothing for the same directory; pass the resolved path.

**When files appear.** The `session_index.jsonl` line, the session directory and `state.json` exist within 1 s of starting `kimi -p`, before the model has answered. So a spawner can learn the session id of a running turn from disk even though stdout reveals it only at the end.

**`kimi session list --json`** returns `[{id, title, workDir, sessionDir, createdAt, updatedAt, archived, metadata}]`, most recently updated first, with `--cwd`, `--all`, `--archived`, `--limit`. There is no `session delete` command (ACP has `session/delete`).

**`state.json.lastTurnReason`** was `completed`, `cancelled` or `failed` in every probed case.

### `wire.jsonl`

Every line has `type`; most have `agentId` and `time` (epoch ms). Event types in a completed tool-call turn, in order:

```
metadata                      {"protocol_version":"1.5","created_at":...}
environment.set_binding
profile.bind                  modelAlias, thinkingEffort, profileName, systemPrompt (full text), activeToolNames, agentsMdPaths
changeNotifier.snapshot       skillsHash, subagentNames, workspacePrimaryByWorkspace
permission.set_mode           {"mode":"auto"}
agent.message.appended        the user message, with meta.promptId and createdAt (ISO-8601)
agent.turn.started            turnId (integer from 0), queueItemId
turn.prompt                   turnId, input [{type:"text",text}], origin {kind:"user"}
context.append_message        the user message plus two <system-reminder> user messages (date; auto permission mode)
plugin.session_start
context.append_loop_event     event.type step.begin
tool_select.initialized       the tool list with descriptions
llm.tools_snapshot
llm.request                   provider, model, modelAlias, thinkingEffort, messageCount, turnStep "0.1"
usage.record                  usage {inputOther, output, inputCacheRead, inputCacheCreation}, usageScope "turn"
token_counting.measured       tokens (context size after the step)
context.append_loop_event     event.type content.part with part.type "think" | "text"
context.append_loop_event     event.type tool.call  {toolCallId, name, args, display}
context.append_loop_event     event.type tool.result {toolCallId, result {output, durationMs}}
context.append_loop_event     event.type step.end {finishReason "tool_use" | "end_turn", usage, latency fields}
... second step repeats llm.request, usage.record, content.part, step.end ...
agent.message.appended        assistant message (content parts incl. think), tool message, assistant message
agent.turn.ended              outcome "done"
turn.ended                    reason "completed", durationMs, requestId
token_counting.turn_recorded
prompt.completed              reason "completed", finishedAt
```

Resumed turns append to the same file with `turnId` 1, 2, 3 and a new `turn.prompt`. Thinking text is recorded in `content.part` and in the assistant `agent.message.appended`, so the session file holds more than stdout does. Token usage per step and per turn is recorded; no cost figures are.

## Resume

- `kimi -p "<prompt>" --session <id>` and `-r <id>` continued the session and answered from its history (the model named the earlier word). `--continue` picked the same session as the most recent for the cwd. The `session.resume_hint` repeats the same id.
- Unknown id: stdout `system.version` only, stderr `error: failed to run prompt: Session "session_0000...0000" not found.`, exit 1, within 1 s, no new session created.
- `--session` and `--continue` are mutually exclusive (docs).

**No session lock.** Two `kimi -p --session <same id>` started at once both exited 0, 5 s and 8 s later, and `wire.jsonl` stayed valid JSONL with the two turns recorded as `turnId` 1 and 2. Whether the second saw the first's turn in its context was not determined. Switchboard's own session lock stays necessary.

## Cancel

Signal sent 6 s into a turn whose tool was `sleep 45 && echo slept`:

| Signal | Exit | Time to exit | Child `bash -c` | stdout after the signal |
|---|---|---|---|---|
| SIGINT | 130 | < 1 s | killed, no `sleep 45` left | `assistant` `tool_calls`, then `{"role":"tool",...,"content":"Session closed"}` |
| SIGTERM | 143 | < 1 s | killed, no `sleep 45` left | same |

No `session.resume_hint` is printed. The session file ends with `step.end` `finishReason "interrupted"`, `tool.result` `{"output":"Session closed","isError":true}`, `agent.turn.ended` `outcome "aborted"`, `turn.ended` `reason "cancelled"`, and `prompt.aborted`; `state.json.lastTurnReason` is `cancelled`. The tool runs as a child `/bin/bash -c cd '<cwd>' && ...` of the `kimi-code` process, and the CLI reaps it itself on either signal.

## Failures

All exit 1, all print `system.version` and nothing else on stdout, and all name the cause on stderr as `error: failed to run prompt: <text>` followed by `See log: ~/.kimi-code/logs/kimi-code.log`.

| Situation | stderr text |
|---|---|
| Never logged in (empty `KIMI_CODE_HOME`) | `No model configured. Run `kimi` and use /login to sign in, then retry; or set default_model in config.toml.` |
| Logged in with `kimi login` but `config.toml` has no provider | same text as above |
| Account without a Kimi Code subscription | `provider.auth_error: Your current subscription does not have access to Kimi Code right now. Upgrade your plan to keep coding with Kimi Code: https://www.kimi.com/code/#pricing` |
| Model alias not in `config.toml` | `Model "<alias>" is not configured in config.toml.` |
| Unknown session id | `Session "<id>" not found.` |
| Provider unreachable | `provider.connection_error: Connection error.` after ten attempts (about 145 s), with `turn.step.retrying` meta lines on stdout; `turn.ended` carries `error {code, message, name, retryable: true}` |

Flag conflicts fail before any of this with `error: Cannot combine --prompt with --yolo.` and `error: Output format is only supported in prompt mode.`, no `system.version`, exit 1.

Not probed: quota exhaustion, a model error after partial output, a static deny rule blocking a tool.

## Authentication

- `kimi login` (device-code flow, `--region mainland-cn|global`) wrote `~/.kimi-code/credentials/kimi-code.json` (keys `access_token`, `refresh_token`, `expires_at`, `expires_in`, `scope`, `token_type`) and nothing else. With only that file, every headless turn failed with the "No model configured" text above, and `kimi acp` `session/new` returned `{"code":-32000,"message":"Authentication required"}` with the stderr log line `no provider configured; complete onboarding via /login or the providers endpoint`.
- The TUI's `/login` then wrote `config.toml`: `default_model`, `[providers."managed:kimi-code"]` (`type`, `api_key`, `base_url`, an `.oauth` sub-table), six `[models."kimi-code/..."]` tables, `[thinking] enabled = true`, and `[services.moonshot_search]` and `[services.moonshot_fetch]` provider tables. A second credentials file `kimi-code-env-<hash>.json` appeared.
- `kimi provider list` then prints `managed:kimi-code  type=kimi  models=6  source=oauth` and `Default model: kimi-code/kimi-for-coding`; before `/login` it printed `No providers configured.`. This is the cheapest non-interactive readiness check: it needs no model call and distinguishes the two states the headless error message does not.
- `kimi doctor config` validates `config.toml` syntax only; it reported `OK` on the empty file.
- API keys go in `config.toml` under a provider table; the CLI does not read `KIMI_API_KEY` from the shell (docs). `KIMI_MODEL_NAME` + `KIMI_MODEL_API_KEY` + `KIMI_MODEL_BASE_URL` synthesize a temporary provider for one process (docs, and used for the connection-error probe above: the session file records it as `modelAlias "__kimi_env_model__"`).

## Models

Aliases provisioned by `/login` on 2026-11-07, from `config.toml`:

| Alias | `display_name` | `max_context_size` | Efforts (`default`) | Notes |
|---|---|---|---|---|
| `kimi-code/kimi-for-coding` | K2.8 Preview | 1,048,576 | low, high, **max** | the default |
| `kimi-code/kimi-for-coding-highspeed` | K2.7 Code Highspeed | 262,144 | none listed | |
| `kimi-code/k3` | K3 | 1,048,576 | low, **high**, max | |
| `kimi-code/k3-256k` | K3-256k | 262,144 | low, **high**, max | no `video_in` |
| `kimi-code/k3.1-pro` | K3.1 Pro | 1,048,576 | low, **high**, max | |
| `kimi-code/k3.1-pro-highspeed` | K3.1 Pro Highspeed 2x Usage Cost | 1,048,576 | low, **high**, max | |

Every alias lists `thinking`, `always_thinking`, `image_in`, `tool_use`; all but the highspeed K2.7 list `dynamically_loaded_tools`.

- `-m kimi-code/k3` dispatched with `llm.request` recording `model "k3"`, `modelAlias "kimi-code/k3"`, `thinkingEffort "high"`, `provider "openai"` (the wire protocol to Moonshot, not the vendor).
- Effort is per model in `config.toml` (`default_effort`); the CLI has no flag for it. The TUI's `/model` and ACP's `session/set_model` and `session/set_config_option` are the documented ways to change it (docs).
- The alias table is written by `/login`, so it can change on the next login without a CLI release.

## MCP servers and skills

- MCP: `~/.kimi-code/mcp.json` (user) and `<cwd>/.kimi-code/mcp.json` (project), `{"mcpServers": {...}}` with `command`/`args`, `url`, or `transport: "sse"` entries (docs). Neither file existed on this machine. Project servers stay disabled under `-p` unless the workspace is trusted, via `KIMI_CODE_TRUST_WORKSPACE=1` or `[workspace] require_trust = false` (docs). The TUI has `/mcp` and `/mcp-config`; no `kimi mcp` subcommand exists in 2.5.2.
- Skills: `~/.kimi-code/skills/`, `~/.agents/skills/`, `<cwd>/.kimi-code/skills/`, `<cwd>/.agents/skills/`, plus `extra_skill_dirs` in `config.toml` (docs). `~/.agents/skills/` is the directory Switchboard already reads for Codex; it held 37 skills here, and `changeNotifier.snapshot` recorded a `skillsHash` for them. Format: `<name>/SKILL.md` with `name` and `description` frontmatter (docs).

## ACP (`kimi acp`)

JSON-RPC 2.0 over stdio. `initialize` needs no account and replied in 0.4 s:

```json
{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true,"audio":false,"embeddedContext":true},"sessionCapabilities":{"list":{},"resume":{},"close":{},"delete":{},"fork":{},"additionalDirectories":{}},"mcpCapabilities":{"http":true,"sse":true},"auth":{"logout":{}}},"authMethods":[{"id":"login","type":"terminal","name":"Login with Kimi account",...}],"agentInfo":{"name":"Kimi Code CLI","version":"2.5.2"}}
```

- The server exits when stdin reaches EOF **from a pipe**. When stdin was a FIFO whose writers had all closed, it kept running (killed after 30 s in two tests). With stdout redirected to a regular file, the reply was written only at exit. Spawn it with pipes on both ends.
- `session/new` before a provider is configured: `{"code":-32000,"message":"Authentication required"}`.
- Documented methods (docs): `session/new|load|resume|list|fork|close|delete|prompt|cancel|set_mode|set_config_option|set_model`, client-side `session/update`, `session/request_permission`, `fs/*`, `terminal/*`. Not probed beyond `initialize`.

## Fork

`kimi fork <id> --yes` printed `Forked to session_9d52... ("Fork: session_5bfa...") in 412ms`, exit 0, with no model call. The new session has `state.json.forkedFrom` set to the source id, `title "Fork: <source id>"`, `titleKind "replaceable"`, a `wire.jsonl` copied from the source with every turn plus a `forked` event, and its own `session_index.jsonl` line. Each turn has `turnId` starting at 0 in the copy as in the source.

## Implications for a Switchboard adapter

- **Spawn:** `kimi -p <prompt> --output-format stream-json [-m <alias>] [--session <id>]`, cwd set, stdin from `/dev/null`, stdout and stderr as pipes. Resume is `--session`; first turns have no id to pass, and `session_index.jsonl` (or the `session.resume_hint` at the end) supplies it afterwards, so Kimi follows the Codex and Antigravity runtime-captured pattern, with a `session_<uuid>` locator that fits neither existing `SessionLocator` variant as-is.
- **Stream:** small vocabulary and no deltas; the UI gets one message per step and sees a tool only once it has finished. Thinking and usage come from `wire.jsonl`, which is the richer source and the one to parse for history.
- **Cancel:** SIGINT or SIGTERM to the process; the CLI kills its own tool child and records `cancelled`. Exit 130/143 with no terminal stream event.
- **Failure classification:** stderr prefix after `error: failed to run prompt: `, with `provider.auth_error:` and `provider.connection_error:` as the structured codes; the same codes appear in `turn.ended.error.code`.
- **Auth and install check:** binary on `PATH` plus `kimi provider list` reporting a provider; a credentials file alone is not enough.
- **Model picker:** read aliases, display names and efforts from `config.toml` rather than hard-coding, since `/login` writes them.

## Not probed

Quota exhaustion shape; account usage read; `/compact` or any compaction path under `-p`; context report; `--add-dir`; images; subagent sessions (`agents/<id>/wire.jsonl`); a static deny rule under `-p`; `kimi web` REST and WebSocket API; Windows and Linux paths.
