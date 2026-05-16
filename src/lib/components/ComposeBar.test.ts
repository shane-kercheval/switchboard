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
    invokeMock.mockResolvedValueOnce("turn-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(state.runtimes[AGENT_A.id]?.run_status).toBe("starting");
    });
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_start",
      turn_id: "turn-1",
      started_at: "2026-05-16T00:00:00Z",
    });
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("processing");
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_end",
      turn_id: "turn-1",
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
});
