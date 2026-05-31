# Persist compose draft + recipient selection per project

Retain the compose bar's message draft and selected agent chips per project,
surviving both project switches and app restarts. Frontend-only; no Rust/IPC
changes.

## Problem

Both the draft text (`ComposeBar.svelte`, `prompt`) and the recipient chip
selection (`selectedIds`) are local component `$state`. The bar is wrapped in
`{#key selection.activeProjectId}` (`App.svelte`), so every project switch
destroys and recreates it, resetting both to empty. Nothing is persisted to
disk either, so a restart loses them too.

The user expectation: select chips (even without sending), switch projects and
return → the same chips are still selected; and a half-written draft survives
both a switch and a restart.

## Approach

One mechanism covers both requirements: lift the per-project compose state into
a `ProjectId`-keyed store backed by machine-local `localStorage`. Switch
survival = the store outlives the component remount; restart survival = the
store hydrates from `localStorage` on load.

### Why localStorage, not `.switchboard/`

A draft is pre-durable UI ergonomics — the same category as the theme
preference, *not* conversation history. System-design §3 already classes a
queued-but-unstarted send as live-UI-only; a draft is earlier still. `.switchboard/`
is git-tracked, and a half-typed message must not sync to a teammate.
`localStorage` is machine-local and origin-scoped, so `make dev DEV_PORT=…`
instances get isolated drafts for free, consistent with the existing per-port
dev-config isolation.

## Changes

- **`src/lib/state/composeStore.ts` (new).** Plain (non-reactive) module: hydrates
  a `Record<ProjectId, { draft: string; selectedIds?: AgentId[] }>` from
  `localStorage` at load; `getCompose` / `setDraft` / `setSelection` accessors
  with synchronous write-through. Tolerates malformed/partial stored JSON
  (starts empty; filters non-string ids). `_testing.reset` / `reloadFromStorage`
  seams for test isolation and exercising the restart path.
  - `selectedIds === undefined` = "no saved selection → use the default
    recipient"; an explicit `[]` = "user deliberately deselected everyone" and
    is honored on restore. Collapsing the two would lose deselect-all.
- **`src/lib/components/ComposeBar.svelte`.**
  - New required `projectId: ProjectId` prop (keying persistence off the roster
    would break for an empty roster — pass it explicitly).
  - Initialize `prompt` and `selectedIds` synchronously from the saved snapshot.
    The parent only mounts the bar once the roster is loaded and non-empty
    (`App.svelte` guards: loading / empty-roster branches sit above it), so the
    init runs against a populated roster — no first-render-empty window. The
    initial reads use `untrack` to state the single-read is deliberate.
  - `initialSelection` resolves the mount-time recipient set:
    - **Single-agent project → the lone agent**, always. It shows no chips
      (nothing to choose), so a saved empty/stale selection must never leave it
      unsendable with no recovery UI.
    - A deliberate deselect-all (saved `[]`) is honored.
    - A saved selection whose agents were all removed falls back to the first
      agent rather than stranding the composer with no recipient.
    - Otherwise: the saved selection, or (none saved) the first agent.
  - Two write-through `$effect`s: draft on every change; selection only while
    `agents.length > 0`. The parent unmounts the bar when a project loses its
    last agent, so the empty-roster skip is defense-in-depth — it guarantees a
    transient empty roster can't clobber the saved chips with `[]` regardless of
    future changes to the parent's gating.
  - Writes are synchronous (no debounce): drafts are tiny, and a deferred write
    would race a send-clear (resurrecting just-sent text) or a switch (writing
    one project's draft into another's slot).
- **`src/App.svelte`.** Pass `projectId={selection.activeProjectId!}` (non-null
  in this branch). Comment the `{#key}` remount: besides re-seeding compose
  state, it resets `sendError`, the `@`-menu, and focus so one project's compose
  state can't bleed into another.
- **`src/lib/state/index.svelte.ts`.** Remove `ui.lastRecipientId` (and the now-
  empty `ui` object, its write in `dispatchUserTurn`, and its reset). Per-project
  persistence subsumes it: an agent belongs to one project, so a global
  last-recipient id only ever matched its own project's roster, and within that
  project the user's actual selection is now persisted directly. It was dead
  weight, so it's removed rather than left to puzzle a future reader.

## Tests

- **`composeStore.test.ts`:** localStorage round-trip; `undefined` vs `[]`
  distinction; unknown-project empty snapshot; malformed-JSON tolerance;
  non-object/non-string filtering.
- **`ComposeBar.test.ts` (new cases):** draft + chips survive a switch remount;
  restore from a previous session (restart, via `reloadFromStorage`); draft
  cleared on send with no resurrection; deliberate deselect-all persists and
  restores as empty; stale saved recipient dropped on restore; drafts isolated
  per project; single-agent project recovers from a saved-but-gone recipient
  (sendable); all-stale multi-agent selection falls back to the default; a
  `rerender({agents: []})` does not clobber the saved selection (the empty-roster
  guard). Existing cases updated to pass `projectId`; compose store reset between
  tests (and in `App.test.ts`).

## Decisions locked

- localStorage (machine-local, frontend-only), keyed by `ProjectId`.
- Synchronous write-through, no debounce.
- `undefined` vs `[]` is a real distinction (no-selection vs deselect-all).
- Selection persisted only while a roster is present.
- No prune-on-load: drafts are bytes against a megabyte quota, and pruning on a
  possibly-incomplete workspace load risks deleting live drafts. If pruning is
  ever wanted, hook it to an explicit directory removal, not a load.

## Accepted tradeoffs

- **Whole-store re-serialization per keystroke.** Each `setDraft`/`setSelection`
  serializes the entire multi-project store, not just the active draft. Negligible
  at realistic project counts and draft sizes; synchronous-no-debounce is the
  correct correctness call. Noted so the O(store-size)-per-keystroke characteristic
  is a known choice, not a surprise.
- **Orphan entries on project deletion.** `ProjectId`s are never reused, so a
  deleted project leaves its compose entry in `localStorage` indefinitely — slow,
  unbounded, harmless growth (bytes against a multi-MB quota). Future hygiene only;
  if ever pruned, on explicit directory removal (see above), never on load.
