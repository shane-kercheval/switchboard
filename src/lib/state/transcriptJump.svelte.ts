/// Cross-component jump requests: the transcript navigator (app header) asks a
/// specific pane's transcript to bring one block to the top of its view.
///
/// A store request rather than component refs: the navigator and the mounted
/// `UnifiedTranscript` instances are three layers apart (App → TranscriptPanes
/// → per-pane transcript), and the codebase's idiom for such cross-surface
/// signals is a small reactive store (`gitRefresh`, `revealProjectBranch`).
/// Addressing is by pane: exactly one instance executes a request, which makes
/// consumption race-free.
///
/// Requests are **consumed** by the executing instance. This matters for the
/// reveal path: jumping into a minimized pane first restores it (`revealPane`),
/// which mounts a fresh transcript instance — that instance picks the pending
/// request up on mount. Without consumption, the request would linger and
/// re-execute every time that pane remounts later (minimize/restore cycles).

import type { AgentId, ProjectId } from "$lib/types";
import { layoutFor, revealPane, type PaneId } from "$lib/state/transcriptPanes.svelte";

type JumpRequest = {
  /// Monotonic sequence; bumps per request so an addressed instance can react.
  seq: number;
  projectId: ProjectId | null;
  paneId: PaneId | null;
  /// UnifiedRow key — grouping-independent (a fan-out is one block in a full
  /// pane but plain rows in a single-recipient pane; row keys are stable
  /// across both). The executing instance resolves the containing block.
  rowKey: string | null;
};

export const jumpRequest = $state<JumpRequest>({
  seq: 0,
  projectId: null,
  paneId: null,
  rowKey: null,
});

/// The navigator overlay's open flag. Lives here (not as `App` local state) so
/// both the ⌘F shortcut and the command-palette entry — which live in `App` —
/// and the header button can drive one source of truth, the same pattern as
/// `commandPalette`'s `open`.
export const navigatorState = $state<{ open: boolean }>({ open: false });

export function openNavigator(): void {
  navigatorState.open = true;
}

export function toggleNavigator(): void {
  navigatorState.open = !navigatorState.open;
}

/// Ask the transcript mounted for `paneId` to bring the block containing `rowKey` to the top.
/// The caller is responsible for revealing the pane first (`revealPane`) —
/// a minimized pane has no mounted transcript to execute the request.
export function requestJump(projectId: ProjectId, paneId: PaneId, rowKey: string): void {
  jumpRequest.seq += 1;
  jumpRequest.projectId = projectId;
  jumpRequest.paneId = paneId;
  jumpRequest.rowKey = rowKey;
}

/// Mark the current request handled (called by the executing instance).
export function consumeJump(seq: number): void {
  if (jumpRequest.seq !== seq) return;
  jumpRequest.projectId = null;
  jumpRequest.paneId = null;
  jumpRequest.rowKey = null;
}

/// Which pane a jump should land in, per the navigator rules: an agent's rows
/// render only in its own pane (membership is exclusive), so its id resolves
/// uniquely; a user row renders in every recipient's pane, and the leftmost
/// wins so exactly one predictable pane moves per click. Eye-hidden members
/// don't count — their rows aren't rendered in the pane, so a jump there would
/// land on nothing. `null` = no visible pane hosts the message (agent
/// unassigned or hidden); the navigator renders those entries disabled.
export function resolveJumpPane(
  projectId: ProjectId,
  rosterIds: AgentId[],
  agentIds: AgentId[],
): PaneId | null {
  const layout = layoutFor(projectId, rosterIds);
  for (const pane of layout.panes) {
    if (agentIds.some((id) => pane.members.includes(id) && !pane.hidden.includes(id))) {
      return pane.id;
    }
  }
  return null;
}

/// The navigator's one entry point: resolve the pane, reveal it (restore a
/// minimized pane; replace the maximized pane — `revealPane`'s existing
/// targeting semantics), then request the jump. The freshly-revealed pane's
/// transcript picks the request up on mount. Returns false when no visible
/// pane hosts the message.
export function jumpToRow(
  projectId: ProjectId,
  rosterIds: AgentId[],
  agentIds: AgentId[],
  rowKey: string,
): boolean {
  const paneId = resolveJumpPane(projectId, rosterIds, agentIds);
  if (paneId === null) return false;
  revealPane(projectId, rosterIds, paneId);
  requestJump(projectId, paneId, rowKey);
  return true;
}

export const _testing = {
  reset(): void {
    jumpRequest.seq = 0;
    jumpRequest.projectId = null;
    jumpRequest.paneId = null;
    jumpRequest.rowKey = null;
    navigatorState.open = false;
  },
};
