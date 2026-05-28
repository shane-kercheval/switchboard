# Transcript preview / collapse

**Status:** proposed, awaiting review.
**Branch:** new worktree off `m4-dispatcher-contention-cancel`.

## Goal & scope

Add a "preview" mode for transcript messages that collapses them to a few lines with a gradient fade, hides tool calls, and shows a per-message expand/collapse toggle. A global toggle in the app header switches all messages into or out of preview mode at once. The feature keeps the user in control: the global mode sets the default; per-message overrides let them expand or collapse individual turns without disturbing the rest.

**In scope:** per-message collapse (preview height + gradient fade + tool-call suppression), per-message toggle button, global toggle in the header, live-streaming turn exemption.

**Out of scope:** persisting preview preference across app restarts, collapsing user messages differently from agent messages, any changes to `ToolCallWidget`'s own expand/collapse behavior.

---

## Milestone 1 — Global preview state

### Goal & Outcome

- `ui.transcriptPreview: boolean` and `ui.userOverrides: Record<string, boolean>` exist in the shared `ui` state object in `src/lib/state/index.svelte.ts`. Both default to `false` / `{}`.
- `_testing.reset()` resets both to their defaults.
- No UI yet — this milestone is pure state plumbing.

### Implementation Outline

Extend the `ui` state object (currently `{ lastRecipientId: AgentId | null }`) with two new fields:
- `transcriptPreview: boolean` defaulting to `false` — the global "all messages in preview mode" flag.
- `userOverrides: Record<string, boolean>` defaulting to `{}` — per-message explicit overrides keyed by turn/row key.

No new functions needed; callers mutate these directly, the same way `ui.lastRecipientId` is set. Update `_testing.reset()` to reset both fields.

**Why both fields in the shared state module, not local to `UnifiedTranscript`:** The global toggle button lives in `App.svelte` (a sibling of `UnifiedTranscript`, not a parent). Lifting both fields to `ui` eliminates prop threading and gives `App.svelte` direct access to clear `userOverrides` when the global toggle fires or the active project changes. This is the established pattern (`lastRecipientId`).

**Project-switch clearing:** In `App.svelte`, add a `$effect` watching `selection.activeProjectId` that sets `ui.userOverrides = {}`. This ensures overrides from one project don't persist when switching to another. `unregisterAgents` is not the right hook — it fires on directory removal, not project switching within a directory.

### Definition of Done

- `ui.transcriptPreview` and `ui.userOverrides` are accessible from any component that imports the state module.
- `_testing.reset()` resets both fields.
- The `index.svelte.ts` state tests verify the reset covers both new fields.
- A project-switch (simulated via `selection.activeProjectId` change) clears `userOverrides`.

---

## Milestone 2 — Per-message preview UI

### Goal & Outcome

- Every completed agent turn and user message in the transcript has a per-message expand/collapse toggle button rendered next to the copy button in the message's meta row.
- When a turn is in preview mode: only the first ~5 lines of text are visible, the bottom fades out with a gradient, tool calls are not rendered, and a placeholder indicates hidden tool calls for tool-only turns.
- When expanded: full content is shown, tool calls render normally.
- The toggle is hidden (like the rest of the meta row) until the message is hovered or focused.
- Live-streaming turns are never collapsed regardless of any state.

### Implementation Outline

**`isPreview` helper.** A function `isPreview(key: string, streaming: boolean): boolean`. The streaming exemption is structurally enforced as the outer guard:

```
streaming ? false : (ui.userOverrides[key] ?? ui.transcriptPreview)
```

This is a hard rule — streaming always wins, even if a per-message override is set. The formula must not be written as `ui.userOverrides[key] ?? (ui.transcriptPreview && !streaming)`, which would allow an override to bypass the guard.

**`toggleTurn` helper.** Flips `ui.userOverrides[key]` relative to the current `isPreview(key, false)` result. Keeping it simple — always set the explicit value — is fine.

**Updating `messageMeta`.** Add a `key: string` parameter to the `messageMeta` snippet, defaulting to `""` to avoid breaking callsites that don't need the toggle. When `key` is non-empty, render a chevron toggle button alongside the copy button. The button calls `toggleTurn(key)` and uses `ICON_BUTTON_CLASS` from `iconButton.ts`.

**Streaming gate at the callsite, not in the snippet.** The `agentRow` callsite passes `key = ""` when `turn.status === "streaming"` — an empty key suppresses the toggle button. The snippet itself has no streaming context; this is intentional. Update all `messageMeta` callsites in `agentRow` and `userMessage` snippets to pass the row/turn key (or `""` for streaming). Fanout column callsites pass the column's agent turn's `turn_id` (readable from `col.rows`). Outcome rows pass no key (they're too short to need preview).

**Icon and direction.** Use a chevron that points down when expanded and up when collapsed (or whichever direction the implementer judges most natural — match the pattern already used in `Sidebar.svelte`'s per-agent collapse chevrons). This is an area where the implementing agent should use their best judgment or make a recommendation; visual direction can be adjusted in review.

**Preview container.** In `agentRow` (and `userMessage`), wrap the content area in a container div that conditionally applies preview styles. When `isPreview(key, streaming)` is true:
- `max-height: 7rem` with `overflow: hidden`.
- `mask-image: linear-gradient(to bottom, black 5.5rem, transparent 7rem)`.

Use absolute length stops, not percentages. Percentage stops scale with the container height — a 2-line message would render half-faded. Absolute stops mean: content up to 5.5rem renders fully opaque (short messages are never faded), the fade transition runs 5.5–7rem, and content beyond 7rem is clipped by `overflow: hidden`. Tailwind v4 supports `[mask-image:...]` arbitrary values; a short inline style is also fine.

**Tool call suppression.** In the `turnBody` snippet, gate `ToolCallWidget` rendering on `!isPreview(turn.turn_id, turn.status === "streaming")`. Text chunks always render. The `StatusChip status="processing"` chip also renders in preview mode — it's the primary visual signal for a streaming turn.

**Tool-only turn placeholder.** When a turn is in preview mode and contains no text items (only tool calls), the content area would otherwise be empty — the user has no indication anything is hidden. In this case, render a small muted label in the body (e.g., `{n} tool call(s)`) so the message doesn't look blank. The precise wording and styling should match the app's existing `text-muted text-xs` tone; the implementing agent should make a reasonable choice here.

**Global toggle in App.svelte.** In the header bar (around line 387, just before the `{#if showAgentsToggle}` block), add a button that:
- Only renders when `activeProject !== null && !settingsOpen`.
- On click: flips `ui.transcriptPreview` and clears `ui.userOverrides = {}`.
- Wrapped in `Tooltip.svelte` (already in the component library) with a label that reflects the current state (e.g., `"Preview messages"` / `"Expand messages"`).
- Uses `ICON_BUTTON_CLASS` for consistent sizing and hover style.

**Icon choice.** The implementing agent should use their best judgment. The `Sidebar.svelte` collapse-all button (lines 96–122) uses a stacked-arrows SVG for "collapse/expand all" and is the closest existing precedent. An Eye/Eye-Slash (show/hide), a lines-with-fold glyph, or an adaptation of the sidebar's arrows are all reasonable starting points. Make a recommendation in the PR and iterate from there.

### Definition of Done

- A `[data-testid="turn-preview-toggle"]` button is present on each completed agent turn and user message and is keyboard-accessible.
- Clicking the per-message toggle expands a previewed turn and collapses an expanded turn, without affecting other turns.
- Enabling global preview collapses all non-streaming turns; streaming turns stay expanded.
- Tool calls are absent from the DOM when a turn is in preview mode.
- A tool-only turn in preview mode renders a visible placeholder indicating hidden tool calls.
- Disabling global preview (global toggle) restores all turns to full view and clears per-message overrides.
- Switching active projects clears per-message overrides.
- Tests in `UnifiedTranscript.test.ts`:
  - Preview mode hides tool call widgets.
  - Per-message toggle expands a single previewed turn; others stay previewed.
  - Streaming turns are never previewed regardless of `ui.transcriptPreview` value — including when `ui.userOverrides[key]` is explicitly set to `true` for that turn (covers the structural guard).
  - Global toggle clears per-message overrides.
  - Tool-only turn in preview mode renders a placeholder.
  - Fanout columns: each column collapses/expands independently.
