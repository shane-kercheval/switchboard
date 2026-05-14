import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { HEARTBEAT_TIMEOUT_MS, type AgentRecord, type NormalizedEvent } from "$lib/types";

// The component under test orchestrates three independent moving parts: the
// `invoke` IPC call (`sendMessage`), the per-agent event subscription
// (`listen`), and the reactive transcript / inFlight state. Pure reducer
// tests cover the state-transition table; these tests cover the *glue* —
// the place where ordering bugs, race conditions, and reactive-state
// staleness actually live.
//
// Approach: capture the `listen` callback so each test fires events on its
// own timeline. Manual control of when events arrive relative to when
// `sendMessage` resolves is what lets us reproduce the fast-events race
// without depending on real timers or fragile timing.

let listenerCallback: ((event: { payload: NormalizedEvent }) => void) | null = null;
let unlistenSpy: ReturnType<typeof vi.fn> = vi.fn();

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, cb: (e: { payload: NormalizedEvent }) => void) => {
    listenerCallback = cb;
    return unlistenSpy;
  }),
}));

const AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000001",
  project_id: "00000000-0000-7000-8000-000000000002",
  name: "assistant",
  harness: "claude_code",
  session_id: "00000000-0000-7000-8000-000000000003",
  created_at: "2026-05-13T00:00:00Z",
};

const TURN_ID = "00000000-0000-7000-8000-0000000000aa";

function fireEv(ev: NormalizedEvent): void {
  if (!listenerCallback) throw new Error("listener not registered yet");
  listenerCallback({ payload: ev });
}

async function waitForListener(): Promise<void> {
  await waitFor(() => expect(listenerCallback).not.toBeNull());
}

async function typeAndSend(text: string): Promise<void> {
  const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
  await fireEvent.input(textarea, { target: { value: text } });
  await fireEvent.click(screen.getByTestId("compose-send"));
}

beforeEach(() => {
  listenerCallback = null;
  unlistenSpy = vi.fn();
  invokeMock.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("AgentPane", () => {
  it("happy path: user turn appears immediately, agent turn streams in, idle restored", async () => {
    invokeMock.mockResolvedValueOnce(TURN_ID);

    const AgentPane = (await import("./AgentPane.svelte")).default;
    render(AgentPane, { props: { agent: AGENT } });
    await waitForListener();

    await typeAndSend("hello");

    // Optimistic user turn renders synchronously (before any event fires).
    expect(screen.getByText("hello")).toBeInTheDocument();

    // While in flight: status is "processing", Send disabled.
    await waitFor(() => {
      expect(screen.getByTestId("agent-status")).toHaveTextContent("processing");
    });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);

    // Stream the agent turn.
    fireEv({ type: "turn_start", turn_id: TURN_ID, started_at: "2026-05-13T00:00:01Z" });
    fireEv({ type: "content_chunk", turn_id: TURN_ID, text: "Hi " });
    fireEv({ type: "content_chunk", turn_id: TURN_ID, text: "there!" });
    fireEv({
      type: "turn_end",
      turn_id: TURN_ID,
      outcome: { status: "completed" },
      ended_at: "2026-05-13T00:00:02Z",
    });

    await waitFor(() => {
      expect(screen.getByText("Hi there!")).toBeInTheDocument();
      expect(screen.getByTestId("agent-status")).toHaveTextContent("idle");
    });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(true);
    // ^ disabled because the textarea is empty; re-typing re-enables.
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "follow-up" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);

    // Both turns rendered as distinct entries (catches the duplicate-id bug:
    // sharing the same id between user + agent turns broke Svelte's keyed
    // rendering and reactivity).
    expect(screen.getByText("hello")).toBeInTheDocument();
    expect(screen.getByText("Hi there!")).toBeInTheDocument();
  });

  it("fast-events race: events arrive before sendMessage resolves; Send re-enables", async () => {
    // The mock harness can emit every event before the IPC reply lands. The
    // component must handle this without leaving inFlightTurnId set to a
    // turn that has already completed.
    let resolveSend!: (value: string) => void;
    invokeMock.mockReturnValueOnce(
      new Promise<string>((resolve) => {
        resolveSend = resolve;
      }),
    );

    const AgentPane = (await import("./AgentPane.svelte")).default;
    render(AgentPane, { props: { agent: AGENT } });
    await waitForListener();

    await typeAndSend("ping");

    // Stream the entire agent turn BEFORE the IPC reply lands.
    fireEv({ type: "turn_start", turn_id: TURN_ID, started_at: "2026-05-13T00:00:01Z" });
    fireEv({ type: "content_chunk", turn_id: TURN_ID, text: "pong" });
    fireEv({
      type: "turn_end",
      turn_id: TURN_ID,
      outcome: { status: "completed" },
      ended_at: "2026-05-13T00:00:02Z",
    });

    // Now resolve the IPC. The post-await check should see the turn already
    // complete and NOT set inFlightTurnId.
    resolveSend(TURN_ID);

    await waitFor(() => {
      expect(screen.getByText("pong")).toBeInTheDocument();
      expect(screen.getByTestId("agent-status")).toHaveTextContent("idle");
    });
    // Once the textarea has content again, Send is enabled — proves
    // inFlightTurnId was correctly left null.
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "again" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("failed turn: error message displayed, status returns to idle, Send re-enables", async () => {
    invokeMock.mockResolvedValueOnce(TURN_ID);

    const AgentPane = (await import("./AgentPane.svelte")).default;
    render(AgentPane, { props: { agent: AGENT } });
    await waitForListener();

    await typeAndSend("trigger failure");

    fireEv({ type: "turn_start", turn_id: TURN_ID, started_at: "2026-05-13T00:00:01Z" });
    fireEv({ type: "content_chunk", turn_id: TURN_ID, text: "partial " });
    fireEv({
      type: "turn_end",
      turn_id: TURN_ID,
      outcome: { status: "failed", kind: "harness_error", message: "model unavailable" },
      ended_at: "2026-05-13T00:00:02Z",
    });

    await waitFor(() => {
      expect(screen.getByTestId("turn-error")).toHaveTextContent("model unavailable");
      expect(screen.getByText("partial")).toBeInTheDocument();
      expect(screen.getByTestId("agent-status")).toHaveTextContent("idle");
    });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "retry" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("sendMessage IPC throw: shows send-error banner, clears sending flag", async () => {
    invokeMock.mockRejectedValueOnce(new Error("dispatch failed"));

    const AgentPane = (await import("./AgentPane.svelte")).default;
    render(AgentPane, { props: { agent: AGENT } });
    await waitForListener();

    await typeAndSend("nope");

    // User turn was appended optimistically — stays in the transcript so
    // the user can see what they tried to send before the retry.
    expect(screen.getByText("nope")).toBeInTheDocument();

    await waitFor(() => {
      expect(screen.getByTestId("send-error")).toHaveTextContent("dispatch failed");
    });
    // Sending flag must be cleared even on throw — otherwise Send stays
    // disabled forever after a failed send.
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "retry" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("heartbeat timeout: silent stream marks turn failed and clears in-flight state", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    invokeMock.mockResolvedValueOnce(TURN_ID);

    const AgentPane = (await import("./AgentPane.svelte")).default;
    render(AgentPane, { props: { agent: AGENT } });
    await waitForListener();

    await typeAndSend("will hang");

    fireEv({ type: "turn_start", turn_id: TURN_ID, started_at: "2026-05-13T00:00:01Z" });
    // No content_chunks, no turn_end — simulates an adapter that violates
    // the stream contract (or a subprocess that hangs).

    // Just before the timeout window: turn is still streaming.
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    expect(screen.getByTestId("agent-status")).toHaveTextContent("processing");

    // Cross the threshold: heartbeat fires, reducer marks the turn failed.
    vi.advanceTimersByTime(200);

    await waitFor(() => {
      expect(screen.getByTestId("turn-error")).toHaveTextContent(
        "no response from harness — retry?",
      );
      expect(screen.getByTestId("agent-status")).toHaveTextContent("idle");
    });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "retry" } });
    expect((screen.getByTestId("compose-send") as HTMLButtonElement).disabled).toBe(false);
  });
});
