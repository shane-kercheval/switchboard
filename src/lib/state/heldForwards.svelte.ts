// Live state for in-flight manual cross-agent forwards — the "waiting for
// {agent}…" sends the user has submitted but that are still holding for their
// source agents' turns to finish (system-design §7).
//
// Project-keyed and in-memory, like `recipientSelection` and the dispatcher's
// queued sends: it survives the `{#key projectId}` remount of the compose bar /
// transcript and pane navigation, but is **live-UI-only** — a held forward is
// not durable across an app restart (it was never written to the journal; the
// journal begins at turn-start, and a held forward hasn't dispatched yet). This
// matches the agreed durability: lost on restart, not on navigation.
//
// Distinct from the dispatcher's queued sends (`pending_sends` / `queuedSendIds`)
// because a held forward issues **no** `send_message` during the hold — the
// frontend dispatches (through the normal send path) only once `forward_message`
// resolves the composed body — so there is no per-agent pending entry to carry
// it. The hold lives here instead.

import type { AgentId, AgentRecord, ProjectId, SendId } from "$lib/types";
import type { PaneId, TranscriptPane } from "$lib/state/transcriptPanes.svelte";

/// One forward source the user picked. An **agent** forwards that agent's latest
/// output; a **pane** forwards each of its members' (membership snapshotted at
/// pick time) — shown as a single named chip but expanded to agent ids at
/// dispatch (the wire content is always per-agent blocks; the pane grouping is
/// UI-only). `name` drives the chip and the "waiting for {name}…" label for both.
export type ForwardSource =
  | { kind: "agent"; id: AgentId; name: string }
  | { kind: "pane"; paneId: PaneId; name: string; members: AgentId[] };

/// Stable identity for dedup / removal / list keys — the agent id or the pane id.
export function forwardSourceKey(source: ForwardSource): string {
  return source.kind === "agent" ? source.id : source.paneId;
}

/// Build an agent forward source. Shared by both forward surfaces so the shape
/// has one definition.
export function forwardSourceForAgent(agent: AgentRecord): ForwardSource {
  return { kind: "agent", id: agent.id, name: agent.name };
}

/// Build a pane forward source — one chip standing for the pane's members,
/// snapshotted at pick time and restricted to currently-live agents (a member
/// removed before dispatch simply drops out). Shared by both forward surfaces.
export function forwardSourceForPane(pane: TranscriptPane, agents: AgentRecord[]): ForwardSource {
  return {
    kind: "pane",
    paneId: pane.id,
    name: pane.name,
    members: pane.members.filter((id) => agents.some((a) => a.id === id)),
  };
}

/// The agent ids a set of sources forwards from, in declared order and
/// de-duplicated — panes expand to their snapshotted members. This is what the
/// dispatch/backend sees; the pane grouping never reaches the wire.
export function expandForwardSources(sources: ForwardSource[]): AgentId[] {
  const ids: AgentId[] = [];
  for (const source of sources) {
    const members = source.kind === "agent" ? [source.id] : source.members;
    for (const id of members) if (!ids.includes(id)) ids.push(id);
  }
  return ids;
}

/// A submitted-but-still-holding forward. Carries everything needed to render
/// the "waiting for {agent}…" entry and to restore the composer (typed body +
/// source chips + recipients) if the hold is cancelled or invalidated.
export interface HeldForward {
  forwardId: string;
  sendId: SendId;
  /// The user's typed body (no forwarded blocks yet — those are composed by the
  /// backend at dispatch). Restored to the composer verbatim on cancel/invalidate.
  body: string;
  sources: ForwardSource[];
  recipients: AgentId[];
}

/// The partial-empty caption for a dispatched forward, keyed by `send_id`. Set
/// only when ≥1 source was skipped for having no output.
///
/// **Live-only by design.** Unlike the forward *marker* (which the transcript
/// derives from the sentinel lines in the message body — durable across reload),
/// this caption cannot be reconstructed: a skipped source leaves no trace in the
/// wire body, so "X had no output" is unrecoverable after a reload. It's a
/// "resolved while you were away" courtesy, not load-bearing history.
export interface ForwardCaption {
  included: string[];
  skipped: string[];
}

const held = $state<Record<ProjectId, HeldForward[]>>({});
const captions = $state<Record<ProjectId, Record<SendId, ForwardCaption>>>({});

/// The project's in-flight held forwards, in submission order ([] when none).
export function heldForwardsFor(projectId: ProjectId): HeldForward[] {
  return held[projectId] ?? [];
}

/// Register a held forward (on submit). Appends so multiple concurrent holds
/// render in submission order.
export function addHeldForward(projectId: ProjectId, forward: HeldForward): void {
  held[projectId] = [...(held[projectId] ?? []), forward];
}

/// Remove a held forward by id (on dispatch/invalidate/cancel). No-op if absent.
export function removeHeldForward(projectId: ProjectId, forwardId: string): void {
  const current = held[projectId];
  if (!current) return;
  const next = current.filter((f) => f.forwardId !== forwardId);
  if (next.length === 0) {
    delete held[projectId];
  } else {
    held[projectId] = next;
  }
}

/// The partial-empty caption for a dispatched forward's `send_id`, or `undefined`
/// when the forward skipped no sources (the common case).
export function forwardCaptionFor(
  projectId: ProjectId,
  sendId: SendId,
): ForwardCaption | undefined {
  return captions[projectId]?.[sendId];
}

/// Record a partial-empty caption for a dispatched forward. Call only when ≥1
/// source was skipped.
export function setForwardCaption(
  projectId: ProjectId,
  sendId: SendId,
  caption: ForwardCaption,
): void {
  captions[projectId] = { ...(captions[projectId] ?? {}), [sendId]: caption };
}

/// The canonical forwarded-block sentinel (`docs/workflow-spec.md` §`send`). The
/// transcript uses this to mark a message as a forward **durably** — derived from
/// the body that the journal persists, so the styling survives reload without a
/// live marker store.
export const FORWARD_SENTINEL = /^=== START forwarded from .+ ===$/m;

/// Test-only reset.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(held)) delete held[key];
    for (const key of Object.keys(captions)) delete captions[key];
  },
};
