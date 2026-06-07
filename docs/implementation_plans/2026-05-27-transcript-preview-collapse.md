# Transcript preview / collapse

**Status:** proposed, aligned for implementation.
**Branch:** `transcript-preview-collapse`.

## Goal & scope

Add a project-scoped compact transcript mode that makes long conversations easier to scan without hiding the current work. In compact mode, older completed transcript units collapse to a short faded preview, tool calls are hidden behind a small placeholder, and users can expand/collapse individual messages or fan-out response columns. The latest completed agent response remains expanded by default. While agents are streaming, their live output is bounded in height so multiple active agents cannot overwhelm the transcript, then the completed latest response expands fully.

**In scope:**

- Project-scoped compact state for the current app session.
- Header-level compact/expanded toggle for the active project.
- Per-message expand/collapse controls for completed user messages and completed standalone agent responses.
- Per-column expand/collapse controls for fan-out response columns.
- Fan-out group-level expand/collapse-all-responses control.
- Completed-turn preview styling with gradient fade.
- Tool call suppression and hidden-tool placeholder in compact completed turns.
- Live-streaming response height cap with internal bottom-pinning while streaming.
- Latest completed agent response exception.

**Out of scope:**

- Persisting compact state across app restarts.
- Changing `ToolCallWidget`'s own expand/collapse behavior.
- Backend, transcript model, or IPC changes.
- Collapsing queued rows or failed/cancelled outcome-only rows.

## User experience

When a project is open in Projects view, the title bar shows a compact transcript icon near the right side, next to the agents-sidebar toggle. It does not render in Settings, Git view, no-project, loading, or no-agent states. Toggling it affects only the active project.

When compact mode is off, completed transcript content behaves as it does today.

When compact mode is on:

- Older completed user messages and completed agent responses collapse to a readable preview of roughly a few lines.
- The preview clips with a soft bottom fade.
- Tool calls are not rendered in collapsed completed responses.
- A response with hidden tools but no visible answer text shows a small muted placeholder such as `2 hidden tool calls`.
- Copy buttons still copy the full copyable content, not just the visible preview.
- The latest completed agent response remains expanded by default. If the latest send was fan-out, all completed agent columns in that latest fan-out group remain expanded by default.
- Manual per-message or per-column overrides win over the default latest-response rule.

While an agent is streaming, its response is not converted into the faded completed preview. Instead, the live body renders normally inside a capped region, for example `max-height: min(600px, 60vh)`, with internal overflow and bottom-pinning so the latest activity stays visible. The working/quiet/cancel affordances remain visible. When streaming completes, the live cap is removed and that response becomes the latest completed agent response, expanded by default.

Fan-out groups have two levels of control:

- Each agent response column can expand/collapse independently.
- The fan-out group has a group-level control that expands or collapses all agent response columns in that group. If any column is expanded, the group control collapses all responses; if all columns are collapsed, it expands all responses.

## Current-code alignment

The original plan assumed a shared `ui` object in `src/lib/state/index.svelte.ts`. That no longer matches the codebase: `index.svelte.ts` owns agent runtime/transcript state (`transcripts`, `runtimes`, listeners, hydration, heartbeats). Compact transcript state should live in a small dedicated frontend state module instead of being mixed into agent runtime state.

Current relevant structure:

- `src/App.svelte` owns the title-bar controls and active-project selection.
- `src/lib/state/workspace.svelte.ts` owns `selection.activeProjectId`.
- `src/lib/components/UnifiedTranscript.svelte` renders standalone rows, fan-out groups, message meta controls, tool widgets, and live working/cancel footers.
- `src/lib/state/unified.ts` builds visible row/group structures. Fan-out groups are already represented as one user row plus per-agent columns, so compact controls should follow those visible units.

## Milestone 1 - Project-scoped compact state

### Goal & outcome

Create session-only compact transcript state keyed by project id. Project A can be compact while Project B remains expanded. Switching projects restores that project's compact state and per-message overrides for the current app session.

### Implementation outline

Add `src/lib/state/transcriptPreview.svelte.ts` with a project-keyed state shape:

```ts
type TranscriptPreviewProjectState = {
  enabled: boolean;
  overrides: Record<string, boolean>;
};
```

Expose helpers rather than encouraging direct mutation:

- `stateFor(projectId: ProjectId): TranscriptPreviewProjectState`
- `isCompact(projectId: ProjectId, key: string, defaultCompact: boolean): boolean`
- `toggleKey(projectId: ProjectId, key: string, defaultCompact: boolean): void`
- `setProjectCompact(projectId: ProjectId, enabled: boolean): void`
- `setManyOverrides(projectId: ProjectId, keys: string[], compact: boolean): void`
- `clearProjectOverrides(projectId: ProjectId): void`
- `_testing.reset(): void`

`setProjectCompact` should clear that project's overrides. Project switching should not clear state; state is intentionally project-scoped. App restart resets everything because the state is in-memory only.

### Definition of done

- Compact state is keyed by `ProjectId`.
- Toggling one project does not affect another project.
- Per-key overrides are scoped to their project.
- `_testing.reset()` clears all preview state.
- Unit tests cover project isolation and override reset.

## Milestone 2 - Completed transcript compact UI

### Goal & outcome

Completed transcript units collapse in compact mode, except for the latest completed agent response set. Users can expand/collapse individual messages and response columns.

### Implementation outline

Pass the active `projectId` into `UnifiedTranscript` from `App.svelte`. This avoids reading workspace selection inside the transcript component and keeps the component explicit.

Define stable preview keys for visible units:

- User row: `user:${row.key}`
- Standalone agent row: `agent:${turn.turn_id}`
- Fan-out agent column: `fanout:${send_id}:${agent_id}`

Determine default compactness per visible unit:

- Compact mode disabled: default expanded.
- Compact mode enabled: completed user messages and completed agent responses default compact.
- Latest completed agent response set defaults expanded.
- Streaming rows do not use completed-preview compactness.
- Queued rows and outcome-only rows do not get compact toggles.

The "latest completed agent response set" is the last completed agent response in the rendered active-project transcript. For a fan-out send, treat all completed agent columns in that latest fan-out group as the latest set.

Add compact controls to `messageMeta`:

- Render next to existing copy/timestamp/model/effort controls.
- Use `ICON_BUTTON_CLASS` and `Tooltip`.
- Use lucide icons from `@lucide/svelte` where possible.
- Keep the control hover/focus-revealed like existing meta actions.
- Use `data-testid="turn-preview-toggle"` for individual message/column controls.

Add completed-preview body styling:

- Use a wrapper around the message/response body.
- When compact: `max-height` around `7rem`, `overflow: hidden`, and an absolute-stop mask gradient such as `linear-gradient(to bottom, black 5.5rem, transparent 7rem)`.
- Use absolute stops rather than percentages so short messages do not fade unnecessarily.

Tool call behavior in compact completed turns:

- Suppress `ToolCallWidget` rendering when the owning completed unit is compact.
- Text answer chunks still render inside the compact wrapper.
- Thinking/reasoning widgets should be hidden in compact mode with other non-answer detail; expanding restores them.
- If a compact response has hidden tools and no visible answer text, render a muted placeholder such as `2 hidden tool calls`.

### Definition of done

- Header compact mode collapses older completed messages/responses only for the active project.
- Latest completed standalone agent response stays expanded by default.
- Latest completed fan-out response columns stay expanded by default.
- Manual individual toggles override the default for their message/column.
- Tool calls and thinking widgets are absent from compact completed responses.
- Tool-only compact responses show a hidden-tools placeholder.
- Copy behavior remains unchanged.

## Milestone 3 - Live streaming cap

### Goal & outcome

Streaming responses remain visible without taking over the transcript. The live body is height-capped and internally follows the latest activity; once the turn completes, it becomes the latest completed response and expands fully.

### Implementation outline

Add a live-body wrapper for streaming agent responses:

- `max-height: min(600px, 60vh)` or a nearby value that feels right in the app.
- `overflow-y: auto`.
- Bottom-pin the internal live body when new content arrives, similar in spirit to the transcript-level auto-pin behavior.
- Keep `workingFooter` and live cancel controls visible outside the scrollable capped body or otherwise reachable without scrolling through all live output.

For fan-out, apply the live cap per streaming column so one verbose agent does not distort the whole group.

Do not apply completed-preview fade or completed tool suppression to streaming content. Streaming tools/text should render normally inside the live cap.

### Definition of done

- Streaming standalone responses are height-capped while streaming.
- Streaming fan-out columns are height-capped independently.
- New streamed content remains visible near the bottom of the capped region.
- Cancel/working/quiet affordances remain visible and usable.
- On completion, the live cap is removed and the response is expanded as the latest completed response.

## Milestone 4 - Fan-out group controls

### Goal & outcome

Users can expand/collapse all agent responses in a fan-out group without touching each column one at a time.

### Implementation outline

Render a group-level response control in the fan-out group's top row, aligned with the shared user-message/header area and visually consistent with other hover/focus transcript controls.

Behavior:

- Collect the preview keys for the fan-out group's agent columns.
- If any controlled column is expanded, clicking collapses all controlled columns.
- If all controlled columns are compact, clicking expands all controlled columns.
- The group control affects only agent response columns, not the shared user message.
- Use `data-testid="fanout-preview-toggle-all"`.

The group-level control can sit near the shared user message's meta controls or in the fan-out group's header row. Prefer the least visually noisy placement after implementation review in the running UI.

### Definition of done

- Fan-out columns can still be expanded/collapsed independently.
- A group-level control expands/collapses all response columns for one fan-out group.
- Group-level toggling writes per-column overrides and does not affect other fan-out groups.
- The control is keyboard-accessible and discoverable through tooltip/aria-label.

## Milestone 5 - Header control and integration tests

### Goal & outcome

The active project's compact mode is controllable from the header and covered by focused tests.

### Implementation outline

In `App.svelte`, render the compact transcript button only when:

- `selection.activeProjectId !== null`
- `!settingsOpen`
- `view.mode !== "git"`
- the roster is loaded
- the active project has at least one agent

Place it near the existing right-side title-bar controls, before the agents-sidebar toggle. Wrap with `Tooltip`; use `data-testid="transcript-compact-toggle"` and `data-tauri-no-drag`.

Icon recommendation:

- Prefer lucide `Rows3`, `ListCollapse`, `Minimize2`, or `Maximize2` depending on available exports and visual fit.
- Use the icon state to indicate compact vs expanded mode.

### Definition of done

Tests should cover:

- Project-scoped compact state isolation.
- Header toggle affects only the active project.
- Header toggle clears that project's per-key overrides.
- Older completed messages compact when enabled.
- Latest completed standalone response stays expanded.
- Latest completed fan-out response columns stay expanded.
- Individual message/column toggle affects only that unit.
- Fan-out group toggle expands/collapses only that fan-out's agent columns.
- Compact completed responses hide tool calls and thinking widgets.
- Tool-only compact responses show a hidden-tools placeholder.
- Streaming responses use live cap, not completed preview.
- Streaming completion removes the live cap and leaves the latest response expanded.

Run the focused frontend tests first, then the broader checks required for the touched surface:

- `pnpm test -- UnifiedTranscript`
- `pnpm test -- App`
- `pnpm test -- transcriptPreview`
- `make lint`

