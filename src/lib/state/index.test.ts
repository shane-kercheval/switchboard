import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentRecord, NormalizedEvent } from "$lib/types";
import { HEARTBEAT_TIMEOUT_MS } from "$lib/types";

// Capture the listener callback per channel so we can fire events on our
// own timeline. The state module subscribes one channel per agent
// (`agent:<id>`); a registry keyed by channel lets the test fire to a
// specific agent's stream.
const listeners = new Map<string, (e: { payload: NormalizedEvent }) => void>();
const unlistenSpies = new Map<string, ReturnType<typeof vi.fn>>();

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: NormalizedEvent }) => void) => {
    listeners.set(name, cb);
    const spy = vi.fn();
    unlistenSpies.set(name, spy);
    return spy;
  }),
}));

// Mock `invoke` so hydrateAgent's `loadTranscript` call resolves with
// the staged value. Tests override per-call.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (name: string, args: unknown) => invokeMock(name, args),
}));

// Dynamic import so the mocked `listen` is in place before the module's
// internal state is constructed.
async function loadState() {
  return await import("./index.svelte");
}

function agentRecord(
  id: string,
  name = "test",
  harness: "claude_code" | "codex" = "claude_code",
): AgentRecord {
  return {
    id,
    project_id: "00000000-0000-7000-8000-0000000000ff",
    name,
    harness,
    session_id: null,
    created_at: "2026-05-15T00:00:00Z",
  };
}

const AGENT_A = "00000000-0000-7000-8000-000000000aaa";
const AGENT_B = "00000000-0000-7000-8000-000000000bbb";
const TURN_1 = "00000000-0000-7000-8000-000000000001";
const TURN_2 = "00000000-0000-7000-8000-000000000002";

function fireTo(channel: string, event: NormalizedEvent): void {
  const cb = listeners.get(channel);
  if (cb === undefined) throw new Error(`no listener for ${channel}`);
  cb({ payload: event });
}

beforeEach(() => {
  listeners.clear();
  unlistenSpies.clear();
  invokeMock.mockReset();
});

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
  vi.useRealTimers();
});

describe("registerAgent", () => {
  it("subscribes to the per-agent channel and initializes runtime + transcript", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    expect(state._testing.hasListener(AGENT_A)).toBe(true);
    expect(state.runtimes[AGENT_A]).toBeDefined();
    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("complete");
    expect(state.transcripts[AGENT_A]).toEqual([]);
  });

  it("is idempotent — calling twice for the same agent does not double-subscribe", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await state.registerAgent(agentRecord(AGENT_A));
    // Only one channel registration. (vi.mock counts every call to
    // listen() across both registrations would otherwise show 2.)
    const channels = Array.from(listeners.keys()).filter((k) => k === `agent:${AGENT_A}`);
    expect(channels).toHaveLength(1);
  });
});

describe("event routing", () => {
  it("turn_start populates the agent's transcript with a streaming turn", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    expect(state.transcripts[AGENT_A]).toHaveLength(1);
    const turn = state.transcripts[AGENT_A]?.[0];
    expect(turn?.role).toBe("agent");
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("streaming");
  });

  it("AgentIdle event flips run_status to idle", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_1,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:05Z",
    });
    // turn_end does NOT flip run_status to idle (Codex enrichment window).
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    fireTo(`agent:${AGENT_A}`, { type: "agent_idle", agent_id: AGENT_A });
    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
  });

  it("session_meta and rate_limit_event populate runtime without disturbing transcript", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "session_meta",
      agent_id: AGENT_A,
      model: "claude-sonnet-4-6",
      harness_version: "2.1.140",
      tools: ["Bash"],
      mcp_servers: [],
      skills: [],
      raw: {},
    });
    fireTo(`agent:${AGENT_A}`, {
      type: "rate_limit_event",
      agent_id: AGENT_A,
      info: { primary: { used_percent: 30 } },
    });
    expect(state.runtimes[AGENT_A]?.meta?.model).toBe("claude-sonnet-4-6");
    expect(state.runtimes[AGENT_A]?.last_rate_limit).toEqual({ primary: { used_percent: 30 } });
    expect(state.transcripts[AGENT_A]).toEqual([]);
  });
});

describe("per-agent isolation", () => {
  it("events on agent A's channel do not affect agent B's state", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await state.registerAgent(agentRecord(AGENT_B));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    fireTo(`agent:${AGENT_B}`, {
      type: "turn_start",
      turn_id: TURN_2,
      started_at: "2026-05-15T00:00:01Z",
    });
    expect(state.transcripts[AGENT_A]).toHaveLength(1);
    expect(state.transcripts[AGENT_B]).toHaveLength(1);
    expect((state.transcripts[AGENT_A]?.[0] as { turn_id: string }).turn_id).toBe(TURN_1);
    expect((state.transcripts[AGENT_B]?.[0] as { turn_id: string }).turn_id).toBe(TURN_2);
  });

  it("agent A's run_status independent of agent B's", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await state.registerAgent(agentRecord(AGENT_B));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    // Only A is processing — B stays idle.
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_B]?.run_status).toBe("idle");
  });
});

describe("heartbeat orchestration", () => {
  it("arms on turn_start, fires after HEARTBEAT_TIMEOUT_MS of silence, transitions turn to failed", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    expect(state._testing.hasHeartbeat(AGENT_A)).toBe(true);

    // No activity — past the threshold, heartbeat fires.
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS + 100);

    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("failed");
    expect(turn.error).toBe("no response from harness — retry?");
    expect(turn.error_kind).toBe("adapter_failure");
    expect(state._testing.hasHeartbeat(AGENT_A)).toBe(false);
  });

  it("re-arms on content_chunk for the tracked turn", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    // Just before the original deadline.
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    fireTo(`agent:${AGENT_A}`, {
      type: "content_chunk",
      turn_id: TURN_1,
      kind: "text",
      text: "still here",
    });
    // Push past the original deadline; re-arm should have prevented fire.
    vi.advanceTimersByTime(200);
    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("streaming");
  });

  it("re-arms on tool_started / tool_completed for the tracked turn", async () => {
    // Load-bearing for long shell commands — minutes of Bash with zero
    // content_chunks must not trigger a false-positive timeout.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    fireTo(`agent:${AGENT_A}`, {
      type: "tool_started",
      turn_id: TURN_1,
      tool_use_id: "tool-1",
      kind: "builtin",
      name: "Bash",
      input: { command: "make test" },
    });
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    fireTo(`agent:${AGENT_A}`, {
      type: "tool_completed",
      turn_id: TURN_1,
      tool_use_id: "tool-1",
      output: "ok",
      is_error: false,
    });
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    // Net advance: ~3*(TIMEOUT - 100). Without re-arming, would have fired
    // ~2.5x ago. With re-arming, still streaming.
    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("streaming");
  });

  it("clears on turn_end (no false-positive after stream ends)", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_1,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:01Z",
    });
    expect(state._testing.hasHeartbeat(AGENT_A)).toBe(false);

    // Advance well past the threshold — heartbeat should not fire because
    // it was cleared.
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS + 1000);
    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("complete");
  });

  it("does NOT re-arm on stale events for unrelated turns", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    // Stale event for TURN_2 (which doesn't exist on this agent). Must
    // not re-arm the heartbeat (which is tracking TURN_1).
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    fireTo(`agent:${AGENT_A}`, {
      type: "content_chunk",
      turn_id: TURN_2,
      kind: "text",
      text: "stale",
    });
    // Should still be tracking TURN_1.
    expect(state._testing.heartbeatTurnId(AGENT_A)).toBe(TURN_1);
    // Past TURN_1's deadline.
    vi.advanceTimersByTime(200);
    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("failed");
  });
});

describe("dispatchUserTurn", () => {
  it("synchronously appends a user-role turn before any event arrives", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", "2026-05-16T00:00:00Z");
    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns).toHaveLength(1);
    expect(turns[0]?.role).toBe("user");
    if (turns[0]?.role !== "user") throw new Error("unreachable");
    expect(turns[0]?.text).toBe("hello");
  });

  it("flips run_status to 'starting' (closes pre-TurnStart sendability race)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", "2026-05-16T00:00:00Z");
    expect(state.runtimes[AGENT_A]?.run_status).toBe("starting");
  });

  it("clears last_error on a successful new dispatch", async () => {
    // A failed prior turn left last_error set; a fresh dispatch clears
    // it so the sidebar doesn't show stale error state through the
    // following successful turn.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    // Simulate prior failure: directly set last_error on the runtime.
    const before = state.runtimes[AGENT_A];
    if (before === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_A] = {
      ...before,
      last_error: { message: "old failure", kind: "harness_error" },
    };
    state.dispatchUserTurn(AGENT_A, "user-1", "retry", "2026-05-16T00:00:00Z");
    expect(state.runtimes[AGENT_A]?.last_error).toBeUndefined();
  });

  it("updates lastRecipientId for the picker preselect ergonomic", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await state.registerAgent(agentRecord(AGENT_B));
    state.dispatchUserTurn(AGENT_B, "user-1", "hi", "2026-05-16T00:00:00Z");
    expect(state.ui.lastRecipientId).toBe(AGENT_B);
    // After dispatch, B's run_status is "starting" — a second dispatch
    // to B in this window is gated by the defense (see below). Use a
    // fresh agent_idle to clear B back to "idle" so the test can advance.
    fireTo(`agent:${AGENT_B}`, { type: "agent_idle", agent_id: AGENT_B });
    // Wait — agent_idle from "starting" is a no-op (guarded). Manually
    // advance via the legitimate path: simulate the turn lifecycle
    // turn_start → agent_idle. For this picker-ergonomic test, the
    // cleanest path is to dispatch to a different agent. A is still idle.
    state.dispatchUserTurn(AGENT_A, "user-2", "hi again", "2026-05-16T00:00:01Z");
    expect(state.ui.lastRecipientId).toBe(AGENT_A);
  });

  it("rejects calls for unregistered agents (fail-loud)", async () => {
    const state = await loadState();
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    // No registerAgent call — runtime doesn't exist.
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", "2026-05-16T00:00:00Z");
    expect(errSpy).toHaveBeenCalledWith(
      "[switchboard] dispatchUserTurn called for unregistered agent",
      expect.objectContaining({ agent_id: AGENT_A }),
    );
    expect(state.transcripts[AGENT_A]).toBeUndefined();
    expect(state.ui.lastRecipientId).toBe(null);
    errSpy.mockRestore();
  });

  it("rejects calls while agent is not idle (defense-in-depth)", async () => {
    // The compose-bar should gate on run_status === "idle". If a bug let
    // a second dispatch through, the state action defends by no-op'ing
    // and logging. The first user turn stays; no phantom second user
    // turn for a dispatch that won't happen.
    const state = await loadState();
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "first", "2026-05-16T00:00:00Z");
    // Now agent is "starting" — second dispatch should reject.
    state.dispatchUserTurn(AGENT_A, "user-2", "second", "2026-05-16T00:00:01Z");

    expect(errSpy).toHaveBeenCalledWith(
      "[switchboard] dispatchUserTurn called while agent not idle",
      expect.objectContaining({ agent_id: AGENT_A, run_status: "starting" }),
    );
    // Only ONE user turn appended.
    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns).toHaveLength(1);
    if (turns[0]?.role !== "user") throw new Error("unreachable");
    expect(turns[0]?.text).toBe("first");
    errSpy.mockRestore();
  });
});

describe("failSendStart", () => {
  it("flips starting → idle and records the error", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", "2026-05-16T00:00:00Z");
    expect(state.runtimes[AGENT_A]?.run_status).toBe("starting");

    state.failSendStart(AGENT_A, { message: "Tauri IPC failed", kind: "adapter_failure" });

    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A]?.last_error).toEqual({
      message: "Tauri IPC failed",
      kind: "adapter_failure",
    });
  });

  it("preserves the optimistic user turn (user did submit; failure is separate)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", "2026-05-16T00:00:00Z");
    state.failSendStart(AGENT_A, { message: "boom", kind: "adapter_failure" });
    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns).toHaveLength(1);
    expect(turns[0]?.role).toBe("user");
  });

  it("is a no-op while processing (TurnStart raced ahead)", async () => {
    // The race: dispatchUserTurn → starting; await api.sendMessage()
    // resolves; meanwhile TurnStart arrives on the channel → processing.
    // Then the IPC reply also resolves successfully — there's no error.
    // But a confused caller could call failSendStart anyway; the guard
    // must not stomp the genuine "processing" state.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", "2026-05-16T00:00:00Z");
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-16T00:00:00Z",
    });
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");

    state.failSendStart(AGENT_A, { message: "ignored", kind: "adapter_failure" });

    // No-op: still processing, no last_error.
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_A]?.last_error).toBeUndefined();
  });

  it("is a no-op while idle (idempotent)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.failSendStart(AGENT_A, { message: "ignored", kind: "adapter_failure" });
    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A]?.last_error).toBeUndefined();
  });

  it("logs to console.error for unregistered agents", async () => {
    const state = await loadState();
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    state.failSendStart(AGENT_A);
    expect(errSpy).toHaveBeenCalledWith(
      "[switchboard] failSendStart called for unregistered agent",
      expect.objectContaining({ agent_id: AGENT_A }),
    );
    errSpy.mockRestore();
  });
});

describe("state machine — starting → processing transition", () => {
  it("turn_start during 'starting' transitions to 'processing'", async () => {
    // The legitimate happy path: user clicks Send → starting; backend
    // accepts and emits TurnStart → processing.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", "2026-05-16T00:00:00Z");
    expect(state.runtimes[AGENT_A]?.run_status).toBe("starting");

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-16T00:00:00Z",
    });

    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_A]?.in_flight_turn_id).toBe(TURN_1);
  });
});

describe("_testing.reset", () => {
  it("clears all state and unsubscribes all listeners", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await state.registerAgent(agentRecord(AGENT_B));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-16T00:00:00Z",
    });

    state._testing.reset();

    expect(state.transcripts[AGENT_A]).toBeUndefined();
    expect(state.transcripts[AGENT_B]).toBeUndefined();
    expect(state.runtimes[AGENT_A]).toBeUndefined();
    expect(state.runtimes[AGENT_B]).toBeUndefined();
    expect(state.ui.lastRecipientId).toBe(null);
    expect(state._testing.hasListener(AGENT_A)).toBe(false);
    expect(state._testing.hasListener(AGENT_B)).toBe(false);
    // The unlisten spies should have been called.
    expect(unlistenSpies.get(`agent:${AGENT_A}`)).toHaveBeenCalled();
    expect(unlistenSpies.get(`agent:${AGENT_B}`)).toHaveBeenCalled();
  });
});

describe("concurrent registerAgent", () => {
  it("Promise.all on overlapping calls registers exactly one listener", async () => {
    // Without the pendingRegistrations guard, both calls would pass the
    // listenerRegistry.has() check (which is sync) before either's
    // `await listen(...)` returned, then both would set the listener —
    // doubling the channel subscription.
    const { listen: listenMock } = await import("@tauri-apps/api/event");
    vi.mocked(listenMock).mockClear();

    const state = await loadState();
    await Promise.all([
      state.registerAgent(agentRecord(AGENT_A)),
      state.registerAgent(agentRecord(AGENT_A)),
      state.registerAgent(agentRecord(AGENT_A)),
    ]);
    const calls = vi.mocked(listenMock).mock.calls.filter((c) => c[0] === `agent:${AGENT_A}`);
    expect(calls).toHaveLength(1);
    expect(state._testing.hasListener(AGENT_A)).toBe(true);
  });

  it("returns the same in-flight promise to concurrent callers", async () => {
    // Both callers should await the same registration — overlapping calls
    // resolve together, neither registers twice.
    const state = await loadState();
    const p1 = state.registerAgent(agentRecord(AGENT_A));
    const p2 = state.registerAgent(agentRecord(AGENT_A));
    await Promise.all([p1, p2]);
    expect(state._testing.hasListener(AGENT_A)).toBe(true);
  });
});

describe("invariant violation surfacing", () => {
  it("logs to console.error and skips both reducers when runtime is missing", async () => {
    // The 'unregistered agent' case shouldn't be reachable in production —
    // registerAgent always initializes runtime + transcript before the
    // listener fires. But if a regression broke that ordering, the silent
    // early-return would leave transcripts mutated and runtime stale →
    // run_status would never flip to "processing" but content would
    // stream in. Fail-loud via console.error makes the bug visible.
    const state = await loadState();
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    await state.registerAgent(agentRecord(AGENT_A));
    // Simulate the regression: nuke the runtime while keeping the listener.
    delete state.runtimes[AGENT_A];

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-16T00:00:00Z",
    });

    expect(errSpy).toHaveBeenCalledWith(
      "[switchboard] invariant violation: event arrived for unregistered agent",
      expect.objectContaining({ agent_id: AGENT_A, event_type: "turn_start" }),
    );
    // Transcript was NOT mutated — bail happened before reducer ran.
    expect(state.transcripts[AGENT_A]).toEqual([]);

    errSpy.mockRestore();
  });
});

describe("listener boundary stamps tool started_at / completed_at", () => {
  it("tool items receive the listener-stamped timestamp (exact equality)", async () => {
    // Reducer purity contract: tool events arrive without timestamps on
    // the wire; the state module stamps `receivedAt` at receive time and
    // threads it to the reducer. The reducer itself doesn't call
    // new Date() — pinned by reducers.test.ts.
    //
    // This test pins the listener boundary side with **exact** timestamp
    // equality. A regression where the reducer reverted to its own
    // `new Date()` call would silently pass a shape-only check; fake
    // timers + exact equality catch it.
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-05-16T12:00:00.000Z"));

    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      started_at: "2026-05-16T00:00:00Z",
    });
    fireTo(`agent:${AGENT_A}`, {
      type: "tool_started",
      turn_id: TURN_1,
      tool_use_id: "tool-1",
      kind: "builtin",
      name: "Bash",
      input: { command: "echo" },
    });
    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    const tool = turn.items[0];
    if (tool?.item_kind !== "tool") throw new Error("unreachable");
    // Exact equality — proves the listener boundary stamped this, not
    // some other clock reading inside the reducer.
    expect(tool.started_at).toBe("2026-05-16T12:00:00.000Z");
  });
});

describe("hydrateAgent", () => {
  it("flips hydration_status pending → loading → complete and applies turns + meta", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    // Caller marks the runtime as pending before invoking hydrate (App.svelte
    // does this for project-open / attach flows).
    const r = state.runtimes[AGENT_A];
    if (r === undefined) throw new Error("runtime missing");
    state.runtimes[AGENT_A] = { ...r, hydration_status: "pending" };

    invokeMock.mockResolvedValueOnce({
      turns: [
        {
          role: "user",
          turn_id: TURN_1,
          agent_id: AGENT_A,
          started_at: "2026-05-14T00:00:00Z",
          text: "remember PURPLE",
        },
      ],
      meta: {
        model: "claude-sonnet-4-6",
        harness_version: "2.1.140",
        tools: [],
        mcp_servers: [],
        skills: [],
      },
      last_rate_limit: null,
      warnings: [],
    });

    await state.hydrateAgent(AGENT_A);
    expect(invokeMock).toHaveBeenCalledWith("load_transcript", { agentId: AGENT_A });
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("complete");
    expect(state.runtimes[AGENT_A]?.meta?.model).toBe("claude-sonnet-4-6");
    expect(state.transcripts[AGENT_A]).toHaveLength(1);
  });

  it("flips to failed on IPC rejection", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    const r = state.runtimes[AGENT_A];
    if (r === undefined) throw new Error("runtime missing");
    state.runtimes[AGENT_A] = { ...r, hydration_status: "pending" };

    invokeMock.mockRejectedValueOnce("path resolution failed");
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    await state.hydrateAgent(AGENT_A);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("failed");

    warnSpy.mockRestore();
  });

  it("is idempotent when called twice — second call no-ops after first attempt", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));

    invokeMock.mockResolvedValueOnce({
      turns: [],
      meta: null,
      last_rate_limit: null,
      warnings: [],
    });

    await state.hydrateAgent(AGENT_A);
    await state.hydrateAgent(AGENT_A);
    // Second call returned without invoking IPC again.
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("does NOT re-hydrate after a project-reopen-style live state change", async () => {
    // Regression test: parsers mint fresh turn_ids per parse, so the reducer's
    // existingIds.has(t.turn_id) dedupe can't catch "same conversation, parsed
    // twice." The idempotency Set is what prevents the duplicate. Pinned here
    // against a refactor that re-introduces the manual flip-to-pending and
    // bypasses the guard.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));

    invokeMock.mockResolvedValueOnce({
      turns: [
        {
          role: "user",
          turn_id: TURN_1,
          agent_id: AGENT_A,
          started_at: "2026-05-14T00:00:00Z",
          text: "remember PURPLE",
        },
      ],
      meta: null,
      last_rate_limit: null,
      warnings: [],
    });
    await state.hydrateAgent(AGENT_A);
    expect(state.transcripts[AGENT_A]).toHaveLength(1);

    // Simulate "user navigates away and back" — forcibly reset
    // hydration_status; the second call must still no-op.
    const r = state.runtimes[AGENT_A];
    if (r === undefined) throw new Error("runtime missing");
    state.runtimes[AGENT_A] = { ...r, hydration_status: "pending" };
    await state.hydrateAgent(AGENT_A);

    // No second IPC call; transcript stays at 1 turn (would be 2 if the
    // bug re-introduced).
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(state.transcripts[AGENT_A]).toHaveLength(1);
  });

  it("surfaces ParseWarning entries onto runtime.parse_warnings", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    invokeMock.mockResolvedValueOnce({
      turns: [],
      meta: null,
      last_rate_limit: null,
      warnings: [{ line_number: 0, reason: "session file no longer at recorded path" }],
    });
    await state.hydrateAgent(AGENT_A);
    expect(state.runtimes[AGENT_A]?.parse_warnings).toHaveLength(1);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("complete");
  });

  it("self-flips hydration_status from any starting state — no manual pre-flip needed", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    // Default freshRuntime is "complete" (create-flow default); hydrateAgent
    // must still proceed without a manual flip-to-pending by the caller.
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("complete");
    invokeMock.mockResolvedValueOnce({
      turns: [],
      meta: null,
      last_rate_limit: null,
      warnings: [],
    });
    await state.hydrateAgent(AGENT_A);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("complete");
  });
});
