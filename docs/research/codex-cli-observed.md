# Research: Codex CLI hands-on observations

**Captured:** 2026-05-09
**Tool version:** codex-cli 0.128.0
**Companion to:** [codex-noninteractive.md](codex-noninteractive.md) (the docs-derived note). This file captures what we actually observed by exercising the CLI.

## Method

Probes were run from a clean scratch directory (`/tmp/switchboard-probe/`) against `codex exec` with various flags. Stream output (JSONL) and on-disk session files were inspected.

## CLI surface relevant to Switchboard

Top-level Codex commands include `exec` (non-interactive), `review`, `mcp`, `plugin`, `mcp-server` (run Codex itself as an MCP server, interesting), `resume` and `fork` (interactive variants), plus `apply`, `sandbox`, etc.

Switchboard uses `codex exec`. Relevant flags:

| Flag | Purpose |
|---|---|
| `--json` | Print events to stdout as JSONL (the only "structured output" mode). |
| `-o, --output-last-message <FILE>` | Also write the final assistant message to a file. |
| `-c, --config <key=value>` | Override any config value (TOML-typed); the primary configuration mechanism — Codex doesn't have one flag per knob the way Claude Code does. |
| `-s, --sandbox <mode>` | `read-only`, `workspace-write`, `danger-full-access`. |
| `--dangerously-bypass-approvals-and-sandbox` | The "yolo" flag. **Undocumented alias `--yolo` is accepted** in 0.128.0 (verified by direct test: `codex exec --yolo --json "..."` runs identically to the long form). The alias does not appear in `--help`; preferring the long form is safer for forward compatibility. |
| `-C, --cd <DIR>` | Working directory. |
| `--add-dir <DIR>` | Additional writable directories. |
| `--skip-git-repo-check` | Allow running outside a git repo (Codex defaults to requiring one). |
| `--ephemeral` | Don't persist the session. |
| `--ignore-user-config` | Skip `$CODEX_HOME/config.toml`. |
| `--ignore-rules` | Skip `.rules` execpolicy files. |
| `--output-schema <FILE>` | JSON Schema for the model's final response shape. |
| `-i, --image <FILE>...` | Attach images to the initial prompt. |
| `-m, --model <MODEL>` | Model selection. |
| `--enable <FEATURE>` / `--disable <FEATURE>` | Feature flags. |

Subcommand under exec: `codex exec resume <session-id> "<prompt>"` for resuming a previous session non-interactively. (Top-level `codex resume` and `codex fork` are interactive; only `exec resume` is for headless use.)

`codex exec` does **not** have an explicit fork analogue — only resume. To branch a session non-interactively, the closest path is to start a new session and feed in summarized prior context, or work at the session-file level (Codex stores sessions as JSONL too — see below).

## Session storage and lifecycle

**Storage location:** `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<session-uuid>.jsonl`

Date-partitioned, timestamped, with the session UUID at the end.

**Format:** newline-delimited JSON. Two notable distinctions from Claude Code:

1. **The session file is much richer than the streamed `--json` output.** The stream is a deliberately minimal subset; the session file contains the full transcript including system prompts, internal events, rate-limit info, and reasoning blocks (encrypted).
2. **Each event has a `timestamp` field and a `type`/`payload` shape.**

Session-file event types observed:

- `session_meta` — session ID, timestamp, cwd, originator (e.g. `"codex_exec"`), CLI version, model_provider, and the **full `base_instructions` system prompt** as text.
- `event_msg` with payload subtypes:
  - `task_started` (with `model_context_window`, e.g. `258400` for GPT-5.4)
  - `user_message`
  - `token_count` (totals + last + `model_context_window` + `rate_limits`)
  - `task_complete` (with `last_agent_message`, `duration_ms`, `time_to_first_token_ms`)
- `response_item` — the actual model conversation items. Roles include `developer` (Codex's framework messages, e.g. permissions instructions and skills listing), `user`, and `assistant`. Content blocks include `input_text`, `output_text`, `reasoning` (with `encrypted_content`).
- `turn_context` — model, sandbox_policy, approval_policy, effort, summary, truncation_policy.

**Resume confirmed:** `codex exec resume <thread_id> "..."` continues the session. The session UUID stays the same; the model recalls prior context.

## `--json` stream — much more terse than Claude Code

Stream events observed across a tool-using turn (Codex runs a shell command):

1. `thread.started` — `{thread_id}`
2. `turn.started` — no payload
3. `item.completed` — `{item: {id, type: "agent_message", text}}` — intermediate "I'm going to..." narration
4. `item.started` — `{item: {id, type: "command_execution", command, aggregated_output: "", exit_code: null, status: "in_progress"}}`
5. `item.completed` — `{item: {id, type: "command_execution", command, aggregated_output, exit_code, status: "completed"}}`
6. `item.completed` — `{item: {id, type: "agent_message", text}}` — final answer
7. `turn.completed` — `{usage: {input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens}}`

That's the entire structured surface Switchboard sees from `--json`. Notably **absent** compared to Claude Code's stream:

- No `system/init` event with environment summary.
- No per-turn cost (`total_cost_usd`).
- No `model_context_window` in any stream event (it IS in the session file).
- No rate-limit events in the stream (also session-file only).
- No tool-name vocabulary beyond `command_execution` and `agent_message` for our probe — Codex is shell-centric, the model expresses everything through commands rather than a typed-tool palette.

Switchboard implication: if we want context-window utilization, rate-limit info, or full reasoning traces from Codex, **we have to read the session file** in addition to (or instead of) the stream. The stream is enough for "did the turn complete and what did the model say"; the session file is what holds the operational details.

### Stop detection: `turn.completed`

Definitive end-of-turn signal is the `turn.completed` event. Reliable, single, comes last.

### Tool calls = shell commands

Codex's primary tool-call vocabulary in non-interactive mode is `command_execution`. Every shell action goes through this — `cat`, `git`, `apply_patch` (for edits), etc. The stream shows the literal command, captures `aggregated_output` and `exit_code`. From Switchboard's POV, this is much simpler to render than Claude Code's wider tool palette but also less semantic — you just see "what command did Codex run."

## Token usage and context window

Token usage IS in the stream's `turn.completed`:

```json
"usage": {
  "input_tokens": 14083,
  "cached_input_tokens": 6528,
  "output_tokens": 23,
  "reasoning_output_tokens": 10
}
```

`model_context_window` is **not** in the stream but IS in the session file's `task_started` and `token_count` events:

```json
{"type":"event_msg","payload":{"type":"task_started","turn_id":"...","model_context_window":258400,...}}
```

For our probe (GPT-5.4), `model_context_window: 258400`.

**Switchboard implication:** Codex's model→max-context map can be derived from the session file rather than hardcoded — but only by reading the session file. If Switchboard prefers stream-only, it needs the hardcoded map.

Open question 10.12 (model→max-context map maintenance) is therefore: for Claude Code we can read it from the stream's `result.modelUsage.<model>.contextWindow`; for Codex we either read the session file or maintain a map. Inconsistent.

## Cost

Codex does **not** expose `total_cost_usd` anywhere observable. Only token counts. To display cost for Codex agents, Switchboard would need to maintain a per-model pricing table and compute it. This was already noted in the docs-derived note; confirmed here.

## Rate limits

Rate-limit info is in the session file's `token_count` events:

```json
"rate_limits": {
  "limit_id": "codex",
  "primary": {"used_percent": 17.0, "window_minutes": 300, "resets_at": ...},
  "secondary": {"used_percent": 5.0, "window_minutes": 10080, "resets_at": ...},
  "plan_type": "plus"
}
```

Two windows reported (300 min = 5 hours; 10080 min = 1 week). Useful to surface to the user.

## Skills mechanism (different from Claude Code)

Looking at the session file's `response_item` events, Codex's skills are passed inline as a `developer` message containing the full list of all available skills (name + description + path). The model has them all in context every turn, rather than auto-discovering by name as Claude Code does.

This is a meaningfully different model: Codex skills are always-loaded context; Claude Code skills are discoverable-via-name. From Switchboard's perspective, both are per-agent concerns we don't mediate, but worth knowing the mechanic differs.

## MCP integration

`codex mcp list` shows configured servers. Today the user has just `tiddly_notes_bookmarks` registered (vs Claude Code's 6). Codex's MCP support is real but practically much narrower than Claude Code's — consistent with our prior research note.

## Sandbox / approval semantics

Codex separates **sandbox mode** (filesystem write boundaries) from **approval policy** (whether to ask before commands). The yolo flag `--dangerously-bypass-approvals-and-sandbox` collapses both to "off." The directory-trust prompt was not encountered in our probe (cwd was `/private/tmp/switchboard-probe`, with `--skip-git-repo-check` — clean test).

`turn_context` event in the session file confirms: `"approval_policy": "never"`, `"sandbox_policy": {"type": "danger-full-access"}`, `"permission_profile": {"type": "disabled"}`.

## Quirks and surprises

- `codex exec "..."` printed `Reading additional input from stdin...` even when prompt was passed as an argument and no stdin was piped. Cosmetic warning to stderr-or-stdout (looked like stdout but quick).
- The full base instructions (system prompt) are in the session file — useful for transparency / debugging, also means session files are not small.
- Codex distinguishes `developer` role (its own framework messages like permissions and skills) from `user` role.
- Reasoning content is encrypted (`encrypted_content`) — we can see reasoning happened, can't see what.

## Error events

Forced by passing `-m invalid-model`:

```json
{"type": "thread.started", "thread_id": "..."}
{"type": "turn.started"}
{"type": "error", "message": "{\"type\":\"error\",\"status\":400,\"error\":{...,\"message\":\"The 'invalid-model' model is not supported when using Codex with a ChatGPT account.\"}}"}
{"type": "turn.failed", "error": {"message": "..."}}
```

Two events emitted: `error` (early signal) and `turn.failed` (terminal). Process exit code is `1`.

**Switchboard implication:** Codex's normalized end-of-turn signal is **`turn.completed` OR `turn.failed`** — not just `turn.completed`. The harness adapter's "wait for end of turn" loop must listen for both. The `error` event before `turn.failed` carries the same information; Switchboard can ignore it and rely solely on `turn.failed` to keep the adapter simpler.

## Cancellation (SIGTERM mid-stream)

Probed by spawning `codex exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox "Write a 100-line poem..."`, waiting until the model was reasoning, then sending SIGTERM to the parent process.

**Process model:** Codex is a **two-process tree**:
- Parent: a Node.js wrapper (`node .../bin/codex`)
- Child: the actual codex binary (Rust, `vendor/aarch64-apple-darwin/codex/codex`)

`pgrep -P <parent_pid>` shows the child. Killing the parent with SIGTERM **does kill the child** — no orphan processes left behind. Verified: after the parent died, `ps -p <child_pid>` shows nothing.

**Best practice for Switchboard**: spawn the harness in its own process group (`Command::process_group(0)` in Rust, `os.setsid()` in Python), then on cancel send the signal to the entire group (`killpg`). This handles both Claude Code's single-process case and Codex's two-process case uniformly without special-casing.

**Exit code:** `0` — the parent catches SIGTERM and exits gracefully. **Switchboard cannot use exit code alone to distinguish "killed mid-stream" from "completed normally"** for Codex. The reliable signal is the absence of a `turn.completed` or `turn.failed` event in the stream before exit.

**Stream output**: captured `thread.started` and `turn.started`, but no `item.completed` or `turn.completed`. The model was in its reasoning phase when killed; no `agent_message` had streamed yet. Switchboard's adapter sees: terminal event missing + process exited = cancelled.

**Session file (`~/.codex/sessions/.../rollout-*.jsonl`)**: notably **richer than the stream** — captured `session_meta`, `turn_context`, `user_message`, `task_started`, a `token_count` event with rate-limit info, and a `reasoning` event (encrypted). But no `agent_message` (model never produced final output). Same conclusion as Claude Code: the partial response content can't be recovered from the session file; only what was streamed is the operator's "here's what we have so far."

**Resume after cancel**: works cleanly. `codex exec resume <thread_id> "Just say 'resumed ok'..."` returned successfully with the expected agent message. The session is in a usable state.

**Switchboard implication**: same shape as Claude Code, with two extras worth handling:
1. Use **process groups** so the kill signal reaches the codex child, not just the Node parent. (Lazy approach — kill just the parent — works on macOS/Linux because the child dies anyway in our tests, but explicit process groups are safer for cross-platform behavior.)
2. **Don't rely on exit code** to detect cancellation — Codex parent exits 0 even after SIGTERM. Detect via "stream ended without `turn.completed`/`turn.failed`."

## Things still worth probing

- **Forking sessions in non-interactive mode.** No native `codex exec fork`. Possibly we copy the session file to a new ID? Worth a future probe.
- **Output schema (`--output-schema`).** How does it shape `task_complete.last_agent_message`?
- **MCP-provided tools showing in stream.** Our test used the bundled tiddly server but the test prompt didn't call its tools. A probe that does would confirm whether MCP tool calls flow through `command_execution` or a separate item type.
- **Auto-compaction in `codex exec`.** Mentioned in docs but not observed (our turn was tiny).
- **`codex exec review` subcommand.** What does it do that `exec` doesn't? Plausibly a structured review mode.

## Resolutions / updates for the system-design

1. **Open question 10.12 (model→max-context map)** — for Codex, `model_context_window` is in session-file events but not the stream. Switchboard can read it from the session file or maintain a map. Either way, Codex needs more work than Claude Code. The plan's open question stays open for Codex but is partially resolved overall.
2. **§5 fork primitive** — Claude Code has native `--fork-session`; Codex does not have a non-interactive equivalent. Should be flagged as harness-asymmetric in the comparison doc.
3. **§5 cost reporting** — confirmed asymmetric: Claude Code surfaces `total_cost_usd`; Codex requires Switchboard to derive cost from token counts × per-model pricing. We should ship a pricing table.
4. **Rate limits** — both harnesses expose them in some form, but locations differ (stream for Claude Code; session file for Codex). Worth explicit note in §5.

## Sources

- Hands-on probes captured in `/tmp/switchboard-probe/` (`codex-hello.out`, `codex-tool-call.out`).
- Session files at `~/.codex/sessions/2026/05/09/rollout-*.jsonl`.
- `codex --help`, `codex exec --help`, `codex exec resume --help`, `codex fork --help`, `codex mcp --help`, `codex mcp list` (v0.128.0).
