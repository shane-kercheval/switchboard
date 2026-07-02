# Claude session-file & stream-parser fidelity

Consolidates two open Claude-parser bugs and a small UX cleanup of the warning surface they produce. Supersedes [`2026-05-24-subagent-rendering-fidelity.md`](2026-05-24-subagent-rendering-fidelity.md) (M1 below lifts that plan's recommendation; original kept for investigation provenance).

## Why this exists

Switchboard's whole value is faithful per-agent attribution. The Claude adapter currently has **two distinct parser-fidelity bugs**, each of which makes the on-screen transcript diverge from reality:

1. **Stream parser mis-attributes subagent work to the parent turn** ([§M1](#milestone-1--stream-parser-honors-parent_tool_use_id-claude)). A delegating turn's live view shows the subagent's internal tool calls as the parent's own; the rehydrated view shows only the `Agent` call. Live ≠ rehydrated. Probed against Claude 2.1.149/2.1.150.
2. **Session-file parser silently drops tool output for out-of-order records** ([§M2](#milestone-2--session-file-parser-tolerates-out-of-order-tool-records-claude)). Claude 2.1.150 can write a `tool_result` to disk **before** its matching `tool_use` (observed ~1s gap in [session `22300f1b-…`](https://example.invalid) at lines 1406/1408 and 1607/1609). Our single-forward-pass parser can't bind them and silently drops the `tool_result`'s output, then surfaces a `ParseWarning`. On rehydration, the affected tool calls render with empty results.

Both are **Claude-only** and live in the harness adapter. They share state (the per-turn tool-use index) and a fix shape (late binding of `tool_result` to `tool_use`), so they consolidate cleanly. M3 cleans up the **warning surface itself** — the user's screen showed an unstyled native browser tooltip; replacing it with the project's `Tooltip` primitive is small, but belongs here since the same component lights up when M1/M2 fire (and they will fire less, but never zero, after these fixes).

The warning surface is **kept and made better**, not removed: it's the project's drift-detection mechanism for upstream CLI changes — the same role live tests play (see `AGENTS.md` "Live testing against real harnesses"). Hiding it would mask future regressions silently.

## Required reading (before implementing)

- [`2026-05-24-subagent-rendering-fidelity.md`](2026-05-24-subagent-rendering-fidelity.md) — the original M1 investigation: ground-truth probes, cross-harness verification, exact stream shapes. **M1 below lifts its recommendation; read the source for the evidence.**
- [`../research/archive/claude-code-cli-observed.md`](../research/archive/claude-code-cli-observed.md) §"Subagent (`Agent` tool) representation" — frozen probe provenance.
- [`../research/harness-behavior.md`](../harness-behavior.md) §1 (failures) and §3.1 (parity matrix) — Claude session-file is class-A for assistant content, which makes these correctness bugs (parity exists; we're dropping it).
- `crates/harness/src/parser.rs` — the **stream** parser (`parse_line`, `parse_assistant_envelope`, `parse_user_envelope`) M1 touches.
- `crates/harness/src/claude_code/session_file.rs` — the **session-file** parser (`handle_tool_result` at `:307-353`, the turn builder) M2 touches.
- `crates/harness/src/transcript.rs` — `ParseWarning` shape and contract.
- `src/lib/components/Sidebar.svelte:192-205` — the warning render site M3 replaces; `src/lib/components/ui/Tooltip.svelte` is the primitive M3 reuses.

## Principles established here (reused across milestones)

- **Adapter-layer only.** Both fixes live in `crates/harness/src/{parser.rs,claude_code/session_file.rs}`. The dispatcher and `commands.rs` stay harness-agnostic — no `match harness {…}` for parser-fidelity work.
- **Live == rehydrated.** A delegating turn renders the same way during streaming and after a project reopen. A turn with out-of-order session-file records renders with full tool output on rehydration, no warnings, no data loss.
- **Warnings stay user-visible** — they're our reverse drift-detector for CLI changes (a new upstream record shape or a regressed ordering invariant produces a warning, and a user telling us about it is the same signal as a live test catching it). Closing M1/M2 reduces the warning count to ~0 for healthy sessions; the surface persists so future drift is still visible.
- **Verbatim probe evidence.** Each fix lands with a recorded fixture from the actual upstream version it was probed against (Claude 2.1.149/2.1.150), so future regressions can be re-detected against the same shape.

---

## Milestone 1 — Stream parser honors `parent_tool_use_id` (Claude)

### Goal & Outcome

A Claude turn that delegates via the `Agent` tool renders the same way in the live stream and after a project reopen: one `ToolStarted{Agent}` + one `ToolCompleted` at the parent turn, with the subagent's internal `Bash`/`Read`/etc. calls **not** mis-attributed to the parent. Matches Gemini's `invoke_agent` treatment (one call/result pair, opaque) and Antigravity's `invoke_subagent` treatment (a separate `brain/<uuid>` conversation). Per-harness uniformity: **from the parent's view, a delegation is a single tool call.**

Outcomes:
- A delegating Claude turn's live transcript shows exactly the `Agent` `ToolStarted`/`ToolCompleted` pair — no `Bash`/`Read`/etc. attributed to the parent turn.
- A delegating Claude turn's rehydrated view (unchanged today — disk side already collapses) **matches** the live view.
- Cross-harness uniformity confirmed (Gemini/Antigravity already do this; Codex has no delegation surface — see [`2026-05-24-subagent-rendering-fidelity.md`](2026-05-24-subagent-rendering-fidelity.md) §"Cross-harness verification").

### Implementation Outline

**The bug** ([`2026-05-24-subagent-rendering-fidelity.md`](2026-05-24-subagent-rendering-fidelity.md) §"What was verified" carries the full evidence — read it for the stream shapes):

```
assistant  parent_tool_use_id=null         tool_use  name=Agent  (toolu_017e…)   ← parent
assistant  parent_tool_use_id=toolu_017e…  tool_use  name=Bash   (toolu_01UE…)   ← SUBAGENT's call  ← MIS-ATTRIBUTED
user       parent_tool_use_id=toolu_017e…  tool_result          (Bash output)    ← SUBAGENT's result ← MIS-ATTRIBUTED
user       parent_tool_use_id=null         tool_result          (Agent aggregate)← report to parent
```

`crates/harness/src/parser.rs` never reads `parent_tool_use_id`, so both subagent-internal records emit `ToolStarted`/`ToolCompleted` at the parent's `turn_id`.

**The fix.** In `parse_line`, read top-level `parent_tool_use_id` (sibling of `type`) **before** dispatching to envelope parsers. When non-null, short-circuit with `ParseOutcome::Skip` — the parent turn's transcript should reflect only the parent's own events plus the `Agent` aggregate result. The `Agent` `tool_use` (parent-owned, `parent_tool_use_id=null`) and its aggregate `tool_result` (also `parent_tool_use_id=null`) remain — that's the single tool-call pair the user sees.

**Disk-side check (no change expected).** The main `<session>.jsonl` already contains only the parent's `Agent` `tool_use` + aggregate `tool_result`; subagent internals live in `~/.claude/projects/<encoded-cwd>/<session-id>/subagents/agent-<id>.jsonl` and we don't read those. The rehydrated view is already the desired collapsed shape. **Verify with a fixture test** before declaring "no disk change needed" — guard against a future regression where Claude inlines subagent records.

**Skip-other-additive-record-types confirmation.** During a subagent run, new `system` subtypes appear (`task_started`, `task_notification`, `status`). Today `parse_system_event` only acts on `subtype == "init"` and skips others — verify with a fixture, no code change needed unless the verification fails. Also confirm the main-file record types `ai-title`, `attachment`, `last-prompt`, `queue-operation` skip cleanly in `session_file.rs`.

**Cross-cutting (`match harness` discipline).** Fix is in the Claude stream parser. No dispatcher branching; Gemini/Antigravity/Codex parsers untouched (they already produce the target shape — verified 2026-05-24).

### Definition of Done

- **Fixture tests** (no live harness needed):
  - A recorded fixture of a delegating Claude turn with parent-tagged subagent events → parser emits `ToolStarted{Agent}` + `ToolCompleted` for the parent turn and **zero** subagent-internal `ToolStarted`/`ToolCompleted` events. Capture the fixture from the probe script in [`2026-05-24-subagent-rendering-fidelity.md`](2026-05-24-subagent-rendering-fidelity.md) §"Reproducible probe (Claude)".
  - Session-file fixture (a recorded main `<session>.jsonl` for a delegating turn) → rehydrated turn shows `Agent` call + aggregate result, matching the live view (regression guard against future inlining).
  - `parse_system_event` skips `task_started`/`task_notification`/`status` cleanly (no warnings, no events).
  - `session_file.rs` skips `ai-title`/`attachment`/`last-prompt`/`queue-operation` records cleanly.
- **Live test** (`#[ignore]`-gated, named `live_claude_subagent_collapses_to_parent_tool_call`, run via `make test-live-claude`):
  - Run the probe prompt ("Use the Agent tool to launch exactly one general-purpose subagent…"); assert the parent turn emits one `ToolStarted{Agent}` + one `ToolCompleted`, and **no** subagent-internal tool events at the parent turn.
- **Cross-check:** `grep parent_tool_use_id` returns hits in `parser.rs` (read site) but **not** in `dispatcher/` / `commands.rs`.
- **Docs:** [`harness-behavior.md`](../harness-behavior.md) updated — note Claude subagent collapsing as the target shape (matches Gemini/Antigravity); `system-design.md` §9 updated: `Task` → `Agent`, "spawn as expected" qualified with the representation note.
- **Out of scope (record):** v2 nested/labeled "expand this delegation" UI is deferred. v1 deliberately collapses a delegation to one tool call. The data supports v2 later (stream has `parent_tool_use_id`; disk has `subagents/*.jsonl`).

---

## Milestone 2 — Session-file parser tolerates out-of-order tool records (Claude)

### Goal & Outcome

Claude 2.1.150 can write a `tool_result` to disk **before** its matching `tool_use` (~1s gap observed). Our parser is single-forward-pass and silently drops the `tool_result`'s output when this happens. Fix: late-bind `tool_result` records when their `tool_use_id` hasn't been seen yet, so the resulting `TurnItem::Tool` carries the real output on rehydration.

Outcomes:
- A session file with an out-of-order `tool_result → tool_use` pair rehydrates with the tool output text correctly bound to the tool call.
- No `ParseWarning` for the out-of-order case — it's now a tolerated, expected pattern (the warning was correctly flagging the data loss, and we're removing the data loss).
- The warning surface still fires for **genuinely** unmatched records (a `tool_result` whose `tool_use_id` never appears anywhere in the turn) — those are still drift signals worth surfacing.

### Implementation Outline

**The bug.** `crates/harness/src/claude_code/session_file.rs:307-353` (`handle_tool_result`) iterates `builder.items` looking for a `TurnItem::Tool` with matching `tool_use_id`. If not found, it pushes a `ParseWarning` and returns — the `tool_result`'s `content` is dropped. Observed pattern (Claude 2.1.150, session `22300f1b-…`):

| File line | Type | Timestamp | Tool ID |
|---|---|---|---|
| 1406 | `user` / `tool_result` | 21:19:55.735Z | `toolu_01HCfsmCs5RXwbgwxigD9Aid` ← arrives first |
| 1408 | `assistant` / `tool_use` | 21:19:55.724Z (11ms earlier) | `toolu_01HCfsmCs5RXwbgwxigD9Aid` ← arrives second |

The two records pair correctly (`parentUuid` chain confirms it), and the `tool_use`'s timestamp predates the `tool_result`'s by 11ms — so the file is not strictly time-ordered.

**The fix — late binding via a deferred-results queue on `ReconstructionState`.** Add `pending_tool_results: Vec<DeferredToolResult>` to `ReconstructionState` (**not** to `AgentTurnBuilder` — the builder doesn't exist between turns, so a tool_result arriving before its turn's first assistant record would otherwise fall through to the "no open agent turn" warning path and lose the output). Each entry carries `(tool_use_id, output, is_error, completed_at, line_number)`.

Both failure paths in today's `handle_tool_result` (`session_file.rs:307-353`) — the `current_agent.is_none()` case at `:322-327` ("no open agent turn") *and* the `!matched` case at `:348-353` ("did not match any open tool") — push to the queue instead of warning. The case distinction disappears: both are "we haven't seen the matching `tool_use` yet."

Binding happens in `handle_assistant`'s `tool_use` block branch (`:278-298`): after appending a `TurnItem::Tool`, drain any `pending_tool_results` entries whose `tool_use_id` matches the just-appended item and apply their `output`/`is_error`/`completed_at`. (Same equality check as today's match loop, inverted: queue is the source, the new builder item is the destination.)

**Restructure the loader/finalize flow so finalize-time warnings survive.** Today `load_claude_transcript` (`session_file.rs:110-112`) does `let warnings = state.warnings.clone(); let mut transcript = state.finalize(); transcript.warnings = warnings;` — `finalize` consumes `state`, so any warnings pushed inside `finalize` or `close_current_agent` are silently dropped. Change `finalize` to attach `state.warnings` to the returned transcript before returning, and drop the caller-side clone+assign. At the end of `finalize` (after the last `close_current_agent`), iterate remaining `pending_tool_results` entries and push a `ParseWarning` for each (`"tool_result for {id} never matched a tool_use"`) — these are the genuinely unmatched cases, now correctly surfaced to the caller.

This preserves the warning surface for real drift while eliminating the false positive for the out-of-order pattern. The queue is per-session (`ReconstructionState` lifetime), bounded by the file's tool count, and adds no measurable cost.

**Scope discipline.** Claude session-file parser only. The stream parser is unaffected (live events arrive strictly in time order — the disk re-ordering is a write-side artifact, not a stream-side one). Codex/Gemini/Antigravity session-file parsers untouched unless verification turns up the same pattern (M2's DoD live test catches the Claude case).

**Honest limitation to record in a code comment** at the queue site: "Claude Code 2.1.150 can write `tool_result` records before their matching `tool_use` on disk (observed ~1s gap). The deferred queue tolerates arbitrary ordering within a session file; unmatched entries surface as warnings only at finalize."

### Definition of Done

- **Fixture tests** (assert on the **returned `LoadedTranscript`**, not on intermediate `state.warnings` — the loader path is what production goes through, and the finalize-warning attachment is part of what's being fixed):
  - A session-file fixture with an out-of-order `tool_result → tool_use` pair (same turn, builder exists) → rehydrated `TurnItem::Tool` carries the full `output` and `is_error`, and **no** `ParseWarning` on the returned transcript.
  - A session-file fixture where the `tool_result` arrives **before any assistant record exists for the turn** (no `current_agent` at queue time, then `handle_assistant` creates the builder and the matching `tool_use` arrives) → same result: bound output, zero warnings. This is the case-(a) regression guard — the lifecycle case the builder-scoped queue would have missed.
  - A session-file fixture with a `tool_result` whose `tool_use_id` never appears anywhere → exactly **one** `ParseWarning` **on the returned transcript** (regression guard for both genuine drift and the finalize-warning-drop bug being fixed in the same milestone).
  - The order-preserved case (tool_use before tool_result, today's healthy pattern) continues to bind directly without queueing — no behavioral change.
  - Capture the fixture from the affected session file (lines around 1400-1410 of session `22300f1b-…`); record file path + line numbers in the fixture's header comment so a future engineer can re-derive.
- **Live test** (`#[ignore]`-gated, named `live_claude_tool_results_bind_after_restart`, run via `make test-live-claude`):
  - Run a prompt that triggers several Read/Bash tool calls in sequence; quit Switchboard; rehydrate; assert every `TurnItem::Tool` has non-empty `output` and **no** parse warnings on the loaded transcript. This is the end-to-end TUI-parity check.
- **Negative-case check:** corrupting the captured fixture by removing the `tool_use` record entirely → exactly one warning produced (genuine miss); the corresponding `TurnItem` is absent (no orphan with empty output).
- **Docs:**
  - [`harness-behavior.md`](../harness-behavior.md) — add a row under §1 or §3 noting Claude 2.1.150 out-of-order disk writes as a tolerated pattern; reference this milestone as the fix.
  - [`harness-update-review.md`](../research/harness-update-review.md) §3 — add "session-file ordering invariant" to the dependency surface, so future version bumps are checked against this assumption.
  - Code comment at the queue site recording the v2.1.150 evidence + the "if a future version crosses turn boundaries" caveat.

---

## Milestone 3 — Warning surface UX cleanup

### Goal & Outcome

The transcript-warning indicator in the agent sidebar uses the native HTML `title=` attribute today, so warning lists render as an unstyled browser tooltip (visible in the user's screenshot — black box, default monospace, no formatting). Replace with the project's `Tooltip` primitive and render each warning as a list row instead of newline-joined text. Cosmetic but frequently-visible — these warnings persist for the lifetime of a project session and are the first thing a user sees when something parses oddly.

The warning **count and label stay**; only the hover surface changes. The surface remains prominent — it's our drift-detection signal.

### Outcomes

- Hovering the "N transcript warnings" indicator shows the project's themed `Tooltip` (consistent with other hover surfaces in the app), not a raw browser tooltip.
- Each warning renders as a discrete list row inside the tooltip (`line N: <reason>`), with the line number visually distinct (monospaced or styled).
- The indicator and its testid (`agent-parse-warnings`) keep their current names so existing tests still target them.

### Implementation Outline

**Site.** `src/lib/components/Sidebar.svelte:192-205`. Replace the `title={runtime.parse_warnings.map(...).join("\n")}` pattern with a `Tooltip` from `src/lib/components/ui/Tooltip.svelte`. The trigger is the existing `⚠ {N} transcript warning(s)` text; the content is a small `<ul>` (or styled `<div>` stack), one row per warning with `line_number` rendered in `font-mono` and `reason` in default text.

**Extend `Tooltip.svelte` with a backwards-compatible `children` slot.** Today the primitive only accepts `label: string` (`Tooltip.svelte:10-19`) and renders `<div class="text-[13px] font-medium">{label}</div>` (`:37`). All four existing callers (`SettingsButton`, `ComposeBar` ×3, `SidebarToggleButton`) pass `label` as a string. To support the warning-row list, change `label: string` → `label?: string`, add `children?: Snippet` to `Props`, and replace the content `<div>` with:

```svelte
{#if children}
  {@render children()}
{:else if label}
  <div class="text-[13px] font-medium">{label}</div>
{/if}
```

Existing callers continue to work unchanged (label mode). The warning-list use site passes its `<ul>` as the default slot (`children` mode). `shortcut` remains meaningful only in `label` mode — document this with a one-line comment on the prop (slot mode owns its own layout).

**Reuse the existing semantic tokens** (`text-warning`, `text-xs`, `font-mono`) — no new tokens, no new shared component.

**Keep responsive sizing reasonable** — a transcript with 50 warnings shouldn't render a 1000px-tall tooltip. Cap visible rows at ~10 with a "+ N more" footer if exceeded (warnings beyond the cap remain in the data; just not rendered to avoid a wall of text).

### Definition of Done

- **Component test:** mock a runtime with two `parse_warnings` → the rendered DOM contains a `Tooltip` (not a `title` attribute) keyed to the warning indicator; the tooltip's content includes both `line_number` + `reason` strings rendered as separate elements (children-slot mode).
- **Component test:** mock a runtime with 12 warnings → tooltip shows 10 rows + a "+ 2 more" footer (or the chosen cap behavior).
- **Regression check (Tooltip primitive):** confirm an existing label-only caller (e.g. `SettingsButton` or one of the `ComposeBar` tooltips) still renders the `<div class="text-[13px] font-medium">{label}</div>` content — prefer extending an existing component test for one of these callers over adding a separate primitive-level test, to avoid test sprawl.
- **Manual verification:** in `make dev`, open a project whose Claude session triggers the warning (e.g. the one in the user's screenshot); confirm the tooltip is themed, readable, and lists warnings cleanly.
- **No behavioral regression:** the `agent-parse-warnings` testid still exists; the warning count still renders identically; tooltip activation still works on hover for mouse users (a11y for keyboard users is a `Tooltip` primitive concern — inherit whatever it provides).

---

## Out of scope (do not build)

- **Nested/labeled "expand this delegation" UI** — the data supports it (stream has `parent_tool_use_id`, disk has `subagents/<id>.jsonl`), but v1 deliberately collapses a delegation to one tool call. v2.
- **Reading the subagent sidecar `subagents/<id>.jsonl` files** — same deferral; v1 ignores them.
- **Suppressing the warning surface entirely** — deliberately kept; it's the drift detector for upstream CLI changes.
- **Other harnesses' parser fidelity** — Gemini's `invoke_agent` already-opaque, Antigravity's `invoke_subagent` runs as a separate brain conversation we don't read, Codex has no delegation surface (all verified 2026-05-24). Re-verify only when a CLI version bump per [`harness-update-review.md`](../research/harness-update-review.md) suggests change.
- **Generalizing the deferred-results queue to other parsers** — Codex's `event_msg/mcp_tool_call_end` + `response_item/function_call` pairing has its own ordering invariant (already handled in `codex/session_file.rs`). Don't preemptively unify; revisit only if a similar bug appears.
- **Session-file ordering "fix" upstream** — out of our control. The fix is on our parsing side.
