import type { AgentId, AgentRecord, ProjectId } from "$lib/types";
import { supportsManualCompaction } from "$lib/harnessCapabilities";
import { runtimes, transcripts } from "$lib/state/index.svelte";
import { conversations } from "$lib/state/workspace.svelte";
import { looksLikeItHasASession, projectFolderAvailable } from "$lib/state/forkAvailability";
import { isIsoTimestampAfter } from "$lib/utils";

/// Why "Compact all", at the top of the agents sidebar, is unavailable for the
/// whole project, or `null`. The same two conditions as Fork's project rule,
/// for the same reason: `agentIsCompactible` reads each agent's loaded turns
/// and the project's reopened markers, and both are empty while the project's
/// history loads, and after a failed load until a retry succeeds. Compacting
/// then would compact every agent, including ones compacted just before the
/// restart, which the backend has no reason to refuse. With the folder gone,
/// every compaction fails at spawn, one failure row per agent.
export function compactAllBlock(projectId: ProjectId): string | null {
  if (!projectFolderAvailable(projectId)) {
    return "This project's folder is unavailable, so its agents can't run.";
  }
  if (conversations[projectId]?.status !== "complete") {
    return "Compact all is available once the project's history has loaded.";
  }
  return null;
}

/// Whether "Compact all" would compact `agent`, in a project `compactAllBlock`
/// allows: its harness can compact, its own history has loaded, it has a
/// conversation to compact, and its latest action isn't already a compaction.
/// `newestCompactionMarkerAt` is the time of the agent's newest reopened
/// compaction marker in the project's conversation, which the sidebar derives
/// once per history load, or `undefined` when it has none.
///
/// Advisory in one direction only. The backend refuses an agent with no
/// session, and that refusal shows on the agent's row while the others proceed.
/// It does not refuse a redundant compaction, which is why an agent whose own
/// history is still loading or failed to load is skipped here: with no turns
/// and no markers to read, its latest action can't be seen. Hidden agents are
/// not excluded: hiding is a view setting, and a hidden agent still receives
/// sends and still fills its context.
export function agentIsCompactible(
  agent: AgentRecord,
  newestCompactionMarkerAt: string | undefined,
): boolean {
  if (!supportsManualCompaction(agent.harness)) return false;
  if (runtimes[agent.id]?.hydration_status !== "complete") return false;
  if (!looksLikeItHasASession(agent)) return false;
  return !latestActionIsACompaction(agent.id, newestCompactionMarkerAt);
}

/// Whether nothing has happened on the agent since a compaction: one is queued
/// or running, or the newest finished thing is a compaction that compacted. A
/// failed or cancelled compaction compacted nothing, so it doesn't count, and
/// the agent stays eligible. Queued work is checked first because it is newer
/// than anything in the turn list; a queued context report is skipped because
/// it isn't conversation and leaves no row, and a queued item already asked to
/// cancel is skipped because it is about to be dropped.
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
