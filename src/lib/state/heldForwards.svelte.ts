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
import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";

/// One forward source the user picked: always a single agent whose latest output
/// gets forwarded. Agents are the first-class unit everywhere — picking a *pane*
/// is a selection convenience that expands to one source per member agent at pick
/// time (see `forwardSourceAgentsForPane`), so a pane is never stored or displayed
/// as a chip. `name` drives the chip and the "waiting for {name}…" label.
export type ForwardSource = { id: AgentId; name: string };

/// Stable identity for dedup / removal / list keys — the agent id.
export function forwardSourceKey(source: ForwardSource): string {
  return source.id;
}

/// Build an agent forward source. Shared by every forward surface so the shape
/// has one definition.
export function forwardSourceForAgent(agent: AgentRecord): ForwardSource {
  return { id: agent.id, name: agent.name };
}

/// Expand a pane to one forward source per *currently-live* member agent, in pane
/// member order (a member removed before pick simply drops out). This is the only
/// place a pane meets forwarding: callers add the returned sources individually
/// (deduped against what's already attached), so no pane entity is ever stored.
export function forwardSourceAgentsForPane(
  pane: TranscriptPane,
  agents: AgentRecord[],
): ForwardSource[] {
  return pane.members
    .map((id) => agents.find((a) => a.id === id))
    .filter((a): a is AgentRecord => a !== undefined)
    .map(forwardSourceForAgent);
}

/// The agent ids a set of sources forwards from, de-duplicated and in order. This
/// is what the dispatch/backend sees.
export function expandForwardSources(sources: ForwardSource[]): AgentId[] {
  const ids: AgentId[] = [];
  for (const source of sources) if (!ids.includes(source.id)) ids.push(source.id);
  return ids;
}

/// Reconcile persisted forward sources against the live roster, for restore.
///
/// A source names an agent that may have been removed or renamed since the draft
/// was written. Removed agents are dropped — forwarding from them would fail at
/// dispatch. Survivors take the roster's *current* name, because the chip's `name`
/// is display-only and a stale one would show the user an agent that no longer
/// exists under that label.
export function reconcileForwardSources(
  sources: readonly ForwardSource[],
  agents: readonly AgentRecord[],
): ForwardSource[] {
  return sources
    .map((source) => agents.find((agent) => agent.id === source.id))
    .filter((agent): agent is AgentRecord => agent !== undefined)
    .map(forwardSourceForAgent);
}

/// `reconcileForwardSources` across a per-field map, dropping fields left empty so
/// a restored draft carries no keys for arguments whose every source is gone.
export function reconcileForwardSourceMap(
  map: Readonly<Record<string, ForwardSource[]>>,
  agents: readonly AgentRecord[],
): Record<string, ForwardSource[]> {
  const out: Record<string, ForwardSource[]> = {};
  for (const [field, sources] of Object.entries(map)) {
    const kept = reconcileForwardSources(sources, agents);
    if (kept.length > 0) out[field] = kept;
  }
  return out;
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

/// The canonical manual-forward sentinel (`docs/workflow-spec.md` §`send`). The
/// transcript uses this to mark a message as a forward **durably** — derived from
/// the body that the journal persists, so the styling survives reload without a
/// live marker store. Forward-only on purpose: it drives the manual-forward
/// `data-forwarded` marker, which a workflow aggregation should not trip.
///
/// SYNCHRONIZED WITH THE BACKEND WIRE SHAPE: this must match the string emitted by
/// `crates/harness/src/forward.rs` (`compose_forwarded_message`). The broader
/// *banding* matcher — `QUOTED_BLOCK_SENTINEL` / `QUOTED_BLOCK` in
/// `UnifiedTranscript.svelte`, which also covers the `response from` aggregation
/// shape from `crates/workflow/src/template.rs` — is the presentation concern and
/// lives in the component. Change a sentinel on either language → change both.
export const FORWARD_SENTINEL = /^=== START forwarded from .+ ===$/m;

/// Test-only reset.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(held)) delete held[key];
    for (const key of Object.keys(captions)) delete captions[key];
  },
};
