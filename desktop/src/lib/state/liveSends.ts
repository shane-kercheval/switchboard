import type { AgentId, AgentRecord, SendId } from "$lib/types";
import type { RuntimeMap, TranscriptMap } from "$lib/state/types";

/// Every live send across `agents`, mapped to the agents it is live for — both
/// still-queued sends (`pending_sends`, excluding ones already being cancelled)
/// and currently-streaming turns. This is what the compose-bar stop and the
/// per-project cancel both act on: cancelling each `(send_id, agentIds)` halts
/// everything the agents are running and have queued.
///
/// Pure selector (no singleton reach-in) so both call sites and tests pass their
/// own state. A streaming turn only exists while the agent's `run_status` is
/// `"processing"`, so the transcript scan is skipped for every other agent —
/// idle agents (the common case for non-active projects) cost nothing.
export function buildLiveSendsMap(
  agents: AgentRecord[],
  runtimes: RuntimeMap,
  transcripts: TranscriptMap,
): Map<SendId, AgentId[]> {
  const bySend = new Map<SendId, AgentId[]>();
  const add = (sendId: SendId, agentId: AgentId): void => {
    const arr = bySend.get(sendId) ?? [];
    if (!arr.includes(agentId)) arr.push(agentId);
    bySend.set(sendId, arr);
  };
  for (const agent of agents) {
    const runtime = runtimes[agent.id];
    for (const p of runtime?.pending_sends ?? []) {
      if (!p.cancel_requested) add(p.send_id, agent.id);
    }
    if (runtime?.run_status === "processing") {
      for (const turn of transcripts[agent.id] ?? []) {
        if (turn.role === "agent" && turn.status === "streaming" && turn.send_id !== undefined) {
          add(turn.send_id, agent.id);
        }
      }
    }
  }
  return bySend;
}
