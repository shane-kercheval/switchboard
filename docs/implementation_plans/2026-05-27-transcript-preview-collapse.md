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

When a project is open in Projects view, the title bar shows a compact transcript icon near the right side, next to the agents-sidebar toggle. It does not render in Settings, Git view, no-project, loading, or no-agent states. The header control affects only the active project.

The header control is a normalize/toggle action:

- Expanded with no manual overrides: enable compact mode.
- Compact with no manual overrides: disable compact mode.
- Any state with manual overrides: enable compact mode and clear that project's overrides.

This gives the user a reliable reset after manually opening or closing several transcript units. "Compact mode" still honors the latest-response exception: older eligible transcript units collapse, while the latest completed agent response stays expanded by default.

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
- `normalizeProjectCompact(projectId: ProjectId): void`
- `hasOverrides(projectId: ProjectId): boolean`
- `setManyOverrides(projectId: ProjectId, keys: string[], compact: boolean): void`
- `clearProjectOverrides(projectId: ProjectId): void`
- `_testing.reset(): void`

`setProjectCompact` should set the requested project mode and clear that project's overrides. `normalizeProjectCompact` should implement the header action: if the project has any overrides, set `enabled = true` and clear overrides; otherwise invert `enabled` and clear overrides. Project switching should not clear state; state is intentionally project-scoped. App restart resets everything because the state is in-memory only.

### Definition of done

- Compact state is keyed by `ProjectId`.
- Toggling one project does not affect another project.
- Per-key overrides are scoped to their project.
- `normalizeProjectCompact` enables compact mode and clears overrides whenever overrides are present.
- `_testing.reset()` clears all preview state.
- Unit tests cover project isolation, override reset, and header-normalize behavior.

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

The "latest completed agent response set" is based on completion recency, not rendered transcript order. Among agent turns with `status === "complete"`, choose the turn with the greatest `ended_at ?? started_at`. If that turn has a `send_id` that belongs to a fan-out group, treat all completed agent columns in that fan-out group as the latest set. Failed, cancelled, queued, and streaming rows do not qualify as latest completed responses. While a newer send is still streaming, the previous latest completed response remains expanded and the streaming response uses the live cap.

Add compact controls to `messageMeta`:

- Render next to existing copy/timestamp/model/effort controls.
- Use `ICON_BUTTON_CLASS` and `Tooltip`.
- Use lucide icons from `@lucide/svelte` where possible.
- Keep the control hover/focus-revealed like existing meta actions.
- Use `data-testid="turn-preview-toggle"` for individual message/column controls.
- Make the compact control opt-in at each `messageMeta` call site. User rows, completed standalone agent rows, and fan-out agent columns pass a control; outcome-only and queued rows do not.

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
- Latest completed standalone agent response, selected by terminal recency, stays expanded by default.
- Latest completed fan-out response columns, selected from the latest completed turn's `send_id`, stay expanded by default.
- Manual individual toggles override the default for their message/column.
- Outcome-only and queued rows do not render compact toggles.
- Tool calls and thinking widgets are absent from compact completed responses.
- Tool-only compact responses show a hidden-tools placeholder.
- Copy behavior remains unchanged.

## Milestone 3 - Live streaming cap

### Goal & outcome

Streaming responses remain visible without taking over the transcript. The live body is height-capped and internally follows the latest activity; once the turn completes, it becomes the latest completed response and expands fully.

### Implementation outline

Split streaming rendering into a capped content region and a sibling live footer. Do not wrap the existing whole `turnBody` snippet with the live cap, because `workingFooter` currently includes the working/quiet label and cancel control. The cap applies only to streamed text/tool content; `workingFooter` renders outside the scrollable region for both standalone rows and fan-out columns.

Add a live content wrapper for streaming agent responses:

- `max-height: min(600px, 60vh)` or a nearby value that feels right in the app.
- `overflow-y: auto`.
- `data-testid="turn-live-scroll"` for focused tests and browser inspection.
- Bottom-pin each internal live body when new content arrives.

Each capped live region needs independent pinning state keyed by its streaming unit. The existing outer transcript auto-pin still keeps the transcript near the active rows, but once live content is capped the outer container stops growing with every streamed token/tool update; the inner live region's bottom-pin is therefore required for latest activity to remain visible.

For fan-out, apply the live cap per streaming column so one verbose agent does not distort the whole group.

Do not apply completed-preview fade or completed tool suppression to streaming content. Streaming tools/text should render normally inside the live cap.

### Definition of done

- Streaming standalone responses are height-capped while streaming.
- Streaming fan-out columns are height-capped independently.
- New streamed content remains visible near the bottom of the capped region.
- Cancel/working/quiet affordances render outside `turn-live-scroll` and remain visible and usable.
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

The button should call `normalizeProjectCompact(projectId)`, not blindly invert the boolean. Tooltip and aria label should reflect the action:

- Has overrides: `Reset compact transcript`
- Compact off, no overrides: `Compact transcript`
- Compact on, no overrides: `Expand transcript`

Icon recommendation:

- Prefer lucide `Rows3`, `ListCollapse`, `Minimize2`, or `Maximize2` depending on available exports and visual fit.
- Use the icon state to indicate compact vs expanded mode.

### Definition of done

Tests should cover:

- Project-scoped compact state isolation.
- Header control affects only the active project.
- Header control clears that project's per-key overrides.
- Header control enables compact mode, rather than expanding, when overrides are present.
- Older completed messages compact when enabled.
- Latest completed standalone response is selected by completion recency, not rendered order.
- Latest completed fan-out response columns are selected from the latest completed turn's `send_id`.
- A slow earlier-anchored send that finishes after a later rendered send stays expanded.
- Individual message/column toggle affects only that unit.
- Outcome-only and queued rows do not show compact toggles.
- Fan-out group toggle expands/collapses only that fan-out's agent columns.
- Compact completed responses hide tool calls and thinking widgets.
- Tool-only compact responses show a hidden-tools placeholder.
- Streaming responses use live cap, not completed preview.
- `turn-working` renders outside `turn-live-scroll`.
- Streaming completion removes the live cap and leaves the latest response expanded.

Because the riskiest behavior is visual layout and real scroll behavior, do not rely only on jsdom component tests. Before considering the feature done, run the app and verify in a real browser/in-app browser with:

- Long completed text: compact preview fades and clips without fading short messages.
- Tool-only completed response: compact placeholder is visible.
- Long streaming standalone response: live content is capped, bottom-pinned, and cancel/working controls stay visible.
- Long streaming fan-out responses: each column caps and bottom-pins independently.

Run the focused frontend tests first, then the broader checks required for the touched surface:

- `pnpm test -- UnifiedTranscript`
- `pnpm test -- App`
- `pnpm test -- transcriptPreview`
- `make lint`
