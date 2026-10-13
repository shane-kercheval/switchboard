import type { AgentId, AgentRecord } from "$lib/types";
import { supportsManualCompaction } from "$lib/harnessCapabilities";
import { runtimes, transcripts } from "$lib/state/index.svelte";
import { looksLikeItHasASession } from "$lib/state/forkAvailability";
import { isIsoTimestampAfter } from "$lib/utils";

/// Whether "Compact all", at the top of the agents sidebar, would compact
/// `agent`: its harness can compact, it has a conversation to compact, and its
/// latest action isn't already a compaction. `newestCompactionMarkerAt` is the
/// time of the agent's newest reopened compaction marker in the project's
/// conversation, which the sidebar derives once per history load, or
/// `undefined` when it has none.
///
/// Advisory, like the fork conditions: the backend's gate is the authority, and
/// an agent this misjudges gets a refusal on its own row while the others
/// proceed. Hidden agents are not excluded here: hiding is a view setting, and a
/// hidden agent still receives sends and still fills its context.
export function agentIsCompactible(
  agent: AgentRecord,
  newestCompactionMarkerAt: string | undefined,
): boolean {
  if (!supportsManualCompaction(agent.harness)) return false;
  if (!looksLikeItHasASession(agent)) return false;
  return !latestActionIsACompaction(agent.id, newestCompactionMarkerAt);
}

/// Whether nothing has happened on the agent since a compaction: one is queued
/// or running, or the newest finished thing is a compaction that compacted. A
/// failed or cancelled compaction compacted nothing, so it doesn't count, and
/// the agent stays eligible. Queued work is checked first because it is newer
/// than anything in the turn list; a queued context report is skipped because
/// it isn't conversation and leaves no row.
function latestActionIsACompaction(agentId: AgentId, markerAt: string | undefined): boolean {
  const pending = (runtimes[agentId]?.pending_sends ?? []).filter(
    (entry) => entry.kind !== "context_report" && entry.cancel_requested !== true,
  );
  const newestPending = pending.at(-1);
  if (newestPending !== undefined) return newestPending.kind === "compaction";
  const newestTurn = (transcripts[agentId] ?? []).at(-1);
  const turnAt =
    newestTurn === undefined
      ? undefined
      : newestTurn.role === "agent"
        ? (newestTurn.ended_at ?? newestTurn.started_at)
        : newestTurn.started_at;
  if (markerAt !== undefined && (turnAt === undefined || isIsoTimestampAfter(markerAt, turnAt))) {
    return true;
  }
  return (
    newestTurn?.role === "agent" &&
    newestTurn.kind === "compaction" &&
    (newestTurn.status === "streaming" || newestTurn.status === "complete")
  );
}
