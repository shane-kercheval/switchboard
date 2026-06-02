import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
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

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";

const AGENT_A: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: PROJECT_ID,
  name: "alice",
  harness: "claude_code",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-05-16T00:00:00Z",
};
const AGENT_B: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "bob",
  harness: "codex",
  session_locator: null,
  created_at: "2026-05-16T00:00:01Z",
};

async function loadComposeStore() {
  return await import("$lib/state/composeStore");
}

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
  (await loadComposeStore())._testing.reset();
});

describe("ComposeBar", () => {
  it("hides the recipient field for a single agent but still sends to it", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValueOnce("msg-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("grows the message box with content up to its max height", async () => {
    const scrollHeight = vi.spyOn(HTMLTextAreaElement.prototype, "scrollHeight", "get");
    const getComputedStyleSpy = vi.spyOn(window, "getComputedStyle");
    try {
      const state = await loadState();
      await state.registerAgent(AGENT_A);

      getComputedStyleSpy.mockReturnValue({ maxHeight: "192px" } as CSSStyleDeclaration);
      scrollHeight.mockImplementation(function (this: HTMLTextAreaElement): number {
        if (this.value.includes("six")) return this.style.height === "auto" ? 240 : 192;
        if (this.value === "short again") return 72;
        return 96;
      });
      const ComposeBar = (await import("./ComposeBar.svelte")).default;
      render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
      await tick();
      const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
      expect(textarea.style.height).toBe("96px");
      expect(textarea.style.overflowY).toBe("hidden");

      await fireEvent.input(textarea, { target: { value: "one\ntwo\nthree\nfour\nfive\nsix" } });
      await tick();
      expect(textarea.style.height).toBe("192px");
      expect(textarea.style.overflowY).toBe("auto");

      await fireEvent.input(textarea, { target: { value: "short again" } });
      await tick();
      expect(textarea.style.height).toBe("72px");
      expect(textarea.style.overflowY).toBe("hidden");
    } finally {
      scrollHeight.mockRestore();
      getComputedStyleSpy.mockRestore();
    }
  });

  it("toggles a recipient on and off by clicking its chip", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "ping @bo" } });
    // bob is offered (alice is already selected); Enter picks bob as the sole recipient.
    await screen.findByTestId(`recipient-option-${AGENT_B.id}`);
    await fireEvent.keyDown(textarea, { key: "Enter" });

    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    // The "@bo" token is stripped; the text typed before it (with its space) stays.
    expect(textarea.value).toBe("ping ");
  });

  it("a bare @ offers All / Clear actions that bulk-select and deselect", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

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

  it("the empty-draft stop cancels ALL live sends, not just the latest", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    invokeMock.mockResolvedValue("msg-x");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    // Send #1 to alice (default selected).
    await fireEvent.input(textarea, { target: { value: "to alice" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    // Send #2 to bob only (toggle alice off, bob on).
    await fireEvent.click(chip(AGENT_A.id));
    await fireEvent.click(chip(AGENT_B.id));
    await fireEvent.input(textarea, { target: { value: "to bob" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    // Two distinct sends are now live → the stop affordance covers all of them.
    await waitFor(() => {
      expect(screen.getByTestId("compose-send")).toHaveAttribute("aria-label", "Cancel all sends");
    });
    await fireEvent.click(screen.getByTestId("compose-send"));

    const cancelCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "cancel_send");
    const cancelledSendIds = new Set(cancelCalls.map((c) => (c[1] as { sendId: string }).sendId));
    expect(cancelledSendIds.size).toBe(2); // both sends cancelled, not just the last
    const cancelledRecipients = cancelCalls.flatMap(
      (c) => (c[1] as { recipients: string[] }).recipients,
    );
    expect(cancelledRecipients).toEqual(expect.arrayContaining([AGENT_A.id, AGENT_B.id]));
  });

  it("uses Mod+Enter to cancel when the empty-draft send button is in stop mode", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    await fireEvent.click(chip(AGENT_B.id));

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "go" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      expect(screen.getByTestId("compose-send-error")).toHaveTextContent("bob exploded");
    });
    expect(state.runtimes[AGENT_B.id]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A.id]?.run_status).toBe("starting");
    // alice is still pending → just her user turn; bob's failure surfaces as a
    // failed agent turn beneath his user turn.
    expect((state.transcripts[AGENT_A.id] ?? []).length).toBe(1);
    const bobTurns = state.transcripts[AGENT_B.id] ?? [];
    expect(bobTurns.length).toBe(2);
    const bobFailed = bobTurns[1];
    expect(bobFailed?.role === "agent" ? bobFailed.status : null).toBe("failed");
  });

  it("clears the prompt on submit but keeps the recipients selected (sticky)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });

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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
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
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
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

describe("ComposeBar persistence", () => {
  it("retains draft and recipient selection across a project-switch remount", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const ComposeBar = (await import("./ComposeBar.svelte")).default;

    const first = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "half-written" },
    });
    await fireEvent.click(chip(AGENT_B.id)); // alice (default) + bob
    first.unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "half-written",
    );
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
  });

  it("restores draft and selection persisted by a previous session (restart)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const store = await loadComposeStore();
    store.setDraft(PROJECT_ID, "from last time");
    store.setSelection(PROJECT_ID, [AGENT_B.id]);
    store._testing.reloadFromStorage(); // drop in-memory copy; re-read localStorage

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe(
      "from last time",
    );
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
  });

  it("clears the persisted draft on send so it can't reappear next time", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");
    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "send me" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => expect(textarea.value).toBe(""));
    const store = await loadComposeStore();
    expect(store.getCompose(PROJECT_ID).draft).toBe("");
  });

  it("persists a deliberate deselect-all and restores it as empty (not the default)", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const ComposeBar = (await import("./ComposeBar.svelte")).default;

    const first = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await fireEvent.click(screen.getByTestId("recipient-clear"));
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    const store = await loadComposeStore();
    expect(store.getCompose(PROJECT_ID).selectedIds).toEqual([]);
    first.unmount();

    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "false");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("drops a saved recipient whose agent no longer exists on restore", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const store = await loadComposeStore();
    store.setSelection(PROJECT_ID, ["00000000-0000-7000-8000-00000000dead", AGENT_A.id]);
    store._testing.reloadFromStorage();

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
    // The ghost id is pruned from the persisted set too (init re-persists).
    expect(store.getCompose(PROJECT_ID).selectedIds).toEqual([AGENT_A.id]);
  });

  it("keeps drafts isolated per project", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    const OTHER_PROJECT = "00000000-0000-7000-8000-0000000000ee";

    const first = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });
    await fireEvent.input(screen.getByTestId("compose-textarea"), {
      target: { value: "project one's draft" },
    });
    first.unmount();

    render(ComposeBar, { props: { projectId: OTHER_PROJECT, agents: [AGENT_A] } });
    expect((screen.getByTestId("compose-textarea") as HTMLTextAreaElement).value).toBe("");
  });

  it("recovers a single-agent project from a saved selection whose agent is gone", async () => {
    // Saved "send to bob" against a project that now has only alice: bob is
    // filtered out, and a single-agent project shows no chips — so without the
    // single-agent guard the composer would be unsendable with no recovery UI.
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    invokeMock.mockResolvedValue("msg-1");
    const store = await loadComposeStore();
    store.setSelection(PROJECT_ID, [AGENT_B.id]);
    store._testing.reloadFromStorage();

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A] } });

    expect(screen.queryByTestId("recipient-field")).toBeNull(); // no chips for one agent
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.[1]).toMatchObject({ agentId: AGENT_A.id });
    });
  });

  it("falls back to the default when a saved multi-agent selection is all stale", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const store = await loadComposeStore();
    store.setSelection(PROJECT_ID, [
      "00000000-0000-7000-8000-00000000dea1",
      "00000000-0000-7000-8000-00000000dea2",
    ]);
    store._testing.reloadFromStorage();

    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] } });
    // All saved ids are gone → default to the first agent rather than empty.
    expect(chip(AGENT_A.id)).toHaveAttribute("data-selected", "true");
    expect(chip(AGENT_B.id)).toHaveAttribute("data-selected", "false");
  });

  it("a transient empty roster does not clobber the saved selection", async () => {
    const state = await loadState();
    await state.registerAgent(AGENT_A);
    await state.registerAgent(AGENT_B);
    const ComposeBar = (await import("./ComposeBar.svelte")).default;
    const store = await loadComposeStore();

    const { rerender } = render(ComposeBar, {
      props: { projectId: PROJECT_ID, agents: [AGENT_A, AGENT_B] },
    });
    await fireEvent.click(chip(AGENT_B.id)); // persist alice + bob
    expect(store.getCompose(PROJECT_ID).selectedIds).toEqual([AGENT_A.id, AGENT_B.id]);

    await rerender({ projectId: PROJECT_ID, agents: [] });
    // The roster-gated write must skip the empty roster, leaving the save intact.
    expect(store.getCompose(PROJECT_ID).selectedIds).toEqual([AGENT_A.id, AGENT_B.id]);
  });
});
