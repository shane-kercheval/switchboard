// Notify the user when a send finishes — every recipient of one compose-bar
// submit, not each one individually.
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
// So a send is registered with its full recipient set at dispatch, and each
// recipient is settled by an explicit signal carrying its outcome.
//
// **Workflow steps are excluded structurally.** Only sends the frontend
// dispatches are ever registered; a workflow's sends originate in the backend and
// are merely observed here, so they cannot notify per step. The whole-run
// notification is the backend's (`workflow_commands.rs`). Nothing needs to detect
// or filter them.

import * as api from "$lib/api";
import type { AgentId, ProjectId, SendId } from "$lib/types";

/// How one recipient's turn ended. `cancelled` is the only outcome that argues
/// *against* notifying — the user asked for it and was present to ask.
export type RecipientOutcome = "completed" | "failed" | "cancelled";

type TrackedSend = {
  projectId: ProjectId;
  projectName: string;
  /// Every recipient the send was dispatched to, captured before any IPC call so
  /// a rejection cannot erase one that was supposed to be there.
  recipients: Map<AgentId, string>;
  outcomes: Map<AgentId, RecipientOutcome>;
};

const tracked = new Map<SendId, TrackedSend>();

/// Register a send the frontend is dispatching. `recipients` must be the full
/// target set, captured before the per-recipient IPC calls begin.
export function registerSend(
  sendId: SendId,
  projectId: ProjectId,
  projectName: string,
  recipients: { id: AgentId; name: string }[],
): void {
  if (recipients.length === 0) return;
  tracked.set(sendId, {
    projectId,
    projectName,
    recipients: new Map(recipients.map((r) => [r.id, r.name])),
    outcomes: new Map(),
  });
}

/// Record one recipient's terminal outcome. Notifies once when the last
/// registered recipient settles, then forgets the send.
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

  send.outcomes.set(agentId, outcome);
  if (send.outcomes.size < send.recipients.size) return;

  tracked.delete(sendId);
  const message = describe(send);
  if (message === null) return;
  void api
    .notify(send.projectId, message.title, message.body)
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
  for (const [sendId, send] of [...tracked.entries()]) {
    for (const agentId of agentIds) {
      if (send.recipients.has(agentId)) settleRecipient(sendId, agentId, "cancelled");
    }
  }
}

/// What to say, or `null` for a send that shouldn't notify.
///
/// A send whose recipients were *all* cancelled is silent: the user did that
/// deliberately and was present to do it. One survivor is enough to notify —
/// cancelling three of four agents still leaves a result worth hearing about.
function describe(send: TrackedSend): { title: string; body: string } | null {
  const settled = [...send.outcomes.entries()].filter(([, o]) => o !== "cancelled");
  if (settled.length === 0) return null;

  const names = settled.map(([id]) => send.recipients.get(id) ?? "agent");
  const failed = settled.filter(([, o]) => o === "failed").length;
  const title =
    failed === settled.length
      ? settled.length === 1
        ? "Agent failed"
        : "Agents failed"
      : failed > 0
        ? "Agents finished, some failed"
        : settled.length === 1
          ? "Agent finished"
          : "Agents finished";
  return { title, body: `${send.projectName}: ${names.join(", ")}` };
}

/// Test-only reset — the tracker is module state that would otherwise leak
/// between cases.
export const _testing = {
  reset(): void {
    tracked.clear();
  },
  size(): number {
    return tracked.size;
  },
};
