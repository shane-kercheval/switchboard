# Research: Gemini CLI for Switchboard

**Captured:** 2026-05-13
**Tool version:** gemini-cli v0.42.0 (stable; weekly stable / preview / nightly release channels)
**Status:** **Docs-derived only.** This document was assembled from Google's official Gemini CLI docs, the open-source repo, and Google's pricing pages. Hands-on probing (fixture capture, stream-event verification, SIGTERM behaviour, session-file inspection) is **pending** and lands in M3 implementation alongside fixture capture for the Gemini adapter. Anything labeled "verify in M3" below is a known unknown.

**Companion to:** [claude-code-cli-observed.md](claude-code-cli-observed.md) and [codex-cli-observed.md](codex-cli-observed.md) — Gemini is the third harness Switchboard targets, slotted as M3 (per the v1 roadmap) ahead of the multi-agent UI and dispatcher work.

## Why we're researching it

Switchboard's M3 adds Gemini CLI as a third harness. The rationale, from the conversation that triggered this research: ironing out the per-harness adapter abstraction with three concrete implementations before moving to multi-agent UI / dispatcher contention (current M4) hardens the architecture before more complexity lands on top.

## CLI surface

**Binary:** `gemini`. Installable via `npm i -g @google/gemini-cli`, `npx @google/gemini-cli`, `brew install gemini-cli`, or MacPorts. Open source at <https://github.com/google-gemini/gemini-cli>.

**Headless mode trigger:** `-p` / `--prompt` flag, OR any non-TTY stdin (e.g., piped input). When triggered, no interactive prompts fire — output goes to stdout per the configured format. This is first-class, not bolted on.

**Relevant flags (planned probe coverage in M3):**

| Flag | Purpose |
|---|---|
| `-p, --prompt <text>` | Headless prompt. Triggers non-interactive mode. |
| `--output-format <fmt>` | `json` (single blob at completion) or stream-json (NDJSON). Both documented. |
| `-r, --resume [tag]` | Resume the most recent session, or a named checkpoint. |
| `-m, --model <model>` | Model selection (e.g., `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-3.1-pro-preview`). |
| `--checkpointing` | **Removed as a flag in v0.11**; moved to `~/.gemini/settings.json`. Flag-relocation churn worth tracking. |

The full flag surface is not yet captured here — defer to M3 fixture capture.

## Stream format

**Two formats supported, per Google's docs:**

1. **`--output-format json`** — single JSON blob at completion: `{response, stats, error?}`. Closest analog: Codex's batch JSON (no real-time visibility).
2. **Stream-json (NDJSON)** — newline-delimited events with types `init` / `message` / `tool_use` / `tool_result` / `error` / `result`. **This is the closest analog to Claude Code's stream-json of any CLI surveyed.** `result` is the terminal event, semantically equivalent to Claude Code's `result` event.

**Exit codes documented:** `0` success, `1` general error, `42` input error, `53` turn-limit exceeded.

**For Switchboard:** the stream-json format maps almost 1:1 onto Switchboard's existing `AdapterEvent` contract — `init` → `SessionMeta`, `message` content → `ContentChunk`, `tool_use` / `tool_result` → M2.2's `ToolStarted` / `ToolCompleted`, `result` → `TurnEnd`. The mapping table needs verification once fixtures are captured in M3.

## Session storage and resume

**Session ID model:** Gemini CLI **assigns its own session ID** — there is no documented `--session-id <uuid>` flag for the caller to pre-generate the ID. This matches Codex's model and is the opposite of Claude Code's caller-controlled UUID.

**Storage location:** `~/.gemini/` (verify in M3 — exact subdirectory structure not yet captured). Not per-cwd encoded the way Claude Code stores `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`.

**Resume mechanism:**
- `--resume` / `-r` flag with no argument: resume most recent session.
- `--resume <tag>` with a tag: resume a named checkpoint. Tags are managed via `/resume save <tag>` and `/resume resume <tag>` slash-commands inside the interactive CLI.

**Implication for Switchboard:** Switchboard needs a Codex-style sidecar to map `AgentId` → Gemini's CLI-assigned session ID. The pattern is already planned for Codex in M2 — Gemini reuses it. The session-file enrichment pattern (read `~/.gemini/.../<session>.jsonl` for metadata not on the stream) is also expected; verify in M3.

## Authentication

Four methods documented, all working in headless mode:

1. **Google OAuth personal account** — Gemini Code Assist for individuals. **Free tier: 1,000 requests/day.** This is the standout for Switchboard's onboarding story.
2. **Gemini API key** — issued from Google AI Studio. Free tier: 250 requests/user/day.
3. **Vertex AI** — Google Cloud project (Express or regular). Per-token billing.
4. **Google Workspace / Code Assist Standard/Enterprise** — enterprise subscription.

Auth method determines quota bucket (see Pricing).

## Pricing — the key finding

**Google does NOT split headless from interactive billing.** This is the most permissive of the three vendors:

| Auth method | Quota | Headless vs interactive split? |
|---|---|---|
| OAuth personal (free) | 1,000 req/day | No — same pool |
| Google AI Pro subscription | 1,500 req/day | No — same pool |
| Google AI Ultra subscription | 2,000 req/day | No — same pool |
| Code Assist Standard | 1,500 req/day | No — same pool |
| Code Assist Enterprise | 2,000 req/day | No — same pool |
| Gemini API key (free) | 250 req/user/day | N/A — pay-as-you-go |
| Vertex AI | Per-token, no daily cap | N/A — pay-as-you-go |

Compare to Claude Code (split into dedicated Agent SDK credit pool on 2026-06-15) and Codex (one pool, auth-method-determined). Google sits closest to Codex but with a more generous free tier.

**Per-token pricing (Vertex AI / API):**

| Model | Input ($/M) | Output ($/M) | Context window |
|---|---|---|---|
| Gemini 3.1 Pro Preview | $2 (≤200k) / $4 (>200k) | $12 (≤200k) / $18 (>200k) | 1M |
| Gemini 2.5 Pro | $1.25 (≤200k) | $10 (≤200k) | 1M |
| Gemini 2.5 Flash | $0.30 | $2.50 | 1M |

**For comparison:** Claude Sonnet ~$3/$15 per M; Claude Opus ~$15/$75. Gemini 2.5 Pro is ~2.4× cheaper than Sonnet at a similar tier with the same 1M context window. Gemini 2.5 Flash is cheap enough to be a fan-out workhorse.

## MCP / tool use

Yes — MCP servers are configurable in `~/.gemini/settings.json`. Parity with Claude Code and Codex on this dimension. **Verify in M3:** exact JSON shape of MCP tool events on the stream.

## Process model

Single Node process per invocation. **Documentation is silent on SIGTERM cancellation semantics.** This is a known unknown that needs empirical verification before Switchboard's M4 cancellation work can rely on it.

## Maturity and known risks

**Maturity:** v0.42 with weekly stable releases since mid-2025. Headless mode, output formats, MCP support are all documented stable. **API surface is settling but not yet 1.0** — the `--checkpointing` flag relocation in v0.11 is the kind of churn subprocess integrations have to absorb.

**Risks for Switchboard:**

1. **Pre-1.0 churn.** Subprocess integrations break when CLI flags move. Pin to a known-good version in M3; monitor release notes.
2. **SIGTERM behaviour undocumented.** Verify in M3 before relying on it for M4 cancel.
3. **Auth-mode detection complexity.** Four auth methods × per-method quota types adds branching to Switchboard's billing-awareness UI (M4 / M7 design decision).
4. **Maintenance burden of a third pricing table.** If Switchboard ever derives cost from tokens (open M4 design decision), Gemini adds a third per-model pricing table to keep current.

## Switchboard-implications summary

**Strengths:**

- **Cleanest billing model of the three.** No headless-vs-interactive split to explain. 1,000-req/day free OAuth tier removes the monetary barrier for users trying Switchboard.
- **Stream format maps 1:1 onto existing `AdapterEvent` vocabulary.** Less translation work than Codex.
- **Cheapest per-token rates at coding-grade tier.** Gemini 2.5 Pro at $1.25/$10 vs Sonnet at $3/$15.
- **Most of M2's abstraction work transfers directly.** CLI-owned session ID + sidecar pattern is identical to Codex.

**Weaknesses:**

- Pre-1.0 stability; expect flag churn.
- Undocumented SIGTERM behaviour.
- Adds a third per-vendor surface (pricing table, research notes, fixture maintenance).

**Slot in roadmap:** new M3 (was multi-agent UI, now M4). Reasoning: third concrete adapter forces the abstraction to be load-bearing before more complexity lands on top of it.

## Pending verification (M3 fixture capture)

The following are docs-derived and need hands-on confirmation before the adapter implementation can rely on them:

1. Exact event shapes for `init` / `message` / `tool_use` / `tool_result` / `error` / `result` on the stream-json output.
2. Session-file location and format under `~/.gemini/`.
3. SIGTERM behaviour (does the subprocess exit cleanly? does it propagate to child processes?).
4. Empty / whitespace-only prompt behaviour.
5. Behaviour when `--resume <tag>` references a non-existent tag (error code? silent fresh session? — parallel to Claude Code's `--resume <unknown-uuid>` rejection).
6. Auth-method detection from a Switchboard perspective (how does the adapter know which billing pool the user is on?).
7. Tool-use stream shape relative to Switchboard's M2.2 `ToolStarted` / `ToolCompleted` event vocabulary.
8. Concurrent invocation safety (parallel `gemini -p` from the same cwd — verified safe for Claude Code, presumed safe here).

## Sources

- <https://github.com/google-gemini/gemini-cli> — repo
- <https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/headless.md> — headless docs
- <https://geminicli.com/docs/resources/quota-and-pricing/> — quota and pricing
- <https://ai.google.dev/gemini-api/docs/pricing> — Gemini API pricing
- <https://geminicli.com/docs/cli/tutorials/session-management/> — sessions
- <https://geminicli.com/docs/cli/checkpointing/> — checkpointing

---

## Findings during M3.1 (2026-05-17)

Hands-on probing against a real `gemini` CLI (v0.42.0, OAuth-personal auth as `shane.kercheval@gmail.com`) to resolve the eight "Pending verification" items above. Findings are recorded **as they're observed**, mirroring the Codex `M2.1` / `M2.3` / `M2.4-prep` pattern. Each section calls out **CONFIRMED**, **CORRECTED**, or **NEW** vs. the pre-probe planning doc above.

### CORRECTED: a `--session-id <uuid>` flag DOES exist — caller-controlled session IDs are supported

Pre-probe planning said: *"Gemini CLI assigns its own session ID — there is no documented `--session-id <uuid>` flag for the caller to pre-generate the ID. This matches Codex's model and is the opposite of Claude Code's caller-controlled UUID."*

`gemini --help` (v0.42.0) shows:

```
--session-id  Start a new session with a manually provided UUID.  [string]
```

**Implication for Switchboard**: Gemini's session-ID model is **Claude-Code-shaped, not Codex-shaped.** The M3 adapter can pre-generate a UUID at agent registration and pass it via `--session-id`, mirroring `ClaudeCodeAdapter::build_args`. **The Codex-style per-agent sidecar pattern is NOT required for Gemini.** This is a meaningful simplification vs. the docs-derived plan.

### CORRECTED: `--resume` takes `"latest"` or an index, not a tag

Pre-probe planning said: *"`--resume <tag>` with a tag: resume a named checkpoint. Tags are managed via `/resume save <tag>` and `/resume resume <tag>` slash-commands."*

`gemini --help` shows:

```
-r, --resume        Resume a previous session. Use "latest" for most recent or index number (e.g. --resume 5)  [string]
--list-sessions     List available sessions for the current project and exit.  [boolean]
--delete-session    Delete a session by index number (use --list-sessions to see available sessions).  [string]
```

**Implication for Switchboard**: `--resume <index>` is **per-project ordinal** — fragile for an adapter (the index of a given session changes as new sessions are added). The right approach for Switchboard is **`--session-id <uuid>` on every dispatch** — first dispatch creates the session with that ID, subsequent dispatches re-pass the same ID. The same pattern as Claude Code (`--session-id` first turn, `--resume` subsequent turns) may not apply here — needs empirical confirmation that re-passing `--session-id` with the same UUID continues the existing session rather than erroring.

### NEW: full flag surface (v0.42.0)

Captured from `gemini --help` so future plan revisions can reference it without re-running probes. Notable for Switchboard:

| Flag | Purpose | Switchboard relevance |
|---|---|---|
| `-p, --prompt <text>` | Headless prompt. Triggers non-interactive mode. | M3.2 adapter spawn arg. |
| `-o, --output-format` | `text` / `json` / `stream-json` | `stream-json` for the adapter. |
| `--session-id <uuid>` | **Caller-controlled UUID** (see correction above). | Replaces sidecar requirement. |
| `-r, --resume` | `"latest"` or index. | Avoid; use `--session-id`. |
| `--list-sessions` | Enumerate sessions for the project. | Potentially useful for attach-existing-session flow. |
| `-y, --yolo` | Auto-approve all actions. | Permissions-skip equivalent of Claude's `--dangerously-skip-permissions`. |
| `--approval-mode` | `default` / `auto_edit` / `yolo` / `plan` | More granular than yolo. |
| `--skip-trust` | Trust the current workspace for this session. | May be required headlessly — verify. |
| `-m, --model <model>` | Model override. | Tested-via-probe candidate. |
| `--raw-output` | Disable model-output sanitisation. | Off by default; do not enable. |
| `--acp` | "ACP mode" (Agent Communication Protocol?). | Not relevant for M3 v1; flag it. |
| `--sandbox` | Run in sandbox. | Not relevant for M3 v1. |
| `-w, --worktree` | Spawn in a new git worktree. | Not relevant for M3 v1. |

The `mcp` / `extensions` / `skills` / `hooks` / `gemma` subcommands are out of scope for M3 dispatch.

### NEW: `~/.gemini/` layout — per-project partitioning by *project name*, not cwd encoding

Pre-probe planning said: *"Storage location: `~/.gemini/` (verify in M3 — exact subdirectory structure not yet captured). Not per-cwd encoded the way Claude Code stores `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`."*

Observed layout (with `~/.gemini/projects.json` mapping cwd → project name):

```
~/.gemini/
├── projects.json                  # { "projects": { "/abs/path/to/cwd": "<project-name>" } }
├── settings.json                  # { security.auth.selectedType: "oauth-personal", ... }
├── state.json                     # tipsShown, defaultBannerShownCount
├── oauth_creds.json               # OAuth tokens (file mode 0600)
├── google_accounts.json           # { active: "<email>", old: [] }
├── installation_id                # opaque ID file
├── trustedFolders.json            # workspace trust
├── history/<project-name>/        # currently only contains `.project_root`
└── tmp/<project-name>/
    ├── .project_root              # contains the absolute cwd path (39 bytes here)
    ├── logs.json                  # parallel telemetry log
    └── chats/
        └── session-<YYYY-MM-DDTHH-MM>-<8hex>.jsonl
```

For our test cwd (`/Users/shanekercheval/repos/switchboard`), project name was `switchboard` (cwd basename). The session-file path is therefore:

```
~/.gemini/tmp/<project-name>/chats/session-<startTime>-<sessionId-first-8-chars>.jsonl
```

**Implications for Switchboard**:

- Project-name comes from `projects.json`. The first headless dispatch in a new cwd presumably populates this mapping; Switchboard's transcript loader should consult `projects.json` to find the right `<project-name>` directory.
- **Session filename is NOT just `<session-id>.jsonl`** — it embeds the start time, with `:` replaced by `-`. Looking up a session file by ID alone requires a glob (`chats/session-*-<id-prefix>.jsonl`) similar to Codex's date-partition glob.
- The `<id-prefix>` in the filename is **the first 8 chars** of the session UUID (`7ead3891-d825-...` → `7ead3891`). Sufficient to disambiguate but not full-UUID.
- The `projectHash` field in line 1 of the session file (`e8bd54e8...`, 64 hex chars) appears to be sha256 of the cwd — second identifier for the same project, independent of the friendly `<project-name>` mapping. **Verify in next probe** whether this is sha256 of the absolute cwd path.

### NEW: session-file format is a JSONL stream of records *plus* MongoDB-style `$set` mutation records

Inspecting an existing interactive-session file (`session-2026-05-17T21-09-7ead3891.jsonl`, 8 lines):

```jsonl
{"sessionId":"7ead3891-...","projectHash":"e8bd54e8...","startTime":"2026-05-17T21:09:40.043Z","lastUpdated":"2026-05-17T21:09:40.043Z","kind":"main"}
{"id":"...","timestamp":"...","type":"user","content":[{"text":"Hello"}]}
{"$set":{"lastUpdated":"2026-05-17T21:10:16.774Z"}}
{"id":"...","timestamp":"...","type":"gemini","content":"","thoughts":[{"subject":"...","description":"...","timestamp":"..."}],"tokens":{"input":13118,"output":74,"cached":0,"thoughts":137,"tool":0,"total":13329},"model":"gemini-3-flash-preview"}
{"$set":{"lastUpdated":"..."}}
{"id":"<same-id-as-prior-gemini-record>","timestamp":"...","type":"gemini","content":"","thoughts":[...],"tokens":{...},"model":"...","toolCalls":[{...}]}
{"id":"...","timestamp":"...","type":"gemini","content":"Hello! I'm Gemini CLI...","thoughts":[],"tokens":{...},"model":"..."}
{"$set":{"lastUpdated":"..."}}
```

**Critical observations**:

- **Record types**: `{sessionId, projectHash, startTime, kind}` (line 1, session header); `{type:"user", content:[{text}]}` (user turn); `{type:"gemini", content, thoughts, tokens, model, toolCalls?}` (gemini turn); `{$set: {...}}` (mutation record). Unlike Claude / Codex, the format **mixes event records and mutation records**.
- **`$set` records mutate the header line 1** (`lastUpdated` field) — implies the on-disk format is a log-of-mutations, not a pure event stream. A naive parser that treats every line as an event would emit phantom turns; the parser must filter `$set` records.
- **Same Gemini turn record gets re-emitted as it accrues data.** Lines 4 and 6 share `id=77f8e81d-...`. Line 4 is the gemini turn pre-tool-call; line 6 is the same turn with `toolCalls` appended. Implies **last-write-wins by `id`** when reconstructing a turn from the file. Critical for the transcript hydrator.
- **Token telemetry is on every gemini record**: `{input, output, cached, thoughts, tool, total}`. Per-turn cost and usage live here. Maps naturally to `TurnUsage` plus a new `tool` and `thoughts` token bucket.
- **Model is on every gemini record**: `model:"gemini-3-flash-preview"`. (The default in v0.42.0 is `gemini-3-flash-preview`, not the `gemini-2.5-pro` referenced in pricing tables — Google ships fast-moving model defaults.)
- **`thoughts` are first-class records**: `thoughts:[{subject, description, timestamp}]` per gemini record. Maps cleanly onto Switchboard's reserved `ContentKind::Thinking` variant (M2 was right to reserve this — Gemini exercises it from day one).
- **`toolCalls` envelope is richer than Claude/Codex**: each tool has `{id, name, args, result:[{functionResponse:{...}}], status, resultDisplay (markdown), description, displayName, renderOutputAsMarkdown}`. The `result` is an array of `functionResponse` objects (Google-AI-SDK shape, not Anthropic's tool_result shape). `resultDisplay` is pre-rendered markdown — a UI affordance Switchboard doesn't use today but is worth preserving for future "raw output" view.
- **Auto-invoked builtin tools**: the interactive Gemini auto-fired an `update_topic` tool on the first turn (managing its own conversation-state metadata). Headless mode may behave differently — verify in next probe whether `update_topic` fires under `-p`. **If it does**, every Gemini turn will emit at least one ToolStarted/ToolCompleted pair even for trivial replies, which affects how the UI renders short messages.

### NEW: separate `logs.json` "telemetry" sidecar — distinct from the session file

`~/.gemini/tmp/<project-name>/logs.json` is a parallel JSON array of user-message events:

```json
[{ "sessionId": "...", "messageId": 0, "type": "user", "message": "Hello", "timestamp": "..." }]
```

**Not the source of truth** — the session file is. Switchboard's transcript loader should ignore `logs.json` unless we find a use for it (looks like analytics/telemetry, not state).

### CONFIRMED: auth-mode detection is straightforward from `~/.gemini/settings.json`

Pre-probe planning called auth-mode detection "complex" (4 methods × per-method quota types). Reality is simpler:

```json
{ "security": { "auth": { "selectedType": "oauth-personal" } } }
```

The `selectedType` field is one of `oauth-personal` / `gemini-api-key` / `vertex-ai` / `workspace`-equivalent. Switchboard's auth-probe equivalent for Gemini is `~/.gemini/settings.json` existence + `security.auth.selectedType` ∈ supported set. Cheap, no API calls.

### Switchboard-implications-update (revisions to the planning summary)

- **No Codex-style sidecar required.** `--session-id <uuid>` lets us follow the Claude-Code pattern: pre-generate a UUID at agent registration, pass on every dispatch. **Major simplification.**
- **Transcript loader must handle MongoDB-style `$set` mutation records** by filtering them out (or applying them to the in-memory header state — Switchboard only uses the header for `model` and `sessionId`, so filtering is sufficient).
- **Transcript loader must dedupe by `id`** within gemini-record stream — same record id can appear multiple times as data accrues; last-write wins.
- **Filename glob lookup**: `~/.gemini/tmp/<project-name>/chats/session-*-<first-8-of-session-id>.jsonl`. Need to consult `~/.gemini/projects.json` to resolve cwd → `<project-name>`.
- **Reserved `ContentKind::Thinking` will be exercised** for the first time across all three harnesses. M2's forward-compat reservation pays off.
- **Tool-event mapping** needs care: Gemini's `toolCalls[].result[].functionResponse` shape differs from both Anthropic and Codex; the adapter projects into the existing `ToolCompleted` envelope (`output: String`, `is_error: bool`) by extracting `response.output` and inferring `is_error` from `status != "success"`.
- **Token telemetry has a `thoughts` and `tool` token bucket** the existing `TurnUsage` doesn't carry. Decision for the M3 plan: add `thoughts_tokens: Option<u64>` and `tool_tokens: Option<u64>` as additive fields, or drop them. Both are useful for "what did this turn cost" UX but neither is load-bearing for v1.

### NEW: workspace-trust gate blocks headless mode by default — adapter must pass `--skip-trust`

First headless dispatch in a fresh cwd:

```
$ cd /tmp/gemini-probe-happy && gemini -p "..." --output-format stream-json --session-id <uuid> --yolo
[exit 0, empty stdout]
[stderr] Gemini CLI is not running in a trusted directory. To proceed, either use `--skip-trust`,
         set the `GEMINI_CLI_TRUST_WORKSPACE=true` environment variable, or trust this directory
         in interactive mode.
```

**Critical for Switchboard**:
- Exits **0 with empty stdout** and an error on stderr. A naïve "exit-code-only" failure detection misses this. The adapter must treat "empty stream + workspace-trust message on stderr" as a `Failed { AdapterFailure }` or possibly a new `Failed { ... }` variant, OR pre-empt it by passing `--skip-trust` on every spawn.
- **Recommendation**: pass `--skip-trust` unconditionally in `GeminiAdapter::build_args`. The user-bound cwd in Switchboard is by definition the user's own working directory; Switchboard's workspace-trust semantic is "the user owns this folder," which is what `--skip-trust` asserts. (The `GEMINI_CLI_TRUST_WORKSPACE=true` env var is equivalent but env-vars on subprocesses are harder to audit than CLI flags.)

### CONFIRMED: headless stream-json vocabulary (happy path)

Captured fixture: `crates/harness/tests/fixtures/gemini/happy-path.stream.jsonl` (4 lines).

```jsonl
{"type":"init","timestamp":"...","session_id":"<uuid>","model":"gemini-3-flash-preview"}
{"type":"message","timestamp":"...","role":"user","content":"<prompt>"}
{"type":"message","timestamp":"...","role":"assistant","content":"ack","delta":true}
{"type":"result","timestamp":"...","status":"success","stats":{...}}
```

Maps cleanly onto Switchboard's existing event vocabulary:

| Gemini stream event | Switchboard `AdapterEvent` |
|---|---|
| `init` | `SessionMeta` (carries `session_id` + `model`). |
| `message` role=user | **Ignore.** Echo of the prompt; Switchboard already has the prompt text. |
| `message` role=assistant (`delta:true`) | `ContentChunk { kind: Text, text: content }`. |
| `result` status=success | `TurnEnd { outcome: Completed, usage: <from stats> }`. |

**Notes**:
- `delta:true` is present on every assistant message in stream-json mode. Small replies fit in one chunk; longer replies presumably stream as multiple `message` records (verify with a longer prompt).
- **`stats` shape** in `result`: `{total_tokens, input_tokens, output_tokens, cached, input, duration_ms, tool_calls, models:{<model>:{...}}}`. `input` and `input_tokens` are duplicates (legacy alias?); `cached` appears both top-level and inside `models[model]`. Maps to `TurnUsage { input_tokens, output_tokens, cached_input_tokens: cached, ... }`. **No `context_window` in stats** — same gap as Codex; Switchboard can populate from the session file's per-turn record (which carries `tokens.total` but also no explicit window) or leave `None`.

### CONFIRMED: session-file format under headless mode

Captured fixture: `crates/harness/tests/fixtures/gemini/happy-path.session.jsonl`.

```jsonl
{"sessionId":"<uuid>","projectHash":"<sha256(realpath(cwd))>","startTime":"...","lastUpdated":"...","kind":"main"}
{"id":"<uuid>","timestamp":"...","type":"user","content":[{"text":"<prompt>"}]}
{"$set":{"lastUpdated":"..."}}
{"id":"<uuid>","timestamp":"...","type":"gemini","content":"<reply>","thoughts":[],"tokens":{"input":N,"output":N,"cached":0,"thoughts":0,"tool":0,"total":N},"model":"gemini-3-flash-preview"}
{"$set":{"lastUpdated":"..."}}
```

**Key confirmations**:
- **`projectHash` IS `sha256(realpath(cwd))`**. Verified: `sha256("/private/tmp/gemini-probe-happy") == ca746c1f8cbf...` matches the stored hash exactly. On macOS, `/tmp/X` resolves to `/private/tmp/X` first.
- **`thoughts:[]` in headless mode.** The interactive-session sample I inspected had populated `thoughts` arrays; headless yields empty. Whether headless can be coaxed into emitting thoughts (via a flag) is a future probe; for v1 the adapter need not emit `ContentChunk { kind: Thinking }` events from Gemini.
- **No `update_topic` auto-tool in headless.** The interactive sample's auto-fired `update_topic` does not appear here. Headless turns are clean.
- **`$set` mutation records present** and confirmed to be header mutations (just `lastUpdated`) — safely filterable by the transcript parser.

### NEW: `projects.json` populates automatically on first headless dispatch

After the probe, `~/.gemini/projects.json` gained `"/private/tmp/gemini-probe-happy": "gemini-probe-happy"` (the project name is the cwd basename). The transcript loader **must consult `projects.json` to resolve cwd → project name** before locating the chats directory.

**Switchboard implication**: if the user moves their working directory between sessions but the cwd is the same, `projects.json` keeps mapping it consistently. If they rename their working directory, a new entry is created — the old session files remain reachable only by their `<old-name>` in the loader. Acceptable for v1; surface in the M3 plan if it becomes a real problem.

### NEW: `--yolo` triggers an "Approval mode overridden" warning on stderr when paired with un-trusted cwd

```
[stderr] Approval mode overridden to "default" because the current folder is not trusted.
```

Resolved by adding `--skip-trust`. The two informational lines `"YOLO mode is enabled. All tool calls will be automatically approved."` are also harmless and duplicated. Switchboard should not parse stderr for these (the post-`--skip-trust` happy-path stderr still contains the YOLO-enabled messages).

### CONFIRMED + CORRECTED: tool-use stream vocabulary — and `tool_result.output` is elided for `read_file` (and likely other user-data tools)

Captured fixtures: `crates/harness/tests/fixtures/gemini/tool-use.stream.jsonl` (9 lines), `tool-use.session.jsonl` (8 records).

Stream events (in order):

```jsonl
{"type":"init","timestamp":"...","session_id":"...","model":"..."}
{"type":"message","role":"user","content":"..."}
{"type":"tool_use","timestamp":"...","tool_name":"update_topic","tool_id":"update_topic_1779056026792_0","parameters":{...}}
{"type":"tool_use","timestamp":"...","tool_name":"read_file","tool_id":"read_file_1779056026793_1","parameters":{"file_path":"MARKER.txt"}}
{"type":"tool_result","timestamp":"...","tool_id":"update_topic_...","status":"success","output":"## 📂 Topic: ..."}
{"type":"tool_result","timestamp":"...","tool_id":"read_file_...","status":"success","output":""}   ← EMPTY
{"type":"message","role":"assistant","content":"SWITCHBOARD_GEMINI_PROBE_TOOL_5F8A2","delta":true}
{"type":"message","role":"assistant","content":"1","delta":true}                                     ← multi-chunk
{"type":"result","status":"success","stats":{...,"tool_calls":2,...}}
```

Stream → Switchboard mapping (full):

| Gemini event | Switchboard `AdapterEvent` |
|---|---|
| `tool_use` | `ToolStarted { tool_use_id: tool_id, name: tool_name, input: parameters, kind: <see below> }`. |
| `tool_result` | `ToolCompleted { tool_use_id: tool_id, output, is_error: status != "success" }`. |
| `message` role=assistant `delta:true` | `ContentChunk` — multiple records per turn, accumulate. |

**The `read_file.output` stream gap — load-bearing for M3.2 design**:

- `update_topic` tool_result carries a rendered-markdown summary in `output` (~200 chars).
- `read_file` tool_result carries `output:""` — the actual file content is **not in the stream**.
- The session file's `toolCalls[].result[].functionResponse.response.output` carries the **full real output** ("SWITCHBOARD_GEMINI_PROBE_TOOL_5F8A21\n").

**Switchboard's `ToolCompleted.output: String` contract was implicitly "the actual tool output text."** Gemini's stream violates this for read-like tools — empty stream-side, populated session-file-side. Three architectural options for M3.2:

1. **Stream as the source of truth.** Accept that for Gemini, `ToolCompleted.output` is sometimes empty even on success. Live UI shows "tool ran, no output text"; transcript hydration from the session file is where users see the real output. The sentinel-driven live test pattern from M2.7 won't work here — the live tool-use test for Gemini must assert lifecycle only, not content.
2. **Post-stream enrichment from session file.** Codex already does post-terminal enrichment for `context_window` / `RateLimitEvent`. M3.2 could similarly read the session file after `result` and emit `ToolCompleted` events with the real `output` from disk. More complex, but preserves the existing wire-format semantic.
3. **Hybrid.** Emit `ToolCompleted` from the stream with whatever output it carries; consumers reading tool output for display already use the transcript hydrator path on project re-open. Live UI just shows what the stream gave.

**Recommendation for the M3 plan: option 1.** It's the simplest, matches Switchboard's "live = best-effort, hydration = authoritative" pattern (already used for Codex's `context_window`), and avoids growing the adapter surface. Document the asymmetry in the M3.2 adapter doc and update `crates/harness/tests/README.md`'s "intentionally not covered live" section to add: "Gemini tool-output content (stream elides it for read-like tools; covered by the session-file round-trip test instead)."

**Decision is M3.2-level — pin it in the M3 plan, do not relitigate during implementation.**

### NEW: `update_topic` IS a Gemini builtin auto-tool, fires in headless mode on non-trivial prompts

The happy-path probe (single-word "ack" reply) did **not** trigger `update_topic`. The tool-use probe (file-read with a multi-sentence intent) **did**. Pattern: Gemini self-manages a "current topic" context using the `update_topic` builtin tool, and fires it whenever the model judges a turn substantive enough to warrant updating its internal context.

**Switchboard implication**:
- **Every non-trivial Gemini turn emits at least one `ToolStarted`/`ToolCompleted` pair** for `update_topic`. The UI's "tool calls" affordance will show `update_topic` constantly across Gemini's transcript.
- **Options for M3.2**:
  - (a) Surface `update_topic` as-is — users see "Gemini did Update Topic Context" inline.
  - (b) Filter it in the adapter — never emit `ToolStarted`/`ToolCompleted` for `update_topic`.
  - (c) Tag it as a synthetic "internal" tool kind (new `ToolKind::Internal` variant?) so the UI can hide-by-default but reveal in a "show internal" mode.
- **Recommendation for the M3 plan: (b) for v1**, with (c) deferred to v2 if user feedback says they want visibility. Rationale: `update_topic` carries no information the user needs (the model's own conversation-state metadata, not project state), and showing it on every turn pollutes the unified transcript.
- **Decision is M3.2-level.**

### NEW: tool-use envelope shape (session-file form is richer than stream form)

Session-file `toolCalls[]` entry:

```jsonc
{
  "id": "<tool_id>",
  "name": "<tool_name>",
  "args": {...},
  "result": [{"functionResponse": {"id": "<tool_id>", "name": "<tool_name>", "response": {"output": "<real-output>"}}}],
  "status": "success" | "<failure>",
  "timestamp": "...",
  "resultDisplay": "<rendered-markdown>",   // pre-rendered for UI; not in stream
  "description": "<one-line>",
  "displayName": "<friendly-name>",
  "renderOutputAsMarkdown": true|false
}
```

Switchboard's transcript hydrator must extract `result[0].functionResponse.response.output` for the `output` field of `TurnItem::Tool`. The `resultDisplay` / `description` / `displayName` / `renderOutputAsMarkdown` fields are pre-rendered UI metadata Switchboard doesn't currently use (we render our own UI); for M3 v1 we ignore them, surface in a follow-up if users want richer tool-call rendering.

### NEW: gemini-turn records are split when tools are involved — one record per "round"

A user-perceived turn (one user prompt → one assistant reply with tools) appears in the session file as:
- 1 user record.
- 1 gemini record with `content:""` and `toolCalls:[...]` (the tool-fetching round).
- 1 gemini record with `content:"<final reply>"` (the user-facing reply round).

These are TWO gemini records with **different `id`s**. The transcript hydrator's "Turn" must aggregate user record + all subsequent gemini records until the next user record, interleaving text and tool items per timestamp.

**Note on the M2.6 `TurnItem` model**: the existing interleaved `items: Vec<TurnItem>` shape already handles this correctly — concatenate every gemini-record's `content` as a `TurnItem::Text` and emit each tool call as a `TurnItem::Tool`, all ordered by timestamp. No new types needed.

### NEW: assistant content streams as multiple chunks when output is non-trivial

The tool-use probe's reply ("SWITCHBOARD_GEMINI_PROBE_TOOL_5F8A21") arrived in **two** `message` records:

```jsonl
{"type":"message","role":"assistant","content":"SWITCHBOARD_GEMINI_PROBE_TOOL_5F8A2","delta":true}
{"type":"message","role":"assistant","content":"1","delta":true}
```

Confirms `delta:true` records are streaming chunks (not full-message-each). `ContentChunk` events accumulate in the consumer.

### CONFIRMED: resume semantics — `--session-id <uuid>` first turn, `--resume <uuid>` subsequent turns

Three probes pinned this down:

| Command | Outcome |
|---|---|
| `gemini -p ... --session-id <existing-uuid>` | **Exit 42**: `Error starting session: Session ID "<uuid>" already exists. Use --resume to resume it, or provide a different ID.` |
| `gemini -p ... --resume <existing-uuid>` | **Exit 0**: session continues from prior context (verified: Gemini correctly recalled the prior user message). |
| `gemini -p ... --resume latest` | **Exit 0**: resumes the most recent session in this cwd. |
| `gemini -p ... --resume <nonexistent-uuid>` | **Exit 42**: `Error resuming session: Invalid session identifier "<uuid>". Searched for sessions in /Users/<user>/.gemini/tmp/<project>/chats. Use --list-sessions to see available sessions, then use --resume {number}, --resume {uuid}, or --resume latest.` |

**The help text is misleading**: `--help` says "Use 'latest' for most recent or index number (e.g. --resume 5)" — but `--resume <uuid>` is also valid (and required for Switchboard's per-agent model). Confirmed by both the success case and by the error message itself, which explicitly lists `--resume {uuid}` as a valid form.

**Switchboard adapter pattern (mirrors Claude Code's, not Codex's)**:
- Pre-generate a session UUID at agent registration.
- First dispatch: `--session-id <uuid>`.
- Subsequent dispatches: `--resume <uuid>`.
- Detect first-vs-subsequent by **checking session-file existence** at the expected path — same pattern as `ClaudeCodeAdapter::build_args`. No Codex-style sidecar required.

### CONFIRMED: error-path stream vocabulary lives inside `result`, not a separate event

Pre-probe docs claimed an `error` event type. **It does not exist as a separate stream event.** Instead, failures flow as `result` with `status:"error"` and an `error` sub-object:

```jsonl
{"type":"init","session_id":"...","model":"does-not-exist-model"}
{"type":"message","role":"user","content":"say hi"}
{"type":"result","status":"error","error":{"type":"unknown","message":"[API Error: Requested entity was not found.]"},"stats":{...zero...}}
```

Captured fixture: `crates/harness/tests/fixtures/gemini/error-invalid-model.stream.jsonl` (3 lines + stderr).

**Three distinct exit codes observed**:

| Exit code | Meaning | Stream output |
|---|---|---|
| 0 | Success | Full happy-path with `result.status:"success"`. |
| 1 | Runtime/API error (e.g., invalid model, network) | Partial stream ending in `result.status:"error"`. Stack trace also dumped to stderr (do not parse). |
| 42 | Input error (empty prompt, existing session-id, unknown resume target) | **Empty stdout**. Useful error message on stderr. |

**Adapter mapping (M3.2 design)**:
- `result.status:"error"` → `TurnEnd { outcome: Failed { kind: HarnessError, message: error.message } }`. The `error.type` field can be passed through verbatim into a future `FailureKind::HarnessError`-with-classification variant, but for v1 the unstructured `message` is sufficient.
- **Exit 42 with empty stream** → `TurnEnd { outcome: Failed { kind: AdapterFailure, message: <captured stderr> } }`. The adapter should pre-validate empty prompts before spawning to avoid burning a request, but must still handle the case if a user-provided prompt is empty whitespace.
- **Exit 0 with no `result` event** → `TurnEnd { outcome: Failed { kind: AdapterFailure, message: "subprocess exited without terminal event" } }`. This is the SIGTERM/cancellation case (see below).

**Note on Codex parallel**: the pre-existing `FailureKind::AuthFailure` variant was added in M2.3 for both Claude (top-level `error:"authentication_failed"`) and Codex (stream `turn.failed.error.message` containing `"401 Unauthorized"`). Gemini's auth-failure shape was **not** probed in M3.1 (would require breaking the user's auth, risky), but the `result.status:"error"` shape suggests auth failures will surface there with a recognizable `error.message` substring — verify in M3.2 implementation.

### CONFIRMED: empty / whitespace prompt behaviour

| Prompt | Exit | Stream |
|---|---|---|
| `""` (empty) | 42 | 0 stream lines, no stderr-explicit message |
| `"   "` (whitespace) | 0 | Full happy-path; Gemini treats it as "no specific intent" and replies conversationally. |

**Switchboard implication**: pre-validate empty (post-strip if desired) at the adapter level. Whitespace-only is treated as a valid prompt by Gemini.

### CONFIRMED: SIGTERM behaviour — process-group kill is clean; exit code is 0 (indistinguishable from success)

Probe used Python's `subprocess.Popen(start_new_session=True)` (equivalent to Switchboard's `Command::process_group(0)`) and `os.killpg(pgid, SIGTERM)`:

- **Bare-PID SIGTERM does NOT propagate.** Sending `kill -TERM <node-pid>` to the gemini parent node process is *ignored* — the process completes the full turn anyway. Confirmed: a 60-second 100-number counting turn ran to completion after SIGTERM.
- **Process-group SIGTERM works cleanly.** With `start_new_session=True` + `killpg(pgid, SIGTERM)`, gemini terminates within milliseconds, **exit code 0**, no orphan processes. (Switchboard's existing M2 adapter pattern of spawning into a new process group with `Command::process_group(0)` then `killpg` is exactly what's needed here.)
- **Exit code 0 means cancellation is indistinguishable from success by exit code alone.** The adapter must check for the presence of a terminal `result` event in the stream. Absence of `result` + EOF → synthesize `TurnEnd { outcome: Failed { kind: AdapterFailure, message: "cancelled (no terminal event before EOF)" } }`. This matches the existing M2 contract for "subprocess died without a terminal event."

**Bonus finding**: Gemini's `invoke_agent` builtin tool can dispatch internal sub-agents that take a long time to complete (~47s for a "count to 100" sub-agent). For Switchboard's M4 per-turn timeout work, this means a single Gemini "turn" can legitimately exceed 60s of wall clock even on a simple-looking prompt. Generous defaults required.

### CONFIRMED: `invoke_agent` subagents are OPAQUE in the stream — no mis-attribution (2026-05-24, gemini 0.42.0)

Follow-up to the bonus finding above, probing a **tool-using** subagent (the earlier note used a "count to 100" subagent that ran no tools). Prompt dispatched a `generalist` subagent instructed to run `echo hello-from-subagent` and report; invocation `gemini -p … --output-format stream-json --yolo --skip-trust`. Stream:

```
tool_use     tool_name=update_topic     (Gemini's planning/UI tool — unrelated)
tool_use     tool_name=invoke_agent     parameters={prompt, agent_name:"generalist"}
tool_result  tool_id=update_topic…      status=success
tool_result  tool_id=invoke_agent…      status=success            (no `output` field)
message      role=assistant             "done"
result
```

**The subagent's internal `run_shell_command` does NOT appear as a stream tool event** — there is no nested/parent-tagged tool call, and `run_shell_command`/`run_command` appear nowhere in the stream. The subagent runs fully opaquely: the stream shows only the `invoke_agent` `tool_use`/`tool_result` pair, and the parent agent's final `message`. (The subagent's reported output didn't even surface in a top-level message here — the assistant just said "done".)

**Implication:** Gemini does **not** have Claude's subagent mis-attribution gap (see [`claude-code-cli-observed.md` §"Subagent (`Agent` tool) representation"](claude-code-cli-observed.md) and [`../implementation_plans/2026-05-24-subagent-rendering-fidelity.md`](../implementation_plans/2026-05-24-subagent-rendering-fidelity.md)). Our adapter maps `invoke_agent` → one `ToolStarted`/`ToolCompleted` pair and the subagent internals never reach the parser. **No Gemini parser change needed** — `invoke_agent`-as-one-tool-call is already the target shape the Claude fix aims for. (Caveat: the probe hit free-tier quota — 3 "exhausted your capacity" retries — but still completed with valid structure; quota affected timing, not the event shapes.)

### CRITICAL — NEW: concurrent dispatches in the same cwd can corrupt the on-disk session file if session-id prefixes collide

**This is the most important M3 finding** and a real abstraction-load-bearing surprise. Two concurrent `gemini -p` invocations from the same cwd with session IDs that happen to share their first 8 hex characters write to the **same** session file (`session-<startTime>-<prefix>.jsonl`), interleaving each other's records.

Captured fixture: `crates/harness/tests/fixtures/gemini/interleaved-collision.session.jsonl`. Both probes used session IDs `00000000-...-009` and `00000000-...-00A` (prefix `00000000` collision). The resulting file has **two** header records, two user records, and two gemini records all interleaved chronologically. **Live correctness was unaffected** — in-memory stream events were correct for each subprocess, and the `result` events arrived correctly tagged to their respective `session_id`s. **Hydration from disk is broken** — a naïve parser would see two transcripts mashed together.

**Why this matters for Switchboard's UUID v7 scheme**:

Switchboard mints `AgentId`s and turn IDs using **UUID v7** (time-ordered). UUID v7's first 48 bits are the unix timestamp in milliseconds — meaning **any two UUID v7s minted in the same millisecond share their first 8 hex chars exactly.** This is not a hypothetical: in workflow / fan-out scenarios (M6), two agents dispatched in parallel within the same millisecond would collide deterministically.

Gemini takes the first 8 chars of the session UUID to form the session-file name. This is a Gemini implementation choice we can't change. The collision would corrupt Switchboard's transcript hydration in any project with concurrent Gemini dispatches.

**Three mitigation options for M3.2**:

1. **Use UUID v4 for Gemini session IDs.** UUID v4 is 122 bits of random; first-8-char collision probability is ~1/2^32. Random across 32 bits is essentially never. Cleanest fix; localized to the Gemini adapter; rest of Switchboard keeps using UUID v7.
2. **Use UUID v7 with a randomized 4-byte prefix override.** Generate UUID v7, then XOR the first 4 bytes with random — keeps the time-ordering for storage but randomizes the first 8 chars for filename purposes. Complex.
3. **Robust hydrator**: scan all session files in the cwd's chats directory, parse each one's header(s), and match by full `sessionId` rather than by filename. Handles collision gracefully but slow (`O(files in cwd's chat dir)` per load).

**Recommended for M3.2**: **option 1** (UUID v4 for Gemini session IDs only). Single localized adapter-level choice; no impact on AgentId / TurnId / ProjectId semantics elsewhere. Document the deviation in the Gemini adapter's module docstring.

Note: this is the kind of finding that should have been a milestone-level review point per the v1 plan's "if the Gemini adapter requires modifying `AdapterEvent` / `NormalizedEvent` / `HarnessAdapter` beyond purely additive variants, that's a signal." It does **not** require modifying any of those — the change is internal to the adapter's session-id generation. So M2's abstraction holds; **this is a Gemini-implementation-detail issue, not an abstraction-leak issue.** Still worth surfacing prominently in the M3 plan.

### CONFIRMED: `update_topic` builtin fires in headless mode on non-trivial prompts (CORRECTED from earlier hypothesis)

An earlier note in this appendix (after the happy-path probe) tentatively said `update_topic` doesn't fire in headless. **That was wrong.** The tool-use probe with a more substantive prompt **did** trigger it. The trivially-simple happy-path probe (1-word reply) didn't trigger it. Pattern: Gemini decides per-turn whether to update its internal topic context, and headless mode does fire `update_topic` for non-trivial turns. M3.2 should filter `update_topic` from the emitted ToolStarted/ToolCompleted events for v1.

### CONFIRMED: thoughts appear opportunistically in the session file, never in the stream

The resume probe (asking Gemini to recall a prior message) produced a session-file gemini record with `thoughts:[{subject, description, timestamp}]`. The happy-path and tool-use probes had `thoughts:[]`. **Gemini decides per-turn whether to emit thoughts.** The stream-json output **never carries thoughts** under any of the three probes — even when the session file has them. The transcript hydrator can surface thoughts via `ContentKind::Thinking` items; the live stream never emits them.

### Pending items not yet probed (deferred to M3.2 implementation)

These remain unverified and should be confirmed during M3.2:

1. **Auth-failure shape.** Requires breaking the user's auth (risky during M3.1); deferred. Expected to surface in `result.status:"error"` with a recognizable `error.message` — verify when implementing the auth-failure stream pattern.
2. **`--list-sessions` output format.** Useful for a future attach-existing-session UX; not v1 critical.
3. **Multi-chunk streaming under genuinely long replies.** The SIGTERM probe surfaced ~100 `message` records with `delta:true`, confirming streaming works at scale. Further verification not needed.
4. **MCP tool events from a real MCP server.** Switchboard does not maintain its own MCP infrastructure; once a user has a real MCP server wired into Gemini, M3.2's MCP-tool path can be verified live. For v1 we project `mcp_*`-prefixed tools through the existing `ToolKind::Mcp` path; fixture-cover with inline JSON if M3.5 needs it.
5. **Workspace trust + `--skip-trust` long-term.** Whether `--skip-trust` is stable across Gemini CLI versions or will be deprecated is unknown. Track upstream release notes.

### Summary of M3.1 corrections to the planning doc

| Planning-doc claim | M3.1 reality |
|---|---|
| "No `--session-id` flag; CLI assigns its own session ID" | **Wrong.** `--session-id <uuid>` exists; caller-controlled. |
| "Sidecar pattern (Codex-style) required for AgentId ↔ session-id mapping" | **Wrong.** Claude-Code pattern (`--session-id` first, `--resume <uuid>` after) is supported. **No sidecar needed.** |
| "`--resume <tag>` — tags managed via slash commands" | **Wrong.** `--resume` takes `latest`, an index, or a UUID. No tag system in headless. |
| "Stream events: `init` / `message` / `tool_use` / `tool_result` / `error` / `result`" | **Mostly right** — but `error` is not a separate event type; failures live in `result.status:"error"`. |
| "Tool-result `output` carries the tool's real output" | **Wrong for read-like tools.** Stream's `output` is empty for `read_file`; full output lives only in the session file. |
| "Session storage at `~/.gemini/` (structure not yet captured)" | Path layout fully captured. Per-project partitioning by friendly project name (cwd basename); `projects.json` maps cwd → name. |
| "Auth-mode detection complexity" | **Simpler than expected**: `~/.gemini/settings.json` carries `security.auth.selectedType` as a single string. |
| "Switchboard's UUID v7 scheme is harness-neutral" | **Wrong for Gemini.** UUID v7 first-8-char collisions in the same millisecond corrupt Gemini's session file. Use UUID v4 for Gemini session IDs.

### Switchboard implications (revised, post-M3.1)

**Strengths confirmed**:
- Stream-json vocabulary maps cleanly onto existing `AdapterEvent` types — **no new wire-format variants required**. M2's abstraction holds.
- Claude-Code-pattern session continuity (`--session-id` / `--resume`) is supported, so **no sidecar required**.
- Auth-mode detection from a file is simple.
- SIGTERM via process-group kill works cleanly, matching the existing M2 adapter spawn pattern.
- Reserved `ContentKind::Thinking` is ready for Gemini's thoughts (when surfaced).

**Real costs / gotchas**:
- **`tool_result.output` is empty for read-like tools in the stream.** Live tool-output content is unavailable for some tool kinds; transcript hydration from the session file is authoritative.
- **`update_topic` auto-fires on most turns.** Filter at the adapter level for v1.
- **Workspace-trust gate.** Adapter must pass `--skip-trust` on every spawn.
- **UUID v7 session-ID prefix collisions.** Use UUID v4 for Gemini session IDs (single-adapter localized choice).
- **`$set` mutation records in session file.** Parser must filter these.
- **Gemini-turn records are split into multiple records when tools are involved.** Hydrator aggregates by user→gemini boundaries; dedupes gemini records by `id` (last-wins).

**Captured fixtures** (`crates/harness/tests/fixtures/gemini/`):
- `happy-path.stream.jsonl` + `happy-path.session.jsonl` — 1-word reply, no tools.
- `tool-use.stream.jsonl` + `tool-use.session.jsonl` — `update_topic` + `read_file` tool calls.
- `resume.stream.jsonl` — `--resume <uuid>` continuation.
- `error-invalid-model.stream.jsonl` + `error-invalid-model.stderr.txt` — `result.status:"error"` shape.
- `error-unknown-resume.stderr.txt` — exit-42 stderr-only error.
- `interleaved-collision.session.jsonl` — two-session interleave demonstrating the prefix-collision hazard.

API budget used: ~7 small turns + 2 zero-API probes (workspace-trust rejection, empty-prompt rejection). Roughly 1% of the 1,000-req/day free OAuth tier. Comfortable headroom remains for M3.2 implementation probes.

## Cancellation / SIGTERM (M4.3) — wired and verified end-to-end

M4.3 wires token-driven cancellation: the adapter's producer `select!`s the
stdout read against the turn's `CancellationToken`; on cancel it kills the
subprocess **process group** via `terminate_then_kill` (SIGTERM → ~2s grace →
SIGKILL — `crates/harness/src/subprocess.rs`) and ends the stream without a
terminal event (the dispatcher synthesizes `TurnEnd { Cancelled }`).

This composes with the already-confirmed SIGTERM behaviour above
("process-group kill is clean; exit code is 0"): a process-group SIGTERM tears
`gemini` down within milliseconds, so the grace window is rarely reached.

**Verified end-to-end (2026-05-23):** `live_gemini_cancel_terminates_and_synthesizes_cancelled`
(run via `make test-live` / `make test-live-gemini`) passes — firing the token
mid-turn produces a `Cancelled` outcome with the agent returning to idle.
