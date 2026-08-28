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

import type { AgentId, AgentRecord, ForwardSourceRef, ProjectId, SendId } from "$lib/types";
export type { ForwardSourceRef };
import { answerTextOf } from "./unified";
import type { Turn } from "$lib/state/types";
import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";

/// One forward source the user picked: always a single agent whose latest output
/// gets forwarded. Agents are the first-class unit everywhere — picking a *pane*
/// is a selection convenience that expands to one source per member agent at pick
/// time (see `forwardSourceAgentsForPane`), so a pane is never stored or displayed
/// as a chip. `name` drives the chip and the "waiting for {name}…" label.
/// A source may live in **another project**, so it carries its owner. `projectId`
/// is what lets the backend resolve a source whose project this process never
/// opened — an agent id alone names no project to open. `projectName` is display
/// only: it qualifies the chip and the "waiting for …" row when the source is
/// foreign, so two same-named agents in different projects stay distinguishable.
///
/// Both are optional for one reason: **persisted drafts written before this
/// existed**. A restored draft with no `projectId` belongs to the draft's own
/// project, and `upgradeForwardSource` fills it in before anything reaches IPC —
/// the wire type itself requires the owner (see `ForwardSourceRef` in
/// `commands.rs`), so the ambiguity is resolved at the edge and never travels.
export type ForwardSource = {
  id: AgentId;
  name: string;
  projectId?: ProjectId;
  projectName?: string;
};

/// Fill in a legacy source's owner from the project whose draft it was restored
/// from. Applied on restore so an un-upgraded source can never reach the wire.
export function upgradeForwardSource(
  source: ForwardSource,
  draftProjectId: ProjectId,
): ForwardSource {
  return source.projectId ? source : { ...source, projectId: draftProjectId };
}

/// Whether this source belongs to a project other than the one composing the send
/// — the condition for qualifying its label with the project name.
export function isForeignSource(source: ForwardSource, currentProjectId: ProjectId): boolean {
  return source.projectId !== undefined && source.projectId !== currentProjectId;
}

/// The chip / waiting-row label: bare agent name in-project, `agent · project`
/// when foreign (falling back to the bare name if the project name is unknown).
export function forwardSourceLabel(source: ForwardSource, currentProjectId: ProjectId): string {
  return isForeignSource(source, currentProjectId) && source.projectName
    ? `${source.name} · ${source.projectName}`
    : source.name;
}

/// Stable identity for dedup / removal / list keys — the agent id.
export function forwardSourceKey(source: ForwardSource): string {
  return source.id;
}

/// Build an agent forward source. Shared by every forward surface so the shape
/// has one definition.
export function forwardSourceForAgent(
  agent: AgentRecord,
  project?: { id: ProjectId; name: string },
): ForwardSource {
  return project
    ? { id: agent.id, name: agent.name, projectId: project.id, projectName: project.name }
    : { id: agent.id, name: agent.name, projectId: agent.project_id };
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
    .map((agent) => forwardSourceForAgent(agent));
}

/// The agent ids a set of sources covers — for **UI dedup only** (hiding an
/// already-picked agent from a menu). The wire uses [`expandForwardSources`],
/// which carries each source's owning project; the two are not interchangeable
/// and confusing them is how a foreign source loses its project on the way out.
export function forwardSourceIds(sources: readonly ForwardSource[]): AgentId[] {
  const ids: AgentId[] = [];
  for (const source of sources) if (!ids.includes(source.id)) ids.push(source.id);
  return ids;
}

/// The sources as the **backend receives them** — one ref per distinct agent,
/// in declared order, each carrying its owning project.
///
/// `currentProjectId` is the fallback owner for a source that has none: a draft
/// written before sources carried a project. Resolving that here, at the wire
/// boundary, is what lets the backend's type require the owner unconditionally.
export function expandForwardSources(
  sources: ForwardSource[],
  currentProjectId: ProjectId,
): ForwardSourceRef[] {
  const refs: ForwardSourceRef[] = [];
  for (const source of sources) {
    if (refs.some((r) => r.agent_id === source.id)) continue;
    refs.push({ agent_id: source.id, project_id: source.projectId ?? currentProjectId });
  }
  return refs;
}

/// Reconcile persisted forward sources against the live roster, for restore.
///
/// A **local** source names an agent that may have been removed or renamed since
/// the draft was written. Removed local agents are dropped — forwarding from them
/// would fail at dispatch. Survivors take the roster's *current* name, because
/// the chip's `name` is display-only and a stale one would show the user an agent
/// that no longer exists under that label.
///
/// A **foreign** source is kept as-is. `agents` is the *current project's* roster,
/// so a foreign source is absent from it by definition — matching on it would
/// delete every cross-project chip on each remount (a project switch, a Git-view
/// toggle), which is exactly the silent-disappearance this restore path exists to
/// prevent. Validation of foreign sources belongs to the backend, which opens the
/// declared project and verifies ownership at dispatch; a genuinely deleted agent
/// then fails the send with a clear message instead of vanishing beforehand.
///
/// `draftProjectId` is the project this draft belongs to: it identifies which
/// sources are local and upgrades legacy sources that predate `projectId`.
export function reconcileForwardSources(
  sources: readonly ForwardSource[],
  agents: readonly AgentRecord[],
  draftProjectId: ProjectId,
): ForwardSource[] {
  return sources
    .map((source) => upgradeForwardSource(source, draftProjectId))
    .filter((source) => {
      if (isForeignSource(source, draftProjectId)) return true;
      return agents.some((agent) => agent.id === source.id);
    })
    .map((source) => {
      // A foreign source keeps its stored labels here: this runs synchronously at
      // composer construction, and its project's roster is an async read. Names are
      // refreshed afterwards by [`refreshForeignSourceLabels`].
      if (isForeignSource(source, draftProjectId)) return source;
      const agent = agents.find((a) => a.id === source.id);
      return agent ? forwardSourceForAgent(agent) : source;
    });
}

/// Apply a freshly-read roster to already-restored sources: update the agent
/// name and project name for any source owned by `projectId`, leave everything
/// else untouched. Pure, so the caller decides when a read has landed.
///
/// Split from [`reconcileForwardSources`] because restore is synchronous and a
/// roster read is async IPC — the refresh cannot happen during reconciliation,
/// which is the one path where a stale name is actually on screen. Returns the
/// same array reference when nothing changed, so a caller can skip a write.
export function refreshForeignSourceLabels(
  sources: readonly ForwardSource[],
  projectId: ProjectId,
  roster: readonly AgentRecord[],
  projectName?: string,
): ForwardSource[] {
  let changed = false;
  const next = sources.map((source) => {
    if (source.projectId !== projectId) return source;
    const agent = roster.find((a) => a.id === source.id);
    // A source whose agent is gone keeps its stored label: the backend refuses
    // the send with a clear error, which beats a chip mutating under the user.
    const name = agent?.name ?? source.name;
    const nextName = projectName ?? source.projectName;
    if (name === source.name && nextName === source.projectName) return source;
    changed = true;
    return { ...source, name, projectName: nextName };
  });
  return changed ? next : (sources as ForwardSource[]);
}

/// `reconcileForwardSources` across a per-field map, dropping fields left empty so
/// a restored draft carries no keys for arguments whose every source is gone.
export function reconcileForwardSourceMap(
  map: Readonly<Record<string, ForwardSource[]>>,
  agents: readonly AgentRecord[],
  draftProjectId: ProjectId,
): Record<string, ForwardSource[]> {
  const out: Record<string, ForwardSource[]> = {};
  for (const [field, sources] of Object.entries(map)) {
    const kept = reconcileForwardSources(sources, agents, draftProjectId);
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
/// - `unknown` — not determinable here, so **render no marker at all**. This is
///               the state for a source in another project: readiness is read
///               from the current project's loaded transcripts, which by
///               definition don't contain a foreign agent. An explicit member
///               rather than reusing `ready` or `empty`, because both of those
///               are claims — and `empty` in particular renders a "this will
///               block your send" warning that is the *inverse* of the truth for
///               a healthy foreign source.
export type ForwardReadiness = "ready" | "pending" | "empty" | "unknown";

/// Readiness for one source, given the project composing the send. A foreign
/// source short-circuits to `unknown`: its transcript isn't loaded here, and
/// guessing from an absent transcript yields `empty`, which is a false warning.
export function sourceReadinessFor(
  source: ForwardSource,
  currentProjectId: ProjectId,
  turnsFor: (id: AgentId) => readonly Turn[] | undefined,
): ForwardReadiness {
  if (isForeignSource(source, currentProjectId)) return "unknown";
  return forwardReadiness(turnsFor(source.id));
}

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
