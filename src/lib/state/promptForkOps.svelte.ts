/// Project-scoped lifecycle state for a prompt-mode fork send.
///
/// **Why this exists at all.** A prompt fork spans two awaits — rendering the
/// prompt (which can open a browser for an MCP sign-in and wait minutes) and
/// registering the branch. The ComposeBar that started it is remounted on every
/// project switch (`{#key projectId}`), so the operation routinely outlives its
/// own component. Everything the operation needs to coordinate with *whichever*
/// composer is mounted when it finishes therefore cannot live in component-local
/// `$state`:
///
/// - **Single-flight.** `sending` is component-local, so a replacement composer
///   starts at `false` and would happily submit a second fork while the first is
///   still rendering — two branches from one parent, each carrying a message the
///   user believes they sent once.
/// - **Outcome delivery.** A `sendError` written by a destroyed instance is
///   invisible. A failed fork would look like nothing happened.
/// - **Reconciliation.** When the operation clears the composer it consumed, a
///   composer mounted in the meantime holds its own copy of that content in
///   local state and would go on displaying a prompt that was already sent.
///
/// **This is not a second source of truth for compose content.** `composeStore`
/// stays authoritative for what the composer holds; this module holds only
/// lifecycle metadata (which phase, which outcome, and a counter saying "the
/// store changed underneath you"). The operation's captured payload rides in the
/// async closure that owns it, not here.

import type { AgentId, ProjectId } from "$lib/types";

/// `rendering` covers the prompt render and any sign-in detour — nothing durable
/// exists yet, so this phase can still abort cleanly. `registering` means the
/// branch is being created; from the moment it succeeds the send is committed.
export type PromptForkPhase = "rendering" | "registering";

export type PromptForkOperation = {
  id: string;
  phase: PromptForkPhase;
  sourceId: AgentId;
};

/// A finished operation's message for the composer that is mounted *now*, which
/// may not be the one that started it. `tone` selects the composer's error vs.
/// notice treatment; the text is already user-facing.
export type PromptForkOutcome = {
  id: string;
  message: string;
  tone: "error" | "notice";
};

const operations = $state<Record<ProjectId, PromptForkOperation | undefined>>({});
const outcomes = $state<Record<ProjectId, PromptForkOutcome | undefined>>({});
/// Bumped each time an operation clears the compose store out from under a
/// possibly-mounted composer. Read reactively; the value itself is meaningless.
const consumed = $state<Record<ProjectId, number>>({});

export function promptForkOperation(projectId: ProjectId): PromptForkOperation | undefined {
  return operations[projectId];
}

/// Claim the project's prompt-fork slot. Returns the operation id, or `null` when
/// one is already in flight — the caller must refuse rather than start a second.
///
/// **This is the authority; the composer's busy state is its projection.** In
/// practice a second submit never gets here, because the compose bar derives
/// "busy" from `promptForkOperation` and disables itself first. Keep both: the
/// projection is what the user sees, this is what makes the invariant true for
/// any future caller that doesn't consult it.
export function beginPromptFork(projectId: ProjectId, sourceId: AgentId): string | null {
  if (operations[projectId] !== undefined) return null;
  const id = crypto.randomUUID();
  operations[projectId] = { id, phase: "rendering", sourceId };
  outcomes[projectId] = undefined;
  return id;
}

/// Move a claimed operation to its next phase. A no-op if the slot has since been
/// taken by a different operation, so a stale continuation cannot resurrect one.
export function advancePromptFork(projectId: ProjectId, id: string, phase: PromptForkPhase): void {
  const current = operations[projectId];
  if (current?.id !== id) return;
  operations[projectId] = { ...current, phase };
}

/// Release the slot and publish the outcome (if any) for the mounted composer.
export function endPromptFork(
  projectId: ProjectId,
  id: string,
  outcome?: Omit<PromptForkOutcome, "id">,
): void {
  if (operations[projectId]?.id !== id) return;
  operations[projectId] = undefined;
  outcomes[projectId] = outcome === undefined ? undefined : { id, ...outcome };
}

export function promptForkOutcome(projectId: ProjectId): PromptForkOutcome | undefined {
  return outcomes[projectId];
}

/// Drop a published outcome — the user has acted again, so the message is stale.
export function clearPromptForkOutcome(projectId: ProjectId): void {
  outcomes[projectId] = undefined;
}

/// Signal that the compose store was reset by an operation rather than by the
/// mounted composer, so that composer must re-read it instead of persisting its
/// own now-stale locals back over the reset.
export function markComposerConsumed(projectId: ProjectId): void {
  consumed[projectId] = (consumed[projectId] ?? 0) + 1;
}

export function composerConsumedCount(projectId: ProjectId): number {
  return consumed[projectId] ?? 0;
}

export const _testing = {
  reset(): void {
    for (const key of Object.keys(operations)) delete operations[key];
    for (const key of Object.keys(outcomes)) delete outcomes[key];
    for (const key of Object.keys(consumed)) delete consumed[key];
  },
};
