// The compose recipient set, per project — the **single source of truth for
// "who receives the send."**
//
// Pane targeting (Cmd+click, `@panename`, Cmd+Alt+N) and the
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
// **Two write paths, one derived policy.** A prompt send builds its message
// across an IPC await and re-checks the selection afterwards, refusing (with an
// explanation) if a captured recipient left the set. A pane gesture landing in
// that window would refuse the send for something the user never meant to do,
// so gestures are held off while a send is building its message. That state is
// **not stored here** — it is asked of `composeOperations`
// (`operationBlocksTargeting`), which reports it from the phase of the project's
// in-flight send. It used to be a boolean this module owned and ComposeBar wrote
// from nine places, which is how a remounted bar could freeze targeting on
// behalf of an operation whose only release had already fired. Hence:
//   - `targetRecipients` / `selectAgent` / `deselectAgent` — the
//     **user-targeting** paths (every pane gesture); refused while a send is
//     building its message.
//   - `setRecipients` — the **raw** path for internal reconciliation
//     (ComposeBar's mount seed and stale-agent pruning). Pruning must bypass the
//     policy: if a captured recipient is *removed* mid-render, the prune firing
//     is exactly what lets the post-render check correctly refuse the send.

import type { AgentId, ProjectId } from "$lib/types";
import { operationBlocksTargeting } from "$lib/state/composeOperations.svelte";

const store = $state<Record<ProjectId, AgentId[]>>({});

// Not reactive: read only inside event handlers at write time, never rendered.

/// The project's current recipient set ([] when none).
export function selectionFor(projectId: ProjectId): AgentId[] {
  return store[projectId] ?? [];
}

/// Raw replace — internal reconciliation only (see module comment). User
/// gestures go through `targetRecipients`.
export function setRecipients(projectId: ProjectId, ids: AgentId[]): void {
  store[projectId] = ids;
}

/// Replace the recipient set from a user-targeting gesture (Cmd+click,
/// `@panename`, Cmd+Alt+N) with the pane's full member list
/// (replace semantics — same meaning as `@agentname`). Refused (returns
/// false) while targeting is locked for the project.
export function targetRecipients(projectId: ProjectId, ids: AgentId[]): boolean {
  if (operationBlocksTargeting(projectId)) return false;
  store[projectId] = ids;
  return true;
}

/// Add one agent to the recipient set — the "added an agent to a pane" gesture
/// reflecting the new member as a selected compose chip. Lock-aware like
/// `targetRecipients` (refused mid-render); a no-op when already selected.
export function selectAgent(projectId: ProjectId, agentId: AgentId): boolean {
  if (operationBlocksTargeting(projectId)) return false;
  const current = store[projectId] ?? [];
  if (!current.includes(agentId)) store[projectId] = [...current, agentId];
  return true;
}

/// Remove one agent from the recipient set — the "removed an agent from a pane"
/// gesture deselecting its compose chip. Lock-aware; a no-op when not selected.
export function deselectAgent(projectId: ProjectId, agentId: AgentId): boolean {
  if (operationBlocksTargeting(projectId)) return false;
  const current = store[projectId] ?? [];
  if (current.includes(agentId)) store[projectId] = current.filter((id) => id !== agentId);
  return true;
}

/// Test-only reset.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(store)) delete store[key];
  },
};
