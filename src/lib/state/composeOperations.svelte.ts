/// Project-scoped lifecycle state for a compose operation that awaits.
///
/// **Why this exists.** Three send paths have an `await` between submit and
/// dispatch — an ordinary saved-prompt send, a prompt fork, and a plain fork —
/// and the ComposeBar that starts one is remounted on every project switch
/// (`{#key projectId}`), so the operation routinely outlives its own component.
/// Everything it needs to coordinate with *whichever* composer is mounted when it
/// finishes therefore cannot live in component-local `$state`:
///
/// - **Single-flight.** A component's own `sending` flag resets to `false` in a
///   replacement bar, which showed the same prompt with an enabled Send.
///   Submitting again sent it twice; pressing Fork instead sent it to the parent
///   *and* to a new branch — one action, two sends, two agents.
/// - **Outcome delivery.** A `sendError` written by a destroyed instance is
///   invisible: a failed send looked like nothing had happened.
/// - **Reconciliation.** When an operation clears the composer it consumed, a
///   composer mounted in the meantime holds its own copy of that content and
///   would go on displaying a message that was already sent.
/// - **Ownership.** Every continuation must be able to ask "is this still mine?"
///   after each await, because the user can now abandon a stuck one.
///
/// **One claim per project, and it is the sole busy authority.** The compose bar
/// derives its busy state from this module alone rather than also keeping a local
/// flag: two representations of "someone owns this composer" is the bug this
/// whole feature kept producing. It follows that only one operation can be in
/// flight per project, so no two can race for the targeting lock or the store.
///
/// **This is not a second source of truth for compose content.** `composeStore`
/// stays authoritative for what the composer holds; this module holds lifecycle
/// metadata only. An operation's captured payload rides in the async closure that
/// owns it.

import type { AgentId, ProjectId } from "$lib/types";

/// `rendering` — building the message; nothing durable exists yet, so an
/// operation here can still abort cleanly.
/// `awaiting_user` — parked on a browser sign-in. The wait is unbounded (the
/// backend's credential commit is deliberately un-timed, so a stall here would
/// otherwise hold the composer forever), which is why this is the phase that
/// offers abandonment, and the one phase where pane targeting stays live.
/// `registering` — the branch is being created; once it succeeds the send is
/// committed and dispatch is no longer optional.
/// A phase is an object rather than a bare string so the data that belongs to one
/// phase can only exist in it: the provider name is meaningful exactly while
/// parked on a sign-in, and a mounted composer needs it to name what it is
/// waiting for — including a composer that did not start the wait.
export type ComposeOperationPhase =
  | { name: "rendering" }
  | { name: "awaiting_user"; provider: string }
  | { name: "registering" };

/// Phases are per-kind: a plain fork has nothing to render, and only a fork
/// registers. Making the invalid combinations unrepresentable is cheaper than
/// documenting them.
export type ComposeOperation =
  | {
      id: string;
      kind: "prompt_send";
      phase: Extract<ComposeOperationPhase, { name: "rendering" | "awaiting_user" }>;
    }
  | { id: string; kind: "prompt_fork"; sourceId: AgentId; phase: ComposeOperationPhase }
  | {
      id: string;
      kind: "plain_fork";
      sourceId: AgentId;
      phase: Extract<ComposeOperationPhase, { name: "registering" }>;
    };

export type ComposeOperationInit =
  | { kind: "prompt_send" }
  | { kind: "prompt_fork"; sourceId: AgentId }
  | { kind: "plain_fork"; sourceId: AgentId };

/// A finished operation's message for the composer that is mounted *now*, which
/// may not be the one that started it. `tone` selects the composer's error vs.
/// notice treatment; the text is already user-facing.
export type ComposeOutcome = {
  id: string;
  message: string;
  tone: "error" | "notice";
};

const operations = $state<Record<ProjectId, ComposeOperation | undefined>>({});
const outcomes = $state<Record<ProjectId, ComposeOutcome | undefined>>({});
/// Bumped each time an operation clears the compose store out from under a
/// possibly-mounted composer. Read reactively; the value itself is meaningless.
const consumed = $state<Record<ProjectId, number>>({});

export function operationFor(projectId: ProjectId): ComposeOperation | undefined {
  return operations[projectId];
}

/// Whether `id` still owns the project's slot. **Every continuation must ask this
/// after every await, before any side effect** — rendering again, registering a
/// branch, dispatching, clearing or reconciling compose state, moving the
/// recipient selection, or publishing an outcome. Abandonment releases the slot
/// while the underlying call is still running, so a late success that skips this
/// check acts on a composer it no longer owns.
export function ownsOperation(projectId: ProjectId, id: string): boolean {
  return operations[projectId]?.id === id;
}

/// Claim the project's compose slot, or `null` when one is already in flight —
/// the caller must refuse rather than start a second.
export function beginOperation(projectId: ProjectId, init: ComposeOperationInit): string | null {
  if (operations[projectId] !== undefined) return null;
  const id = crypto.randomUUID();
  operations[projectId] =
    init.kind === "plain_fork"
      ? { id, kind: init.kind, sourceId: init.sourceId, phase: { name: "registering" } }
      : init.kind === "prompt_fork"
        ? { id, kind: init.kind, sourceId: init.sourceId, phase: { name: "rendering" } }
        : { id, kind: init.kind, phase: { name: "rendering" } };
  outcomes[projectId] = undefined;
  return id;
}

/// Move a claimed operation to another of *its kind's* phases. A no-op if the
/// slot has since been taken or abandoned, so a stale continuation cannot
/// resurrect one, and a no-op for a phase the kind doesn't have.
export function setOperationPhase(
  projectId: ProjectId,
  id: string,
  phase: ComposeOperationPhase,
): void {
  const current = operations[projectId];
  if (current?.id !== id) return;
  switch (current.kind) {
    case "prompt_send":
      if (phase.name === "registering") return;
      operations[projectId] = { ...current, phase };
      return;
    case "prompt_fork":
      operations[projectId] = { ...current, phase };
      return;
    case "plain_fork":
      if (phase.name !== "registering") return;
      operations[projectId] = { ...current, phase };
      return;
  }
}

/// Give up waiting on an operation without cancelling the work behind it.
///
/// Deliberately not called "cancel": the backend call keeps running and may still
/// succeed — the credential commit behind a sign-in is unbounded precisely so it
/// can't report a failure it didn't have. What this does is stop the *composer*
/// waiting, so the project becomes usable again; the abandoned continuation then
/// fails its `ownsOperation` check and acts on nothing.
export function abandonOperation(
  projectId: ProjectId,
  id: string,
  outcome?: Omit<ComposeOutcome, "id">,
): void {
  if (operations[projectId]?.id !== id) return;
  operations[projectId] = undefined;
  outcomes[projectId] = outcome === undefined ? undefined : { id, ...outcome };
}

/// Release the slot and publish the outcome (if any) for the mounted composer.
export function finishOperation(
  projectId: ProjectId,
  id: string,
  outcome?: Omit<ComposeOutcome, "id">,
): void {
  if (operations[projectId]?.id !== id) return;
  operations[projectId] = undefined;
  outcomes[projectId] = outcome === undefined ? undefined : { id, ...outcome };
}

/// The pending outcome, if any. Read reactively so a composer already on screen
/// notices one published *after* it mounted — the common case, since a failure
/// usually lands once the user has navigated back.
export function outcomeFor(projectId: ProjectId): ComposeOutcome | undefined {
  return outcomes[projectId];
}

/// Take the outcome and clear it in one step.
///
/// **Deliver-once, not a persistent fallback.** Rendering straight from this map
/// left a stale failure on screen through every later action and every revisit of
/// the project. Consuming it into the mounted composer's own status makes it
/// behave like any other message: shown once, replaced by the next thing.
export function takeOutcome(projectId: ProjectId): ComposeOutcome | undefined {
  const outcome = outcomes[projectId];
  if (outcome !== undefined) outcomes[projectId] = undefined;
  return outcome;
}

/// Drop a published outcome without reading it — a new operation supersedes it.
export function clearOutcome(projectId: ProjectId): void {
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
