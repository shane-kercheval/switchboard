# Agent card: usage limits, environment, and context breakdown

Three things the Claude Code stream already delivers — or will deliver on request — that the agent
card does not show, found by probing Claude Code **2.1.274** on 2026-09-17:

1. **Usage limits.** `rate_limit_event` carries every window the desktop app shows (5-hour, weekly,
   and a per-model weekly), each with a used fraction, on every turn. The Claude card shows a reset
   clock and an overage flag and no numbers; the Codex card shows "5-hour used: 42%" as bare text.
2. **Environment inventory.** `system/init` names every loaded MCP server *with its connection
   status*, every custom agent, plugin, memory file, tool, and slash command. The card shows two
   counts.
3. **Context breakdown.** `/context` now works in `-p` mode — it returns, for free and without a
   model call, the same report as the desktop's context popover: tokens by category (messages, system
   tools, MCP tools, skills, agents, memory files, autocompact buffer, free space) and per item. Our
   docs say it returns nothing; that was true once.

Codex **0.154.0** was probed the same way (below). It is the mirror image: its live stream is nearly
empty and everything useful is in the session file we already read — where we parse three records
and discard the rest. Nothing is hiding somewhere we do not look.

This plan renders all three on the card, for both harnesses where the harness supplies the data —
every limit as the same meter the context bar already is, the environment as an expandable list, and
the breakdown as a panel opened from the context meter — and corrects the docs. **The UI below is a best guess to iterate on in the app, not a spec to
defend.** Get the data on screen in the described shape; adjust the shape after.

## What the stream carries (verified)

None of this is in Anthropic's public docs, and none of it is in OpenAI's. **Every milestone that
reads a field discovered by probe carries a live test against the real CLI** — M2 the usage windows,
M3 the Claude `init` inventory and the Codex `world_state` extraction, M4 the context report. A
fixture test keeps passing against a recorded shape after the CLI has moved; only a live test
notices. Per `AGENTS.md` → "Live testing against real harnesses".

### Usage windows

A Fable turn emits **two** `rate_limit_event`s. The first:

```json
{
  "status": "allowed", "resetsAt": 1789692600, "rateLimitType": "five_hour",
  "overageStatus": "rejected", "overageDisabledReason": "org_level_disabled", "isUsingOverage": false,
  "unifiedWindows": {
    "five_hour": { "utilization": 0.33, "resetsAt": 1789692600 },
    "seven_day": { "utilization": 0.52, "resetsAt": 1789916400 }
  }
}
```

The second, because the per-model window was past its warning threshold:

```json
{
  "status": "allowed_warning", "resetsAt": 1789916400, "rateLimitType": "seven_day_overage_included",
  "utilization": 0.79, "surpassedThreshold": 0.75, "isUsingOverage": false,
  "unifiedWindows": {
    "five_hour": { "utilization": 0.33, "resetsAt": 1789692600 },
    "seven_day": { "utilization": 0.52, "resetsAt": 1789916400 },
    "seven_day_overage_included": { "utilization": 0.79, "resetsAt": 1789916400 }
  }
}
```

A Sonnet turn emits one event with only `five_hour` and `seven_day`. The desktop's "Weekly · all
models 52%" and "Weekly · Fable 79%" are `seven_day` and `seven_day_overage_included`; the numbers
match exactly. `utilization` is a 0–1 fraction of the window **used**.

The CLI binary's own key list (`strings`): `five_hour`, `seven_day`, `seven_day_overage_included`,
`seven_day_opus`, `seven_day_sonnet`, `seven_day_cowork`, `seven_day_omelette`,
`seven_day_oauth_apps`. `seven_day_overage_included` is the weekly cap for a server-side allowlist of
models (`tengu_usage_overage_included_models`); the stream never names the models, and the window is
present only on turns run on one of them.

### `system/init`

Keys: `agents`, `capabilities`, `claude_code_version`, `cwd`, `mcp_servers`, `memory_paths`,
`model`, `output_style`, `permissionMode`, `plugins`, `session_id`, `skills`, `slash_commands`,
`tools`, plus fast-mode and telemetry flags. Shapes that matter:

- `mcp_servers`: `[{ "name", "status", "source" }]` — status observed `connected` and `needs-auth`;
  source observed `user` and `claudeai`.
- `agents`: `["claude", "Explore", "general-purpose", "Jenny", "Plan", "statusline-setup"]` — names.
- `plugins`: `[{ "name", "path", "source", "version" }]`.
- `memory_paths`: `{ "auto": "<dir>" }` — a map, not a list; the concrete memory *files* are only
  named by `/context`.
- `tools` (109 names), `slash_commands` (98 names), `skills` (30 names).

The adapter already parses `tools`, `mcp_servers` (name + status), and `skills` into `SessionMeta`
and also carries the whole object as `raw`; the frontend reducer drops `raw`.

### `/context`

`claude -p "/context" --output-format json` (with `--resume <session-id>` for an existing
conversation) returns the report as `result` text: `total_cost_usd: 0`, `duration_api_ms: 0`, no
`modelUsage` — a local command, ~0.5s. Markdown, in this order:

```
## Context Usage
**Model:** claude-fable-5-1
**Tokens:** 34.7k / 1m (3%)
### Estimated usage by category
| Category | Tokens | Percentage |        ← System prompt, System tools, MCP tools, MCP tools (deferred),
                                            System tools (deferred), Custom agents, Memory files, Skills,
                                            Messages, Free space, Autocompact buffer
### MCP Tools
| Tool | Server | Tokens |
### Custom Agents
| Agent Type | Source | Tokens |
### Memory Files
| Type | Path | Tokens |
### Skills
| Skill | Source | Tokens |               ← token cells here are approximate: "~30", "< 20"
```

Token cells are human-formatted: `366`, `4k`, `1.9k`, `952.1k`, `1m`, `~30`, `< 20`.

**The report is also emitted as structured JSON, live and on disk** (re-probed with Switchboard's
exact argv — `--output-format stream-json --include-partial-messages --verbose
--dangerously-skip-permissions --add-dir /`). The markdown is a rendering of this object, and the
object is exact where the text is approximate (the `~30` skill rows are `31` and `16`):

- **Live**: the synthetic `assistant` event (`message.model: "<synthetic>"`) carries
  `local_command_run: {"command": "context", "args": ""}`, `local_command_source` (the markdown),
  and **`context_usage`**: `{ model, total_tokens, raw_max_tokens, percentage, categories:
  [{name, tokens, kind: "used" | "deferred" | "buffer" | "free"}], mcp_tools: [{name, server_name,
  tokens}], memory_files: [{path, type, tokens}], agents: [{agent_type, source, tokens}], skills:
  [{name, source, tokens}] }`. The `result` carries `local_command: "context"` and the markdown as
  `result`; `modelUsage` is `{}`, so no context-window snapshot is derived from a report turn.
- **On disk**, three records per call: an `isMeta` `<local-command-caveat>` `user` record, a
  `<command-name>/context</command-name>` `user` record (both `entrypoint: "sdk-cli"`), then a
  `system/local_command` record — `entrypoint: "sdk-cli"`, `parentUuid` = the command record's
  uuid, `content` = the markdown inside `<local-command-stdout>`, **`commandRun: {"command":
  "context", "args": ""}`**, and **`contextUsage`** (same object, camelCase key).

**What the disk parser does with them today: drops them.** `handle_system` in
`claude_code/session_file.rs` pairs a local-command *input* to its output only when the input is
itself a `system/local_command` record with a leading `/`; a `<command-name>` `user` record is
housekeeping and never opens a pending command, so the output record hits the "orphaned sdk-cli
local-command output" warning and is discarded. (An earlier draft of this plan said the pair became
a completed agent turn; it does not.)

**Regenerate the M4 fixtures rather than hunting for the originals** — the probe is free (no model
call) and takes about a minute, and a fixture recorded against the CLI version you build on is worth
more than one recorded today:

```sh
mkdir -p /tmp/ctxprobe && cd /tmp/ctxprobe
sid=$(python3 -c "import uuid;print(uuid.uuid4())")
claude -p --output-format stream-json --include-partial-messages --verbose \
  --dangerously-skip-permissions --add-dir / --session-id "$sid" -- "/context" > stream.jsonl
# session file: ~/.claude/projects/-tmp-ctxprobe/$sid.jsonl
```

Record both, truncating the MCP-tool list to a handful of entries across two servers (the real one
is 80 rows) and keeping one `~N` and one `< N` skill row so the markdown fallback path has something
approximate to parse. Note the CLI version in the fixture's header comment.

### Codex (0.154.0)

**The `--json` stream** carries `thread.started`, `turn.started`, `item.*`, and `turn.completed`
with token usage. No model, no rate limits, no tools, no environment. Everything below is in the
rollout (`~/.codex/sessions/…/rollout-*.jsonl`), which `codex/session_file.rs` already reads
post-terminal (class B — durable, no sidecar needed).

- `session_meta`: `cli_version`, `originator`, `model_provider`, `history_mode`, and
  `base_instructions` — the full 21 KB system prompt.
- `turn_context` (we read `model`, `effort`, `cwd`): also `approval_policy`, `sandbox_policy`,
  `permission_profile`, `personality`, `summary`, `collaboration_mode`, `multi_agent_version`.
- `world_state.state` (not read at all): `host_skills` — a markdown block listing skill roots and
  every skill with name, description, and path; `environments` — cwd, shell, status, timezone, and
  the filesystem permission profile; `permissions.approved_command_prefixes` — the user's approved
  command allowlist (4.7 KB on the probe account); `personality`; `collaboration_mode`;
  `multi_agent_mode`.
- `event_msg/task_started.model_context_window` and `token_usage_record` with per-turn and
  cumulative thread usage.
- `event_msg/token_count.rate_limits` (we read `primary` / `secondary`):

  ```json
  { "limit_id": "codex", "limit_name": null,
    "primary": { "used_percent": 67.0, "window_minutes": 10080, "resets_at": 1789845487 },
    "secondary": null,
    "credits": { "has_credits": false, "unlimited": false, "balance": "0" },
    "individual_limit": null, "spend_control_reached": null,
    "plan_type": "prolite", "rate_limit_reached_type": null }
  ```

  On this plan `secondary` is null and `primary` is the **weekly** window. G8's "primary ~5-hour +
  secondary weekly" describes one plan's shape, not the contract; the code already labels from
  `window_minutes`, so only the doc is wrong. From the binary: `rate_limit_reached_type` takes
  `allowed` / `limit_reached` / `primary_window`, and `spend_control_reached` is a
  `SpendControlLimitDetails` — the "you are blocked" signals, as distinct from "how full". Neither
  was observed populated. The binary's `RateLimitWindowSnapshot` also carries `remaining` /
  `remaining_percent` fields never seen in output; decision 1's used-standard covers them if they
  appear.

**Codex has no tool or MCP inventory anywhere** — not in the stream, not in the rollout. The only
way to learn the tool list is to ask the model (costs tokens, unverifiable). MCP server names come
from `config.toml` via our loader with status `"configured"`; `codex mcp list` reports real
status and auth per server but is a separate subprocess, not part of a turn.

**Codex has no `/context`.** `codex exec "/status"`, `"/context"`, and `"/usage"` are all answered
by the *model*, in fluent prose, from inference — 58 output tokens for a fabricated status report.
The §3.9 fake-success hazard, confirmed for three more commands.

## Decisions

Settled here; the rationale must survive into code comments where marked.

**Meters and limits**

1. **One meter primitive, used by every gauge on the card.** Context-after-last-turn, each Claude
   window, each Codex window, and every category row in the breakdown panel render through one
   `ui/` component: label left; detail and percentage right; a track-and-fill bar beneath. Built
   first (M1); everything after is a call site. The value shown is **used** — the desktop, the
   screenshot, and Codex all agree — and **"used" is the standard across harnesses**: a source that
   reports remaining (neither wired harness does; Antigravity's unbuilt `/usage` payload does,
   harness-behavior §5.1) is converted at the derivation boundary, never at the primitive.
   *(Comment on the primitive's `value` prop.)*
2. **No Rust change to the limits data path.** The payload is opaque end to end and the metadata
   sidecar persists it verbatim, so `unifiedWindows` already arrives and survives restart. The Rust
   addition for limits is a live test. If a data-path change turns out to be needed, this premise
   was wrong — stop and say so.
3. **`unifiedWindows` is authoritative when present; the top-level `resetsAt` / `rateLimitType`
   pair is the fallback only.** When absent — an older CLI, or a future one that drops an
   undocumented field — the cell degrades to a single meter for the top-level window with no
   percentage. The fallback exists because the field is undocumented; it is not dead code.
   *(Comment on the fallback branch.)*
4. **Known window keys render; unknown keys are dropped.** In order: `five_hour` → "5-hour limit";
   `seven_day` → "Weekly · all models"; `seven_day_overage_included` → "Weekly · <model>" (decision
   5); `seven_day_opus` → "Weekly · Opus"; `seven_day_sonnet` → "Weekly · Sonnet". The other three
   keys are not Claude Code windows on any plan we can probe; a junk label is worse than a dropped
   window. The full list is recorded in harness-behavior so a future probe can extend the map.
   *(Comment on the label map, naming the binary as the source.)*
5. **The per-model window is labeled with the model that produced the snapshot.** The
   label uses the model's **family name** (`agentSelection.ts::claudeModelFamilyLabel`, e.g.
   `claude-fable-5-1` → "Fable"), not the raw stream id: the id needs 136px against an 85px budget at
   the default sidebar width, and the family name is the word the user selected the model by. The
   window arrives only on turns run on an allowlisted model, so the model of the turn that delivered
   the event is a truthful label. The `rate_limit_event` reducer records the runtime's observed model
   beside the payload as `last_rate_limit_model`. The sidecar carries the same-turn model beside the
   exact payload, so the family label survives reload without borrowing the session's older model.
   A legacy sidecar without that additive field falls back to "Weekly · model-specific". A live event
   overwrites both fields. *(Comment on the reducer arm and the fallback label.)*
6. **The CLI's threshold warning becomes the meter's warning tone.** `status: "allowed_warning"`
   names the window in `rateLimitType` and the threshold in `surpassedThreshold`; that meter fills
   with the `warning` token. The tooltip does not repeat the harness's internal cutoff. The second
   event of a turn carries the superset, so last-write-wins is correct. `status: "rejected"` stays
   the failure path (harness-behavior §1.4).
7. **The overage escalation and the snapshot-age line are unchanged.** "⚡ using credits" is about
   what is being billed, not how full a window is; it stays beneath the meters. The rehydrated
   "snapshot from …" line stays: Claude's payload is stream-only, and a days-old weekly percentage is
   exactly its case. `overageStatus` / `overageDisabledReason` are not rendered.
8. **Reset-passed hides a meter, per window.** A window whose `resetsAt` is in the past is dropped;
   the others still render. No dim, no age threshold.
9. **Inline reset text is relative at every distance; the absolute date lives in the tooltip.**
   "in 16 min" / "in 3 h" / "in 5 d" beside the meter; the full date and time on hover. Computed at
   render from `Date.now()`, not on a timer — recorded as a known limitation.

   Two earlier drafts of this decision put an absolute form inline ("Resets Sun 8:00 AM", then
   "Resets Sat, Sep 19, 8:00 AM" once a bare weekday was found ambiguous six days out). **Both are
   superseded**, and the reason is measured rather than argued: the meter's label, its reset text and
   its percentage share a column ~177px wide at the default sidebar width, and an absolute date takes
   ~100px of it — it crowded the label it belonged to off the card. A relative countdown is compact
   at every distance, and rendering no weekday retires the ambiguity the second draft existed to fix.
   `utils.ts::formatResetCountdown` is therefore pure arithmetic with no locale or time-zone
   formatting; the cells' tooltips carry the absolute date via `formatResetDateTime`, where there is
   room for it.
10. **Both harnesses use the same label strings for the same window.** Codex `primary` (300 min) and
    `secondary` (10080 min) are "5-hour limit" and "Weekly · all models"; its bare `used_percent`
    fallback is a meter labeled "Quota". *(Comment where the Codex labels are derived.)*
11. **The context meter gains the token detail** — "121.1k / 1M" beside the percentage — and, on
    harnesses that support the breakdown, a trailing chevron that opens the panel (decision 20).

**Environment inventory**

*Governing rule for everything on the card:* **surface the information; manage the clutter with
disclosure, never by dropping it.** Every list collapses to a summary line by default and expands on
click; a long list (tools, commands) collapses to a count and expands to the full list. Nothing the
harness reports is withheld because it is long. What is withheld is only what carries no meaning
for the user (telemetry flags, internal capability strings).

12. **`SessionMeta` gains typed fields shaped to fit both harnesses; the frontend never reads
    `raw`.** `agents: Vec<String>`, `plugins: Vec<{name, version, source}>`, `memory_paths:
    Vec<String>`, `slash_commands: Vec<String>`, `approved_commands: Vec<String>`, `source:
    Option<String>` on `McpServerStatus`; `skills` becomes `Vec<{name, description: Option, path:
    Option}>` (Claude supplies names, Codex all three); and `settings: Vec<{label, value}>` — the
    harness's run settings as display pairs (Claude: permission mode, output style; Codex: sandbox,
    approval policy, personality, shell, timezone). A label/value list rather than one field per
    setting because the two harnesses share no setting names and the card renders them identically.
    **Each list is `Option<Vec<_>>`: `None` means the runtime source did not report it; `Some([])`
    means it reported an empty list.** The distinction is load-bearing for decision 13 — a Claude
    `init` with zero MCP servers is authoritative, and filling it from a config file would show
    servers as loaded that are not. Typed because the rate-limit payload's opaque-`unknown` pattern
    exists for a shape we did not control and did not want to model; this shape we are choosing to
    model. *(Comment on the struct, on the `Option` semantics.)*

    **Where each harness fills them.** Claude: `parse_session_meta` from `system/init`, live.
    Codex: the post-terminal session-file enrichment in `codex/session_file.rs` — the same path
    `rate_limits` already takes — from `world_state` (skills parsed out of the `host_skills`
    markdown block; environment; approved commands) and `turn_context` (settings). Codex MCP
    servers stay on the config loader with `"configured"` status; `codex mcp list` is out of scope.
    *(Comment on the Codex extractor: the skills block is a scraped markdown format like the
    context report, hence a fixture-driven parser with a names-only fallback.)*
13. **Precedence: a runtime-supplied list replaces completely, including an empty one; the config
    loaders fill a list only when no runtime source ever supplied it.** This holds at every merge
    point — the live `session_meta` reducer, `merge_meta_with_loaders` on reload, and the sidecar
    overlay — and is the opposite of the rate-limit overlay's fill-if-empty, for a stated reason: the
    loaders are registries without status, and the runtime value is what the harness actually
    loaded. There are no loader-only appends when a runtime list is present. Codex MCP servers are
    the one list with no runtime source, so they stay loader-backed with status `"configured"`.
    *(Comment on `merge_meta_with_loaders`: replace-not-fill, and why it departs from the rate-limit
    rule.)*

    **Delivery.** Codex emits `SessionMeta` after **every** turn's enrichment, not only the first —
    inventory changes between turns must reach the card. `AdapterEvent::SessionMeta` gains a
    `source: SessionMetaSource { StreamOnly, SessionFileBacked }` discriminator mirroring
    `RateLimitSource`, dropped at the `NormalizedEvent` boundary; the dispatcher persists the
    inventory snapshot only for `StreamOnly`, which keeps it harness-agnostic. Claude's inventory
    (stream-only, class C) therefore lands in the metadata sidecar with a capture time and renders
    with the "as of" qualifier after a reload — resolving G14 by the convention the rate-limit
    snapshot set: a stale status is fine when it says it is stale. Codex's is re-read from the
    rollout (class B) and never persisted. *(Comment on the sidecar field, citing G14; comment on
    the source enum, citing `RateLimitSource`.)*
14. **The two chips become an "Environment" disclosure on an expanded card.** A collapsed card
    keeps the model, effort, context meter, and critical quota warnings visible, but omits
    Environment entirely: authentication warnings are common for configured servers a user does not
    rely on and do not justify making every compact card taller. Expanding the card reveals one
    trigger row. It shows "View details" when the inventory is healthy, "N need auth" for the
    `needs-auth` status actually observed, and "N need attention" for any other non-connected
    status, with both counts when both apply. The complete inventory opens in a bounded, named
    popover so long lists never extend the card. Its sections include MCP servers (name, `StatusDot`,
    source), plugins (name @ version), memory paths (basename, full path on hover), skills, tools,
    slash commands, custom agents, and settings (permission mode, output style); the long lists use
    nested disclosures. Status vocabulary: `connected` → the healthy green dot, `needs-auth` and
    anything else → the warning dot with the raw status as its label — the set is unknown beyond the
    two observed, so unknown statuses must show, not hide.
    A Codex card renders the same row from the subset it has: MCP servers (config names, no status
    dot — "configured" is not a runtime status and must not render as one), skills with
    descriptions from the rollout, approved commands behind a count line, and the settings line.
    Before an agent's first turn, Codex skills come from the existing `codex/skills.rs` scanner
    (`~/.agents/skills` and `<cwd>/.agents/skills`, the roots Codex's documentation lists) and are
    labelled as configured, not loaded; the scanner is incomplete — Codex also loads system and
    plugin roots — and is deliberately **not** extended to reproduce Codex's discovery. Once the
    agent has run, the rollout is the authority (decision 13). Sections a harness reports nothing
    for never render.

**Context breakdown**

15. **The report is a maintenance work item on the agent's queue, exactly as compaction is.** It
    writes to the session file, so it must serialize with turns under the same session lock, and the
    per-agent actor is the only writer. New `WorkPayload::ContextReport` / `TurnKind::ContextReport`;
    journals nothing (no send, no link, no outcome marker — the compaction reasoning in
    `2026-09-16-claude-compaction.md` decision 6 applies verbatim); current-turn waiters resolve
    `Idle` at drain; `PeekCurrentTurn` reports it running through drain. It is invisible to
    forwarding. *(Comment on each dispatcher arm, alongside the compaction comments.)*
16. **It is a distinct adapter operation with its own capability predicate.**
    `HarnessKind::supports_context_report` (Claude only — the fake-success hazard in harness-behavior
    §3.9 applies to a `/context` *prompt* on Codex and Antigravity exactly as it does to `/compact`);
    `HarnessAdapter::context_report` defaulting to `UnsupportedOperation`; the Claude adapter's
    `Invocation::Context` positional is `/context`, bare and unescaped by construction, sharing every
    flag with a send. Fails closed with no session file, as `compact` does. *(Comments mirror the
    compaction ones.)*
17. **No transcript row, ever — not queued, not live, not on reload — so the report carries its own
    request state.** A report is not conversation. The pending entry kind is `"context_report"`; the
    unified view renders nothing for it (unlike a queued compaction); `turn_start` creates no agent
    turn for it; the completion event carries the report to runtime state, not to the transcript.
    Because there is no row, there is no place for a failure to show — compaction's failed row is
    what tells the user a compaction failed, and the sidebar deliberately renders no `last_error`.
    So `AgentRuntime` keeps `context_report_request: { send_id, turn_id?, phase: queued | running |
    done | failed | cancelled, error? }`, set on dispatch and advanced by `turn_start`, `turn_end`,
    `message_failed`, `message_cancelled`, and `failSendStart`. **It is cleared only by the next
    report dispatch, never by an ordinary send** — otherwise a failure message would vanish the
    moment the user sent a message. One slot: the panel opener does not dispatch while a request is
    already queued or running, so closing and reopening the dialog cannot orphan the first request's
    correlation. **While a request is in flight the panel shows only the spinner**, withholding any
    previous report: rendering old numbers and swapping them seconds later reads as the panel
    changing its mind. A *settled* failure is the opposite case and keeps the previous report —
    nothing further is coming to replace it. *(Comment on the pending-kind branch and on the request
    record's clear rule.)*
18. **`ContextReport` is deserialized from the structured object; the markdown is the fallback.**
    Live, from `assistant.context_usage`; on disk, from `contextUsage`. Categories keep the CLI's
    `kind`; every item row keeps its exact integer tokens. Only when the object is absent (an older
    CLI) does the markdown parser run, and only that path carries an `approximate` flag for `~30` /
    `< 20`. A decode failure on either path does not fail the turn: the event carries
    `unparsed: true` with the raw markdown, and the panel shows the raw text. The live test asserts
    the structured object is still present and decodes. *(Comment on the parser: structured first;
    markdown is a scraped format kept only as the fallback, hence the raw-text safety net.)*

    **In `StreamMode::ContextReport` the parser swallows the synthetic envelope.** The report's
    `assistant` event is intercepted before ordinary assistant handling: no `ContentChunk` for its
    text, no synthetic `TurnIdentity`, so nothing downstream — the dispatcher's `captured_text`, the
    forward path — has to know to ignore report text. *(Comment on the interception.)*
19. **Durable through the session file, not the sidecar — routed on the self-describing output
    record, no pairing.** In `handle_system`, before the pending-command pairing: an `sdk-cli`
    `local_command` record whose `commandRun.command == "context"` emits
    `SystemMarker::ContextReport { report }` and returns — from `contextUsage` when present, from the
    markdown otherwise (decision 18) — so the routing does not depend on the structured object being
    there, and the fallback path cannot fall into the orphaned-output branch. No agent turn, no
    pending input required. The transcript renders nothing for that marker kind; the frontend derives
    the latest report per agent from the markers and stamps it "as of" the marker's time; live, the
    completion event overwrites it. Other local-command records keep today's path. The `isMeta`
    caveat record that precedes the command is treated by `is_meta_continuation` as a mid-turn
    continuation; the fixture test must show it neither extends the preceding agent turn nor pulls
    the following turn backward. *(Comment on the routing branch, naming `commandRun` as the key.)*
20. **UI: a panel opened from the context meter's icon, and from the agent menu ("Context
    breakdown…").** `Dialog`, titled "Context breakdown · <agent>". **Opening is the refresh**:
    every entry point dispatches a fresh report, so the panel carries **no Refresh or Analyze
    button** — the spinner says one is on its way, and re-opening is how the user asks for another,
    including after a failure. A button would only name the action the open already performed, and
    the report costs nothing (it runs locally and bills no tokens).

    **Both entry points are disabled while the agent is busy.** A report shares the per-agent FIFO
    with sends, so on a busy agent it waits out the in-flight turn *and* every queued send — and
    since opening is what dispatches, the panel would be a featureless spinner for that whole time.
    Refusing at the door keeps the wait to the ~1s an idle report takes, which is the wait the
    spinner-only panel is designed around, and keeps a maintenance turn from taking queue position
    ahead of the user's work. The one exception is the agent's *own* report: the dispatch makes the
    agent busy, so an in-flight `context_report_request` keeps the entry points live or the user
    would be locked out of the panel their report is filling (re-opening rides that run and
    dispatches nothing). Header: the context meter with
    model and "as of". Body: one meter per category, in the CLI's order, label left, "<tokens> ·
    <percent>" right; deferred-tool rows show tokens only (the CLI reports no percentage). Below,
    collapsible sections — MCP tools grouped by server with per-server totals (the flat 80-row table
    is unreadable), custom agents, memory files, skills — each `ExpandCollapseIcon`, collapsed by
    default, followed by a "Raw report" disclosure. Row-level meters reuse the primitive with no
    detail text where the CLI gives none.

## Required reading before implementing

- `docs/implementation_plans/2026-09-16-claude-compaction.md` — the template for M4: the capability
  predicate, the distinct adapter operation, the dispatcher work item, and the reasoning for
  journaling nothing. M4 adds a sibling, not a variant.
- `docs/ui-conventions.md` — token model (`bg-active` track; a fill names a role; `warning` is its
  own role), "Reach for a primitive before hand-rolling", and the `StatusDot` /
  `ExpandCollapseIcon` / `Dialog` entries.
- `docs/harness-behavior.md` §0 (the `/context` intercept claim M5 corrects), §1.4 and §3 (quota
  claims), §3.9 (fake-success hazard), G14; `docs/system-design.md` §7 "Per-harness sidebar surface"
  and §9 capability matrix.
- `AGENTS.md` → "Harness registries … are harness-owned" (the config-loader fallback decision 13
  keeps) and "Live testing against real harnesses".
- `src/lib/components/Sidebar.svelte`: `contextUtilization` and the `agent-context-bar` markup;
  `rateLimitView`, `codexRateLimitView`, `codexWindowLabel`, `rateLimitLabel`; the two rate-limit
  cells; the `agent-meta` chip row; `startCompaction` and the `agent-action-compact` menu item.
- `src/lib/state/reducers.ts` (`session_meta`, `rate_limit_event`, `hydrate`, `turn_start`,
  `pendingKind`) and `src/lib/state/types.ts` (`AgentRuntime`, `AgentMeta`, `PendingSend`).
- `crates/harness/src/parser.rs` (`parse_session_meta`, `parse_rate_limit_event`, the compaction
  `StreamMode`), `crates/harness/src/claude_code/mod.rs` (`Invocation`, `build_args`, `compact`),
  `crates/harness/src/claude_code/session_file.rs` (`pending_local_command`,
  `finish_pending_local_command`, `extract_local_command_output`), `crates/harness/src/transcript.rs`
  (`SystemMarker`), `crates/harness/src/meta_sidecar.rs`.
- `crates/harness/src/codex/session_file.rs` (the post-terminal `enrichment` and its `rate_limits`
  extraction — the pattern Codex's inventory follows), `codex/config.rs` (`load_mcp_servers`,
  `CONFIGURED_STATUS`), `codex/skills.rs`.
- `crates/dispatcher/src/lib.rs`: every `TurnKind::Compaction` arm.
- `crates/app/src/commands.rs`: `compact_agent_impl` and its gates; the reload path that builds
  `LoadedTranscript.meta` from the config loader.
- `src/lib/components/Sidebar.test.ts` (context-bar, Claude and Codex rate-limit blocks, meta chips)
  and `crates/dispatcher/tests/dispatcher_with_mock.rs` (the compaction fixture and tests).

---

## Milestone 1 — The meter primitive, proven on the context bar

### Goal & Outcome

One component draws every "how full is this" gauge in the app, and the context bar is its first
user.

- The context bar reads "Context used" left, "121k / 1M" and "12%" right, bar beneath — same
  position and clean-hide rules as today. Label shortened from "Context after last turn" during M1:
  measured in WebKit, the full phrase needs 115px against an 85px budget at the default 240px
  sidebar, so it rendered clipped to "Context after l…". The "as of the last completed turn"
  qualifier moves to the row's tooltip when M2 adds one. Token density is `k`/`M` via
  `utils.ts::formatTokens`, which rounds to whole thousands above 10k — hence "121k", not "121.1k".
- A reset-time helper renders "in 16 min" / "in 3 h" / "in 5 d" from a future instant,
  deterministically under test. (M2 revised this to relative-only — see decision 9.)
- Nothing about the rate-limit cells or the chips changes yet.

### Implementation Outline

**Primitive.** A `ui/` component taking a label, a 0–1 used fraction, optional detail text, optional
tone (`neutral` default, `warning`), optional trailing snippet (M1 needs none; the chevron in
decision 11 lands in M4), and a test id. Track `bg-active`; fill `bg-fg` or `bg-warning`. Percentage
formatted in one place with the rounding the Codex cell uses today; width clamps at 100%. No tooltip
inside — call sites own hover.

**Reset text.** A future-tense sibling of `utils.ts::relativeTime` with injectable `now`; relative
under 24 hours, weekday + clock beyond; the clock form for anything not in the future, never a
negative.

**Context bar.** Replace the hand-rolled markup with the primitive. Detail is
`context_tokens_after_turn` / `context_window` in the `k`/`M` density `CompactionTurn.svelte`'s
`tokens()` already uses — lift that formatter to a shared place rather than copy it. Keep
`agent-context-bar` as the outer test id.

### Definition of Done

- Primitive unit tests: label/detail/percentage text; fill width from value; clamp above 1; tone
  switches the fill token; rounding matches the Codex cell's current output for the same inputs.
- Reset-text unit tests with fixed `now`, **`TZ` and locale pinned in the test setup, asserting
  literal strings** (computing the expectation through the same formatter would test nothing): 16
  minutes → "in 16 min"; 3 hours → "in 3 h"; 2 days → the literal weekday + clock; a past instant →
  the literal clock form.
- Sidebar context-bar tests pass with the new copy; one new assertion for the token detail.
- `make check` green. No docs.

---

## Milestone 2 — Every usage window, both harnesses

### Goal & Outcome

A Claude agent's card shows how full each of its usage windows is; a Codex card shows the same thing
the same way.

- Claude: "5-hour limit", "Weekly · all models", and — after a turn on a per-model-capped model —
  "Weekly · <that model>", each a meter with "Resets in …" and the used percentage.
- A window the CLI flags as past its warning threshold fills amber.
- Hover shows each window's full reset date and time, the overage window when billing to credits,
  and after a restart the "snapshot from …" line.
- A window whose reset has passed disappears; the others stay. "⚡ using credits" still appears
  beneath the meters when overaging.
- An older or changed CLI that sends no `unifiedWindows` shows a single meter for the top-level
  window with no percentage — never a blank cell, never a wrong number.
- Codex's two windows render as the same meters with the same labels; its bare-percentage fallback
  renders as a "Quota" meter.
- A live test fails if a future release stops sending `unifiedWindows` or changes its shape.

### Implementation Outline

**Runtime state (decision 5).** `AgentRuntime.last_rate_limit_model?: string`, stamped by the
`rate_limit_event` arm from the runtime's observed model and restored by `hydrate` when the metadata
sidecar carries the model paired with that snapshot.

**Derivation (`Sidebar.svelte`).** Replace `rateLimitView`'s single `window` with a list in the
meter's shape read from `unifiedWindows` in decision 4's order, with the label map and the
model-aware label. Defensive shape-read like the Codex one: `utilization` not a number in `[0, 1]`
or `resetsAt` not a number → skip; reset-passed → skip. Warning tone from the top-level `status` /
`rateLimitType` / `surpassedThreshold`. Fallback per decision 3. `overage` untouched.
`codexRateLimitView` keeps its logic and adopts the shared labels.

**Rendering.** Both cells render their list through the primitive, then (Claude) the amber overage
line, then the tooltip content. A shared snippet for the two near-identical cells is at the
implementer's discretion.

**Live drift guard.** `live_claude_rate_limit_carries_unified_windows`, one "ack" turn:
`unifiedWindows.five_hour` and `.seven_day` each with number `utilization` in `[0, 1]` and number
`resetsAt`. Shape, not values; do not assert the per-model window (plan- and model-dependent).

### Definition of Done

`Sidebar.test.ts`: two windows → two meters, fraction → percent (`0.27` → "27%"), reset text present;
three windows with `meta.model` seeded → third meter "Weekly · <model>", generic label for a legacy
snapshot hydrated without a model; `allowed_warning` on `seven_day_overage_included` → that meter warning
tone, no tooltip threshold line, others neutral; reset-passed on `five_hour` → only the weekly meter;
overage → amber line beneath the meters + tooltip window; no `unifiedWindows` → one meter, no
percentage (the existing Claude tests are this coverage — retitle any whose name reads as the primary
path); unknown key `seven_day_cowork` beside the known two → exactly two meters; malformed
`utilization` (string, `> 1`, `< 0`) → that window skipped; Codex both windows with shared labels,
bare `used_percent` → "Quota" meter. Reducer: `rate_limit_event` stamps the model; a later `hydrate`
does not overwrite it. Rust: the live test passes on `make test-live-claude`.

---

## Milestone 3 — Environment inventory

### Goal & Outcome

The card says what the agent has loaded and whether it is usable, not just how many.

- An expanded Claude card shows a compact Environment trigger. It carries only actionable status;
  the full counts and inventory open in a bounded popover instead of changing card height.
- A server that reports anything other than `connected` shows as a warning with the status as its
  label in the expanded card and popover. Collapsed cards omit Environment status.
- After a restart the list is what the last turn loaded, marked "as of <time>"; an agent that has
  never run shows the registry from the config loader as today, with no status.
- A Codex card shows the same disclosure from its rollout: skills with descriptions, the
  approved-command allowlist behind a count, and settings (sandbox, approval policy, personality,
  shell, timezone); MCP servers appear as configured names without a status dot.
- Antigravity cards show whatever subset their harness reports; empty sections never render.

### Implementation Outline

**Adapter — Claude (decision 12).** Extend `SessionMeta`, `McpServerStatus`, and the skill entry
shape; parse the new fields in `parse_session_meta` (`memory_paths` is a map — take its values);
wire type and `AgentMeta` in the frontend follow. The `session_meta` reducer carries them through.

**Adapter — Codex (decision 12, 13).** In `codex/session_file.rs`, extend the post-terminal
enrichment to read `world_state.state` and `turn_context` into the same typed fields: skills from
the `host_skills` markdown (`- <name>: <description> (file: <root>/<path>)` lines, roots table
expanded), settings from `turn_context` and `world_state.environments`, approved commands from
`permissions.approved_command_prefixes` (joined with spaces). In `codex/mod.rs`, emit `SessionMeta`
after every enrichment, dropping the `is_first_turn` gate (the loaders were already documented as
fresh on every emission; the reducer overwrites). Record a fixture rollout from a probe session,
with `base_instructions` truncated, under `crates/harness/tests/fixtures/codex/`. A `host_skills`
block that fails to parse yields `Some([])` for skills plus a parse warning, not a failed load; a
rollout with no `world_state` yields `None`.

**Precedence and persistence (decision 13).** `AdapterEvent::SessionMeta` gains `source`; the
dispatcher persists an inventory snapshot to `MetaSidecar` (typed fields plus `captured_at`) only for
`StreamOnly`. `merge_meta_with_loaders` becomes replace-not-fill: a `Some` list from the parser or
snapshot replaces the loader's list even when empty; `None` takes the loader's. The sidecar overlay
applies the same rule and stamps `meta_as_of`. `codex/skills.rs` is unchanged apart from its module
doc recording that it is the pre-first-turn fallback and incomplete by design. Schema version bumps
only if the file's existing fields change shape; an additive optional field does not need one.

**UI (decision 14).** Replace the chips with an expanded-card trigger and bounded `Popover`.
`StatusDot` communicates server status; `Tooltip` with the supplemental delay exposes full memory
paths. Keep `agent-meta` as the outer test id.

### Definition of Done

- Claude parser unit tests: an init with all fields populates them; an init without them (older
  CLI) yields empty defaults; `memory_paths` map → values list; `source` optional.
- Codex fixture tests: the recorded rollout yields the skills with descriptions and expanded paths,
  the settings pairs, and the approved commands; a rollout without `world_state` (older CLI) yields
  `None`; a malformed `host_skills` block yields `Some([])` plus a warning. Two-turn adapter test
  with `world_state` changed between turns → the second `SessionMeta` carries the change.
- Precedence: `merge_meta_with_loaders` with an explicit-empty runtime list and a non-empty loader
  list → empty wins; runtime list conflicting with the loader → runtime wins with no appends; `None`
  → loader. `load_codex_transcript` end to end with a scanner directory disjoint from the rollout's
  skills → the rollout's. Claude sidecar overlay with a loader registry present → snapshot statuses
  win and `as_of` is set; never-dispatched agent → loader, no `as_of`. Dispatcher: a
  `SessionFileBacked` meta is not persisted.
- Sidecar round-trip test for the inventory snapshot; a sidecar without it reads as absent.
- `Sidebar.test.ts`: collapsed cards omit Environment while retaining model, effort, context, and
  critical quota warnings; expanded cards expose the Environment trigger and full popover;
  `needs-auth` → warning dot with label; unknown status string → warning dot with that string; "as
  of" shown only when rehydrated; a Codex agent renders its sections with no status dot on MCP rows;
  an agent with an empty inventory renders no environment row.
- Existing chip tests updated to the new row.
- **Live drift guards — the milestone reads eight undocumented fields across two harnesses and has
  none today.** `live_claude_session_meta_carries_inventory`: one "ack" turn, asserts `system/init`
  still yields `agents`, `plugins` (with `name` and `version`), `memory_paths`, `slash_commands`, and
  `source` on at least one MCP server — shape and presence, never counts or names, which are
  account-specific. `live_codex_world_state_yields_inventory`: two "ack" turns on one session,
  asserts the first yields skills parsed out of `host_skills` with non-empty descriptions plus the
  settings pairs and approved commands, and that the **second** turn emits `SessionMeta` too (the
  `is_first_turn` removal — a fixture cannot see that gate). The `host_skills` scrape is the most
  fragile read in the plan; this is the only thing that would notice the format moving. Names carry
  their harness per the live-test naming convention.
- Docs: G14 marked closed (M5 does the writing).

---

## Milestone 4 — Context breakdown

### Goal & Outcome

The user can see what is occupying a Claude agent's context window, per category and per item.

- The context meter on a Claude card shows a breakdown icon; clicking it (or "Context breakdown…"
  in the agent menu) opens the panel and starts a fresh analysis every time. The panel has no
  button: re-opening it is how a newer breakdown is requested.
- The breakdown icon and menu item are disabled while the agent is working, so the report is never
  queued behind a turn; they stay live while the agent's own report runs, and re-opening then shows
  that run's spinner rather than starting a second.
- Running it on an idle agent fills the panel in about a second, showing only the spinner until it
  lands — a previous breakdown stays hidden rather than being swapped out under the reader. A
  failed analysis says why, beside the last report it managed to take; re-opening retries it.
- The panel shows the model, used/window tokens, a meter per category in the CLI's order, and
  collapsible per-item sections: MCP tools grouped by server, custom agents, memory files, skills.
- No row appears in the transcript for the report — live or after reopening the project.
- After reopening, the panel shows the last report taken, marked "as of" its time.
- A report the parser cannot read still opens the panel with the raw text and a note, and the turn
  is not marked failed.
- Codex and Antigravity cards show no chevron and no menu item.

### Implementation Outline

Follow the compaction plan's milestone structure — capability + adapter + parser, then dispatcher,
then app command + frontend — and mirror its tests one for one where the shape is the same. **Land
the disk-parser change and its fixture first**, before the work item exists, so a report can never
reach `forward.rs::latest_completed_agent_text` as an agent's answer.

**Capability and adapter (decision 16).** `supports_context_report` in `switchboard_core` beside
`supports_manual_compaction`, with the same fake-success rationale. `HarnessAdapter::context_report`
with the `UnsupportedOperation` default; Codex and Antigravity adapter tests assert the refusal.
Claude: `Invocation::Context`, the `compaction_argv_matches_send_argv_except_the_positional` test
extended to cover it, fail-closed on a missing session file, `StreamMode::ContextReport`.

**Parser (decision 18).** In `StreamMode::ContextReport` the stream parser intercepts the synthetic
`assistant` event, decodes `context_usage` into `ContextReport` (falling back to
`local_command_source` markdown when absent), emits no content chunks and no `TurnIdentity` for it,
and produces the completion event carrying the report (or `unparsed` + raw). Both decoders live in
the harness crate, are pure, and are tested against the recorded stream fixture plus edge cases: the
object stripped (markdown path), an unknown category `kind` (kept, rendered by name), a malformed
object (`unparsed`, raw text retained). The `AdapterEvent` / `NormalizedEvent` gain
`ContextReport { agent_id, report }`.

**Dispatcher (decision 15).** `WorkPayload::ContextReport`, `TurnKind::ContextReport`; every
`TurnKind::Compaction` arm gains the sibling with the same behaviour except that the frontend is told
`pending_kind: "context_report"`; the mock adapter gains a `context_report` scenario. Tests mirror the
compaction set: runs at once when idle, queues behind a running turn and an earlier send, journals
nothing on complete/fail/cancel, waiters resolve idle, not removable as a queued message, unsupported
harness fails the message without starting a turn.

**Disk parser (decision 19).** `SystemMarker::ContextReport { report: ContextReport }` in
`transcript.rs`; `handle_system` routes an `sdk-cli` `local_command` with `commandRun.command ==
"context"` to it before the pairing logic, re-using the decoders. The fixture is the recorded
session file with a completed turn on each side of the three records; it pins: one marker, zero
agent turns, zero parse warnings, the preceding turn not extended by the `isMeta` caveat record, the
following turn not merged backward, and `latest_completed_agent_text` not returning the report. The
same fixture with `contextUsage` removed → marker via the markdown path. The merge treats it like
every `Turn::System` (never a send slot).

**App (decision 15, 17).** `context_report_agent_impl` with the three gates `compact_agent_impl`
has; `#[tauri::command]` shim; `AppError` variants for unsupported / no session, worded for the user.

**Frontend (decision 17, 20).** `PendingSend.kind` gains `"context_report"`; `dispatchContextReport`
beside `dispatchCompaction` sets `context_report_request`; the unified view renders no row for the
pending entry and `turn_start` appends no agent turn for the kind but advances the request to
`running`; `turn_end` / `message_failed` / `message_cancelled` / `failSendStart` advance it to its
terminal phase; the completion event lands in `AgentRuntime.last_context_report` (+ `_as_of`); on
hydrate the latest `context_report` marker fills it when absent; `UnifiedTranscript` renders nothing
for the marker kind. The panel is a new component taking the report and the request state; the
context meter's chevron and the menu item open it; its button is disabled while a request is queued
or running.

**Live drift guard.** `live_claude_context_report_parses`: a fresh "ack" turn, then a
`context_report` on that session; asserts the structured object was present (not the fallback), the
category rows present, and used/window tokens positive. One extra local command, no extra model
call.

### Definition of Done

- Harness: capability test; Codex and Antigravity refusal tests; Claude argv test; decoder fixtures
  above; stream-parser test that a `ContextReport` mode turn completes carrying the report, emits no
  content chunks and no `TurnIdentity`, and that a garbled object completes with `unparsed`.
- Disk parser: the fixture assertions listed in the outline; an unrelated `sdk-cli` local-command
  pair still → completed turn.
- Dispatcher: the mirrored compaction set.
- App: gates (unsupported harness, no session, unmaterialized fork) and the happy path against the
  mock.
- Frontend: reducer tests for the pending kind (no row, no turn), each request-phase transition, the
  clear-only-on-next-report rule (an ordinary send leaves a `failed` request in place), the
  completion event, and hydrate fill-if-empty from the marker; component-level tests with mocked
  `invoke`/`listen` for completion arriving before the IPC resolves, IPC rejection, runtime failure
  after start, cancel while queued, cancel while running — each asserting the dialog copy and that
  the prior report and its timestamp remain; panel tests for empty state, parsed rendering (category
  meters, grouped MCP section), unparsed fallback, and the disabled button while in flight; Sidebar
  tests for chevron/menu presence gated on harness.
- Live test passes on `make test-live-claude`.

---

## Milestone 5 — Correct the record

Small; compress accordingly.

- `docs/harness-behavior.md`
  - §0: `/context` no longer produces the zero-token synthetic envelope in `-p`; it returns the
    report as `result` text (2.1.274), and writes the three session-file records. Keep the
    space-prefix rule and its rationale intact — the report action bypasses it by construction like
    compaction.
  - §1.4 Claude row and §3 "Rate-limit / quota" cell: `unifiedWindows`, the `allowed_warning`
    second event, the full key list and which render, and the per-model window's model-gating;
    fix "5-hour + weekly `overageResetsAt`" (it is the overage credit window).
  - §3 metadata table: the inventory fields now surfaced and persisted; G14 → ✅ closed with the
    as-of convention named; a new row for the context breakdown (Claude ✅ on demand; Codex /
    Antigravity ❌ — same hazard as §3.9).
  - §3.9 or a sibling §3.10: the `/context` protocol, cost (none), session-file footprint, and the
    disk-parser routing; name the live test. In §3.9's fake-success list, add the Codex `/status`,
    `/context`, `/usage` probes (model-authored, 58 output tokens) as evidence.
  - G8 (closed): the window shape is per plan — `secondary` can be null and `primary` can be the
    weekly window; labels derive from `window_minutes`. Record `rate_limit_reached_type`
    (`allowed` / `limit_reached` / `primary_window`) and `spend_control_reached` as the blocked
    signals, **observed only null — unverified, not built against**; and that Codex exposes no tool
    or MCP inventory in stream or rollout (`codex mcp list` is the only status source).
  - §6: a 2.1.274 entry for all three observations.
- `docs/system-design.md`
  - §7 sidebar matrix: "Quota % (window used)" Claude ✓; add rows for environment inventory and
    context breakdown. The "Quota %" rationale: "Anthropic's tier is closer to a hard limit without a
    visible counter" is false and goes. §7 per-agent actions: add "Context breakdown".
  - §9 matrix: "Read rate-limit / quota" Claude native (stream-only, undocumented, live-guarded);
    per-row note no longer "Codex only"; a "Context breakdown" row.
- `docs/ui-conventions.md`: the meter primitive under "which primitive for what".
- `README.md`: one "Agent CLI support and limitations" entry — context breakdown is Claude Code
  only, with why in product terms.

Done when grepping the harness docs for "no public stream signal", "only Codex", "without a visible
counter", "weekly `overageResetsAt`", or the §0 claim that `/context` emits no text finds nothing.

## Known limitations to record

- Reset text and the reset-passed gate are computed per render, not on a timer.
- **The card reflects the windows supplied by the latest event.** A per-model weekly meter shown
  after a Fable turn disappears after a Sonnet turn on the same agent, even though the limit is
  still in force; retaining it live would not survive a reload (the sidecar persists the raw event),
  so it is not retained.
- A legacy metadata sidecar without the additive model field reads "Weekly · model-specific" until
  the next live event refreshes the snapshot.
- Which models the per-model window covers is a server-side allowlist the stream never names.
- **A threshold warning naming a window we do not render is dropped along with the window.** The
  amber tone is attached to the flagged window, so if `rateLimitType` names a key outside the
  rendered set (`seven_day_cowork` and its siblings), the CLI's "near a cap" signal is lost. Not
  observed in any probe — the rendered keys cover every window seen across Claude 2.1.263–2.1.274 —
  so this is unobserved rather than impossible. Deliberately not filled with a generic amber line:
  the probe that would reveal such a plan is the same one that would supply the window's real label,
  at which point it renders properly and a generic line is dead code.
- The context report's structured object is undocumented; the markdown fallback and raw-text safety
  net cover its absence, and the live test is the tripwire.
- Each report writes three records into the agent's session file; the CLI's own TUI shows them on
  resume. Same trade compaction makes.
- **Only Switchboard-dispatched reports are captured; a `/context` run in the resumed terminal is
  not.** The routing sits behind the existing `entrypoint == "sdk-cli"` gate, and a probe of the
  interactive TUI (claude 2.1.274) shows why moving it would make things worse rather than better.
  The terminal *does* write a `commandRun: {"command": "context"}` record — so the gate, not the
  key, is what excludes it — but that record's content is the terminal's own coloured rendering
  (ANSI escapes and box-drawing glyphs), not the markdown table, and it carries **no `contextUsage`
  object at all**. Decoding it would produce `unparsed` plus a panel full of escape codes. The
  readable markdown exists on a *separate* record — `user`, `isMeta: true`, child of the command
  record by `parentUuid` — which `is_meta_continuation` currently skips cleanly (verified: it does
  not leak into the preceding agent turn). Capturing it would therefore need cross-record pairing,
  which is exactly what keying on the self-describing record was chosen to avoid, plus a carve-out
  in that guard, for a report that could only ever be the rounded markdown one.
- Memory *files* are named only by the report; `init` gives the memory directory.
- Codex: no runtime tool or MCP status exists in the stream or rollout; MCP rows show configured
  names only, and pre-first-turn skills come from an incomplete scanner labelled as configured. The
  blocked-state fields are recorded, not rendered, until observed populated.
- **`world_state` is snapshot-plus-delta, and the reader folds per key.** A `full: false` record
  carries only the keys that changed, so a last-record-wins read would erase `host_skills` the
  moment any delta landed (measured: 77 of 1,222 local rollouts carry more than one record, one
  carried 28). Recorded because the fold is the non-obvious part — the single-record probe the plan
  was written from shows none of it.
- **The `host_skills` scrape can degrade silently in one direction.** A body with the expected
  headings but reworded entry lines yields an empty list plus a warning, which renders as no Skills
  section — indistinguishable on the card from an account with no skills. The live guard is what
  makes this loud; there is no in-app signal, deliberately, because an error row on a display-only
  registry would be worse than an absent section.
- **`is_first_dispatch_after_attach` and its `AppState::needs_session_meta` bookkeeping are now
  inert.** Their only consumer was Codex's first-turn `SessionMeta` gate, which this milestone
  removed. They are documented as inert rather than deleted: the removal touches the app layer's
  attach flow and a documented lock ordering, which is its own change and carries its own risk.
- **The Environment row's lists are keyed by index, never by name.** A recorded Claude `init`
  (`tool-vocabulary.jsonl`) lists `deep-research` twice among 21 skills, and Svelte throws on a
  duplicate `{#each}` key in production as well as dev, with no error boundary in the app to catch
  it. The rows are replaced wholesale on every event and carry no per-item state, so a name key
  bought nothing; duplicates are preserved rather than merged so the count matches what the harness said.
- **An absent `init` key yields `None`, not the "empty defaults" this milestone's Definition of Done
  first said.** The two readings differ only for a list that has a config-file fallback, and there
  `None` is clearly right: an older CLI that never emitted `mcp_servers` must fall back to the
  registry, while `Some([])` would claim an authoritative zero and blank the section. The DoD wording
  predates decision 12's `Option` semantics; decision 12 governs.

## Out of scope (deliberately)

- Rendering the three non-Claude-Code window keys, or any key not in decision 4's map.
- A minute ticker for reset text.
- Running the report automatically (per turn, or on open). It is on demand; the panel says how old it
  is.
- Fast-mode state, `capabilities`, and the telemetry flags on `init` — internal, no user meaning.
- Codex `plan_type` and `credits` (not wanted), and running `codex mcp list` as a side subprocess
  for MCP status.
- Rendering Codex's `base_instructions` (the 21 KB system prompt) or the blocked-state fields.
- Per-turn cache-hit ratios, per-model cost on multi-model turns, subagent stats, time-to-first-token
  — all present on `result`, none asked for.
