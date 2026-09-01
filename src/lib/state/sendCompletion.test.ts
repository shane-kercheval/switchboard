import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentId, ProjectId, SendId, TurnId } from "$lib/types";

// Typed to `notify`'s real signature so `mock.calls` is a tuple the assertions
// can index — an untyped `vi.fn()` makes every call an empty tuple.
const notifyMock = vi.fn<(projectId: ProjectId, title: string, body: string) => Promise<void>>(
  async () => {},
);
vi.mock("$lib/api", () => ({
  notify: (projectId: ProjectId, title: string, body: string) => notifyMock(projectId, title, body),
}));

// A bare dynamic import with no workspace graph behind it — the leaf property
// this module is required to keep. If this ever needs more mocks to load, the
// module has grown a dependency it shouldn't have.
const {
  registerSend,
  markRecipientStarted: recordStartedRecipient,
  noteProjectStates,
  recordWorkflowTerminal,
  settleRecipient,
  settleAgentIdle,
  settleAgentsRemoved,
  settleTurn,
  forgetProjects,
  isFlushing,
  hasTrackedActivity,
  _testing,
} = await import("./sendCompletion");

const readingMode = await import("./readingMode.svelte");

const PROJECT = "p-1" as ProjectId;
const OTHER_PROJECT = "p-2" as ProjectId;
const SEND = "s-1" as SendId;
const SECOND_SEND = "s-2" as SendId;
const A = "ag-a" as AgentId;
const B = "ag-b" as AgentId;
const TURN = "turn-1" as TurnId;
const SECOND_TURN = "turn-2" as TurnId;

let nextTurn = 1;
function markRecipientStarted(sendId: SendId, agentId: AgentId): void {
  recordStartedRecipient(sendId, agentId, `turn-${nextTurn++}` as TurnId);
}

const register = (
  recipients: { id: AgentId; name: string }[] = [{ id: A, name: "claude" }],
  sendId: SendId = SEND,
): void => registerSend(sendId, PROJECT, "switchboard", recipients);

const both = [
  { id: A, name: "claude" },
  { id: B, name: "codex" },
];

/// The observer's push. Absence of a project from `busyProjects` means idle, so
/// tests that never call this run against an idle project — which is what makes
/// the `waitingForIdle` guard, not the pushed flag, the thing holding back a
/// flush while sends are in flight.
function pushBusy(busy: boolean, projectId: ProjectId = PROJECT): void {
  noteProjectStates([{ projectId, projectName: "switchboard", busy }]);
}

/// The single notification the test expects, with presence asserted once rather
/// than at every index.
function lastCall(): [ProjectId, string, string] {
  const call = notifyMock.mock.calls.at(-1);
  if (call === undefined) throw new Error("expected a notification");
  return call;
}

function expectTrackerEmpty(): void {
  expect(_testing.size()).toBe(0);
  expect(_testing.projectCount()).toBe(0);
  expect(_testing.startedTurnCount()).toBe(0);
}

beforeEach(() => {
  notifyMock.mockClear();
  _testing.reset();
  nextTurn = 1;
});

afterEach(() => {
  _testing.reset();
  readingMode._testing.reset();
});

describe("project-completion tracker", () => {
  it("notifies once when a single-agent send completes", async () => {
    register();
    recordStartedRecipient(SEND, A, TURN);
    settleTurn(TURN, A, "completed");
    expect(notifyMock).not.toHaveBeenCalled();
    settleAgentIdle(A);

    expect(notifyMock).toHaveBeenCalledOnce();
    const [projectId, title, body] = lastCall();
    expect(projectId).toBe(PROJECT);
    expect(title).toBe("Agent finished");
    // Names the project and the agent — with the app possibly in front, a
    // notification that doesn't say *which* project finished is worse than none.
    expect(body).toBe("switchboard: claude");
    expectTrackerEmpty();
  });

  it("waits for the last recipient of a fan-out, then notifies once", async () => {
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);

    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SEND, B, "completed");
    expect(notifyMock).not.toHaveBeenCalled();
    settleAgentIdle(B);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[2]).toBe("switchboard: claude, codex");
  });

  it("notifies only after the final turn in one agent's queued activity", async () => {
    // The regression `7fe9b23` fixed: a second send queued onto a busy agent must
    // not produce a notification between the two turns.
    register();
    markRecipientStarted(SEND, A);
    register([{ id: A, name: "claude" }], SECOND_SEND);

    settleRecipient(SEND, A, "completed");
    expect(notifyMock).not.toHaveBeenCalled();

    markRecipientStarted(SECOND_SEND, A);
    settleRecipient(SECOND_SEND, A, "completed");
    expect(notifyMock).not.toHaveBeenCalled();

    settleAgentIdle(A);
    expect(notifyMock).toHaveBeenCalledOnce();
    expectTrackerEmpty();
  });

  it("notifies once for two sends to disjoint agents in one project", async () => {
    // The behaviour change this milestone exists for. These sends share no
    // recipient, so the previous connected-batch model treated them as unrelated
    // and notified for the first while the second agent was still working.
    register([{ id: A, name: "claude" }], SEND);
    markRecipientStarted(SEND, A);
    register([{ id: B, name: "codex" }], SECOND_SEND);
    markRecipientStarted(SECOND_SEND, B);

    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SECOND_SEND, B, "completed");
    settleAgentIdle(B);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[2]).toBe("switchboard: claude, codex");
    expectTrackerEmpty();
  });

  it("keeps projects independent of each other", async () => {
    register([{ id: A, name: "claude" }], SEND);
    markRecipientStarted(SEND, A);
    registerSend(SECOND_SEND, OTHER_PROJECT, "other", [{ id: B, name: "codex" }]);
    markRecipientStarted(SECOND_SEND, B);

    settleRecipient(SECOND_SEND, B, "completed");
    settleAgentIdle(B);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[0]).toBe(OTHER_PROJECT);

    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).toHaveBeenCalledTimes(2);
    expect(lastCall()[0]).toBe(PROJECT);
    expectTrackerEmpty();
  });

  it("waits for a still-working agent before reporting another send's failure", async () => {
    register(both);
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, A, "failed");

    register([{ id: A, name: "claude" }], SECOND_SEND);
    settleRecipient(SECOND_SEND, A, "failed");
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SEND, B, "completed");
    settleAgentIdle(B);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Agents finished, some failed");
    expect(lastCall()[2]).toBe("switchboard: claude, codex");
    expectTrackerEmpty();
  });

  it("recovers a started turn that reaches idle without a terminal outcome", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    register();
    recordStartedRecipient(SEND, A, TURN);

    settleAgentIdle(A);

    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Agent failed");
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("idle without a terminal outcome"),
      expect.objectContaining({ turnId: TURN, sendId: SEND, agentId: A }),
    );
    expectTrackerEmpty();

    register([{ id: A, name: "claude" }], SECOND_SEND);
    recordStartedRecipient(SECOND_SEND, A, SECOND_TURN);
    settleTurn(SECOND_TURN, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).toHaveBeenCalledTimes(2);
    expect(lastCall()[1]).toBe("Agent finished");
    expectTrackerEmpty();
    warn.mockRestore();
  });

  it("keeps registered but unstarted work pending through an idle event", async () => {
    register();

    settleAgentIdle(A);

    expect(notifyMock).not.toHaveBeenCalled();
    expect(_testing.size()).toBe(1);
    expect(_testing.projectCount()).toBe(1);
    expect(_testing.startedTurnCount()).toBe(0);

    recordStartedRecipient(SEND, A, TURN);
    settleTurn(TURN, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).toHaveBeenCalledOnce();
    expectTrackerEmpty();
  });

  it("uses the latest project name when later work joins the project", async () => {
    register();
    markRecipientStarted(SEND, A);
    registerSend(SECOND_SEND, PROJECT, "renamed switchboard", [{ id: A, name: "claude" }]);
    markRecipientStarted(SECOND_SEND, A);
    settleRecipient(SEND, A, "completed");
    settleRecipient(SECOND_SEND, A, "completed");
    settleAgentIdle(A);

    expect(lastCall()[2]).toBe("renamed switchboard: claude");
    expectTrackerEmpty();
  });

  it("says so when the send failed", async () => {
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "failed");
    settleAgentIdle(A);
    expect(lastCall()[1]).toBe("Agent failed");
  });

  it("distinguishes a partial failure from a clean run", async () => {
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, A, "completed");
    settleRecipient(SEND, B, "failed");
    settleAgentIdle(A);
    settleAgentIdle(B);
    expect(lastCall()[1]).toBe("Agents finished, some failed");
  });

  it("reports mixed queued work on one agent as a partial failure", async () => {
    // Counting failures per *agent* rather than per outcome collapses this into
    // "Agent failed" — telling the user their agent failed when half its work
    // succeeded. This is the queued-messages flow, so it is a common case.
    register();
    markRecipientStarted(SEND, A);
    register([{ id: A, name: "claude" }], SECOND_SEND);
    markRecipientStarted(SECOND_SEND, A);

    settleRecipient(SEND, A, "completed");
    settleRecipient(SECOND_SEND, A, "failed");
    settleAgentIdle(A);

    expect(lastCall()[1]).toBe("Agent finished, some failed");
    expect(lastCall()[2]).toBe("switchboard: claude");
  });

  it("summarizes the tail rather than naming every subject", async () => {
    // macOS truncates a long body mid-name; an explicit remainder still tells the
    // user how much finished.
    const many = ["one", "two", "three", "four", "five"].map((name, i) => ({
      id: `ag-${i}` as AgentId,
      name,
    }));
    registerSend(SEND, PROJECT, "switchboard", many);
    for (const agent of many) settleRecipient(SEND, agent.id, "completed");

    expect(lastCall()[1]).toBe("Agents finished");
    expect(lastCall()[2]).toBe("switchboard: one, two, three and 2 more");
  });

  it("stays silent when the user cancelled everything", async () => {
    // The user did this deliberately and was present to do it.
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, A, "cancelled");
    settleRecipient(SEND, B, "cancelled");
    settleAgentIdle(A);
    settleAgentIdle(B);
    expect(notifyMock).not.toHaveBeenCalled();
    expectTrackerEmpty();
  });

  it("still notifies about the survivors of a partial cancel", async () => {
    // Cancelling one of two agents doesn't make the other's result uninteresting,
    // and the cancelled one is left out of the text rather than reported as done.
    register(both);
    settleRecipient(SEND, A, "cancelled");
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, B, "completed");
    settleAgentIdle(B);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[2]).toBe("switchboard: codex");
    expectTrackerEmpty();
  });

  it("notifies a send whose IPC was rejected for every recipient", async () => {
    // The regression test for this whole design, and the "no edge" case for the
    // level-checked flush: a pre-dispatch rejection emits no agent event at all
    // and never makes the project busy, so neither a liveness-derived
    // implementation nor an edge-triggered flush would ever fire.
    register(both);
    settleRecipient(SEND, A, "failed");
    settleRecipient(SEND, B, "failed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Agents failed");
    expectTrackerEmpty();
  });

  it("notifies survivors when the last outcomes settle after the project looks idle", async () => {
    // The level-check regression guard. Cancelling a queued send removes it from
    // send liveness *before* `message_cancelled` carries the outcome, so the
    // project reads idle while outcomes are still missing. An edge-triggered
    // flush fires there, correctly declines, and then never gets another edge —
    // swallowing the survivor's notification for good.
    register(both);
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).not.toHaveBeenCalled();

    // The project goes idle (B's queued send was cancelled and has dropped out of
    // liveness) while B's outcome is still unknown.
    pushBusy(false);
    expect(notifyMock).not.toHaveBeenCalled();

    // The backend's cancellation finally lands, with no further idle transition.
    settleRecipient(SEND, B, "cancelled");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[2]).toBe("switchboard: claude");
    expectTrackerEmpty();
  });

  it("holds the notification while the pushed state says the project is busy", async () => {
    // What a workflow between steps looks like to this module: no live send, no
    // pending outcome, but the project is not finished.
    register();
    markRecipientStarted(SEND, A);
    pushBusy(true);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).not.toHaveBeenCalled();

    pushBusy(false);
    expect(notifyMock).toHaveBeenCalledOnce();
    expectTrackerEmpty();
  });

  it("ignores a repeat signal for a recipient that already settled", async () => {
    // An agent can emit `message_failed` and a synthesized `turn_end` for the same
    // dispatch; the second must not complete the send twice.
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, A, "failed");
    settleRecipient(SEND, A, "completed");
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SEND, B, "completed");
    settleAgentIdle(A);
    settleAgentIdle(B);
    expect(notifyMock).toHaveBeenCalledOnce();
    // The first outcome for a recipient wins — a later duplicate can't rewrite it.
    expect(lastCall()[1]).toBe("Agents finished, some failed");
  });

  it("never notifies again after a project has settled", async () => {
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    pushBusy(false);
    expect(notifyMock).toHaveBeenCalledOnce();
    expectTrackerEmpty();
  });

  it("ignores sends it never registered", async () => {
    // How workflow *steps* are excluded: the backend dispatches them, so they are
    // never registered here and cannot notify individually. No detection needed.
    settleRecipient("s-workflow" as SendId, A, "completed");
    settleRecipient(undefined, A, "completed");
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it("ignores an agent that was not a recipient of the send", async () => {
    register([{ id: A, name: "claude" }]);
    settleRecipient(SEND, B, "completed");
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it("still notifies the survivors when one recipient's agent is removed", async () => {
    // Deleting an agent mid-fan-out must not swallow the other agents' results.
    // A removed agent is treated exactly like a cancelled one: unknowable
    // outcome, left out of the text, not allowed to block the send.
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    settleAgentsRemoved([A]);
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SEND, B, "completed");
    settleAgentIdle(B);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[2]).toBe("switchboard: codex");
    expectTrackerEmpty();
  });

  it("stays silent and drops the work when every recipient is removed", async () => {
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    settleAgentsRemoved([A, B]);
    expect(notifyMock).not.toHaveBeenCalled();
    expectTrackerEmpty();
  });

  it("preserves an outcome already recorded before the agent was removed", async () => {
    // Teardown races a real terminal event; whichever arrives first wins, and the
    // second is a no-op rather than a rewrite.
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, A, "completed");
    settleAgentsRemoved([A, B]);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Agent finished");
    expect(lastCall()[2]).toBe("switchboard: claude");
    expectTrackerEmpty();
  });

  it("ignores removal of an agent that was never a recipient", async () => {
    register([{ id: A, name: "claude" }]);
    settleAgentsRemoved([B]);
    expect(notifyMock).not.toHaveBeenCalled();
    expect(_testing.size()).toBe(1);
    expect(_testing.projectCount()).toBe(1);
    expect(_testing.startedTurnCount()).toBe(0);
  });

  it("registers nothing for an empty recipient set", async () => {
    registerSend(SEND, PROJECT, "switchboard", []);
    expectTrackerEmpty();
  });

  it("never flushes an empty accumulator", async () => {
    // Load-bearing for reading mode: an idle project with nothing tracked must
    // produce no flush at all, or enabling the mode on a quiet project would
    // immediately switch itself back off.
    pushBusy(false);
    expect(notifyMock).not.toHaveBeenCalled();
    expect(_testing.projectCount()).toBe(0);
  });
});

describe("workflow run terminals", () => {
  it("reports a workflow that finishes into a quiet project", async () => {
    // The "workflow-only edge": the condition becomes true with no send event to
    // re-evaluate it, so a settlement-only trigger would never fire.
    recordWorkflowTerminal(PROJECT, "review-and-recommend", "complete");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Workflow finished");
    expect(lastCall()[2]).toBe("review-and-recommend");
    expectTrackerEmpty();
  });

  it("uses the project name once the observer has pushed it", async () => {
    pushBusy(true);
    recordWorkflowTerminal(PROJECT, "review-and-recommend", "complete");
    expect(notifyMock).not.toHaveBeenCalled();
    pushBusy(false);
    expect(lastCall()[2]).toBe("switchboard: review-and-recommend");
  });

  it("says so when the workflow failed", async () => {
    recordWorkflowTerminal(PROJECT, "review-and-recommend", "failed");
    expect(lastCall()[1]).toBe("Workflow failed");
  });

  it("stays silent for a cancelled run", async () => {
    // The user asked for it and was present to ask — the same rule the deleted
    // backend notification applied, and the same rule sends already follow.
    recordWorkflowTerminal(PROJECT, "review-and-recommend", "cancelled");
    expect(notifyMock).not.toHaveBeenCalled();
    expectTrackerEmpty();
  });

  it("waits for live sends before reporting a workflow terminal", async () => {
    // One notification for the whole project, not one for the run and another for
    // the sends — the two-notification case that motivated deleting the backend's
    // run-terminal notification.
    register();
    markRecipientStarted(SEND, A);
    recordWorkflowTerminal(PROJECT, "review-and-recommend", "complete");
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Work finished");
    expect(lastCall()[2]).toBe("switchboard: claude, review-and-recommend");
    expectTrackerEmpty();
  });

  it("reports a mixed quiet-down with one failure among several subjects", async () => {
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    recordWorkflowTerminal(PROJECT, "review-and-recommend", "failed");
    settleRecipient(SEND, A, "completed");
    settleRecipient(SEND, B, "completed");
    settleAgentIdle(A);
    settleAgentIdle(B);

    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Work finished, some failed");
    expect(lastCall()[2]).toBe("switchboard: claude, codex, review-and-recommend");
  });

  it("records a terminal for a run this session never held", async () => {
    // A background-started run terminalizes without ever appearing in
    // `workflowRuns`, so the payload is treated as self-contained.
    pushBusy(true);
    register();
    markRecipientStarted(SEND, A);
    recordWorkflowTerminal(PROJECT, "never-seen", "complete");
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    pushBusy(false);

    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[2]).toBe("switchboard: claude, never-seen");
  });
});

describe("flush re-entrancy marker", () => {
  it("marks a project while its notification is in flight and clears after", async () => {
    // Reading mode's fallback reads this: clearing the mode restores the
    // project's visibility, which would suppress a notification still on its way
    // to the gate.
    let release: () => void = () => {};
    notifyMock.mockImplementationOnce(
      async () =>
        await new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);

    expect(isFlushing(PROJECT)).toBe(true);
    release();
    await vi.waitFor(() => expect(isFlushing(PROJECT)).toBe(false));
  });

  it("does not mark a project whose flush was silent", async () => {
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "cancelled");
    settleAgentIdle(A);

    expect(notifyMock).not.toHaveBeenCalled();
    expect(isFlushing(PROJECT)).toBe(false);
  });
});

describe("consecutive quiet-downs", () => {
  /// A delivery whose promise the test controls, so overlapping deliveries can be
  /// resolved out of order.
  function deferNotify(): () => void {
    let release: () => void = () => {};
    notifyMock.mockImplementationOnce(
      async () =>
        await new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    return () => release();
  }

  function completeOnce(sendId: SendId): void {
    registerSend(sendId, PROJECT, "switchboard", [{ id: A, name: "claude" }]);
    markRecipientStarted(sendId, A);
    settleRecipient(sendId, A, "completed");
    settleAgentIdle(A);
  }

  it("notifies a second quiet-down while the first notification is still in flight", async () => {
    // The in-flight marker must not double as a flush guard: nothing retries once
    // the promise settles, so suppressing here would drop the notification.
    const releaseFirst = deferNotify();
    completeOnce(SEND);
    expect(isFlushing(PROJECT)).toBe(true);

    completeOnce(SECOND_SEND);

    expect(notifyMock).toHaveBeenCalledTimes(2);
    releaseFirst();
    expectTrackerEmpty();
  });

  it("stays flushing until every overlapping delivery settles", async () => {
    // A Set here would report "nothing in flight" as soon as *either* delivery
    // resolved. M3's fallback reads this to decide whether clearing reading mode
    // could suppress a notification still travelling to the gate, so an early
    // `false` reintroduces exactly that suppression.
    const releaseFirst = deferNotify();
    const releaseSecond = deferNotify();
    completeOnce(SEND);
    completeOnce(SECOND_SEND);
    expect(notifyMock).toHaveBeenCalledTimes(2);

    releaseSecond();
    // Let the second delivery's `finally` actually run before asserting. With a
    // Set this is the point the marker wrongly clears.
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect(isFlushing(PROJECT)).toBe(true);

    releaseFirst();
    await vi.waitFor(() => expect(isFlushing(PROJECT)).toBe(false));
  });
});

describe("project teardown", () => {
  it("discards a removed project's activity without notifying", async () => {
    // The removal path is genuinely notifiable, not merely leaky: an
    // already-completed recipient keeps its outcome, so the accumulator can be
    // complete and worth reporting. Re-adding the directory later would otherwise
    // release a banner about work that finished before the removal.
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, A, "completed");
    pushBusy(true);

    forgetProjects([PROJECT]);
    expect(notifyMock).not.toHaveBeenCalled();
    expectTrackerEmpty();

    // The re-add: the observer sees the project again and pushes it idle. Nothing
    // is left to release.
    pushBusy(false);
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it("leaves other projects untouched", async () => {
    register([{ id: A, name: "claude" }], SEND);
    registerSend(SECOND_SEND, OTHER_PROJECT, "other", [{ id: B, name: "codex" }]);

    forgetProjects([PROJECT]);

    expect(_testing.projectCount()).toBe(1);
    settleRecipient(SECOND_SEND, B, "completed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[0]).toBe(OTHER_PROJECT);
  });

  it("leaves an in-flight delivery's marker to its own lifecycle", async () => {
    // Forgetting the project must not clear a marker for a notification that is
    // genuinely still travelling; the delivery decrements it in its own `finally`.
    let release: () => void = () => {};
    notifyMock.mockImplementationOnce(
      async () =>
        await new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(isFlushing(PROJECT)).toBe(true);

    forgetProjects([PROJECT]);
    expect(isFlushing(PROJECT)).toBe(true);

    release();
    await vi.waitFor(() => expect(isFlushing(PROJECT)).toBe(false));
  });
});

describe("project name in the notification body", () => {
  it("keeps a name already learned when a send registers without one", () => {
    // Both writers record only names they actually have, so neither can clobber
    // the other. Without the guard, whether the body named its project would
    // depend on which writer arrived last.
    pushBusy(false);
    registerSend(SEND, PROJECT, "", [{ id: A, name: "claude" }]);
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);

    const [, , body] = lastCall();
    expect(body).toBe("switchboard: claude");
  });

  it("drops the prefix when no writer ever supplied a name", () => {
    registerSend(SEND, PROJECT, "", [{ id: A, name: "claude" }]);
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);

    const [, , body] = lastCall();
    expect(body).toBe("claude");
  });
});

describe("reading mode auto-off", () => {
  // The flush is the sole owner of clearing reading mode. These exercise it in
  // isolation — no activity observer, so nothing else could be doing the work.

  it("clears reading mode on the silent path, where nothing is worth notifying", () => {
    readingMode.toggleReadingMode(PROJECT);
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "cancelled");
    settleAgentIdle(A);

    // A wholly-cancelled project stays silent, but it *did* go quiet, so the
    // user is back in charge and the compose box has to come back.
    expect(notifyMock).not.toHaveBeenCalled();
    expect(readingMode.isReadingMode(PROJECT)).toBe(false);
  });

  it("holds reading mode on until the notification has been delivered", async () => {
    let release: () => void = () => {};
    notifyMock.mockImplementationOnce(
      async () =>
        await new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    readingMode.toggleReadingMode(PROJECT);
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);

    // Clearing restores the project's visibility to the gate, which would
    // suppress this very notification — so it must wait for delivery.
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(readingMode.isReadingMode(PROJECT)).toBe(true);

    release();
    await vi.waitFor(() => expect(readingMode.isReadingMode(PROJECT)).toBe(false));
  });

  it("does not clear reading mode armed by a send that landed during delivery", async () => {
    // Auto reading mode arms at dispatch, so a send inside the notify round
    // trip is a real sequence: the stale release must not switch off the mode
    // the new send just armed. The new activity's own flush owns it from here.
    let release: () => void = () => {};
    notifyMock.mockImplementationOnce(
      async () =>
        await new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    readingMode.enterReadingMode(PROJECT);
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(isFlushing(PROJECT)).toBe(true);

    // The next send dispatches while the notification is still travelling.
    register([{ id: A, name: "claude" }], SECOND_SEND);
    readingMode.enterReadingMode(PROJECT);

    release();
    // The stale release resolves and skips the clear; only the second send's
    // own settlement may end the mode.
    await vi.waitFor(() => expect(isFlushing(PROJECT)).toBe(false));
    expect(readingMode.isReadingMode(PROJECT)).toBe(true);

    markRecipientStarted(SECOND_SEND, A);
    settleRecipient(SECOND_SEND, A, "completed");
    settleAgentIdle(A);
    await vi.waitFor(() => expect(readingMode.isReadingMode(PROJECT)).toBe(false));
  });

  it("clears reading mode even when the notification fails to deliver", async () => {
    notifyMock.mockRejectedValueOnce(new Error("no notification permission"));
    readingMode.toggleReadingMode(PROJECT);
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);

    // A failed notify still leaves the project quiet; stranding the user with a
    // hidden compose box is the worse outcome.
    await vi.waitFor(() => expect(readingMode.isReadingMode(PROJECT)).toBe(false));
  });

  it("leaves other projects' reading mode alone", async () => {
    readingMode.toggleReadingMode(PROJECT);
    readingMode.toggleReadingMode(OTHER_PROJECT);
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);

    await vi.waitFor(() => expect(readingMode.isReadingMode(PROJECT)).toBe(false));
    expect(readingMode.isReadingMode(OTHER_PROJECT)).toBe(true);
  });

  it("reports tracked activity while a flush is still pending", () => {
    // The other half of the fallback's guard: with something tracked, the flush
    // will run and own the clear, so the fallback has to stay out of the way.
    expect(hasTrackedActivity(PROJECT)).toBe(false);
    register();
    markRecipientStarted(SEND, A);
    expect(hasTrackedActivity(PROJECT)).toBe(true);

    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(hasTrackedActivity(PROJECT)).toBe(false);
  });

  it("reports tracked activity for a workflow terminal held back by a busy project", () => {
    pushBusy(true);
    recordWorkflowTerminal(PROJECT, "review", "complete");
    expect(hasTrackedActivity(PROJECT)).toBe(true);
  });
});
