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

const chip = (id: string) => screen.getByTestId(`recipient-chip-${id}`);

beforeEach(() => {
  listeners.clear();
  invokeMock.mockReset();
});

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
});

describe("ComposeBar", () => {
  it("hides the recipient field for a single agent but still sends to it", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValueOnce("msg-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    expect(screen.queryByTestId("recipient-field")).toBeNull();
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.[1]).toMatchObject({ agentId: AGENT_A.id, prompt: "hi" });
      expect(typeof (calls[0]?.[1] as { sendId?: unknown }).sendId).toBe("string");
    });
  });

  it("shows a toggle chip per agent; the first is selected by default", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("toggles a recipient on and off by clicking its chip", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id));
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    // Toggle alice off; bob stays on.
    await fireEvent.click(chip(AGENT_A.id));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
  });

  it("@-quick-add: typing @bob opens the menu, selects via keyboard, strips the token", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "ping @bo" } });
    // bob is offered (alice is already selected); Enter picks the highlighted.
    await screen.findByTestId(`recipient-option-${AGENT_B.id}`);
    await fireEvent.keyDown(textarea, { key: "Enter" });

    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    // The "@bo" token is stripped; the text typed before it (with its space) stays.
    expect(textarea.value).toBe("ping ");
  });

  it("a bare @ offers All / Clear actions that bulk-select and deselect", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "@" } });

    // All → every agent selected.
    await fireEvent.click(await screen.findByTestId("recipient-option-all"));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    expect(textarea.value).toBe(""); // the @ token is stripped

    // Clear → none selected.
    await fireEvent.input(textarea, { target: { value: "@" } });
    await fireEvent.click(await screen.findByTestId("recipient-option-clear"));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("hides All when everyone is selected and Clear when no one is", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    // Select everyone (alice is default; add bob) → All has nothing to do.
    await fireEvent.keyDown(document.body, { key: "2", metaKey: true });
    await fireEvent.input(textarea, { target: { value: "@" } });
    expect(await screen.findByTestId("recipient-option-clear")).toBeInTheDocument();
    expect(screen.queryByTestId("recipient-option-all")).toBeNull();

    // Clear everyone → Clear has nothing to do.
    await fireEvent.click(screen.getByTestId("recipient-option-clear"));
    await fireEvent.input(textarea, { target: { value: "@" } });
    expect(await screen.findByTestId("recipient-option-all")).toBeInTheDocument();
    expect(screen.queryByTestId("recipient-option-clear")).toBeNull();
  });

  it("Mod+N toggles the Nth agent (sidebar order)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });
    // alice (index 0) selected by default; bob not.
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");

    // Mod+2 toggles the second agent on; Mod+1 toggles the first off.
    await fireEvent.keyDown(document.body, { key: "2", metaKey: true });
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    await fireEvent.keyDown(document.body, { key: "1", metaKey: true });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
  });

  it("Mod+Shift+A selects every agent", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    await fireEvent.keyDown(document.body, { key: "a", metaKey: true, shiftKey: true });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
  });

  it("fans one message out to all selected recipients sharing one send_id", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    invokeMock.mockResolvedValue("msg-x");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id)); // select both

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "status?" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(calls).toHaveLength(2);
    });
    const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
    const agentIds = calls.map((c) => (c[1] as { agentId: string }).agentId).sort();
    expect(agentIds).toEqual([AGENT_A.id, AGENT_B.id].sort());
    const sendIds = new Set(calls.map((c) => (c[1] as { sendId: string }).sendId));
    expect(sendIds.size).toBe(1);
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(1);
    expect((state.transcripts[AGENT_B.id] ?? []).length).toBe(1);
  });

  it("turns the empty-draft send button into cancel for the latest live send", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    invokeMock.mockResolvedValue("msg-x");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    await fireEvent.click(chip(AGENT_B.id));
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "status?" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Cancel send");
    });
    await fireEvent.click(screen.getByTestId("compose-send"));

    const cancelCall = invokeMock.mock.calls.find(([cmd]) => cmd === "cancel_send");
    expect(cancelCall?.[1]).toMatchObject({
      recipients: expect.arrayContaining([AGENT_A.id, AGENT_B.id]),
    });
  });

  it("uses Mod+Enter to cancel when the empty-draft send button is in stop mode", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "first" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Cancel send");
    });
    await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });

    const sendCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message");
    const cancelCall = invokeMock.mock.calls.find(([cmd]) => cmd === "cancel_send");
    expect(sendCalls).toHaveLength(1);
    expect(cancelCall?.[1]).toMatchObject({ recipients: [AGENT_A.id] });
  });

  it("send-while-busy is un-gated: Send stays enabled while a recipient is processing", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "first" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    fireTo(`agent:${AGENT_A.id}`, {
      type: "turn_start",
      turn_id: "turn-1",
      message_id: "msg-1",
      started_at: "2026-05-16T00:00:00Z",
    });
    await waitFor(() => expect(state.runtimes[AGENT_A.id]?.run_status).toBe("processing"));

    await fireEvent.input(textarea, { target: { value: "second" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Send");
  });

  it("a per-recipient IPC failure fails only that recipient and surfaces the error", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    // Dispatch order is selection order: alice (default) then bob.
    invokeMock.mockResolvedValueOnce("msg-a").mockRejectedValueOnce(new Error("bob exploded"));

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });
    await fireEvent.click(chip(AGENT_B.id));

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "go" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("bob exploded");
    });
    expect(state.runtimes[AGENT_B.id]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("starting");
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(1);
    expect((state.transcripts[AGENT_B.id] ?? []).length).toBe(1);
  });

  it("clears the prompt on submit but keeps the recipients selected (sticky)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => expect(textarea.value).toBe(""));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
  });

  it("Clear and Escape (with composer focus) both deselect all recipients", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");

    await fireEvent.click(screen.getByTestId("recipient-clear"));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");

    // Re-select, then clear via Escape while the composer holds focus.
    await fireEvent.click(chip(AGENT_A.id));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    textarea.focus();
    await fireEvent.keyDown(window, { key: "Escape" });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
  });

  it("Escape is a no-op when focus is outside the composer", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");

    // Focus an element outside the compose surface; Escape must not clear the
    // recipients (Escape is overloaded across the app and only owns the
    // composer's selection while the composer has focus).
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    outside.focus();
    await fireEvent.keyDown(window, { key: "Escape" });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    outside.remove();
  });
});
