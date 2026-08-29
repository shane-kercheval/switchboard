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
import { answerTextOf } from "./unified";
import type { Turn } from "$lib/state/types";
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

/// Put sources in the live roster's order — the same order as the agent cards —
/// while dropping removed agents, de-duplicating ids, and refreshing display names.
export function orderForwardSources(
  sources: readonly ForwardSource[],
  agents: readonly AgentRecord[],
): ForwardSource[] {
  const sourceIds = new Set(sources.map((source) => source.id));
  return agents.filter((agent) => sourceIds.has(agent.id)).map(forwardSourceForAgent);
}

/// Expand a pane to one forward source per *currently-live* member agent, in live
/// roster order (a member removed before pick simply drops out). This is the only
/// place a pane meets forwarding: callers add the returned sources individually
/// (deduped against what's already attached), so no pane entity is ever stored.
export function forwardSourceAgentsForPane(
  pane: TranscriptPane,
  agents: AgentRecord[],
): ForwardSource[] {
  const memberIds = new Set(pane.members);
  return agents.filter((agent) => memberIds.has(agent.id)).map(forwardSourceForAgent);
}

/// Convert sources to the agent ids the backend sees, in live roster order. A
/// source whose agent was removed before submission is intentionally omitted,
/// matching draft reconciliation and pane expansion.
export function expandForwardSources(
  sources: readonly ForwardSource[],
  agents: readonly AgentRecord[],
): AgentId[] {
  return orderForwardSources(sources, agents).map((source) => source.id);
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
  return orderForwardSources(sources, agents);
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

/// What a forward source will contribute when the send dispatches.
///
/// - `ready`   — the agent is idle with a completed turn; the forward resolves it now.
/// - `pending` — the agent has a turn in flight; the send **holds** for it.
/// - `empty`   — the agent is idle with no forwardable text to contribute;
///               dispatching would **block the whole send** (any empty source
///               invalidates the forward), so the picker flags it first.
export type ForwardReadiness = "ready" | "pending" | "empty";

/// Classify what a source will contribute, from that agent's turns.
///
/// The rule comes from `forward_message_impl` (crates/app/src/commands.rs), which
/// "holds outside any queue while each source agent's current in-flight turn
/// settles, then composes … each source's latest **completed** output."
/// Three consequences the shape of this function depends on:
///
/// 1. **An in-flight turn means `pending` regardless of history.** An agent with an
///    older completed turn *and* a newer streaming one forwards the *new* turn, so
///    it is not ready — the send waits. Readiness is therefore "has completed output
///    AND nothing in flight", not "has completed output". A predicate written as
///    `hasCompleted || isStreaming` gets exactly this case backwards.
/// 2. **`empty` warns of a blocked send.** Any source resolving with no forwardable
///    text invalidates the whole forward at the backend, so this flag is the
///    user's chance to see it before submitting. Advisory only — the backend
///    validator is the single enforcement point; this derivation can be stale.
/// 3. **Completed is not enough — the *newest completed* turn must carry text.**
///    The backend forwards `latest_completed_agent_text`, which is blank for a
///    turn that produced only tool/thinking items, and it reads the newest
///    completed turn — so an older textual completion behind a newer empty one
///    still resolves empty.
///
/// A source whose **newest turn failed or was cancelled is `ready`**, not
/// `empty`: the backend forwards a generated failure note for it
/// (`latest_turn_failure_note`) — non-empty, deliberately forwardable content
/// ("tell the next agent that X failed"), so the send is not blocked.
export function forwardReadiness(turns: readonly Turn[] | undefined): ForwardReadiness {
  const agentTurns = (turns ?? []).filter((turn) => turn.role === "agent");
  if (agentTurns.some((turn) => turn.status === "streaming")) return "pending";
  const newest = agentTurns.at(-1);
  if (newest === undefined) return "empty";
  if (newest.status === "failed" || newest.status === "cancelled") return "ready";
  // Newest completed turn, matching the backend's rev-find — and extracted via
  // the canonical `answerTextOf` so "what counts as the answer" (thinking and
  // tools excluded) lives in exactly one place and readiness cannot drift from
  // copy/forward/render semantics.
  const newestCompleted = [...agentTurns].reverse().find((turn) => turn.status === "complete");
  if (newestCompleted === undefined) return "empty";
  return answerTextOf(newestCompleted).trim().length > 0 ? "ready" : "empty";
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
  /// Display name of the prompt being filled, when this hold is a prompt
  /// forward (`body` is always `""` in that case — the render happens
  /// server-side after every argument's sources resolve, so there is nothing
  /// else to show the user while it waits). Undefined for a plain-body
  /// forward, which has no prompt.
  promptName?: string;
}

const held = $state<Record<ProjectId, HeldForward[]>>({});

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
  },
};
