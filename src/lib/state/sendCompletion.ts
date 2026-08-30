// Notify the user when a **project** goes quiet — one notification per project,
// describing everything that happened, delivered when nothing is left running in
// it. Not per send, and not per workflow run.
//
// **Why project scope.** This started per-send, then widened to "connected"
// sends (those sharing a recipient) to stop a notification firing between two
// queued turns on one agent. Both were narrower than the question the user is
// actually asking, which is "can I proceed?" — answerable only when the whole
// project is idle. Two sends to *disjoint* agents used to produce two
// notifications, one of them while the other agent was still working. Project
// scope also makes connectivity trivial, which is why the transitive
// batch-merging machinery this module used to carry is gone rather than adapted.
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
// So settlement stays **event-driven**: a send is registered with its full
// recipient set before any IPC call, each outcome is retained, and delivery waits
// for the dispatcher's authoritative queue-drained `agent_idle`. Project scope
// changed *what* accumulates and *when the accumulation is complete* — it did not
// change what triggers settlement. Do not reintroduce a liveness diff.
//
// **The flush is level-checked, not edge-triggered.** `flushIfReady` re-evaluates
// the whole condition on *both* every settlement event and every pushed
// idle-state change, because neither trigger alone is sufficient: settlement-only
// misses a workflow terminalizing as the last activity (the condition becomes
// true with no send event to re-check it), and transition-only misses every
// outcome that settles *after* the project already looks idle — cancelling a
// queued send clears it from liveness before `message_cancelled` carries the
// outcome, and an IPC-rejected send never makes the project busy at all. Those
// are hazards three and two above.
//
// **This module is a leaf.** It imports `$lib/api`, types, and `./readingMode`
// (itself a leaf), which is what lets its tests exercise it through a bare
// dynamic import with no workspace graph behind it. It must not import workspace
// state — in particular not `projectIsIdle`, dynamically or otherwise. Both
// triggers therefore arrive from outside: settlement events come in from
// `./index.svelte` below, and the idle-state trigger is *pushed in* from above by
// the activity observer via `noteProjectStates`.
//
// **The flush owns turning reading mode off.** A project going quiet is exactly
// when the user has something to act on, so it is also when reading mode has done
// its job. Clearing lives here rather than in a watcher because the two actions
// must be *ordered*: clearing restores the project's real visibility to the gate,
// which would suppress the very notification that ended the mode. See
// `flushIfReady`.
//
// **Workflow runs are folded in, but workflow *steps* are still excluded
// structurally.** Only sends the frontend dispatches are ever registered, so a
// workflow's per-step sends (which the backend originates) cannot notify. What
// this module does own is the run's *terminal*, reported through
// `recordWorkflowTerminal` — previously a separate backend notification that
// fired at run-end regardless of whether the project was still busy, producing
// two notifications where the user wanted one.
//
// **Known limitation — the starvation tradeoff.** A long-running agent now delays
// notification of a quick answer from an unrelated agent in the same project.
// Accepted deliberately: the contract is "the project is quiet, come back," which
// is the question being answered. If it proves painful the fix is a separate
// per-agent notification option, **not** a revert to batch scope.
//
// **Known limitation — webview lifetime.** With the backend's run-terminal
// notification deleted, a webview crash during a long unattended run loses that
// run's notification. Accepted because manual-send notifications already have
// exactly this exposure; this removed an accidental asymmetry rather than
// introducing a new failure class.

import * as api from "$lib/api";
import type { AgentId, ProjectId, SendId, TurnId } from "$lib/types";
import { clearReadingMode } from "./readingMode.svelte";

/// How one recipient's turn ended. `cancelled` is the only outcome that argues
/// *against* notifying — the user asked for it and was present to ask.
export type RecipientOutcome = "completed" | "failed" | "cancelled";

/// How many agents/workflows the notification body names before summarizing the
/// rest as a count.
const SUBJECTS_NAMED_IN_BODY = 3;

/// How a workflow run ended, as the progress channel reports it. `interrupted`
/// is absent by construction: a crashed run has no live process to emit a
/// terminal event, so it surfaces only through the seed-on-subscribe query and
/// never reaches this module. Don't add an event path for it.
export type WorkflowTerminalStatus = "complete" | "failed" | "cancelled";

type TrackedSend = {
  projectId: ProjectId;
  /// Every recipient the send was dispatched to, captured before any IPC call so
  /// a rejection cannot erase one that was supposed to be there.
  recipients: Map<AgentId, string>;
  outcomes: Map<AgentId, RecipientOutcome>;
};

type ProjectActivity = {
  sends: Set<SendId>;
  recipients: Map<AgentId, string>;
  /// Agents with a turn that started and have not yet emitted the dispatcher's
  /// authoritative queue-drained `agent_idle` event.
  waitingForIdle: Set<AgentId>;
  workflows: { workflow: string; status: WorkflowTerminalStatus }[];
};

const tracked = new Map<SendId, TrackedSend>();
const activity = new Map<ProjectId, ProjectActivity>();
const startedByTurn = new Map<TurnId, { sendId: SendId; agentId: AgentId }>();
/// Display names, learned from either `registerSend` or the observer's push, and
/// read when composing the notification body. Kept separately from
/// `ProjectActivity` so a name known before any activity exists still applies.
const projectNames = new Map<ProjectId, string>();
/// Projects the observer currently reports as *not* idle. Absence means idle:
/// a project the observer has never evaluated has nothing running that it knows
/// about, and the in-flight case is covered independently by `waitingForIdle`.
const busyProjects = new Set<ProjectId>();
/// Projects whose flush has delivered a notification that is still in flight —
/// the marker M3's reading-mode fallback reads, so it cannot clear the mode (and
/// with it restore the project's visibility) while the notification that clearing
/// would suppress is still travelling to the gate.
///
/// Deliberately **not** a re-entrancy guard on `flushIfReady`. Re-entrancy is
/// already impossible structurally: the flush deletes the project's accumulator
/// synchronously *before* the delivery promise, so a second call finds nothing to
/// flush. Using this as a guard as well would be actively wrong — activity
/// registered and completed while a previous notification is still in flight is a
/// genuinely new quiet-down, and suppressing it would drop that notification
/// entirely, since nothing would retry once the promise settled.
///
/// **A count, not a flag, precisely because that second quiet-down is allowed.**
/// Two deliveries for one project can overlap, and with a `Set` whichever settled
/// first would clear the marker while the other was still travelling — reporting
/// "nothing in flight" at the moment M3 most needs the opposite answer.
///
/// The delivery owns this lifecycle end to end: only `flushIfReady` increments and
/// only its `finally` decrements. `forgetProjects` deliberately leaves it alone —
/// clearing it externally would diverge the marker from reality while a real
/// notification is still on its way.
const flushing = new Map<ProjectId, number>();

function ensureActivity(projectId: ProjectId): ProjectActivity {
  const existing = activity.get(projectId);
  if (existing !== undefined) return existing;
  const created: ProjectActivity = {
    sends: new Set(),
    recipients: new Map(),
    waitingForIdle: new Set(),
    workflows: [],
  };
  activity.set(projectId, created);
  return created;
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
  projectNames.set(projectId, projectName);
  const project = ensureActivity(projectId);
  tracked.set(sendId, {
    projectId,
    recipients: new Map(recipients.map((r) => [r.id, r.name])),
    outcomes: new Map(),
  });
  project.sends.add(sendId);
  for (const recipient of recipients) project.recipients.set(recipient.id, recipient.name);
}

/// Record that a recipient's turn actually started. The project's notification
/// now waits for `agent_idle`, which the dispatcher emits only after that agent's
/// queued backlog drains — a terminal turn alone is insufficient, since another
/// queued turn can start immediately after it with no idle event in between.
export function markRecipientStarted(
  sendId: SendId | undefined,
  agentId: AgentId,
  turnId: TurnId,
): void {
  if (sendId === undefined) return;
  const send = tracked.get(sendId);
  if (send === undefined || !send.recipients.has(agentId)) return;
  const project = activity.get(send.projectId);
  if (project === undefined) return;
  startedByTurn.set(turnId, { sendId, agentId });
  project.waitingForIdle.add(agentId);
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

/// Record one recipient's terminal outcome, then re-check the project's flush
/// condition.
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
  flushIfReady(send.projectId);
}

/// Record the authoritative queue-drained boundary for one agent.
export function settleAgentIdle(agentId: AgentId): void {
  const affected = new Set<ProjectId>();

  for (const [turnId, started] of [...startedByTurn.entries()]) {
    if (started.agentId !== agentId) continue;
    startedByTurn.delete(turnId);
    const send = tracked.get(started.sendId);
    if (send === undefined || send.outcomes.has(agentId)) continue;
    console.warn(
      "[switchboard] agent became idle without a terminal outcome; treating the turn as failed",
      { turnId, sendId: started.sendId, agentId },
    );
    settleRecipient(started.sendId, agentId, "failed");
  }

  for (const [projectId, project] of activity) {
    if (!project.waitingForIdle.delete(agentId)) continue;
    affected.add(projectId);
  }
  for (const projectId of affected) flushIfReady(projectId);
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
  const affected = new Set<ProjectId>();
  for (const [projectId, project] of activity) {
    for (const agentId of agentIds) {
      if (project.waitingForIdle.delete(agentId)) affected.add(projectId);
    }
  }
  for (const projectId of affected) flushIfReady(projectId);
}

/// Record a workflow run reaching its terminal. Called from the progress-channel
/// handler **before** it drops the run from `workflowRuns` — that drop is the
/// mutation that flips the project idle, so recording after it leaves a window
/// where the flush can run without this outcome and silently omit it.
///
/// The run need not have been previously known: a terminal can arrive for a run
/// this session never held (a background start), so the payload is treated as
/// self-contained.
export function recordWorkflowTerminal(
  projectId: ProjectId,
  workflow: string,
  status: WorkflowTerminalStatus,
): void {
  ensureActivity(projectId).workflows.push({ workflow, status });
  flushIfReady(projectId);
}

/// Push the projects the activity observer currently sees as busy, with their
/// display names. This is the **idle-state trigger** — the second of the flush's
/// two inputs, and the reason this module never has to read workspace state.
/// Called on every observer evaluation, not only on transitions, so the flush
/// stays level-checked.
export function noteProjectStates(
  states: { projectId: ProjectId; projectName: string; busy: boolean }[],
): void {
  for (const { projectId, projectName, busy } of states) {
    if (projectName !== "") projectNames.set(projectId, projectName);
    if (busy) busyProjects.add(projectId);
    else busyProjects.delete(projectId);
  }
  for (const projectId of [...activity.keys()]) flushIfReady(projectId);
}

/// Discard everything tracked for these projects **silently** — never flushing.
///
/// Called from the workspace teardown paths (directory removal, project delete),
/// alongside the cleanup every other per-project store already gets. Two rules,
/// both load-bearing:
///
///   - **Silent.** A deleted project must not notify. Without this the removal
///     path is genuinely notifiable, not merely leaky: `settleAgentsRemoved`
///     leaves an already-`completed` recipient alone (deliberately — a real
///     outcome beats a teardown guess), so the accumulator can be complete and
///     worth reporting. It cannot flush at teardown because the stale
///     `busyProjects` entry blocks it, and the project then drops out of the
///     observer's candidate set so nothing clears that entry — until the same
///     directory is re-added, whose first idle push releases a notification about
///     work that finished before the removal.
///   - **Call before agent teardown.** Run after `unregisterAgents` and
///     `settleAgentsRemoved` can classify the teardown outcomes and flush first;
///     running before leaves it a no-op on an already-empty tracker.
///
/// `flushing` is deliberately untouched: a delivery already in flight still is,
/// and owns its own decrement.
export function forgetProjects(projectIds: ProjectId[]): void {
  const forgotten = new Set(projectIds);
  for (const [sendId, send] of [...tracked.entries()]) {
    if (forgotten.has(send.projectId)) tracked.delete(sendId);
  }
  for (const [turnId, started] of [...startedByTurn.entries()]) {
    if (!tracked.has(started.sendId)) startedByTurn.delete(turnId);
  }
  for (const projectId of forgotten) {
    activity.delete(projectId);
    projectNames.delete(projectId);
    busyProjects.delete(projectId);
  }
}

/// Whether a notification for `projectId` is currently in flight. Read by M3's
/// reading-mode fallback so it cannot clear the mode — and with it restore the
/// project's visibility — while the notification that clearing would suppress is
/// still travelling to the gate.
export function isFlushing(projectId: ProjectId): boolean {
  return (flushing.get(projectId) ?? 0) > 0;
}

/// Whether this module is tracking anything for `projectId` that could still
/// produce a notification. The other half of the reading-mode fallback's guard:
/// with something tracked, the flush will run and own the clear, so the fallback
/// must stay out of the way.
export function hasTrackedActivity(projectId: ProjectId): boolean {
  const project = activity.get(projectId);
  if (project === undefined) return false;
  return project.sends.size > 0 || project.workflows.length > 0;
}

/// The whole flush condition, re-evaluated from scratch on every trigger. Level-
/// checked by construction: it never asks "did something just change", only
/// "is everything done now".
function flushIfReady(projectId: ProjectId): void {
  const project = activity.get(projectId);
  if (project === undefined) return;
  // Never flush an empty accumulator: it has nothing to report, and M3 depends on
  // this — without it, enabling reading mode on a quiet project would trigger a
  // silent flush that immediately switched the mode back off.
  if (project.sends.size === 0 && project.workflows.length === 0) return;
  if (busyProjects.has(projectId)) return;
  if (project.waitingForIdle.size > 0) return;

  const sends: TrackedSend[] = [];
  for (const sendId of project.sends) {
    const send = tracked.get(sendId);
    if (send === undefined) continue;
    if (send.outcomes.size < send.recipients.size) return;
    sends.push(send);
  }

  const message = describe(projectId, project, sends);
  for (const [turnId, started] of startedByTurn) {
    if (project.sends.has(started.sendId)) startedByTurn.delete(turnId);
  }
  for (const sendId of project.sends) tracked.delete(sendId);
  activity.delete(projectId);

  if (message === null) {
    // Nothing to say (a wholly-cancelled project), but the project *did* go
    // quiet, so reading mode is still done. Safe to clear immediately here —
    // with no notification travelling to the gate there is nothing for the
    // restored visibility to suppress.
    clearReadingMode(projectId);
    return;
  }
  flushing.set(projectId, (flushing.get(projectId) ?? 0) + 1);
  void api
    .notify(projectId, message.title, message.body)
    .catch((e: unknown) => console.error("[switchboard] notify failed", e))
    .finally(() => {
      // **Clear reading mode only after `notify` has resolved.** Clearing makes
      // the project visible again, and `set_visible_project` and `notify` are
      // independent IPC calls — restoring visibility first would let the gate see
      // the project as on screen and suppress this very notification. Awaiting is
      // a genuine happens-before: the gate decides synchronously inside the Rust
      // command handler, before it returns. The natural implementation — an
      // effect watching idle state — gets this wrong intermittently, and worst
      // under real IPC latency, so a jsdom test with a mocked `invoke` can pass
      // while production loses the notification.
      //
      // Runs on rejection too: a failed notify still leaves the project quiet,
      // and stranding the user with a hidden compose box is the worse outcome.
      clearReadingMode(projectId);
      // Released last, so the reading-mode fallback (which reads `isFlushing`)
      // can never observe "nothing in flight" while this delivery is still
      // deciding. Floors at zero rather than trusting the key to exist: a
      // `finally` can land after `_testing.reset`, and a decrement must never
      // leave a negative count that would pin `isFlushing` true forever.
      const remaining = (flushing.get(projectId) ?? 1) - 1;
      if (remaining > 0) flushing.set(projectId, remaining);
      else flushing.delete(projectId);
    });
}

/// What to say, or `null` when nothing is worth reporting.
///
/// Cancelled outcomes are dropped on both sides: a wholly-cancelled project is
/// silent because the user did that deliberately and was present to do it, and a
/// cancelled workflow run is silent for the same reason (the rule the deleted
/// backend notification applied). One survivor is enough to notify — cancelling
/// three of four agents still leaves a result worth hearing about.
function describe(
  projectId: ProjectId,
  project: ProjectActivity,
  sends: TrackedSend[],
): { title: string; body: string } | null {
  const settled = sends
    .flatMap((send) => [...send.outcomes.entries()])
    .filter(([, outcome]) => outcome !== "cancelled");
  const runs = project.workflows.filter((run) => run.status !== "cancelled");
  if (settled.length === 0 && runs.length === 0) return null;

  const agentIds = [...new Set(settled.map(([id]) => id))];
  // Counted over *outcomes*, not over deduplicated subjects. Counting per agent
  // ("did this agent fail at all?") collapses partial failure into total failure
  // whenever there is one agent — so one agent with a completed and a failed send
  // reported "Agent failed" for work that half succeeded.
  const failed =
    settled.filter(([, outcome]) => outcome === "failed").length +
    runs.filter((run) => run.status === "failed").length;
  const total = settled.length + runs.length;

  // The noun follows what actually finished, so the title stays specific in the
  // common single-kind cases and degrades to "Work" only when both mix.
  const noun =
    agentIds.length > 0 && runs.length > 0
      ? "Work"
      : runs.length > 0
        ? runs.length === 1
          ? "Workflow"
          : "Workflows"
        : agentIds.length === 1
          ? "Agent"
          : "Agents";
  const title =
    failed === total
      ? `${noun} failed`
      : failed > 0
        ? `${noun} finished, some failed`
        : `${noun} finished`;

  const names = [
    ...agentIds.map((id) => project.recipients.get(id) ?? "agent"),
    ...runs.map((run) => run.workflow),
  ];
  // Summarize the tail rather than letting macOS truncate the body mid-name: an
  // explicit remainder count still tells the user how much finished.
  const listed =
    names.length > SUBJECTS_NAMED_IN_BODY
      ? `${names.slice(0, SUBJECTS_NAMED_IN_BODY).join(", ")} and ${names.length - SUBJECTS_NAMED_IN_BODY} more`
      : names.join(", ");
  // A graceful fallback, not dead code: names arrive from `registerSend` and the
  // observer's push, and a project with a live workflow subscription is already an
  // observer candidate (its roster key is set before the subscription), so an
  // absent name is a startup race only. Sourcing it here instead would need
  // `workflows` → `workspace`, the cycle this module's leaf rule forbids — so the
  // body drops the prefix rather than the notification.
  const projectName = projectNames.get(projectId) ?? "";
  return { title, body: projectName === "" ? listed : `${projectName}: ${listed}` };
}

/// Test-only reset — the tracker is module state that would otherwise leak
/// between cases.
export const _testing = {
  reset(): void {
    tracked.clear();
    activity.clear();
    startedByTurn.clear();
    projectNames.clear();
    busyProjects.clear();
    flushing.clear();
  },
  size(): number {
    return tracked.size;
  },
  projectCount(): number {
    return activity.size;
  },
  startedTurnCount(): number {
    return startedByTurn.size;
  },
};
