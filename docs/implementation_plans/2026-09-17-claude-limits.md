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

This plan renders all three on the card — every limit as the same meter the context bar already is,
the environment as an expandable list, and the breakdown as a panel opened from the context meter —
and corrects the docs. **The UI below is a best guess to iterate on in the app, not a spec to
defend.** Get the data on screen in the described shape; adjust the shape after.

## What the stream carries (verified)

None of this is in Anthropic's public docs. The live tests in M2 and M4 are the drift guards.

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

**It writes to the session file.** Three records per call: an `isMeta` `<local-command-caveat>` user
record, a `<command-name>/context</command-name>` user record, and a `system/local_command` record
whose `content` is the whole report inside `<local-command-stdout>`. Our disk parser
(`claude_code/session_file.rs`) currently treats a local-command input/output pair as a **completed
agent turn** with the output as its text — so without M4's parser change, every breakdown would
reappear on reload as a Claude response containing the report.

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
5. **The per-model window is labeled with the model that produced the snapshot — live only.** The
   window arrives only on turns run on an allowlisted model, so the model of the turn that delivered
   the event is a truthful label. The `rate_limit_event` reducer records the runtime's observed model
   beside the payload as `last_rate_limit_model`. The sidecar does not carry it; after a reload the
   label is "Weekly · model-specific", and the snapshot line already says the value is old. A live
   event overwrites both fields, so a stale label cannot outlive the next turn. *(Comment on the
   reducer arm and the fallback label.)*
6. **The CLI's threshold warning becomes the meter's warning tone.** `status: "allowed_warning"`
   names the window in `rateLimitType` and the threshold in `surpassedThreshold`; that meter fills
   with the `warning` token and its tooltip gains "above N% of this limit". The second event of a
   turn carries the superset, so last-write-wins is correct. `status: "rejected"` stays the failure
   path (harness-behavior §1.4).
7. **The overage escalation and the snapshot-age line are unchanged.** "⚡ using credits" is about
   what is being billed, not how full a window is; it stays beneath the meters. The rehydrated
   "snapshot from …" line stays: Claude's payload is stream-only, and a days-old weekly percentage is
   exactly its case. `overageStatus` / `overageDisabledReason` are not rendered.
8. **Reset-passed hides a meter, per window.** A window whose `resetsAt` is in the past is dropped;
   the others still render. No dim, no age threshold.
9. **Reset text is relative when near, absolute when far.** "Resets in 16 min" / "Resets in 3 h"
   under 24 hours; "Resets Sun 8:00 AM" beyond. Full date and time in the tooltip. Computed at render
   from `Date.now()`, not on a timer — recorded as a known limitation.
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

12. **`SessionMeta` gains typed fields; the frontend never reads `raw`.** `agents: Vec<String>`,
    `plugins: Vec<{name, version, source}>`, `memory_paths: Vec<String>` (the map's values),
    `slash_commands: Vec<String>`, `permission_mode: Option<String>`, `output_style: Option<String>`,
    and `source: Option<String>` on `McpServerStatus`. All default
    empty, so Codex and Antigravity emit them empty and the card clean-hides them. Typed because the
    rate-limit payload's opaque-`unknown` pattern exists for a shape we did not control and did not
    want to model; this shape we are choosing to model. *(Comment on the struct.)*
13. **The inventory is persisted to the metadata sidecar with a capture time, and rendered with the
    "as of" qualifier after a reload.** This resolves G14 by the convention the rate-limit snapshot
    already set: a stale status is fine when it says it is stale. On reload the sidecar snapshot wins
    over the config-loader registry (it is what the last turn actually loaded, with status); the
    loader remains the fallback for an agent that has never dispatched. The reducer's fill-if-empty
    / live-overwrites contract applies unchanged. *(Comment on the sidecar field, citing G14.)*
14. **The two chips become an "Environment" disclosure row on the card.** Collapsed: one line of
    counts with the only status that matters called out — "MCP 7 · 2 need auth · Agents 6 · Plugins
    1 · Skills 30 · Memory 1". Expanded: sections in that order — MCP servers (name, `StatusDot`,
    source), custom agents (names), plugins (name @ version), memory paths (basename, full path on
    hover), skills, tools, and slash commands — each of the last three a count line ("Tools · 109")
    that expands to the full sorted list — and a final settings line (permission mode, output
    style). Uses `ExpandCollapseIcon` at both levels; per-agent collapsed state, default collapsed,
    ephemeral like the card's other disclosure. Status vocabulary: `connected` →
    the idle/neutral dot, `needs-auth` and anything else → the warning dot with the raw status as its
    label — the set is unknown beyond the two observed, so unknown statuses must show, not hide.

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
17. **No transcript row, ever — not queued, not live, not on reload.** A report is not conversation.
    The pending entry kind is `"context_report"`; the unified view renders nothing for it (unlike a
    queued compaction); `turn_start` creates no agent turn for it; the completion event carries the
    report to runtime state, not to the transcript. *(Comment on the pending-kind branch.)*
18. **Parsed in Rust into a structured `ContextReport`, shipped as JSON.** Model, used and window
    token counts, the category rows, and the four item tables, each row keeping its **raw token
    string** and a parsed integer estimate with an `approximate` flag (for `~30` / `< 20`). A parse
    failure does not fail the turn: the event carries `unparsed: true` with the raw text, and the
    panel shows the raw text. Parsing is fixture-driven off a recorded report; the live test asserts
    the current CLI still parses. *(Comment on the parser: markdown is a scraped format, hence the
    raw-text fallback.)*
19. **Durable through the session file, not the sidecar.** The disk parser routes a local-command
    pair whose command is `/context` to a new `SystemMarker::ContextReport { report }` instead of a
    completed agent turn; the transcript renders nothing for that marker kind; the frontend derives
    the latest report per agent from the markers and stamps it "as of" the marker's time. Live, the
    completion event overwrites it. Other local-command pairs keep today's completed-turn path.
    *(Comment on the routing branch.)*
20. **UI: a panel opened from the context meter's chevron, and from the agent menu ("Context
    breakdown…").** `Dialog`, titled "Context breakdown · <agent>". Header: the context meter with
    model and "as of". Body: one meter per category, in the CLI's order, label left, "<tokens> ·
    <percent>" right; deferred-tool rows show tokens only (the CLI reports no percentage). Below,
    collapsible sections — MCP tools grouped by server with per-server totals (the flat 80-row
    table is unreadable), custom agents, memory files, skills — each `ExpandCollapseIcon`, collapsed
    by default. Footer: "Refresh" (runs the report; while the agent is busy the button reads
    "Queued — runs after the current turn"), and a "Raw report" disclosure. Empty state when no
    report exists: one sentence and an "Analyze context" button. Row-level meters reuse the primitive
    with no detail text where the CLI gives none.

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

- The context bar reads "Context after last turn" left, "121.1k / 1M" and "12%" right, bar beneath —
  same position and clean-hide rules as today.
- A reset-time helper renders "in 16 min" / "in 3 h" / "Sun 8:00 AM" from a future instant,
  deterministically under test.
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
- Reset-text unit tests with fixed `now`: 16 minutes → "in 16 min"; 3 hours → "in 3 h"; 2 days →
  weekday + clock; a past instant → clock form.
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
- Hover shows each window's full reset date and time, the threshold line when flagged, the overage
  window when billing to credits, and after a restart the "snapshot from …" line.
- A window whose reset has passed disappears; the others stay. "⚡ using credits" still appears
  beneath the meters when overaging.
- An older or changed CLI that sends no `unifiedWindows` shows a single meter for the top-level
  window with no percentage — never a blank cell, never a wrong number.
- Codex's two windows render as the same meters with the same labels; its bare-percentage fallback
  renders as a "Quota" meter.
- A live test fails if a future release stops sending `unifiedWindows` or changes its shape.

### Implementation Outline

**Runtime state (decision 5).** `AgentRuntime.last_rate_limit_model?: string`, stamped by the
`rate_limit_event` arm from the runtime's observed model; `hydrate` leaves it absent.

**Derivation (`Sidebar.svelte`).** Replace `rateLimitView`'s single `window` with a list in the
meter's shape read from `unifiedWindows` in decision 4's order, with the label map and the
model-aware label. Defensive shape-read like the Codex one: `utilization` not a number in `[0, 1]`
or `resetsAt` not a number → skip; reset-passed → skip. Warning tone from the top-level `status` /
`rateLimitType` / `surpassedThreshold`. Fallback per decision 3. `overage` untouched.
`codexRateLimitView` keeps its logic and adopts the shared labels.

**Rendering.** Both cells render their list through the primitive, then (Claude) the amber overage
line, then the tooltip content as today plus the threshold line. A shared snippet for the two
near-identical cells is at the implementer's discretion.

**Live drift guard.** `live_claude_rate_limit_carries_unified_windows`, one "ack" turn:
`unifiedWindows.five_hour` and `.seven_day` each with number `utilization` in `[0, 1]` and number
`resetsAt`. Shape, not values; do not assert the per-model window (plan- and model-dependent).

### Definition of Done

`Sidebar.test.ts`: two windows → two meters, fraction → percent (`0.27` → "27%"), reset text present;
three windows with `meta.model` seeded → third meter "Weekly · <model>", generic label after
`hydrate` without a model; `allowed_warning` on `seven_day_overage_included` → that meter warning
tone + tooltip threshold line, others neutral; reset-passed on `five_hour` → only the weekly meter;
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

- A Claude card's chip row becomes one collapsed line: "MCP 7 · 2 need auth · Agents 6 · Plugins 1 ·
  Skills 30 · Memory 1". Expanding lists each MCP server with a status dot and source, the agents,
  the plugins with versions, the memory paths, and — each behind its own count line that expands to
  the full list — the skills, the 109 tools, and the 98 slash commands, then a settings line
  (permission mode, output style).
- A server that reports anything other than `connected` shows as a warning with the status as its
  label; the collapsed line counts them.
- After a restart the list is what the last turn loaded, marked "as of <time>"; an agent that has
  never run shows the registry from the config loader as today, with no status.
- Codex and Antigravity cards show whatever subset their harness reports; empty sections never render.

### Implementation Outline

**Adapter (decision 12).** Extend `SessionMeta` and `McpServerStatus`; parse the new fields in
`parse_session_meta` (`memory_paths` is a map — take its values); wire type and `AgentMeta` in the
frontend follow. The `session_meta` reducer carries them through. Other harnesses' adapters emit the
defaults.

**Persistence (decision 13).** `MetaSidecar` gains an inventory snapshot (the typed fields plus
`captured_at`), recorded on `SessionMeta` like the rate-limit snapshot; the reload path fills
`meta` from it when present (with an `as_of` beside it, mirroring `last_rate_limit_as_of`) and from
the config loader otherwise. Schema version bumps only if the file's existing fields change shape;
an additive optional field does not need one.

**UI (decision 14).** Replace the chips with the disclosure row. Reuse the card's collapsed-state
pattern for the per-agent expanded flag. `StatusDot` for status, `Tooltip` with the supplemental
delay for full memory paths. Keep `agent-meta` as the outer test id.

### Definition of Done

- Parser unit tests: an init with all fields populates them; an init without them (older CLI,
  another harness) yields empty defaults; `memory_paths` map → values list; `source` optional.
- Sidecar round-trip test for the inventory snapshot; a sidecar without it reads as absent.
- `Sidebar.test.ts`: collapsed line text with counts and the needs-auth count; expanded sections
  present/absent by data; `needs-auth` → warning dot with label; unknown status string → warning dot
  with that string; "as of" shown only when rehydrated; a Codex agent with empty inventory renders
  no environment row.
- Existing chip tests updated to the new row.
- Docs: G14 marked closed (M5 does the writing).

---

## Milestone 4 — Context breakdown

### Goal & Outcome

The user can see what is occupying a Claude agent's context window, per category and per item.

- The context meter on a Claude card shows a chevron; clicking it (or "Context breakdown…" in the
  agent menu) opens the panel. First open shows an empty state with "Analyze context".
- Running it on an idle agent fills the panel in about a second. On a busy agent the button says it
  is queued and the panel fills when the current turn finishes.
- The panel shows the model, used/window tokens, a meter per category in the CLI's order, and
  collapsible per-item sections: MCP tools grouped by server, custom agents, memory files, skills.
- No row appears in the transcript for the report — live or after reopening the project.
- After reopening, the panel shows the last report taken, marked "as of" its time.
- A report the parser cannot read still opens the panel with the raw text and a note, and the turn
  is not marked failed.
- Codex and Antigravity cards show no chevron and no menu item.

### Implementation Outline

Follow the compaction plan's milestone structure — capability + adapter + parser, then dispatcher,
then app command + frontend — and mirror its tests one for one where the shape is the same.

**Capability and adapter (decision 16).** `supports_context_report` in `switchboard_core` beside
`supports_manual_compaction`, with the same fake-success rationale. `HarnessAdapter::context_report`
with the `UnsupportedOperation` default; Codex and Antigravity adapter tests assert the refusal.
Claude: `Invocation::Context`, the `compaction_argv_matches_send_argv_except_the_positional` test
extended to cover it, fail-closed on a missing session file, `StreamMode::ContextReport`.

**Parser (decision 18).** In `StreamMode::ContextReport` the stream parser takes the `result` text
and produces the completion event carrying `ContextReport` (or `unparsed` + raw). The markdown parser
lives in the harness crate, is pure, and is tested against a recorded fixture of the full report plus
edge fixtures: empty MCP section, approximate skill tokens, an unknown category row (kept, rendered by
name), a table with a missing column (row skipped, report still `parsed`). The `AdapterEvent` /
`NormalizedEvent` gain `ContextReport { agent_id, report }`.

**Dispatcher (decision 15).** `WorkPayload::ContextReport`, `TurnKind::ContextReport`; every
`TurnKind::Compaction` arm gains the sibling with the same behaviour except that the frontend is told
`pending_kind: "context_report"`; the mock adapter gains a `context_report` scenario. Tests mirror the
compaction set: runs at once when idle, queues behind a running turn and an earlier send, journals
nothing on complete/fail/cancel, waiters resolve idle, not removable as a queued message, unsupported
harness fails the message without starting a turn.

**Disk parser (decision 19).** `SystemMarker::ContextReport { report: ContextReport }` in
`transcript.rs`; `finish_pending_local_command` routes a pair whose command record names `/context`
to it, re-using the markdown parser; a fixture of a session file with a `/context` pair pins that no
agent turn is produced and the marker carries the parsed report. The merge treats it like every
`Turn::System` (never a send slot).

**App (decision 15, 17).** `context_report_agent_impl` with the three gates `compact_agent_impl`
has; `#[tauri::command]` shim; `AppError` variants for unsupported / no session, worded for the user.

**Frontend (decision 17, 20).** `PendingSend.kind` gains `"context_report"`; `dispatchContextReport`
beside `dispatchCompaction`; the unified view renders no row for the pending entry and `turn_start`
appends no agent turn for the kind; the completion event lands in
`AgentRuntime.last_context_report` (+ `_as_of`); on hydrate the latest `context_report` marker fills
it when absent; `UnifiedTranscript` renders nothing for the marker kind. The panel is a new component
taking the report and the busy/queued state; the context meter's chevron and the menu item open it.

**Live drift guard.** `live_claude_context_report_parses`: a fresh "ack" turn, then a
`context_report` on that session; asserts `parsed`, the category rows present, and used/window
tokens positive. One extra local command, no extra model call.

### Definition of Done

- Harness: capability test; Codex and Antigravity refusal tests; Claude argv test; markdown parser
  fixtures above; stream-parser test that a `ContextReport` mode turn completes carrying the report
  and that a garbled result completes with `unparsed`.
- Disk parser: the `/context` pair fixture → marker, no agent turn; an unrelated local-command pair
  still → completed turn.
- Dispatcher: the mirrored compaction set.
- App: gates (unsupported harness, no session, unmaterialized fork) and the happy path against the
  mock.
- Frontend: reducer tests for the pending kind (no row, no turn), the completion event, and hydrate
  fill-if-empty from the marker; panel component tests for empty state, parsed report rendering
  (category meters, grouped MCP section), unparsed fallback, queued button state; Sidebar tests for
  chevron/menu presence gated on harness.
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
    second event, the full key list and which render, the per-model window's model-gating and
    live-only label; fix "5-hour + weekly `overageResetsAt`" (it is the overage credit window).
  - §3 metadata table: the inventory fields now surfaced and persisted; G14 → ✅ closed with the
    as-of convention named; a new row for the context breakdown (Claude ✅ on demand; Codex /
    Antigravity ❌ — same hazard as §3.9).
  - §3.9 or a sibling §3.10: the `/context` protocol, cost (none), session-file footprint, and the
    disk-parser routing; name the live test.
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
- The per-model window's model label is live-only; after reload it reads "Weekly · model-specific".
- Which models the per-model window covers is a server-side allowlist the stream never names.
- The context report is a scraped markdown format; the raw-text fallback is the safety net, and the
  live test is the tripwire.
- Each report writes three records into the agent's session file; the CLI's own TUI shows them on
  resume. Same trade compaction makes.
- Memory *files* are named only by the report; `init` gives the memory directory.

## Out of scope (deliberately)

- Rendering the three non-Claude-Code window keys, or any key not in decision 4's map.
- A minute ticker for reset text.
- Running the report automatically (per turn, or on open). It is on demand; the panel says how old it
  is.
- Fast-mode state, `capabilities`, and the telemetry flags on `init` — internal, no user meaning.
- Per-turn cache-hit ratios, per-model cost on multi-model turns, subagent stats, time-to-first-token
  — all present on `result`, none asked for.
