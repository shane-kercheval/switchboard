import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentRecord, NormalizedEvent } from "$lib/types";

// `transcriptRevision` is the re-anchor change signal: a monotonic counter
// bumped by `setTranscript`, replacing the per-chunk content-digest walk the
// transcript used to recompute. These tests pin that it advances for every
// produced-content path (so the scroll re-anchor still fires) and that the
// single-writer `setTranscript` is the one thing that moves it.

const listeners = new Map<string, (e: { payload: NormalizedEvent }) => void>();

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: NormalizedEvent }) => void) => {
    listeners.set(name, cb);
    return vi.fn();
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
}));

async function loadState() {
  return await import("./index.svelte");
}

function agentRecord(id: string): AgentRecord {
  return {
    id,
    project_id: "00000000-0000-7000-8000-0000000000ff",
    name: "test",
    harness: "claude_code",
    session_locator: null,
    created_at: "2026-05-15T00:00:00Z",
  };
}

const AGENT_A = "00000000-0000-7000-8000-000000000aaa";
const TURN_1 = "00000000-0000-7000-8000-000000000001";
const MESSAGE_1 = "00000000-0000-7000-8000-0000000000f1";

function fireTo(channel: string, event: NormalizedEvent): void {
  const cb = listeners.get(channel);
  if (cb === undefined) throw new Error(`no listener for ${channel}`);
  cb({ payload: event });
}

beforeEach(async () => {
  listeners.clear();
  (await loadState())._testing.reset();
});

describe("transcript revision (the re-anchor change signal)", () => {
  it("advances for every produced-content path: chunk, tool start/complete, turn end, user turn", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:00Z",
    });

    const bumps = (run: () => void): boolean => {
      const before = state.getTranscriptRevision();
      run();
      return state.getTranscriptRevision() > before;
    };

    expect(
      bumps(() =>
        fireTo(`agent:${AGENT_A}`, {
          type: "content_chunk",
          turn_id: TURN_1,
          kind: "text",
          text: "hello",
        }),
      ),
    ).toBe(true);
    expect(
      bumps(() =>
        fireTo(`agent:${AGENT_A}`, {
          type: "tool_started",
          turn_id: TURN_1,
          tool_use_id: "t1",
          kind: "builtin",
          name: "Read",
          input: {},
        }),
      ),
    ).toBe(true);
    expect(
      bumps(() =>
        fireTo(`agent:${AGENT_A}`, {
          type: "tool_completed",
          turn_id: TURN_1,
          tool_use_id: "t1",
          output: "ok",
          is_error: false,
        }),
      ),
    ).toBe(true);
    expect(
      bumps(() =>
        fireTo(`agent:${AGENT_A}`, {
          type: "turn_end",
          turn_id: TURN_1,
          outcome: { status: "completed" },
          ended_at: "2026-05-16T00:00:09Z",
        }),
      ),
    ).toBe(true);
    expect(bumps(() => state.dispatchUserTurn(AGENT_A, "user-1", "hi", [], "send-1"))).toBe(true);
  });

  it("does NOT advance for content-free events (the reducer returns the same reference)", async () => {
    const state = await loadState();
    await state.registerAgent(agentRecord(AGENT_A));
    fireTo(`agent:${AGENT_A}`, {
      type: "turn_start",
      turn_id: TURN_1,
      message_id: MESSAGE_1,
      send_id: MESSAGE_1,
      started_at: "2026-05-16T00:00:00Z",
    });
    // A liveness signal mid-stream re-arms the heartbeat but changes no content,
    // so the reducer returns the same transcript reference and the revision must
    // hold — otherwise it would re-anchor the scroll for nothing.
    const before = state.getTranscriptRevision();
    fireTo(`agent:${AGENT_A}`, { type: "liveness", turn_id: TURN_1 });
    expect(state.getTranscriptRevision()).toBe(before);
  });

  it("setTranscript bumps the revision (the single-writer contract's observable)", async () => {
    const state = await loadState();
    const r0 = state.getTranscriptRevision();
    state.setTranscript(AGENT_A, []);
    expect(state.getTranscriptRevision()).toBe(r0 + 1);
  });
});
