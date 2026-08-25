// Notify the user when manually-dispatched activity finishes. A compose submit
// still groups all of its recipients, but additional sends queued onto the same
// busy agent join the same activity batch. The batch finishes only after every
// outcome is known and the dispatcher confirms each started agent's backlog is
// drained with `agent_idle`.
//
// **Why an explicit lifecycle rather than watching liveness.** The obvious
// approach is to diff `buildLiveSendsMap` and notify when a send drops out of it.
// That selector answers "who is live right now" and deliberately keeps nothing
// else, which breaks this feature three ways:
//
//   - By the time a send is no longer live, the outcomes are gone. Whether to
//     notify at all depends on them (a wholly-cancelled send is silent), and so
//     does what the notification says.
//   - A pre-dispatch IPC rejection never reaches the event stream: `ComposeBar`
//     catches it and calls `failSendStart` directly. A send whose IPC rejects for
//     every recipient would vanish from liveness with no event to observe, and
//     silently never notify — the exact failure the user most wants told about.
//   - Cancelling removes a queued send from liveness *before* the backend reports
//     the cancellation, so liveness disappears ahead of the information needed to
//     classify it.
//
// So a send is registered with its full recipient set at dispatch, overlapping
// sends are merged through their busy recipients, each outcome is retained, and
// delivery waits for the authoritative queue-drained boundary. This merge is
// intentionally transitive: overlapping recipient sets form one connected
// activity batch, because notifying for one part while a connected queued turn
// remains live would recreate the intermediate notification this module avoids.
//
// **Workflow steps are excluded structurally.** Only sends the frontend
// dispatches are ever registered; a workflow's sends originate in the backend and
// are merely observed here, so they cannot notify per step. The whole-run
// notification is the backend's (`workflow_commands.rs`). Nothing needs to detect
// or filter them.

import * as api from "$lib/api";
import type { AgentId, ProjectId, SendId, TurnId } from "$lib/types";

/// How one recipient's turn ended. `cancelled` is the only outcome that argues
/// *against* notifying — the user asked for it and was present to ask.
export type RecipientOutcome = "completed" | "failed" | "cancelled";

type TrackedSend = {
  batchId: number;
  /// Every recipient the send was dispatched to, captured before any IPC call so
  /// a rejection cannot erase one that was supposed to be there.
  recipients: Map<AgentId, string>;
  outcomes: Map<AgentId, RecipientOutcome>;
};

type ActivityBatch = {
  id: number;
  projectId: ProjectId;
  projectName: string;
  sends: Set<SendId>;
  recipients: Map<AgentId, string>;
  /// Agents with a turn that started and have not yet emitted the dispatcher's
  /// authoritative queue-drained `agent_idle` event.
  waitingForIdle: Set<AgentId>;
};

const tracked = new Map<SendId, TrackedSend>();
const batches = new Map<number, ActivityBatch>();
const activeBatchByAgent = new Map<AgentId, number>();
const startedByTurn = new Map<TurnId, { sendId: SendId; agentId: AgentId }>();
let nextBatchId = 1;

function createBatch(projectId: ProjectId, projectName: string): ActivityBatch {
  const batch: ActivityBatch = {
    id: nextBatchId,
    projectId,
    projectName,
    sends: new Set(),
    recipients: new Map(),
    waitingForIdle: new Set(),
  };
  nextBatchId += 1;
  batches.set(batch.id, batch);
  return batch;
}

function abandonBatch(batchId: number, reason: string): void {
  console.warn("[switchboard] abandoning damaged activity batch", { batchId, reason });
  const abandonedSendIds = new Set<SendId>();
  for (const [sendId, send] of tracked) {
    if (send.batchId !== batchId) continue;
    abandonedSendIds.add(sendId);
    tracked.delete(sendId);
  }
  for (const [turnId, started] of startedByTurn) {
    if (abandonedSendIds.has(started.sendId) || !tracked.has(started.sendId)) {
      startedByTurn.delete(turnId);
    }
  }
  for (const [agentId, activeBatchId] of activeBatchByAgent) {
    if (activeBatchId === batchId) activeBatchByAgent.delete(agentId);
  }
  batches.delete(batchId);
}

function mergeBatches(batchIds: number[]): ActivityBatch | undefined {
  const uniqueBatchIds = [...new Set(batchIds)];
  const existing: ActivityBatch[] = [];
  let damaged = false;
  for (const batchId of uniqueBatchIds) {
    const batch = batches.get(batchId);
    if (batch === undefined) {
      abandonBatch(batchId, "active reference pointed to a missing batch during merge");
      damaged = true;
      continue;
    }
    if ([...batch.sends].some((sendId) => !tracked.has(sendId))) {
      abandonBatch(batchId, "batch referenced an unknown send during merge");
      damaged = true;
      continue;
    }
    existing.push(batch);
  }
  if (damaged) {
    for (const batch of existing) {
      abandonBatch(batch.id, "connected merge included a damaged batch");
    }
    return undefined;
  }
  const primary = existing[0];
  if (primary === undefined) return undefined;

  for (const batch of existing.slice(1)) {
    for (const sendId of batch.sends) {
      const send = tracked.get(sendId)!;
      primary.sends.add(sendId);
      send.batchId = primary.id;
    }
    for (const [agentId, name] of batch.recipients) primary.recipients.set(agentId, name);
    for (const agentId of batch.waitingForIdle) primary.waitingForIdle.add(agentId);
    for (const [agentId, batchId] of activeBatchByAgent) {
      if (batchId === batch.id) activeBatchByAgent.set(agentId, primary.id);
    }
    batches.delete(batch.id);
  }
  return primary;
}

function batchForRecipients(
  recipients: { id: AgentId; name: string }[],
  projectId: ProjectId,
  projectName: string,
): ActivityBatch {
  const activeBatchIds = [
    ...new Set(
      recipients
        .map((recipient) => activeBatchByAgent.get(recipient.id))
        .filter((batchId): batchId is number => batchId !== undefined),
    ),
  ];
  if (activeBatchIds.length === 0) return createBatch(projectId, projectName);
  const merged = mergeBatches(activeBatchIds);
  if (merged !== undefined) return merged;
  console.warn(
    "[switchboard] active agents referenced no live activity batch; starting a new one",
    {
      projectId,
      activeBatchIds,
    },
  );
  return createBatch(projectId, projectName);
}

/// Register a send the frontend is dispatching. `recipients` must be the full
/// target set, captured before the per-recipient IPC calls begin.
export function registerSend(
  sendId: SendId,
  projectId: ProjectId,
  projectName: string,
  recipients: { id: AgentId; name: string }[],
): void {
  if (recipients.length === 0) return;
  const batch = batchForRecipients(recipients, projectId, projectName);
  batch.projectName = projectName;
  tracked.set(sendId, {
    batchId: batch.id,
    recipients: new Map(recipients.map((r) => [r.id, r.name])),
    outcomes: new Map(),
  });
  batch.sends.add(sendId);
  for (const recipient of recipients) {
    batch.recipients.set(recipient.id, recipient.name);
    activeBatchByAgent.set(recipient.id, batch.id);
  }
}

/// Record that a recipient's turn actually started. Completion notifications
/// for its whole overlapping activity batch now wait for `agent_idle`, which the
/// dispatcher emits only after that agent's queued backlog drains.
export function markRecipientStarted(
  sendId: SendId | undefined,
  agentId: AgentId,
  turnId: TurnId,
): void {
  if (sendId === undefined) return;
  const send = tracked.get(sendId);
  if (send === undefined || !send.recipients.has(agentId)) return;

  const activeBatchId = activeBatchByAgent.get(agentId);
  const batch =
    activeBatchId !== undefined && activeBatchId !== send.batchId
      ? mergeBatches([send.batchId, activeBatchId])
      : batches.get(send.batchId);
  if (batch === undefined) {
    if (tracked.has(sendId)) {
      abandonBatch(send.batchId, "started turn referenced no live activity batch");
    }
    return;
  }
  startedByTurn.set(turnId, { sendId, agentId });
  batch.waitingForIdle.add(agentId);
  activeBatchByAgent.set(agentId, batch.id);
}

/// Settle a started recipient through the turn id supplied by the dispatcher.
/// This avoids recovering send identity from mutable transcript state after the
/// terminal event arrives.
export function settleTurn(turnId: TurnId, agentId: AgentId, outcome: RecipientOutcome): void {
  const started = startedByTurn.get(turnId);
  if (started === undefined) return;
  if (started.agentId !== agentId) {
    console.warn("[switchboard] terminal turn arrived on a different agent channel", {
      turnId,
      expectedAgentId: started.agentId,
      agentId,
    });
    return;
  }
  startedByTurn.delete(turnId);
  settleRecipient(started.sendId, agentId, outcome);
}

/// Record one recipient's terminal outcome. The containing activity batch can
/// notify only after all of its recipients settle and every started agent's
/// queue drains.
///
/// Ignores unknown sends (a workflow's, or one already notified) and repeat
/// signals for a recipient that already settled — both are normal: an agent can
/// emit `message_failed` and a synthesized `turn_end` for the same dispatch.
export function settleRecipient(
  sendId: SendId | undefined,
  agentId: AgentId,
  outcome: RecipientOutcome,
): void {
  if (sendId === undefined) return;
  const send = tracked.get(sendId);
  if (send === undefined) return;
  if (!send.recipients.has(agentId) || send.outcomes.has(agentId)) return;

  const batch = batches.get(send.batchId);
  if (batch === undefined) {
    abandonBatch(send.batchId, "recipient outcome referenced no live activity batch");
    return;
  }
  send.outcomes.set(agentId, outcome);
  if (
    !batch.waitingForIdle.has(agentId) &&
    !hasUnsettledRecipient(batch, agentId) &&
    activeBatchByAgent.get(agentId) === batch.id
  ) {
    activeBatchByAgent.delete(agentId);
  }
  finishBatchIfReady(send.batchId);
}

/// Record the authoritative queue-drained boundary for one agent. A terminal
/// turn alone is insufficient: another queued turn can start immediately after
/// it with no idle event in between.
export function settleAgentIdle(agentId: AgentId): void {
  const affectedBatchIds = new Set<number>();
  const activeBatchId = activeBatchByAgent.get(agentId);
  if (activeBatchId !== undefined) affectedBatchIds.add(activeBatchId);

  for (const [turnId, started] of [...startedByTurn.entries()]) {
    if (started.agentId !== agentId) continue;
    startedByTurn.delete(turnId);
    const send = tracked.get(started.sendId);
    if (send === undefined || send.outcomes.has(agentId)) continue;
    affectedBatchIds.add(send.batchId);
    console.warn(
      "[switchboard] agent became idle without a terminal outcome; treating the turn as failed",
      {
        turnId,
        sendId: started.sendId,
        agentId,
      },
    );
    settleRecipient(started.sendId, agentId, "failed");
  }

  for (const batchId of affectedBatchIds) {
    const batch = batches.get(batchId);
    if (batch === undefined) continue;
    batch.waitingForIdle.delete(agentId);
    if (!hasUnsettledRecipient(batch, agentId) && activeBatchByAgent.get(agentId) === batch.id) {
      activeBatchByAgent.delete(agentId);
    }
    finishBatchIfReady(batch.id);
  }

  if (activeBatchId !== undefined && !batches.has(activeBatchId)) {
    activeBatchByAgent.delete(agentId);
  }
}

function hasUnsettledRecipient(batch: ActivityBatch, agentId: AgentId): boolean {
  for (const sendId of batch.sends) {
    const send = tracked.get(sendId);
    if (send?.recipients.has(agentId) && !send.outcomes.has(agentId)) return true;
  }
  return false;
}

function finishBatchIfReady(batchId: number): void {
  const batch = batches.get(batchId);
  if (batch === undefined || batch.waitingForIdle.size > 0) return;
  const sends: TrackedSend[] = [];
  for (const sendId of batch.sends) {
    const send = tracked.get(sendId);
    if (send === undefined) {
      abandonBatch(batchId, "batch referenced an unknown send during completion");
      return;
    }
    if (send.outcomes.size < send.recipients.size) return;
    sends.push(send);
  }

  const message = describe(batch, sends);
  for (const [turnId, started] of startedByTurn) {
    if (batch.sends.has(started.sendId)) startedByTurn.delete(turnId);
  }
  for (const sendId of batch.sends) tracked.delete(sendId);
  batches.delete(batch.id);
  for (const [agentId, activeBatchId] of activeBatchByAgent) {
    if (activeBatchId === batch.id) activeBatchByAgent.delete(agentId);
  }

  if (message === null) return;
  void api
    .notify(batch.projectId, message.title, message.body)
    .catch((e: unknown) => console.error("[switchboard] notify failed", e));
}

/// Settle every tracked recipient in `agentIds` as cancelled, because those
/// agents are being torn down and will never report an outcome.
///
/// Cancelled rather than forgotten, and per-agent rather than per-send: deleting
/// a whole send would throw away a notification for recipients that finished
/// fine, and leaving the removed agent unsettled is worse still — its slot could
/// never be filled, so the survivors' completions would be silently swallowed and
/// the entry would pin memory for the session. A removed agent and a cancelled
/// one are the same thing here: its outcome is unknowable, so leave it out of the
/// text and don't let it block the others.
export function settleAgentsRemoved(agentIds: AgentId[]): void {
  const removed = new Set(agentIds);
  for (const [turnId, started] of startedByTurn) {
    if (removed.has(started.agentId)) startedByTurn.delete(turnId);
  }
  for (const [sendId, send] of [...tracked.entries()]) {
    for (const agentId of agentIds) {
      if (send.recipients.has(agentId)) settleRecipient(sendId, agentId, "cancelled");
    }
  }
  for (const agentId of agentIds) {
    const batchId = activeBatchByAgent.get(agentId);
    const batch = batchId === undefined ? undefined : batches.get(batchId);
    batch?.waitingForIdle.delete(agentId);
    activeBatchByAgent.delete(agentId);
    if (batch !== undefined) finishBatchIfReady(batch.id);
  }
}

/// What to say, or `null` for a send that shouldn't notify.
///
/// A send whose recipients were *all* cancelled is silent: the user did that
/// deliberately and was present to do it. One survivor is enough to notify —
/// cancelling three of four agents still leaves a result worth hearing about.
function describe(
  batch: ActivityBatch,
  sends: TrackedSend[],
): { title: string; body: string } | null {
  const settled = sends
    .flatMap((send) => [...send.outcomes.entries()])
    .filter(([, outcome]) => outcome !== "cancelled");
  if (settled.length === 0) return null;

  const agentIds = [...new Set(settled.map(([id]) => id))];
  const names = agentIds.map((id) => batch.recipients.get(id) ?? "agent");
  const failed = settled.filter(([, o]) => o === "failed").length;
  const title =
    failed === settled.length
      ? agentIds.length === 1
        ? "Agent failed"
        : "Agents failed"
      : failed > 0
        ? agentIds.length === 1
          ? "Agent finished, some work failed"
          : "Agents finished, some failed"
        : agentIds.length === 1
          ? "Agent finished"
          : "Agents finished";
  return { title, body: `${batch.projectName}: ${names.join(", ")}` };
}

/// Test-only reset — the tracker is module state that would otherwise leak
/// between cases.
export const _testing = {
  reset(): void {
    tracked.clear();
    batches.clear();
    activeBatchByAgent.clear();
    startedByTurn.clear();
    nextBatchId = 1;
  },
  size(): number {
    return tracked.size;
  },
  batchCount(): number {
    return batches.size;
  },
  activeAgentCount(): number {
    return activeBatchByAgent.size;
  },
  startedTurnCount(): number {
    return startedByTurn.size;
  },
  dropBatchForSend(sendId: SendId): void {
    const batchId = tracked.get(sendId)?.batchId;
    if (batchId !== undefined) batches.delete(batchId);
  },
};
