// The compose recipient set, per project — the **single source of truth for
// "who receives the send."**
//
// Pane targeting (header click, Cmd+click, `@panename`, Cmd+Alt+N) and the
// pane coverage borders are all lenses over this one value: gestures *write*
// it, visuals *derive* from it. There is deliberately no stored
// "targeted pane" / "docked pane" anywhere — a second stored representation of
// the target can drift from the real one (drop one chip and a stale pane id
// still highlights the whole pane), and a targeting cue that can lie causes
// the mis-sends the pane UI exists to prevent. If a feature wants to remember
// a target, it derives it from this set instead.
//
// Hoisted out of ComposeBar (which still owns initialization from the
// persisted compose snapshot, pruning, and write-through persistence) so the
// pane layer can read and write the set without a parallel state.
//
// **Two write paths, one lock.** The prompt send renders via an IPC await and
// re-checks the selection afterwards, silently aborting if a captured
// recipient left the set — so ComposeBar freezes its own recipient mutations
// while that render is in flight (`sending`). Pane-targeting gestures live
// outside ComposeBar and must honor the same freeze, or a pane click during
// the render window silently drops the send. Hence:
//   - `targetRecipients` — the **user-targeting** path (every pane gesture);
//     refused while the project's targeting is locked.
//   - `setRecipients` — the **raw** path for internal reconciliation
//     (ComposeBar's mount seed, stale-agent pruning, and its own
//     `sending`-guarded gestures). Pruning must bypass the lock: if a captured
//     recipient is *removed* mid-render, the prune firing is exactly what lets
//     the post-render abort check correctly cancel the send.

import type { AgentId, ProjectId } from "$lib/types";

const store = $state<Record<ProjectId, AgentId[]>>({});

// Not reactive: read only inside event handlers at write time, never rendered.
const targetingLocked: Record<ProjectId, boolean> = {};

/// The project's current recipient set ([] when none).
export function selectionFor(projectId: ProjectId): AgentId[] {
  return store[projectId] ?? [];
}

/// Raw replace — internal reconciliation only (see module comment). User
/// gestures go through `targetRecipients`.
export function setRecipients(projectId: ProjectId, ids: AgentId[]): void {
  store[projectId] = ids;
}

/// Replace the recipient set from a user-targeting gesture (pane header,
/// Cmd+click, `@panename`, Cmd+Alt+N) with the pane's full member list
/// (replace semantics — same meaning as `@agentname`). Refused (returns
/// false) while targeting is locked for the project.
export function targetRecipients(projectId: ProjectId, ids: AgentId[]): boolean {
  if (targetingLocked[projectId] === true) return false;
  store[projectId] = ids;
  return true;
}

/// Add one agent to the recipient set — the "added an agent to a pane" gesture
/// reflecting the new member as a selected compose chip. Lock-aware like
/// `targetRecipients` (refused mid-render); a no-op when already selected.
export function selectAgent(projectId: ProjectId, agentId: AgentId): boolean {
  if (targetingLocked[projectId] === true) return false;
  const current = store[projectId] ?? [];
  if (!current.includes(agentId)) store[projectId] = [...current, agentId];
  return true;
}

/// Remove one agent from the recipient set — the "removed an agent from a pane"
/// gesture deselecting its compose chip. Lock-aware; a no-op when not selected.
export function deselectAgent(projectId: ProjectId, agentId: AgentId): boolean {
  if (targetingLocked[projectId] === true) return false;
  const current = store[projectId] ?? [];
  if (current.includes(agentId)) store[projectId] = current.filter((id) => id !== agentId);
  return true;
}

/// Freeze / unfreeze user targeting for a project (the prompt-render window).
/// Callers MUST guarantee release (try/finally + unlock on unmount): a stuck
/// lock would silently disable pane targeting for the project forever — a
/// strictly worse failure than the silent send-drop the lock prevents.
export function setTargetingLocked(projectId: ProjectId, locked: boolean): void {
  if (locked) {
    targetingLocked[projectId] = true;
  } else {
    delete targetingLocked[projectId];
  }
}

/// Test-only reset.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(store)) delete store[key];
    for (const key of Object.keys(targetingLocked)) delete targetingLocked[key];
  },
};
