// A per-project "focus the compose box" request signal.
//
// Some pane gestures should also move keyboard focus into the composer so the
// user can start typing without a second click — Cmd+click on a pane targets
// its members *and* means "I'm about to write to these agents." TranscriptPanes
// has no handle on ComposeBar's textarea (the two only ever talk through
// module stores, see recipientSelection), so the gesture bumps this nonce and
// ComposeBar, watching it, focuses its own input.
//
// Kept out of recipientSelection deliberately: that store is strictly "who
// receives the send," and a transient focus intent is a different concern. A
// monotonic nonce (not a boolean) so repeated requests each re-fire the effect.

import type { ProjectId } from "$lib/types";

const nonce = $state<Record<ProjectId, number>>({});

/// The project's focus-request nonce. Reactive: an `$effect` reading it re-runs
/// each time `requestComposeFocus` bumps it.
export function composeFocusNonce(projectId: ProjectId): number {
  return nonce[projectId] ?? 0;
}

/// Request that the project's compose box take keyboard focus. Bumps the nonce
/// so a watching ComposeBar focuses its textarea.
export function requestComposeFocus(projectId: ProjectId): void {
  nonce[projectId] = (nonce[projectId] ?? 0) + 1;
}

/// Test-only reset.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(nonce)) delete nonce[key];
  },
};
