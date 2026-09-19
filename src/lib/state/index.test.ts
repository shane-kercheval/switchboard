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

/// The account-scoped usage store, asserted directly where a quota reading used
/// to be asserted on the agent runtime. Cleared by the state module's own
/// `_testing.reset()`, so no separate teardown here.
const usage = await import("./harnessUsage.svelte");

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
    session_locator: null,
    model: null,
    effort: null,
    model_choices: [],
    effort_choices: [],
    created_at: "2026-05-15T00:00:00Z",
  };
}

const AGENT_A = "00000000-0000-7000-8000-000000000aaa";
const AGENT_B = "00000000-0000-7000-8000-000000000bbb";
const TURN_1 = "00000000-0000-7000-8000-000000000001";
const TURN_2 = "00000000-0000-7000-8000-000000000002";
const MESSAGE_1 = "00000000-0000-7000-8000-0000000000f1";

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

describe("local send tracking", () => {
  it("preserves recipients from coalesced same-project sends", async () => {
    const state = await loadState();

    state.noteLocalSend("project-a", "send-a", [AGENT_A]);
    state.noteLocalSend("project-a", "send-b", [AGENT_B]);

    expect(state.getLocalSend("project-a")).toEqual({
      sendId: "send-b",
      seq: 2,
      recipientSeqs: {
        [AGENT_A]: 1,
        [AGENT_B]: 2,
      },
    });
  });
});

describe("event routing", () => {
  it("turn_start populates the agent's transcript with a streaming turn", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    expect(state.transcripts[AGENT_A]).toHaveLength(1);
    const turn = state.transcripts[AGENT_A]?.[0];
    expect(turn?.role).toBe("agent");
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("streaming");
  });

  it("stamps a turn's send_id from the turn_start event when the frontend didn't originate the send", async () => {
    // A workflow (or any backend-originated) fan-out dispatches without a local
    // `pending_sends` entry, so the live grouping must come from the event's
    // own `send_id` — otherwise the fan-out's turns render stacked, not side-by-side.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await state.registerAgent(agentRecord(AGENT_B));
    const SHARED = "11111111-1111-7111-8111-111111111111";
    for (const agent of [AGENT_A, AGENT_B]) {
      fireTo(`agent:${agent}`, {
        type: "turn_start",
        turn_id: crypto.randomUUID(),
        message_id: crypto.randomUUID(),
        send_id: SHARED,
        started_at: "2026-05-15T00:00:00Z",
      });
    }
    // Both turns carry the shared send_id (the grouping key), so the UI lays them
    // out as one fan-out row rather than two stacked sends.
    const a = state.transcripts[AGENT_A]?.[0];
    const b = state.transcripts[AGENT_B]?.[0];
    expect(a?.role === "agent" && a.send_id).toBe(SHARED);
    expect(b?.role === "agent" && b.send_id).toBe(SHARED);
  });

  it("AgentIdle event flips run_status to idle", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
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
      inventory: {
        tools: ["Bash"],
      },
      raw: {},
    });
    fireTo(`agent:${AGENT_A}`, {
      type: "rate_limit_event",
      agent_id: AGENT_A,
      info: { primary: { used_percent: 30 } },
    });
    expect(state.runtimes[AGENT_A]?.meta?.model).toBe("claude-sonnet-4-6");
    // The reading lands in the account-scoped store, not on the agent: the quota
    // it describes belongs to the harness account this agent happens to use.
    expect(usage.harnessUsage.claude_code?.payload).toEqual({ primary: { used_percent: 30 } });
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
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    fireTo(`agent:${AGENT_B}`, {
      type: "turn_start",
      turn_id: TURN_2,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
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
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    // Only A is processing — B stays idle.
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_B]?.run_status).toBe("idle");
  });
});

describe("heartbeat orchestration", () => {
  it("arms on turn_start, fires after HEARTBEAT_TIMEOUT_MS of silence, marks runtime quiet (not failed)", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    expect(state._testing.hasHeartbeat(AGENT_A)).toBe(true);

    // No activity — past the threshold, heartbeat fires.
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS + 100);

    // The turn is NOT failed — it's alive on the backend, just silent. The
    // runtime carries a transient `quiet` flag instead.
    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("streaming");
    expect(turn.error).toBeUndefined();
    expect(state.runtimes[AGENT_A]?.quiet_since).toBeDefined();
    // The watch is retained (entry kept, handle dropped) so the next activity
    // can re-arm and clear quiet.
    expect(state._testing.heartbeatTurnId(AGENT_A)).toBe(TURN_1);
  });

  it("re-arms on liveness (thinking keepalive) for the tracked turn", async () => {
    // A long redacted Claude thinking block (Opus 4.8) emits only `liveness`
    // (redacted thinking deltas); it must keep the turn alive the same way tool
    // events do.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    fireTo(`agent:${AGENT_A}`, { type: "liveness", turn_id: TURN_1 });
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    fireTo(`agent:${AGENT_A}`, { type: "liveness", turn_id: TURN_1 });
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    expect(state.runtimes[AGENT_A]?.quiet_since).toBeUndefined();
  });

  it("activity after quiet clears it and re-arms the timer", async () => {
    // Regression for the latch: once the timer has fired and set quiet, the
    // next activity must clear quiet AND re-arm, so a second silent stretch
    // re-triggers quiet (rather than the indicator sticking forever).
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS + 100);
    expect(state.runtimes[AGENT_A]?.quiet_since).toBeDefined();

    // Activity resumes — quiet clears, timer re-arms.
    fireTo(`agent:${AGENT_A}`, {
      type: "content_chunk",
      turn_id: TURN_1,
      kind: "text",
      text: "back",
    });
    expect(state.runtimes[AGENT_A]?.quiet_since).toBeUndefined();

    // A second silent stretch re-triggers quiet.
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS + 100);
    expect(state.runtimes[AGENT_A]?.quiet_since).toBeDefined();

    // The turn was never failed throughout.
    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("streaming");
  });

  it("re-arms on content_chunk for the tracked turn", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
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
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    });
    vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS - 100);
    fireTo(`agent:${AGENT_A}`, {
      type: "tool_started",
      facet: { facet_kind: "other" },
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
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
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
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
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
    // Past TURN_1's deadline — fires and marks quiet (the stale TURN_2 event
    // did not re-arm), but never fails the turn.
    vi.advanceTimersByTime(200);
    expect(state.runtimes[AGENT_A]?.quiet_since).toBeDefined();
    const turn = state.transcripts[AGENT_A]?.[0];
    if (turn?.role !== "agent") throw new Error("unreachable");
    expect(turn.status).toBe("streaming");
  });
});

describe("dispatchUserTurn", () => {
  it("synchronously appends a user-role turn before any event arrives", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "s1", "2026-05-16T00:00:00Z");
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
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "s1", "2026-05-16T00:00:00Z");
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
    state.dispatchUserTurn(AGENT_A, "user-1", "retry", [], "s1", "2026-05-16T00:00:00Z");
    expect(state.runtimes[AGENT_A]?.last_error).toBeUndefined();
  });

  it("rejects calls for unregistered agents (fail-loud)", async () => {
    const state = await loadState();
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    // No registerAgent call — runtime doesn't exist.
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "s1", "2026-05-16T00:00:00Z");
    expect(errSpy).toHaveBeenCalledWith(
      "[switchboard] dispatchUserTurn called for unregistered agent",
      expect.objectContaining({ agent_id: AGENT_A }),
    );
    expect(state.transcripts[AGENT_A]).toBeUndefined();
    errSpy.mockRestore();
  });

  it("queues a second send while the agent is busy (send-while-busy un-gated)", async () => {
    // Send-while-busy is no longer rejected: the backend queues, so a second
    // dispatch appends its optimistic user turn and lines its send up behind
    // the running one (the FIFO that stamps each response's send_id). The
    // first send's run_status ("starting") is left alone.
    const state = await loadState();
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "first", [], "send-1", "2026-05-16T00:00:00Z");
    state.dispatchUserTurn(AGENT_A, "user-2", "second", [], "send-2", "2026-05-16T00:00:01Z");

    // No "not idle" rejection — both sends are accepted.
    expect(errSpy).not.toHaveBeenCalled();
    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns.map((t) => (t.role === "user" ? t.text : "?"))).toEqual(["first", "second"]);
    // Both sends line up in dispatch order; the running turn stays "starting".
    expect(state.runtimes[AGENT_A]?.pending_sends?.map((p) => p.send_id)).toEqual([
      "send-1",
      "send-2",
    ]);
    expect(state.runtimes[AGENT_A]?.run_status).toBe("starting");
    errSpy.mockRestore();
  });
});

describe("failSendStart", () => {
  it("flips starting → idle and records the error", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "s1", "2026-05-16T00:00:00Z");
    expect(state.runtimes[AGENT_A]?.run_status).toBe("starting");

    state.failSendStart(AGENT_A, "user-1", {
      message: "Tauri IPC failed",
      kind: "adapter_failure",
    });

    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A]?.last_error).toEqual({
      message: "Tauri IPC failed",
      kind: "adapter_failure",
    });
  });

  it("keeps the optimistic user turn and appends a failed agent turn beneath it", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "s1", "2026-05-16T00:00:00Z");
    state.failSendStart(AGENT_A, "user-1", { message: "boom", kind: "adapter_failure" });
    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns).toHaveLength(2);
    expect(turns[0]?.role).toBe("user");
    const failed = turns[1];
    if (failed?.role !== "agent") throw new Error("expected a failed agent turn");
    expect(failed.status).toBe("failed");
    expect(failed.error).toBe("boom");
    expect(failed.send_id).toBe("s1");
  });

  it("does not append a failed agent turn when it no-ops (TurnStart raced ahead)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "s1", "2026-05-16T00:00:00Z");
    state.recordSendAccepted(AGENT_A, "user-1", MESSAGE_1);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:00Z",
    });

    state.failSendStart(AGENT_A, "user-1", { message: "ignored", kind: "adapter_failure" });

    // Entry already consumed by turn_start → no synthetic failed turn; only the
    // user turn and the live (streaming) agent turn exist.
    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns.filter((t) => t.role === "agent" && t.status === "failed")).toHaveLength(0);
  });

  it("is a no-op while processing (TurnStart raced ahead)", async () => {
    // The race: dispatchUserTurn → starting; await api.sendMessage()
    // resolves; meanwhile TurnStart arrives on the channel → processing.
    // Then the IPC reply also resolves successfully — there's no error.
    // But a confused caller could call failSendStart anyway; the guard
    // must not stomp the genuine "processing" state.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "s1", "2026-05-16T00:00:00Z");
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      // The backend echoes the send id the frontend minted, so this always
      // matches the pending entry — correlation is by identity, not position.
      send_id: "s1",
      started_at: "2026-05-16T00:00:00Z",
    });
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");

    state.failSendStart(AGENT_A, "user-1", { message: "ignored", kind: "adapter_failure" });

    // No-op: still processing, no last_error.
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_A]?.last_error).toBeUndefined();
  });

  it("is a no-op while idle (idempotent)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.failSendStart(AGENT_A, "user-1", { message: "ignored", kind: "adapter_failure" });
    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A]?.last_error).toBeUndefined();
  });

  it("logs to console.error for unregistered agents", async () => {
    const state = await loadState();
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    state.failSendStart(AGENT_A, "user-1");
    expect(errSpy).toHaveBeenCalledWith(
      "[switchboard] failSendStart called for unregistered agent",
      expect.objectContaining({ agent_id: AGENT_A }),
    );
    errSpy.mockRestore();
  });
});

describe("message_failed event → transcript", () => {
  it("renders a failed agent turn for a pre-start failure (entry still pending)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "send-1", "2026-05-16T00:00:00Z");
    state.recordSendAccepted(AGENT_A, "user-1", MESSAGE_1);

    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      agent_id: AGENT_A,
      error: "adapter failed to launch",
      at: "2026-05-16T00:00:01Z",
    });

    const turns = state.transcripts[AGENT_A] ?? [];
    const failed = turns.find((t) => t.role === "agent" && t.status === "failed");
    if (failed?.role !== "agent") throw new Error("expected a failed agent turn");
    expect(failed.error).toBe("adapter failed to launch");
    expect(failed.send_id).toBe("send-1");
  });

  it("renders the row in the pre-receipt race (message_failed beats recordSendAccepted)", async () => {
    // The send is dispatched but its `send_message` IPC receipt hasn't landed,
    // so the pending entry has no message_id yet. A backend message_failed must
    // still surface — `pendingEntryFor` resolves it by `send_id` (mirroring the
    // runtime reducer's `pickPendingIndex`), so the transcript and runtime stay
    // on the same entry.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "send-1", "2026-05-16T00:00:00Z");

    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE_1,
      send_id: "send-1",
      agent_id: AGENT_A,
      error: "adapter failed to launch",
      at: "2026-05-16T00:00:01Z",
    });

    const failed = (state.transcripts[AGENT_A] ?? []).find(
      (t) => t.role === "agent" && t.status === "failed",
    );
    if (failed?.role !== "agent") throw new Error("expected a failed agent turn");
    expect(failed.error).toBe("adapter failed to launch");
    expect(failed.send_id).toBe("send-1");
    // Runtime pruned the same entry.
    expect(state.runtimes[AGENT_A]?.pending_sends).toBeUndefined();
  });

  it("does not double-render when the failure is post-start (turn already streaming)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "send-1", "2026-05-16T00:00:00Z");
    state.recordSendAccepted(AGENT_A, "user-1", MESSAGE_1);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:01Z",
    });

    // Out-of-protocol message_failed after the turn started: the entry is gone,
    // so it resolves no send_id and appends nothing — the live turn owns the
    // outcome (a failed turn_end would update it in place).
    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE_1,
      // Matches the send's id (the backend stamps `item.send_id`); the post-start
      // guard finds the already-streaming turn for this send and skips.
      send_id: "send-1",
      agent_id: AGENT_A,
      error: "boom",
      at: "2026-05-16T00:00:02Z",
    });

    const agentTurns = (state.transcripts[AGENT_A] ?? []).filter((t) => t.role === "agent");
    expect(agentTurns).toHaveLength(1);
  });

  it("renders a failed marker for a backend-originated pre-start failure via the event's send_id", async () => {
    // A workflow step's send fails before `turn_start` (e.g. the agent's harness
    // fails to launch). There's no `pending_sends` entry, so the failed marker is
    // attributed from the event's own `send_id` — and it carries that `send_id`,
    // so it renders under the workflow's live user row instead of orphaning.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE_1,
      send_id: "wf-send-1",
      agent_id: AGENT_A,
      error: "agent failed to launch",
      at: "2026-05-16T00:00:00Z",
    });
    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns).toHaveLength(1);
    const t = turns[0];
    expect(t?.role).toBe("agent");
    if (t?.role !== "agent") throw new Error("unreachable");
    expect(t.status).toBe("failed");
    expect(t.send_id).toBe("wf-send-1");

    // Re-delivery of the same failure is a no-op (the failed turn already carries
    // this send_id, so the post-start guard skips it) — no double-render.
    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE_1,
      send_id: "wf-send-1",
      agent_id: AGENT_A,
      error: "agent failed to launch",
      at: "2026-05-16T00:00:01Z",
    });
    expect((state.transcripts[AGENT_A] ?? []).filter((x) => x.role === "agent")).toHaveLength(1);
  });

  it("renders no row for a pre-durable backend failure (send_id null) — matches the empty reload", async () => {
    // A workflow send whose journal write fails: not durably recorded, so the
    // event carries no send_id and there's no pending entry. The live transcript
    // must add nothing (reload reconstructs nothing); the run indicator owns the
    // failure.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE_1,
      send_id: null,
      agent_id: AGENT_A,
      error: "journal write failed",
      at: "2026-05-16T00:00:00Z",
    });
    expect(state.transcripts[AGENT_A] ?? []).toHaveLength(0);
  });
});

const cancelledSendIds = (
  state: Awaited<ReturnType<typeof loadState>>,
  agentId: string,
): string[] =>
  (state.transcripts[agentId] ?? [])
    .filter((t) => t.role === "agent" && t.status === "cancelled")
    .map((t) => (t.role === "agent" ? (t.send_id ?? "?") : "?"));

describe("stopAgent", () => {
  it("fires cancel_agent; backend message_cancelled events prune queued + render cancelled", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    // Two queued sends, both backend-accepted (message_id recorded).
    state.dispatchUserTurn(AGENT_A, "user-1", "first", [], "send-1", "2026-05-16T00:00:00Z");
    state.dispatchUserTurn(AGENT_A, "user-2", "second", [], "send-2", "2026-05-16T00:00:01Z");
    state.recordSendAccepted(AGENT_A, "user-1", "msg-1");
    state.recordSendAccepted(AGENT_A, "user-2", "msg-2");
    invokeMock.mockClear();

    state.stopAgent(AGENT_A);
    expect(invokeMock).toHaveBeenCalledWith("cancel_agent", { agentId: AGENT_A });

    // The backend drops both queued sends and emits a message_cancelled per send;
    // those events (not optimistic synthesis) prune pending + render cancelled.
    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: "msg-1",
      send_id: "send-1",
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:02Z",
    });
    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: "msg-2",
      send_id: "send-2",
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:02Z",
    });

    expect(state.runtimes[AGENT_A]?.pending_sends).toBeUndefined();
    expect(cancelledSendIds(state, AGENT_A)).toEqual(["send-1", "send-2"]);
  });

  it("queued send cancelled via event; running turn via its own terminal (no duplicate)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    // send-1 running (turn_start popped its pending entry).
    state.dispatchUserTurn(AGENT_A, "user-1", "running", [], "send-1", "2026-05-16T00:00:00Z");
    state.recordSendAccepted(AGENT_A, "user-1", MESSAGE_1);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:00Z",
    });
    // send-2 queued behind it, accepted.
    state.dispatchUserTurn(AGENT_A, "user-2", "queued", [], "send-2", "2026-05-16T00:00:01Z");
    state.recordSendAccepted(AGENT_A, "user-2", "msg-2");
    invokeMock.mockClear();

    state.stopAgent(AGENT_A);
    expect(invokeMock).toHaveBeenCalledWith("cancel_agent", { agentId: AGENT_A });

    // Queued send-2: dropped → message_cancelled. Running send-1: Cancelled terminal.
    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: "msg-2",
      send_id: "send-2",
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:02Z",
    });
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_1,
      outcome: { status: "cancelled", source: "user" },
      ended_at: "2026-05-16T00:00:02Z",
    });

    // Exactly one cancelled row per send — no duplicate/detached row.
    expect(cancelledSendIds(state, AGENT_A).sort()).toEqual(["send-1", "send-2"]);
  });

  it("defers the cancel for a send not yet backend-accepted, firing it on accept", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    // Dispatched but no message_id yet (send_message IPC still in flight).
    state.dispatchUserTurn(AGENT_A, "user-1", "racing", [], "send-1", "2026-05-16T00:00:00Z");
    invokeMock.mockClear();

    state.stopAgent(AGENT_A);

    // Deferred: entry flagged, no send-scoped cancel yet (firing now would race
    // the send IPC), and nothing rendered cancelled.
    expect(state.runtimes[AGENT_A]?.pending_sends?.[0]?.cancel_requested).toBe(true);
    expect(invokeMock.mock.calls.some(([c]) => c === "cancel_send")).toBe(false);
    expect(cancelledSendIds(state, AGENT_A)).toEqual([]);

    // Once accepted, the deferred cancel fires; the backend's message_cancelled
    // then prunes + renders.
    state.recordSendAccepted(AGENT_A, "user-1", MESSAGE_1);
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_send",
      expect.objectContaining({ sendId: "send-1", recipients: [AGENT_A] }),
    );
    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: MESSAGE_1,
      send_id: "send-1",
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:02Z",
    });
    expect(state.runtimes[AGENT_A]?.pending_sends).toBeUndefined();
    expect(cancelledSendIds(state, AGENT_A)).toEqual(["send-1"]);
  });

  it("correlates queued cancellations that arrive before accepted receipts", async () => {
    const state = await loadState();
    const tracker = await import("./sendCompletion");
    tracker._testing.reset();
    await state.registerAgent(agentRecord(AGENT_A));
    tracker.registerSend("send-1", "p-1", "switchboard", [{ id: AGENT_A, name: "claude" }]);
    tracker.registerSend("send-2", "p-1", "switchboard", [{ id: AGENT_A, name: "claude" }]);
    state.dispatchUserTurn(AGENT_A, "user-1", "first", [], "send-1");
    state.dispatchUserTurn(AGENT_A, "user-2", "second", [], "send-2");

    state.stopAgent(AGENT_A);
    expect(state.runtimes[AGENT_A]?.pending_sends?.every((p) => p.cancel_requested)).toBe(true);

    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: "msg-2",
      send_id: "send-2",
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:02Z",
    });
    expect(state.runtimes[AGENT_A]?.pending_sends?.map((p) => p.send_id)).toEqual(["send-1"]);
    expect(cancelledSendIds(state, AGENT_A)).toEqual(["send-2"]);

    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: "msg-1",
      send_id: "send-1",
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:03Z",
    });
    expect(state.runtimes[AGENT_A]?.pending_sends).toBeUndefined();
    expect(cancelledSendIds(state, AGENT_A)).toEqual(["send-2", "send-1"]);
    expect(tracker._testing.size()).toBe(0);
    expect(tracker._testing.projectCount()).toBe(0);
    expect(tracker._testing.startedTurnCount()).toBe(0);

    invokeMock.mockClear();
    state.recordSendAccepted(AGENT_A, "user-1", "msg-1");
    state.recordSendAccepted(AGENT_A, "user-2", "msg-2");
    expect(invokeMock.mock.calls.some(([command]) => command === "cancel_send")).toBe(false);
    expect(invokeMock.mock.calls.some(([command]) => command === "notify")).toBe(false);
  });
});

describe("cancelSend pre-accept race", () => {
  it("defers the backend cancel until the send is accepted", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "racing", [], "send-1", "2026-05-16T00:00:00Z");
    invokeMock.mockClear();

    state.cancelSend("send-1", [AGENT_A]);
    // No backend cancel yet (would race the send IPC); entry flagged instead.
    expect(invokeMock.mock.calls.some(([c]) => c === "cancel_send")).toBe(false);
    expect(state.runtimes[AGENT_A]?.pending_sends?.[0]?.cancel_requested).toBe(true);

    state.recordSendAccepted(AGENT_A, "user-1", MESSAGE_1);
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_send",
      expect.objectContaining({ sendId: "send-1", recipients: [AGENT_A] }),
    );
  });

  it("fires the deferred cancel if the turn starts before acceptance is recorded", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "racing", [], "send-1", "2026-05-16T00:00:00Z");
    state.cancelSend("send-1", [AGENT_A]);
    invokeMock.mockClear();

    // The turn started anyway (backend accepted + dispatched before the cancel
    // landed). turn_start consumes the flagged entry → fire the cancel now.
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: "send-1",
      started_at: "2026-05-16T00:00:00Z",
    });
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_send",
      expect.objectContaining({ sendId: "send-1", recipients: [AGENT_A] }),
    );
  });

  it("fires cancel immediately once accepted; the message_cancelled event renders it", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "queued", [], "send-1", "2026-05-16T00:00:00Z");
    state.recordSendAccepted(AGENT_A, "user-1", MESSAGE_1);
    invokeMock.mockClear();

    state.cancelSend("send-1", [AGENT_A]);
    // Accepted → backend has it → cancel fires now (no optimistic synthesis).
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_send",
      expect.objectContaining({ sendId: "send-1", recipients: [AGENT_A] }),
    );
    expect(cancelledSendIds(state, AGENT_A)).toEqual([]); // nothing rendered until the event

    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: MESSAGE_1,
      send_id: "send-1",
      agent_id: AGENT_A,
      at: "2026-05-16T00:00:02Z",
    });
    expect(state.runtimes[AGENT_A]?.pending_sends).toBeUndefined();
    expect(cancelledSendIds(state, AGENT_A)).toEqual(["send-1"]);
  });
});

describe("pending-send pruning (fan-out / queue correctness)", () => {
  it("prunes a failed send's entry so the next send's response isn't mis-stamped", async () => {
    // Regression: a send that fails before turn_start must not leave a stale
    // pending entry that the *next* send's turn_start would consume — which
    // would stamp the retry's response with the failed send's send_id.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "u1", "first", [], "send-A", "2026-05-16T00:00:00Z");
    state.failSendStart(AGENT_A, "u1", { message: "ipc down", kind: "adapter_failure" });
    expect(state.runtimes[AGENT_A]?.pending_sends).toBeUndefined();

    // Retry succeeds; its turn_start must stamp the retry's send_id.
    state.dispatchUserTurn(AGENT_A, "u2", "retry", [], "send-B", "2026-05-16T00:00:01Z");
    state.recordSendAccepted(AGENT_A, "u2", MESSAGE_1);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:02Z",
    });
    // The live (streaming) turn is the retry; a failed turn for send-A also sits
    // in the transcript now, so target the streaming one explicitly.
    const agentTurn = (state.transcripts[AGENT_A] ?? []).find(
      (t) => t.role === "agent" && t.status === "streaming",
    );
    expect(agentTurn?.role === "agent" ? agentTurn.send_id : null).toBe("send-B");
  });

  it("prunes a queued send's IPC failure without stomping the running turn", async () => {
    // Send-while-busy: a queued send's pending entry can fail (IPC) while a
    // different turn is processing. The failure must prune that entry and
    // surface the error, but leave run_status === "processing" untouched.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "u1", "running", [], "send-A", "2026-05-16T00:00:00Z");
    state.recordSendAccepted(AGENT_A, "u1", MESSAGE_1);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:01Z",
    });
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");

    // Queue a second send while busy, then its IPC fails.
    state.dispatchUserTurn(AGENT_A, "u2", "queued", [], "send-B", "2026-05-16T00:00:02Z");
    expect(state.runtimes[AGENT_A]?.pending_sends?.map((p) => p.send_id)).toEqual(["send-B"]);
    state.failSendStart(AGENT_A, "u2", { message: "queue ipc down", kind: "adapter_failure" });

    expect(state.runtimes[AGENT_A]?.pending_sends).toBeUndefined();
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_A]?.last_error?.message).toBe("queue ipc down");
  });
});

describe("state machine — starting → processing transition", () => {
  it("turn_start during 'starting' transitions to 'processing'", async () => {
    // The legitimate happy path: user clicks Send → starting; backend
    // accepts and emits TurnStart → processing.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, "user-1", "hello", [], "s1", "2026-05-16T00:00:00Z");
    expect(state.runtimes[AGENT_A]?.run_status).toBe("starting");

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
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
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:00Z",
    });

    state._testing.reset();

    expect(state.transcripts[AGENT_A]).toBeUndefined();
    expect(state.transcripts[AGENT_B]).toBeUndefined();
    expect(state.runtimes[AGENT_A]).toBeUndefined();
    expect(state.runtimes[AGENT_B]).toBeUndefined();
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
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
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
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:00Z",
    });
    fireTo(`agent:${AGENT_A}`, {
      type: "tool_started",
      facet: { facet_kind: "other" },
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
        inventory: {},
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

  it("carries the inventory's capture time from the IPC reply to the runtime", async () => {
    // The seam the reducer tests cannot see: the backend stamps `meta_as_of`
    // when the inventory came from the metadata sidecar, and the card renders
    // it as "as of <time>". A hydrate that rebuilt its event from a fixed
    // list of fields once dropped it here — every layer tested green while
    // the card presented a days-old snapshot as live.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));

    invokeMock.mockResolvedValueOnce({
      turns: [],
      meta: {
        model: "claude-fable-5-1",
        harness_version: "",
        inventory: { mcp_servers: [{ name: "gmail", status: "needs-auth" }] },
      },
      last_rate_limit: null,
      meta_as_of: "2026-09-17T12:00:00Z",
      warnings: [],
    });

    await state.hydrateAgent(AGENT_A);
    expect(state.runtimes[AGENT_A]?.meta?.inventory.mcp_servers).toEqual([
      { name: "gmail", status: "needs-auth" },
    ]);
    expect(state.runtimes[AGENT_A]?.meta_as_of).toBe("2026-09-17T12:00:00Z");
  });

  it("carries the rate-limit model from the IPC reply to the usage store", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));

    invokeMock.mockResolvedValueOnce({
      turns: [],
      meta: null,
      last_rate_limit: {
        unifiedWindows: {
          seven_day_overage_included: { utilization: 0.79, resetsAt: 1_800_000_000 },
        },
      },
      last_rate_limit_model: "claude-fable-5-1",
      last_rate_limit_as_of: "2026-09-17T12:00:00Z",
      warnings: [],
    });

    await state.hydrateAgent(AGENT_A);
    expect(usage.harnessUsage.claude_code?.model).toBe("claude-fable-5-1");
  });

  it("a live inventory that lands before hydration resolves never inherits the snapshot's age", async () => {
    // Ordering race: the reducer fills meta only where absent, so a live
    // `session_meta` that arrives first must win and the disk snapshot's
    // capture time must not be stamped onto it — an "as of" on a live
    // inventory would age it past the staleness threshold.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));

    let resolveLoad: (v: unknown) => void = () => {};
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolveLoad = r;
        }),
    );
    const hydrating = state.hydrateAgent(AGENT_A);
    await vi.waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("load_transcript", { agentId: AGENT_A }),
    );

    fireTo(`agent:${AGENT_A}`, {
      type: "session_meta",
      agent_id: AGENT_A,
      model: "claude-fable-5-1",
      harness_version: "2.1.274",
      inventory: { mcp_servers: [{ name: "gmail", status: "connected" }] },
      raw: {},
    });

    resolveLoad({
      turns: [],
      meta: {
        model: "claude-fable-5-1",
        harness_version: "",
        inventory: { mcp_servers: [{ name: "gmail", status: "needs-auth" }] },
      },
      last_rate_limit: null,
      meta_as_of: "2026-09-17T12:00:00Z",
      warnings: [],
    });
    await hydrating;

    expect(state.runtimes[AGENT_A]?.meta?.inventory.mcp_servers).toEqual([
      { name: "gmail", status: "connected" },
    ]);
    expect(state.runtimes[AGENT_A]?.meta_as_of).toBeNull();
  });

  it("flips to failed and retains the error text on IPC rejection", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    const r = state.runtimes[AGENT_A];
    if (r === undefined) throw new Error("runtime missing");
    state.runtimes[AGENT_A] = { ...r, hydration_status: "pending" };

    invokeMock.mockRejectedValueOnce(new Error("I/O error reading session file /x.jsonl"));
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    await state.hydrateAgent(AGENT_A);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("failed");
    // The error is retained on the runtime (not just console.warn'd) so the
    // transcript-region banner and the sidebar line can surface it verbatim.
    expect(state.runtimes[AGENT_A]?.hydration_error).toBe(
      "I/O error reading session file /x.jsonl",
    );

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

  it("does not promote aggregate parse warnings onto agent runtime", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    invokeMock.mockResolvedValueOnce({
      turns: [],
      meta: null,
      last_rate_limit: null,
      warnings: Array.from({ length: 500 }, (_, index) => ({
        line_number: index + 1,
        reason: `aggregate warning ${index + 1}`,
      })),
    });
    await state.hydrateAgent(AGENT_A);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("complete");
    expect(state.runtimes[AGENT_A]).not.toHaveProperty("parse_warnings");
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

describe("retryAgentHydration", () => {
  it("clears the sticky guard and re-runs, rendering turns and clearing the failure on success", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));

    // First attempt fails.
    invokeMock.mockRejectedValueOnce(new Error("permission denied"));
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await state.hydrateAgent(AGENT_A);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("failed");
    expect(state.runtimes[AGENT_A]?.hydration_error).toBe("permission denied");

    // Retry succeeds: clears the guard, re-invokes, renders turns, clears error.
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
    await state.retryAgentHydration(AGENT_A);

    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("complete");
    expect(state.runtimes[AGENT_A]?.hydration_error).toBeUndefined();
    expect(state.transcripts[AGENT_A]).toHaveLength(1);
    warnSpy.mockRestore();
  });

  it("ignores a concurrent retry while one is already in flight (no second load, no dup)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));

    // First attempt fails.
    invokeMock.mockRejectedValueOnce(new Error("boom"));
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await state.hydrateAgent(AGENT_A);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("failed");

    // Stage a slow success and fire two retries before it resolves: the second
    // must observe the in-flight "loading" status and no-op, so only one
    // `load_transcript` runs and the turn is not applied twice.
    let resolveLoad: (v: unknown) => void = () => {};
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolveLoad = r;
        }),
    );
    const p1 = state.retryAgentHydration(AGENT_A);
    const p2 = state.retryAgentHydration(AGENT_A);
    resolveLoad({
      turns: [
        {
          role: "user",
          turn_id: TURN_1,
          agent_id: AGENT_A,
          started_at: "2026-05-14T00:00:00Z",
          text: "hi",
        },
      ],
      meta: null,
      last_rate_limit: null,
      warnings: [],
    });
    await Promise.all([p1, p2]);

    // Initial failed load + exactly one retry load = 2 invokes (not 3).
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(state.transcripts[AGENT_A]).toHaveLength(1);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("complete");
    warnSpy.mockRestore();
  });

  it("keeps the failed state when the retry also fails", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));

    invokeMock.mockRejectedValueOnce(new Error("first error"));
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await state.hydrateAgent(AGENT_A);

    invokeMock.mockRejectedValueOnce(new Error("still broken"));
    await state.retryAgentHydration(AGENT_A);

    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(state.runtimes[AGENT_A]?.hydration_status).toBe("failed");
    expect(state.runtimes[AGENT_A]?.hydration_error).toBe("still broken");
    warnSpy.mockRestore();
  });
});

describe("send-completion notifications", () => {
  /// Drive the real listener boundary end to end: register the send the way
  /// `ComposeBar` does, dispatch through the state module, then fire events.
  /// Exercises the wiring, not the tracker's rules (those are unit-tested).
  async function setup() {
    const state = await loadState();
    const tracker = await import("./sendCompletion");
    tracker._testing.reset();
    await state.registerAgent(agentRecord(AGENT_A));
    await state.registerAgent(agentRecord(AGENT_B));
    return { state, tracker };
  }

  const SEND = "00000000-0000-7000-8000-0000000000e1";
  const SECOND_SEND = "00000000-0000-7000-8000-0000000000e2";
  const MESSAGE_2 = "00000000-0000-7000-8000-0000000000f2";
  /// The `notify` command's argument objects, in call order.
  const notified = (): Record<string, unknown>[] =>
    invokeMock.mock.calls
      .filter((c) => c[0] === "notify")
      .map((c) => c[1] as Record<string, unknown>);

  it("notifies once when a fan-out's last recipient queue drains", async () => {
    const { state, tracker } = await setup();
    tracker.registerSend(SEND, "p-1", "switchboard", [
      { id: AGENT_A, name: "claude" },
      { id: AGENT_B, name: "codex" },
    ]);
    for (const [agent, turn] of [
      [AGENT_A, TURN_1],
      [AGENT_B, TURN_2],
    ] as const) {
      state.dispatchUserTurn(agent, `user-${agent}`, "hi", [], SEND);
      fireTo(`agent:${agent}`, {
        type: "turn_start",
        turn_id: turn,
        message_id: MESSAGE_1,
        started_at: "2026-05-15T00:00:00Z",
      } as NormalizedEvent);
    }

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_1,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:01Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, { type: "agent_idle", agent_id: AGENT_A });
    expect(notified()).toHaveLength(0);

    fireTo(`agent:${AGENT_B}`, {
      type: "turn_end",
      turn_id: TURN_2,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:02Z",
    } as NormalizedEvent);
    expect(notified()).toHaveLength(0);
    fireTo(`agent:${AGENT_B}`, { type: "agent_idle", agent_id: AGENT_B });
    expect(notified()).toHaveLength(1);
  });

  it("does not notify between queued turns for the same agent", async () => {
    const { state, tracker } = await setup();
    tracker.registerSend(SEND, "p-1", "switchboard", [{ id: AGENT_A, name: "claude" }]);
    state.dispatchUserTurn(AGENT_A, "user-1", "first", [], SEND);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    } as NormalizedEvent);

    tracker.registerSend(SECOND_SEND, "p-1", "switchboard", [{ id: AGENT_A, name: "claude" }]);
    state.dispatchUserTurn(AGENT_A, "user-2", "second", [], SECOND_SEND);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_1,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:01Z",
    } as NormalizedEvent);
    expect(notified()).toHaveLength(0);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_2,
      message_id: MESSAGE_2,
      started_at: "2026-05-15T00:00:02Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_2,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:03Z",
    } as NormalizedEvent);
    expect(notified()).toHaveLength(0);

    fireTo(`agent:${AGENT_A}`, { type: "agent_idle", agent_id: AGENT_A });
    expect(notified()).toHaveLength(1);
  });

  it("notifies a send whose IPC was rejected, with no agent event at all", async () => {
    // `failSendStart` is a direct call, not an event — the reason the tracker
    // cannot be driven from the event stream alone.
    const { state, tracker } = await setup();
    tracker.registerSend(SEND, "p-1", "switchboard", [{ id: AGENT_A, name: "claude" }]);
    state.dispatchUserTurn(AGENT_A, "user-1", "hi", [], SEND);

    state.failSendStart(AGENT_A, "user-1", { message: "boom", kind: "adapter_failure" });

    const calls = notified();
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ title: "Agent failed" });
  });

  it("removing one recipient still lets the survivor's completion notify", async () => {
    // Driven through `unregisterAgents` rather than the tracker directly: the
    // settlement logic was already right, the missing lifecycle wiring was the
    // defect. Without it the removed agent's slot never fills, so the survivor's
    // completion is silently swallowed.
    const { state, tracker } = await setup();
    tracker.registerSend(SEND, "p-1", "switchboard", [
      { id: AGENT_A, name: "claude" },
      { id: AGENT_B, name: "codex" },
    ]);
    state.dispatchUserTurn(AGENT_B, "user-b", "hi", [], SEND);

    state.unregisterAgents([AGENT_A]);
    expect(notified()).toHaveLength(0);

    fireTo(`agent:${AGENT_B}`, {
      type: "turn_start",
      turn_id: TURN_2,
      message_id: MESSAGE_1,
      started_at: "2026-05-15T00:00:00Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_B}`, {
      type: "turn_end",
      turn_id: TURN_2,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:01Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_B}`, { type: "agent_idle", agent_id: AGENT_B });

    const calls = notified();
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ body: "switchboard: codex" });
  });

  it("stays silent for a workflow's send, which it never registered", async () => {
    // Faithful to the backend-originated path: no `dispatchUserTurn`, so no
    // pending entry — the turn carries its own `send_id` on the event. Workflow
    // steps are excluded structurally, by never being registered, so nothing here
    // has to recognize them as workflow sends.
    const { state } = await setup();
    fireTo(`agent:${AGENT_A}`, {
      type: "user_message",
      send_id: SEND,
      text: "step 1",
      at: "2026-05-15T00:00:00Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: SEND,
      started_at: "2026-05-15T00:00:00Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_1,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:01Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, { type: "agent_idle", agent_id: AGENT_A });

    expect(notified()).toHaveLength(0);
    void state;
  });
});

describe("manual context compaction", () => {
  const COMPACT_SEND = "00000000-0000-7000-8000-00000000c001";
  const COMPACT_PENDING = "00000000-0000-7000-8000-00000000c002";
  const COMPACT_MESSAGE = "00000000-0000-7000-8000-00000000c003";
  const COMPACT_TURN = "00000000-0000-7000-8000-00000000c004";

  it("registers its pending entry before the IPC, and only an idle agent starts", async () => {
    // Registering *before* the call is what makes the pre-receipt race safe:
    // `turn_start` can arrive before `compact_agent` resolves, and it must find
    // an entry to consume. The assertion is on the state observed while the IPC
    // is still unresolved.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    let resolveIpc: (id: string) => void = () => {};
    invokeMock.mockImplementation(
      async () => await new Promise<string>((res) => (resolveIpc = res)),
    );

    const inFlight = state.dispatchCompaction(
      AGENT_A,
      COMPACT_SEND,
      COMPACT_PENDING,
      "2026-05-15T00:00:05Z",
    );

    expect(state.runtimes[AGENT_A]?.pending_sends).toEqual([
      {
        send_id: COMPACT_SEND,
        user_turn_id: COMPACT_PENDING,
        kind: "compaction",
        queued_at: "2026-05-15T00:00:05Z",
      },
    ]);
    expect(state.runtimes[AGENT_A]?.run_status).toBe("starting");
    // No user turn — a compaction is not something the user said.
    expect(state.transcripts[AGENT_A]).toEqual([]);

    resolveIpc(COMPACT_MESSAGE);
    await inFlight;
    expect(state.runtimes[AGENT_A]?.pending_sends?.[0]?.message_id).toBe(COMPACT_MESSAGE);
  });

  it("queues behind a running turn without touching run_status", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    state.dispatchUserTurn(AGENT_A, TURN_1, "go", [], "send-1", "2026-05-15T00:00:00Z");
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_2,
      message_id: MESSAGE_1,
      send_id: "send-1",
      started_at: "2026-05-15T00:00:01Z",
    } as NormalizedEvent);
    invokeMock.mockResolvedValue(COMPACT_MESSAGE);

    await state.dispatchCompaction(AGENT_A, COMPACT_SEND, COMPACT_PENDING, "2026-05-15T00:00:05Z");

    // The live turn keeps the runtime; the compaction just lines up behind it.
    expect(state.runtimes[AGENT_A]?.run_status).toBe("processing");
    expect(state.runtimes[AGENT_A]?.pending_sends).toHaveLength(1);
  });

  it("does not let an unrelated turn consume its pending slot", async () => {
    // The compaction's receipt has not arrived, so its entry is at the front
    // with no `message_id`. A workflow turn starting in that window names its
    // own send, and must take nothing: consuming the compaction's entry would
    // stamp `kind: "compaction"` onto an ordinary answer, hiding its content
    // behind the compaction row, and leave the real compaction unclassified.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    let resolveIpc: (id: string) => void = () => {};
    invokeMock.mockImplementation(
      async () => await new Promise<string>((res) => (resolveIpc = res)),
    );
    const inFlight = state.dispatchCompaction(
      AGENT_A,
      COMPACT_SEND,
      COMPACT_PENDING,
      "2026-05-15T00:00:05Z",
    );

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_2,
      message_id: MESSAGE_1,
      send_id: "send-from-a-workflow",
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);

    const workflowTurn = state.transcripts[AGENT_A]?.find((t) => t.turn_id === TURN_2);
    expect(workflowTurn?.role === "agent" && workflowTurn.kind).toBeUndefined();
    expect(workflowTurn?.role === "agent" && workflowTurn.send_id).toBe("send-from-a-workflow");
    // The compaction's entry is untouched, so its own turn_start still finds it.
    expect(state.runtimes[AGENT_A]?.pending_sends).toHaveLength(1);
    expect(state.runtimes[AGENT_A]?.pending_sends?.[0]?.kind).toBe("compaction");

    resolveIpc(COMPACT_MESSAGE);
    await inFlight;
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: COMPACT_TURN,
      message_id: COMPACT_MESSAGE,
      send_id: COMPACT_SEND,
      started_at: "2026-05-15T00:00:07Z",
    } as NormalizedEvent);
    const compactionTurn = state.transcripts[AGENT_A]?.find((t) => t.turn_id === COMPACT_TURN);
    expect(compactionTurn?.role === "agent" && compactionTurn.kind).toBe("compaction");
  });

  it("consumes its own pending entry when a send was registered after it", async () => {
    // The pre-receipt race the pending list exists to make safe. Both entries
    // are receipt-less for an instant; if the compaction's `turn_start` took the
    // *front* entry it would consume the send's slot and stamp the send's id
    // onto a compaction turn — mis-attributing the send's eventual reply.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    invokeMock.mockResolvedValue(COMPACT_MESSAGE);
    await state.dispatchCompaction(AGENT_A, COMPACT_SEND, COMPACT_PENDING, "2026-05-15T00:00:05Z");
    state.dispatchUserTurn(AGENT_A, TURN_1, "later", [], "send-later", "2026-05-15T00:00:06Z");

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: COMPACT_TURN,
      message_id: COMPACT_MESSAGE,
      send_id: COMPACT_SEND,
      started_at: "2026-05-15T00:00:07Z",
    } as NormalizedEvent);

    const turn = state.transcripts[AGENT_A]?.find((t) => t.turn_id === COMPACT_TURN);
    expect(turn?.role).toBe("agent");
    expect(turn?.role === "agent" && turn.kind).toBe("compaction");
    expect(turn?.role === "agent" && turn.send_id).toBe(COMPACT_SEND);
    // The send's entry survives untouched.
    expect(state.runtimes[AGENT_A]?.pending_sends).toEqual([
      { send_id: "send-later", user_turn_id: TURN_1 },
    ]);
  });

  it("drops a cancelled queued compaction without leaving a row", async () => {
    // Decision 8: nothing ran, and there is no user message for a "cancelled"
    // row to sit under — unlike a cancelled queued send, which renders one.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    invokeMock.mockResolvedValue(COMPACT_MESSAGE);
    await state.dispatchCompaction(AGENT_A, COMPACT_SEND, COMPACT_PENDING, "2026-05-15T00:00:05Z");

    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: COMPACT_MESSAGE,
      send_id: COMPACT_SEND,
      agent_id: AGENT_A,
      at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.pending_sends ?? []).toEqual([]);
    expect(state.transcripts[AGENT_A]).toEqual([]);
  });

  it("renders a failed compaction row when it never starts", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    invokeMock.mockResolvedValue(COMPACT_MESSAGE);
    await state.dispatchCompaction(AGENT_A, COMPACT_SEND, COMPACT_PENDING, "2026-05-15T00:00:05Z");

    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: COMPACT_MESSAGE,
      send_id: COMPACT_SEND,
      agent_id: AGENT_A,
      error: "the harness declined to compact this conversation",
      at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);

    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns).toHaveLength(1);
    const turn = turns[0];
    expect(turn?.role === "agent" && turn.kind).toBe("compaction");
    expect(turn?.role === "agent" && turn.status).toBe("failed");
    expect(turn?.role === "agent" && turn.error).toContain("declined to compact");
  });

  it("surfaces an IPC refusal as a failed compaction row", async () => {
    // A refused harness / no session / unmaterialized branch never reaches the
    // event stream, so the rejection has to render the reason itself.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    invokeMock.mockRejectedValue(new Error("alice has no conversation to compact yet"));

    await state.dispatchCompaction(AGENT_A, COMPACT_SEND, COMPACT_PENDING, "2026-05-15T00:00:05Z");

    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
    expect(state.runtimes[AGENT_A]?.pending_sends ?? []).toEqual([]);
    const turns = state.transcripts[AGENT_A] ?? [];
    expect(turns).toHaveLength(1);
    expect(turns[0]?.role === "agent" && turns[0].error).toContain("no conversation to compact");
    // The shape, not just the text: without the kind this renders as an empty
    // failed *response* with no prompt above it, which reads as a crash rather
    // than as the precondition the backend named.
    expect(turns[0]?.role === "agent" && turns[0].kind).toBe("compaction");
  });

  it("tells the terminal hook the turn was a compaction, so the recap can be fetched", async () => {
    // The recap marker lives in the harness's own session file and nothing else
    // fetches it — the compaction turn itself carries no content.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    const seen: { outcome: string; kind?: string }[] = [];
    state.setTurnTerminalHook((_agentId, outcome, kind) => seen.push({ outcome, kind }));
    invokeMock.mockResolvedValue(COMPACT_MESSAGE);
    await state.dispatchCompaction(AGENT_A, COMPACT_SEND, COMPACT_PENDING, "2026-05-15T00:00:05Z");
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: COMPACT_TURN,
      message_id: COMPACT_MESSAGE,
      send_id: COMPACT_SEND,
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: COMPACT_TURN,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:07Z",
      usage: {
        input_tokens: 0,
        output_tokens: 0,
        context_input_tokens: 120000,
        context_tokens_after_turn: 18000,
        context_window: 200000,
      },
    } as NormalizedEvent);

    expect(seen).toEqual([{ outcome: "completed", kind: "compaction" }]);
    const turn = state.transcripts[AGENT_A]?.find((t) => t.turn_id === COMPACT_TURN);
    // Usage is kept: it is what moves the sidebar context bar.
    expect(turn?.role === "agent" && turn.usage?.context_tokens_after_turn).toBe(18000);
    expect(turn?.role === "agent" && turn.status).toBe("complete");
  });

  it("reports an ordinary response to the terminal hook with no kind", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    const seen: { outcome: string; kind?: string }[] = [];
    state.setTurnTerminalHook((_agentId, outcome, kind) => seen.push({ outcome, kind }));
    state.dispatchUserTurn(AGENT_A, TURN_1, "go", [], "send-1", "2026-05-15T00:00:00Z");
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_2,
      message_id: MESSAGE_1,
      send_id: "send-1",
      started_at: "2026-05-15T00:00:01Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_2,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:02Z",
    } as NormalizedEvent);

    expect(seen).toEqual([{ outcome: "completed", kind: undefined }]);
  });
});

describe("context breakdown requests", () => {
  const SEND = "00000000-0000-7000-8000-00000000d001";
  const PENDING = "00000000-0000-7000-8000-00000000d002";
  const MESSAGE = "00000000-0000-7000-8000-00000000d003";
  const TURN = "00000000-0000-7000-8000-00000000d004";
  const QUEUED_AT = "2026-05-15T00:00:05Z";
  const MEASURED_AT = "2026-05-15T00:00:06Z";

  const REPORT = {
    model: "claude-fable-5-1",
    total_tokens: 48_000,
    max_tokens: 200_000,
    categories: [{ name: "Messages", tokens: 48_000, kind: "used" }],
    raw: "## Context Usage",
  };

  async function dispatched(state: Awaited<ReturnType<typeof loadState>>): Promise<void> {
    invokeMock.mockResolvedValue(MESSAGE);
    await state.dispatchContextReport(AGENT_A, SEND, PENDING, QUEUED_AT);
  }

  it("registers its pending entry before the IPC and renders no transcript row", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    let resolveIpc: (id: string) => void = () => {};
    invokeMock.mockImplementation(
      async () => await new Promise<string>((res) => (resolveIpc = res)),
    );

    const inFlight = state.dispatchContextReport(AGENT_A, SEND, PENDING, QUEUED_AT);

    expect(state.runtimes[AGENT_A]?.pending_sends).toEqual([
      { send_id: SEND, user_turn_id: PENDING, kind: "context_report", queued_at: QUEUED_AT },
    ]);
    expect(state.runtimes[AGENT_A]?.context_report_request).toEqual({
      send_id: SEND,
      phase: "queued",
    });
    expect(state.transcripts[AGENT_A]).toEqual([]);

    resolveIpc(MESSAGE);
    await inFlight;
    expect(state.runtimes[AGENT_A]?.context_report_request?.message_id).toBe(MESSAGE);
  });

  it("advances queued → running → done without creating a turn", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await dispatched(state);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN,
      message_id: MESSAGE,
      send_id: SEND,
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);
    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({
      phase: "running",
      turn_id: TURN,
    });
    expect(state.transcripts[AGENT_A]).toEqual([]);

    fireTo(`agent:${AGENT_A}`, {
      type: "context_report",
      agent_id: AGENT_A,
      report: REPORT,
      at: MEASURED_AT,
    } as unknown as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:07Z",
    } as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.context_report_request?.phase).toBe("done");
    expect(state.runtimes[AGENT_A]?.last_context_report).toEqual(REPORT);
    expect(
      state.runtimes[AGENT_A]?.last_context_report_at,
      "a live report carries when it was measured; nothing else will refresh it",
    ).toBe(MEASURED_AT);
    expect(
      state.transcripts[AGENT_A],
      "a report is not conversation — it gets no row at any phase",
    ).toEqual([]);
  });

  it("records a runtime failure on the request, not in the transcript", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await dispatched(state);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN,
      message_id: MESSAGE,
      send_id: SEND,
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN,
      outcome: { status: "failed", kind: "harness_error", message: "the CLI fell over" },
      ended_at: "2026-05-15T00:00:07Z",
    } as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({
      phase: "failed",
      error: "the CLI fell over",
    });
    // The panel is the only surface this failure has: there is no row for it.
    expect(state.transcripts[AGENT_A]).toEqual([]);
  });

  it("records an IPC rejection on the request and renders no failed row", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    invokeMock.mockRejectedValue(new Error("alice has no conversation to analyze yet"));

    await state.dispatchContextReport(AGENT_A, SEND, PENDING, QUEUED_AT);

    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({
      phase: "failed",
      error: "alice has no conversation to analyze yet",
    });
    expect(state.transcripts[AGENT_A]).toEqual([]);
    expect(state.runtimes[AGENT_A]?.pending_sends).toBeUndefined();
    expect(state.runtimes[AGENT_A]?.run_status).toBe("idle");
  });

  it("records a cancellation while queued", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await dispatched(state);

    fireTo(`agent:${AGENT_A}`, {
      type: "message_cancelled",
      message_id: MESSAGE,
      send_id: SEND,
      agent_id: AGENT_A,
    } as unknown as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.context_report_request?.phase).toBe("cancelled");
    expect(state.transcripts[AGENT_A]).toEqual([]);
  });

  it("records a cancellation while running", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await dispatched(state);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN,
      message_id: MESSAGE,
      send_id: SEND,
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN,
      outcome: { status: "cancelled" },
      ended_at: "2026-05-15T00:00:07Z",
    } as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.context_report_request?.phase).toBe("cancelled");
  });

  it("keeps a settled request through an ordinary send, and the previous report through a failure", async () => {
    // Clearing on a send would make a failure message vanish the moment the
    // user typed anything — which is exactly when they would be looking for it.
    // And a failed refresh must leave the last good breakdown on screen: an old
    // measurement is still the best one available.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await dispatched(state);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN,
      message_id: MESSAGE,
      send_id: SEND,
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "context_report",
      agent_id: AGENT_A,
      report: REPORT,
      at: MEASURED_AT,
    } as unknown as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN,
      outcome: { status: "failed", kind: "harness_error", message: "the CLI fell over" },
      ended_at: "2026-05-15T00:00:07Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, { type: "agent_idle", agent_id: AGENT_A });

    state.dispatchUserTurn(AGENT_A, TURN_1, "unrelated", [], "send-9", "2026-05-15T00:00:08Z");

    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({ phase: "failed" });
    expect(state.runtimes[AGENT_A]?.last_context_report).toEqual(REPORT);
  });

  it("files a report that lands before the IPC resolves", async () => {
    // The whole run can finish inside the `context_report_agent` await: the CLI
    // answers `/context` locally in about half a second. The report must be
    // filed anyway, or the panel that asked for it shows the empty state while
    // the measurement it requested has already arrived and been dropped.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    let resolveIpc: (id: string) => void = () => {};
    invokeMock.mockImplementation(
      async () => await new Promise<string>((res) => (resolveIpc = res)),
    );
    const inFlight = state.dispatchContextReport(AGENT_A, SEND, PENDING, QUEUED_AT);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN,
      message_id: MESSAGE,
      send_id: SEND,
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "context_report",
      agent_id: AGENT_A,
      report: REPORT,
      at: MEASURED_AT,
    } as unknown as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:07Z",
    } as NormalizedEvent);

    resolveIpc(MESSAGE);
    await inFlight;

    expect(state.runtimes[AGENT_A]?.last_context_report).toEqual(REPORT);
    expect(state.runtimes[AGENT_A]?.context_report_request?.phase).toBe("done");
  });

  it("leaves a settled request alone when a later, unrelated turn ends", async () => {
    // The request is cleared only by the next report. Without the turn match on
    // the terminal, the very next message the user sent would flip a *failed*
    // request to "done" — erasing the explanation the panel exists to show,
    // at the moment the user went looking for it.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await dispatched(state);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN,
      message_id: MESSAGE,
      send_id: SEND,
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN,
      outcome: { status: "failed", kind: "harness_error", message: "the CLI fell over" },
      ended_at: "2026-05-15T00:00:07Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, { type: "agent_idle", agent_id: AGENT_A });

    state.dispatchUserTurn(AGENT_A, TURN_1, "next", [], "send-9", "2026-05-15T00:00:08Z");
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_2,
      message_id: MESSAGE_1,
      send_id: "send-9",
      started_at: "2026-05-15T00:00:09Z",
    } as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_end",
      turn_id: TURN_2,
      outcome: { status: "completed" },
      ended_at: "2026-05-15T00:00:10Z",
    } as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({
      phase: "failed",
      error: "the CLI fell over",
    });
  });

  it("records a pre-start failure on the request without inventing a row", async () => {
    // The adapter failed to launch, so the backend reports `message_failed`
    // rather than a terminal. A send renders a failed bubble here; a report has
    // no bubble, and the row that would be invented for it has no prompt above
    // it to make sense of.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await dispatched(state);

    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE,
      send_id: null,
      agent_id: AGENT_A,
      error: "claude does not support this operation",
    } as unknown as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({
      phase: "failed",
      error: "claude does not support this operation",
    });
    expect(state.transcripts[AGENT_A]).toEqual([]);
  });

  it("recovers from a launch failure that beats its own receipt", async () => {
    // The dead end this prevents: the failure carries no send id (nothing was
    // journaled) and the receipt has not arrived, so with no other correlation
    // the request sits at "queued" forever — and the panel's button, which is
    // the only way to start another, stays disabled.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    let resolveIpc: (id: string) => void = () => {};
    invokeMock.mockImplementation(
      async () => await new Promise<string>((res) => (resolveIpc = res)),
    );
    const inFlight = state.dispatchContextReport(AGENT_A, SEND, PENDING, QUEUED_AT);

    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE,
      send_id: null,
      agent_id: AGENT_A,
      error: "claude: command not found",
    } as unknown as NormalizedEvent);
    fireTo(`agent:${AGENT_A}`, { type: "agent_idle", agent_id: AGENT_A });

    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({
      phase: "failed",
      error: "claude: command not found",
    });

    resolveIpc(MESSAGE);
    await inFlight;
    // The late receipt stamps its id without reviving the request.
    expect(state.runtimes[AGENT_A]?.context_report_request?.phase).toBe("failed");
    expect(state.transcripts[AGENT_A]).toEqual([]);
  });

  it("leaves a concurrent send's pre-receipt failure alone", async () => {
    // The entry-based correlation must not claim a concurrent *send*'s failure
    // just because the report's receipt is still in flight.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    let resolveIpc: (id: string) => void = () => {};
    invokeMock.mockImplementation(
      async () => await new Promise<string>((res) => (resolveIpc = res)),
    );
    const inFlight = state.dispatchContextReport(AGENT_A, SEND, PENDING, QUEUED_AT);
    state.dispatchUserTurn(AGENT_A, TURN_1, "go", [], "send-9", "2026-05-15T00:00:07Z");

    fireTo(`agent:${AGENT_A}`, {
      type: "message_failed",
      message_id: MESSAGE_1,
      send_id: "send-9",
      agent_id: AGENT_A,
      error: "the send failed",
    } as unknown as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.context_report_request?.phase).toBe("queued");
    resolveIpc(MESSAGE);
    await inFlight;
  });

  it("does not let an unrelated turn advance its request", async () => {
    // The request correlates on its own ids. A concurrent workflow turn naming
    // its own send must leave the queued report queued — advancing it would
    // make the panel claim the report is running when nothing of the sort is.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    await dispatched(state);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_2,
      message_id: MESSAGE_1,
      send_id: "someone-elses-send",
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);

    expect(state.runtimes[AGENT_A]?.context_report_request?.phase).toBe("queued");
  });

  it("advances a request whose receipt has not landed yet", async () => {
    // The pre-receipt race: `turn_start` can arrive before the IPC resolves, so
    // the request has no `message_id` to match on and `send_id` is the only
    // correlation available.
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    let resolveIpc: (id: string) => void = () => {};
    invokeMock.mockImplementation(
      async () => await new Promise<string>((res) => (resolveIpc = res)),
    );
    const inFlight = state.dispatchContextReport(AGENT_A, SEND, PENDING, QUEUED_AT);

    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN,
      message_id: MESSAGE,
      send_id: SEND,
      started_at: "2026-05-15T00:00:06Z",
    } as NormalizedEvent);
    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({
      phase: "running",
      turn_id: TURN,
    });

    resolveIpc(MESSAGE);
    await inFlight;
    // The late receipt must stamp the id without rolling the phase back.
    expect(state.runtimes[AGENT_A]?.context_report_request).toMatchObject({
      phase: "running",
      message_id: MESSAGE,
    });
  });
});
