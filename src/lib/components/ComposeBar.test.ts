import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import type { AgentRecord, NormalizedEvent } from "$lib/types";

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const listeners = new Map<string, (e: { payload: NormalizedEvent }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: NormalizedEvent }) => void) => {
    listeners.set(name, cb);
    return vi.fn();
  }),
}));

async function loadState() {
  return await import("$lib/state/index.svelte");
}

const AGENT_A: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "alice",
  harness: "claude_code",
  session_id: "00000000-0000-7000-8000-000000000001",
  created_at: "2026-05-16T00:00:00Z",
};
const AGENT_B: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "bob",
  harness: "codex",
  session_id: null,
  created_at: "2026-05-16T00:00:01Z",
};

function fireTo(channel: string, event: NormalizedEvent): void {
  const cb = listeners.get(channel);
  if (cb === undefined) throw new Error(`no listener for ${channel}`);
  cb({ payload: event });
}

beforeEach(() => {
  listeners.clear();
  invokeMock.mockReset();
});

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
});

describe("ComposeBar", () => {
  it("Send is disabled until the chosen recipient is idle and prompt is non-empty", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    // Empty prompt → disabled even though agent is idle.
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });

    // Idle + non-empty prompt → enabled.
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("Send disables while run_status is starting (closes pre-TurnStart race)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    // Outstanding IPC: invoke is pending until we resolve manually.
    let resolveInvoke: (v: string) => void = () => {};
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveInvoke = resolve as (v: string) => void;
        }),
    );

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hello" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Now run_status === "starting" (synchronous after dispatchUserTurn).
    await waitFor(() => {
      expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);
    });
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("starting");

    // Settle the IPC.
    resolveInvoke("turn-1");
  });

  it("IPC failure calls failSendStart, restoring sendability and surfacing error", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockRejectedValueOnce(new Error("backend exploded"));

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hello" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Send-error visible.
    await waitFor(() => {
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("backend exploded");
    });
    // Runtime restored: idle + last_error set.
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A.id]?.last_error?.message).toBe("backend exploded");
  });

  it("preserves textarea text on send failure so the user can retry without retyping", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockRejectedValueOnce(new Error("network down"));

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "important prompt" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(screen.getByTestId("compose-send-error")).toBeInTheDocument();
    });
    // Textarea still has the original text — retry doesn't require retyping.
    expect(textarea.value).toBe("important prompt");
    // Send is enabled again (idle + non-empty prompt).
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("clears textarea on send success when prompt is unchanged", async () => {
    // Happy-path: user submits, IPC succeeds without the user typing
    // anything new during the await, textarea clears for the next
    // message.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValueOnce("turn-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hello" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Wait for the post-await success branch to clear the prompt.
    await waitFor(() => {
      expect(textarea.value).toBe("");
    });
    // User turn is in the transcript; runtime is "starting" (TurnStart
    // hasn't arrived yet — the listener events are separate).
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(1);
  });

  it("preserves new typing on send success when prompt has changed mid-await", async () => {
    // Capture-and-compare pattern: if `prompt.trim() === submittedText`
    // after the await, clear; otherwise preserve the user's new typing.
    // This protects against the rare race where the user types new text
    // during the IPC window and the send succeeds.
    await loadState().then((s) => s.registerAgent(AGENT_A));
    let resolveInvoke: (v: string) => void = () => {};
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveInvoke = resolve as (v: string) => void;
        }),
    );

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "first message" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // While the send is in flight, user types something new.
    await fireEvent.input(textarea, { target: { value: "second draft" } });

    // Send succeeds. Wait for the post-IPC microtask flush; the
    // capture-and-compare branch sees `prompt !== submittedText` and
    // leaves the new typing intact.
    resolveInvoke("turn-1");
    await waitFor(() => {
      expect(textarea.value).toBe("second draft");
    });
  });

  it("optimistic user turn appears immediately on click, before IPC reply", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    let resolveInvoke: (v: string) => void = () => {};
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveInvoke = resolve as (v: string) => void;
        }),
    );

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi there" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // User turn appended synchronously — visible before the IPC settles.
    const turns = state.transcripts[AGENT_A.id] ?? [];
    expect(turns).toHaveLength(1);
    expect(turns[0]?.role).toBe("user");
    if (turns[0]?.role !== "user") throw new Error("unreachable");
    expect(turns[0]?.text).toBe("hi there");

    resolveInvoke("turn-1");
  });

  it("hides recipient picker when only one agent is loaded", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    expect(screen.queryByTestId("recipient-picker")).toBeNull();
  });

  it("shows recipient picker with all agents when more than one is loaded", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    const picker = screen.getByTestId("recipient-picker") as HTMLSelectElement;
    const optionValues = Array.from(picker.options).map((o) => o.value);
    expect(optionValues).toEqual([AGENT_A.id, AGENT_B.id]);
  });

  it("sends to the picker-selected agent (not the default first)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    invokeMock.mockResolvedValueOnce("turn-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    const picker = screen.getByTestId("recipient-picker") as HTMLSelectElement;
    await fireEvent.change(picker, { target: { value: AGENT_B.id } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi bob" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // sendMessage called with agent_id = AGENT_B.
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "send_message",
        expect.objectContaining({ agentId: AGENT_B.id }),
      );
    });
    // User turn appended to B's transcript, not A's.
    expect((state.transcripts[AGENT_B.id] ?? []).length).toBe(1);
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(0);
  });

  it("a second click during 'starting' is rejected by the gate (no double-send)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    let resolveInvoke: (v: string) => void = () => {};
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveInvoke = resolve as (v: string) => void;
        }),
    );

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "first" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await fireEvent.input(textarea, { target: { value: "second" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    expect(invokeMock).toHaveBeenCalledTimes(1);
    // Only ONE user turn appended.
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(1);

    resolveInvoke("turn-1");
  });

  it("Send re-enables after TurnStart → AgentIdle sequence completes", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    // send_message resolves with the accepted-send receipt (message_id); the
    // correlated turn_start later carries that same message_id and a distinct
    // turn_id.
    const messageId = "msg-1";
    const turnId = "turn-1";
    invokeMock.mockResolvedValueOnce(messageId);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(state.runtimes[AGENT_A.id]?.run_status).toBe("starting");
    });
    // The accepted-send receipt is recorded for correlation.
    await waitFor(() => {
      expect(state.runtimes[AGENT_A.id]?.pending_message_id).toBe(messageId);
    });
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_start",
      turn_id: turnId,
      message_id: messageId,
      started_at: "2026-05-16T00:00:00Z",
    });
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_A.id]?.in_flight_turn_id).toBe(turnId);
    expect(state.runtimes[AGENT_A.id]?.pending_message_id).toBeUndefined();
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_end",
      turn_id: turnId,
      outcome: { status: "completed" },
      ended_at: "2026-05-16T00:00:01Z",
    });
    // turn_end does NOT flip back to idle — AgentIdle does.
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("processing");
    fireTo(`agent:${AGENT_A.id}`, { type: "agent_idle", agent_id: AGENT_A.id });
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("idle");

    // Send is back to enabled after entering a fresh prompt.
    await fireEvent.input(textarea, { target: { value: "again" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("message_failed (pre-turn failure) re-enables Send and surfaces the error", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const messageId = "msg-fail-1";
    invokeMock.mockResolvedValueOnce(messageId);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(state.runtimes[AGENT_A.id]?.pending_message_id).toBe(messageId);
    });
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("starting");

    // The send was accepted but failed before any turn started — arrives as
    // a message_failed event keyed by the same message_id.
    fireTo(`agent:${AGENT_A.id}`, {
      type: "message_failed",
      message_id: messageId,
      agent_id: AGENT_A.id,
      error: "journal write failed",
      at: "2026-05-16T00:00:00Z",
    });

    // run_status flips back to idle (Send re-enabled) and the error surfaces.
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A.id]?.pending_message_id).toBeUndefined();
    expect(state.runtimes[AGENT_A.id]?.last_error).toEqual({
      message: "journal write failed",
      kind: "adapter_failure",
    });
    // The optimistic user turn is preserved (the user did submit it).
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(1);
  });
});
