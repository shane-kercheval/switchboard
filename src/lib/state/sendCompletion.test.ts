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

const {
  registerSend,
  markRecipientStarted: recordStartedRecipient,
  settleRecipient,
  settleAgentIdle,
  settleAgentsRemoved,
  settleTurn,
  _testing,
} = await import("./sendCompletion");

const PROJECT = "p-1" as ProjectId;
const SEND = "s-1" as SendId;
const SECOND_SEND = "s-2" as SendId;
const THIRD_SEND = "s-3" as SendId;
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

/// The single notification the test expects, with presence asserted once rather
/// than at every index.
function lastCall(): [ProjectId, string, string] {
  const call = notifyMock.mock.calls.at(-1);
  if (call === undefined) throw new Error("expected a notification");
  return call;
}

function expectTrackerEmpty(): void {
  expect(_testing.size()).toBe(0);
  expect(_testing.batchCount()).toBe(0);
  expect(_testing.activeAgentCount()).toBe(0);
  expect(_testing.startedTurnCount()).toBe(0);
}

beforeEach(() => {
  notifyMock.mockClear();
  _testing.reset();
  nextTurn = 1;
});

afterEach(() => {
  _testing.reset();
});

describe("send-completion tracker", () => {
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
    expect(lastCall()[1]).toBe("Agent finished");
    expect(lastCall()[2]).toBe("switchboard: claude");
  });

  it("retains an intermediate failure in the queue-drained notification", async () => {
    register();
    markRecipientStarted(SEND, A);
    register([{ id: A, name: "claude" }], SECOND_SEND);

    settleRecipient(SEND, A, "failed");
    markRecipientStarted(SECOND_SEND, A);
    settleRecipient(SECOND_SEND, A, "completed");
    settleAgentIdle(A);

    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Agent finished, some work failed");
  });

  it("merges independently busy agents when a later fan-out connects their queues", async () => {
    register([{ id: A, name: "claude" }], SEND);
    markRecipientStarted(SEND, A);
    register([{ id: B, name: "codex" }], SECOND_SEND);
    markRecipientStarted(SECOND_SEND, B);
    register(both, THIRD_SEND);

    settleRecipient(SEND, A, "completed");
    settleRecipient(SECOND_SEND, B, "completed");
    markRecipientStarted(THIRD_SEND, A);
    markRecipientStarted(THIRD_SEND, B);
    settleRecipient(THIRD_SEND, A, "completed");
    settleRecipient(THIRD_SEND, B, "completed");
    settleAgentIdle(A);
    expect(notifyMock).not.toHaveBeenCalled();
    settleAgentIdle(B);

    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Agents finished");
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

  it("abandons every tracker reference when a turn starts against a missing batch", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    register();
    _testing.dropBatchForSend(SEND);

    recordStartedRecipient(SEND, A, TURN);

    expect(notifyMock).not.toHaveBeenCalled();
    expectTrackerEmpty();
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("abandoning damaged activity batch"),
      expect.objectContaining({ reason: expect.stringContaining("started turn") }),
    );
    warn.mockRestore();
  });

  it("abandons every tracker reference when a terminal settles against a missing batch", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    register();
    recordStartedRecipient(SEND, A, TURN);
    _testing.dropBatchForSend(SEND);

    settleTurn(TURN, A, "completed");

    expect(notifyMock).not.toHaveBeenCalled();
    expectTrackerEmpty();

    register([{ id: A, name: "claude" }], SECOND_SEND);
    recordStartedRecipient(SECOND_SEND, A, SECOND_TURN);
    settleTurn(SECOND_TURN, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).toHaveBeenCalledOnce();
    expectTrackerEmpty();
    warn.mockRestore();
  });

  it("abandons every connected batch when a merge encounters damaged state", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    register([{ id: A, name: "claude" }], SEND);
    markRecipientStarted(SEND, A);
    register([{ id: B, name: "codex" }], SECOND_SEND);
    markRecipientStarted(SECOND_SEND, B);
    _testing.dropBatchForSend(SECOND_SEND);

    register(both, THIRD_SEND);

    expect(_testing.size()).toBe(1);
    expect(_testing.batchCount()).toBe(1);
    expect(_testing.activeAgentCount()).toBe(2);
    expect(_testing.startedTurnCount()).toBe(0);
    settleRecipient(THIRD_SEND, A, "failed");
    settleRecipient(THIRD_SEND, B, "failed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Agents failed");
    expectTrackerEmpty();
    warn.mockRestore();
  });

  it("keeps registered but unstarted work pending through an idle event", async () => {
    register();

    settleAgentIdle(A);

    expect(notifyMock).not.toHaveBeenCalled();
    expect(_testing.size()).toBe(1);
    expect(_testing.batchCount()).toBe(1);
    expect(_testing.activeAgentCount()).toBe(1);
    expect(_testing.startedTurnCount()).toBe(0);

    recordStartedRecipient(SEND, A, TURN);
    settleTurn(TURN, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).toHaveBeenCalledOnce();
    expectTrackerEmpty();
  });

  it("uses the latest project name when later work joins an activity batch", async () => {
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

  it("stays silent when the user cancelled the whole send", async () => {
    // The user did this deliberately and was present to do it.
    register(both);
    markRecipientStarted(SEND, A);
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, A, "cancelled");
    settleRecipient(SEND, B, "cancelled");
    settleAgentIdle(A);
    settleAgentIdle(B);
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it("still notifies about the survivors of a partially cancelled send", async () => {
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
    // The regression test for this whole design. A pre-dispatch rejection emits
    // no agent event at all, so a liveness-derived implementation would watch the
    // send disappear and never notify — silently losing the failure the user most
    // wants to hear about.
    register(both);
    settleRecipient(SEND, A, "failed");
    settleRecipient(SEND, B, "failed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[1]).toBe("Agents failed");
  });

  it("does not chain later work onto a recipient that failed before starting", async () => {
    register(both);
    markRecipientStarted(SEND, B);
    settleRecipient(SEND, A, "failed");

    register([{ id: A, name: "claude" }], SECOND_SEND);
    settleRecipient(SECOND_SEND, A, "failed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(lastCall()[2]).toBe("switchboard: claude");

    settleRecipient(SEND, B, "completed");
    settleAgentIdle(B);
    expect(notifyMock).toHaveBeenCalledTimes(2);
    expect(lastCall()[2]).toBe("switchboard: claude, codex");
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

  it("never notifies again after a send has completed", async () => {
    register();
    markRecipientStarted(SEND, A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    settleRecipient(SEND, A, "completed");
    settleAgentIdle(A);
    expect(notifyMock).toHaveBeenCalledOnce();
    expectTrackerEmpty();
  });

  it("ignores sends it never registered", async () => {
    // How workflow steps are excluded: the backend dispatches them, so they are
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

  it("stays silent and drops the send when every recipient is removed", async () => {
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
    expect(_testing.batchCount()).toBe(1);
    expect(_testing.activeAgentCount()).toBe(1);
    expect(_testing.startedTurnCount()).toBe(0);
  });

  it("registers nothing for an empty recipient set", async () => {
    registerSend(SEND, PROJECT, "switchboard", []);
    expectTrackerEmpty();
  });
});
