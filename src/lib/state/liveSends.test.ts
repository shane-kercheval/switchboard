import { describe, expect, it } from "vitest";
import type { AgentRecord } from "$lib/types";
import type { AgentRuntime, RuntimeMap, TranscriptMap } from "./types";
import { freshRuntime } from "./reducers";
import { buildLiveSendsMap } from "./liveSends";

const AGENT_A = "00000000-0000-7000-8000-00000000000a";
const AGENT_B = "00000000-0000-7000-8000-00000000000b";

function agent(id: string): AgentRecord {
  return {
    id,
    project_id: "00000000-0000-7000-8000-0000000000ff",
    name: id,
    harness: "claude_code",
    session_id: null,
    created_at: "2026-05-16T00:00:00Z",
  };
}

function runtime(id: string, over: Partial<AgentRuntime>): AgentRuntime {
  return { ...freshRuntime(id), ...over };
}

function streamingTurn(agentId: string, sendId: string): TranscriptMap[string][number] {
  return {
    role: "agent",
    turn_id: `turn-${sendId}`,
    agent_id: agentId,
    send_id: sendId,
    started_at: "2026-05-16T00:00:00Z",
    status: "streaming",
    items: [],
  };
}

describe("buildLiveSendsMap", () => {
  it("includes non-cancelled pending sends and excludes cancel_requested ones", () => {
    const runtimes: RuntimeMap = {
      [AGENT_A]: runtime(AGENT_A, {
        pending_sends: [
          { send_id: "send-live", user_turn_id: "u1" },
          { send_id: "send-cancelling", user_turn_id: "u2", cancel_requested: true },
        ],
      }),
    };
    const map = buildLiveSendsMap([agent(AGENT_A)], runtimes, {});
    expect([...map.keys()]).toEqual(["send-live"]);
    expect(map.get("send-live")).toEqual([AGENT_A]);
  });

  it("includes streaming turns only while the agent is processing", () => {
    const transcripts: TranscriptMap = { [AGENT_A]: [streamingTurn(AGENT_A, "send-1")] };

    const processing = buildLiveSendsMap(
      [agent(AGENT_A)],
      { [AGENT_A]: runtime(AGENT_A, { run_status: "processing" }) },
      transcripts,
    );
    expect(processing.get("send-1")).toEqual([AGENT_A]);

    // Idle agent: the transcript scan is skipped, so a (stale) streaming turn
    // contributes nothing — the perf gate that keeps idle projects cheap.
    const idle = buildLiveSendsMap(
      [agent(AGENT_A)],
      { [AGENT_A]: runtime(AGENT_A, { run_status: "idle" }) },
      transcripts,
    );
    expect(idle.size).toBe(0);
  });

  it("dedupes an agent that appears in both pending and streaming for one send", () => {
    const map = buildLiveSendsMap(
      [agent(AGENT_A)],
      {
        [AGENT_A]: runtime(AGENT_A, {
          run_status: "processing",
          pending_sends: [{ send_id: "send-1", user_turn_id: "u1" }],
        }),
      },
      { [AGENT_A]: [streamingTurn(AGENT_A, "send-1")] },
    );
    expect(map.get("send-1")).toEqual([AGENT_A]);
  });

  it("groups multiple agents under a shared fan-out send", () => {
    const map = buildLiveSendsMap(
      [agent(AGENT_A), agent(AGENT_B)],
      {
        [AGENT_A]: runtime(AGENT_A, { pending_sends: [{ send_id: "fan", user_turn_id: "u1" }] }),
        [AGENT_B]: runtime(AGENT_B, { pending_sends: [{ send_id: "fan", user_turn_id: "u2" }] }),
      },
      {},
    );
    expect(map.get("fan")).toEqual([AGENT_A, AGENT_B]);
  });
});
