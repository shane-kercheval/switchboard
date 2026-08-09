import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentId, ProjectId, SendId } from "$lib/types";

const notifyMock = vi.fn(async () => {});
vi.mock("$lib/api", () => ({
  notify: (projectId: ProjectId, title: string, body: string) => notifyMock(projectId, title, body),
}));

const { registerSend, settleRecipient, settleAgentsRemoved, _testing } =
  await import("./sendCompletion");

const PROJECT = "p-1" as ProjectId;
const SEND = "s-1" as SendId;
const A = "ag-a" as AgentId;
const B = "ag-b" as AgentId;

const register = (
  recipients: { id: AgentId; name: string }[] = [{ id: A, name: "claude" }],
): void => registerSend(SEND, PROJECT, "switchboard", recipients);

const both = [
  { id: A, name: "claude" },
  { id: B, name: "codex" },
];

beforeEach(() => {
  notifyMock.mockClear();
  _testing.reset();
});

afterEach(() => {
  _testing.reset();
});

describe("send-completion tracker", () => {
  it("notifies once when a single-agent send completes", async () => {
    register();
    settleRecipient(SEND, A, "completed");

    expect(notifyMock).toHaveBeenCalledOnce();
    const [projectId, title, body] = notifyMock.mock.calls[0] as unknown as [
      ProjectId,
      string,
      string,
    ];
    expect(projectId).toBe(PROJECT);
    expect(title).toBe("Agent finished");
    // Names the project and the agent — with the app possibly in front, a
    // notification that doesn't say *which* project finished is worse than none.
    expect(body).toBe("switchboard: claude");
  });

  it("waits for the last recipient of a fan-out, then notifies once", async () => {
    register(both);

    settleRecipient(SEND, A, "completed");
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SEND, B, "completed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(notifyMock.mock.calls[0][2]).toBe("switchboard: claude, codex");
  });

  it("says so when the send failed", async () => {
    register();
    settleRecipient(SEND, A, "failed");
    expect(notifyMock.mock.calls[0][1]).toBe("Agent failed");
  });

  it("distinguishes a partial failure from a clean run", async () => {
    register(both);
    settleRecipient(SEND, A, "completed");
    settleRecipient(SEND, B, "failed");
    expect(notifyMock.mock.calls[0][1]).toBe("Agents finished, some failed");
  });

  it("stays silent when the user cancelled the whole send", async () => {
    // The user did this deliberately and was present to do it.
    register(both);
    settleRecipient(SEND, A, "cancelled");
    settleRecipient(SEND, B, "cancelled");
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it("still notifies about the survivors of a partially cancelled send", async () => {
    // Cancelling one of two agents doesn't make the other's result uninteresting,
    // and the cancelled one is left out of the text rather than reported as done.
    register(both);
    settleRecipient(SEND, A, "cancelled");
    settleRecipient(SEND, B, "completed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(notifyMock.mock.calls[0][2]).toBe("switchboard: codex");
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
    expect(notifyMock.mock.calls[0][1]).toBe("Agents failed");
  });

  it("ignores a repeat signal for a recipient that already settled", async () => {
    // An agent can emit `message_failed` and a synthesized `turn_end` for the same
    // dispatch; the second must not complete the send twice.
    register(both);
    settleRecipient(SEND, A, "failed");
    settleRecipient(SEND, A, "completed");
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SEND, B, "completed");
    expect(notifyMock).toHaveBeenCalledOnce();
    // The first outcome for a recipient wins — a later duplicate can't rewrite it.
    expect(notifyMock.mock.calls[0][1]).toBe("Agents finished, some failed");
  });

  it("never notifies again after a send has completed", async () => {
    register();
    settleRecipient(SEND, A, "completed");
    settleRecipient(SEND, A, "completed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(_testing.size()).toBe(0);
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
    settleAgentsRemoved([A]);
    expect(notifyMock).not.toHaveBeenCalled();

    settleRecipient(SEND, B, "completed");
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(notifyMock.mock.calls[0][2]).toBe("switchboard: codex");
  });

  it("stays silent and drops the send when every recipient is removed", async () => {
    register(both);
    settleAgentsRemoved([A, B]);
    expect(notifyMock).not.toHaveBeenCalled();
    expect(_testing.size()).toBe(0);
  });

  it("preserves an outcome already recorded before the agent was removed", async () => {
    // Teardown races a real terminal event; whichever arrives first wins, and the
    // second is a no-op rather than a rewrite.
    register(both);
    settleRecipient(SEND, A, "completed");
    settleAgentsRemoved([A, B]);
    expect(notifyMock).toHaveBeenCalledOnce();
    expect(notifyMock.mock.calls[0][1]).toBe("Agent finished");
    expect(notifyMock.mock.calls[0][2]).toBe("switchboard: claude");
  });

  it("ignores removal of an agent that was never a recipient", async () => {
    register([{ id: A, name: "claude" }]);
    settleAgentsRemoved([B]);
    expect(notifyMock).not.toHaveBeenCalled();
    expect(_testing.size()).toBe(1);
  });

  it("registers nothing for an empty recipient set", async () => {
    registerSend(SEND, PROJECT, "switchboard", []);
    expect(_testing.size()).toBe(0);
  });
});
