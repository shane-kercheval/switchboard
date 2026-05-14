import { describe, it, expect } from "vitest";
import { appendUserTurn, emptyTranscript, reduce } from "./reducer";
import type { AgentTranscript, NormalizedEvent, Turn } from "./types";

const AGENT_ID = "00000000-0000-7000-8000-000000000000";
const TURN_A = "11111111-1111-7000-8000-111111111111";
const TURN_B = "22222222-2222-7000-8000-222222222222";

function ts(seconds: number): string {
  return new Date(seconds * 1000).toISOString();
}

function expectTurn(transcript: AgentTranscript, index: number): Turn {
  const turn = transcript.turns[index];
  if (turn === undefined) throw new Error(`no turn at index ${index}`);
  return turn;
}

function expectAgentTurn(transcript: AgentTranscript, index: number): Turn & { role: "agent" } {
  const turn = expectTurn(transcript, index);
  if (turn.role !== "agent") throw new Error(`turn at ${index} is not an agent turn`);
  return turn;
}

describe("reducer", () => {
  it("turn_start + N content_chunks + turn_end(completed) → one complete agent turn", () => {
    const events: NormalizedEvent[] = [
      { type: "turn_start", turn_id: TURN_A, started_at: ts(1) },
      { type: "content_chunk", turn_id: TURN_A, text: "Hello, " },
      { type: "content_chunk", turn_id: TURN_A, text: "world" },
      { type: "content_chunk", turn_id: TURN_A, text: "!" },
      {
        type: "turn_end",
        turn_id: TURN_A,
        outcome: { status: "completed" },
        ended_at: ts(2),
      },
    ];
    const final = events.reduce(reduce, emptyTranscript(AGENT_ID));

    expect(final.turns).toHaveLength(1);
    const t = expectAgentTurn(final, 0);
    expect(t.status).toBe("complete");
    expect(t.text).toBe("Hello, world!");
    expect(t.endedAt).toBe(ts(2));
  });

  it("turn_end(failed) populates error and preserves partial text", () => {
    const events: NormalizedEvent[] = [
      { type: "turn_start", turn_id: TURN_A, started_at: ts(1) },
      { type: "content_chunk", turn_id: TURN_A, text: "partial " },
      { type: "content_chunk", turn_id: TURN_A, text: "answer" },
      {
        type: "turn_end",
        turn_id: TURN_A,
        outcome: { status: "failed", kind: "harness_error", message: "rate limit" },
        ended_at: ts(3),
      },
    ];
    const final = events.reduce(reduce, emptyTranscript(AGENT_ID));

    const t = expectAgentTurn(final, 0);
    expect(t.status).toBe("failed");
    expect(t.error).toBe("rate limit");
    expect(t.errorKind).toBe("harness_error");
    expect(t.text).toBe("partial answer");
  });

  it("multiple sequential turns concatenate in arrival order", () => {
    const events: NormalizedEvent[] = [
      { type: "turn_start", turn_id: TURN_A, started_at: ts(1) },
      { type: "content_chunk", turn_id: TURN_A, text: "first" },
      {
        type: "turn_end",
        turn_id: TURN_A,
        outcome: { status: "completed" },
        ended_at: ts(2),
      },
      { type: "turn_start", turn_id: TURN_B, started_at: ts(3) },
      { type: "content_chunk", turn_id: TURN_B, text: "second" },
      {
        type: "turn_end",
        turn_id: TURN_B,
        outcome: { status: "completed" },
        ended_at: ts(4),
      },
    ];
    const final = events.reduce(reduce, emptyTranscript(AGENT_ID));

    expect(final.turns.map((t) => t.id)).toEqual([TURN_A, TURN_B]);
    expect(expectTurn(final, 0).text).toBe("first");
    expect(expectTurn(final, 1).text).toBe("second");
  });

  it("event with unknown turn_id is ignored (cross-turn isolation)", () => {
    let t = emptyTranscript(AGENT_ID);
    t = reduce(t, { type: "turn_start", turn_id: TURN_A, started_at: ts(1) });
    t = reduce(t, {
      type: "turn_end",
      turn_id: TURN_A,
      outcome: { status: "completed" },
      ended_at: ts(2),
    });
    const before = t;

    const after = reduce(before, {
      type: "content_chunk",
      turn_id: TURN_B,
      text: "should not appear",
    });
    expect(after).toEqual(before);
  });

  it("late event for an already-terminalized turn is ignored", () => {
    // The dispatcher's drain task can keep emitting after the frontend has
    // heartbeat-timed-out a turn. Without this guard the failed turn would
    // resurrect with late content.
    let t = emptyTranscript(AGENT_ID);
    t = reduce(t, { type: "turn_start", turn_id: TURN_A, started_at: ts(1) });
    t = reduce(t, { type: "heartbeat_timeout", turn_id: TURN_A });

    const original = t;
    expect(expectAgentTurn(original, 0).status).toBe("failed");

    const after1 = reduce(original, {
      type: "content_chunk",
      turn_id: TURN_A,
      text: "late content",
    });
    expect(after1).toEqual(original);

    const after2 = reduce(original, {
      type: "turn_end",
      turn_id: TURN_A,
      outcome: { status: "completed" },
      ended_at: ts(100),
    });
    expect(after2).toEqual(original);
  });

  it("heartbeat_timeout transitions a streaming turn to failed with adapter_failure kind", () => {
    let t = emptyTranscript(AGENT_ID);
    t = reduce(t, { type: "turn_start", turn_id: TURN_A, started_at: ts(1) });
    t = reduce(t, { type: "content_chunk", turn_id: TURN_A, text: "stuck halfway" });
    t = reduce(t, { type: "heartbeat_timeout", turn_id: TURN_A });

    const turn = expectAgentTurn(t, 0);
    expect(turn.status).toBe("failed");
    expect(turn.error).toBe("no response from harness — retry?");
    expect(turn.errorKind).toBe("adapter_failure");
    expect(turn.text).toBe("stuck halfway");
  });

  it("user turns interleave with agent turns in submit order", () => {
    // Production gives user turns a local crypto.randomUUID() distinct from
    // the backend-assigned agent turn_id — see AgentPane::handleSubmit. Test
    // mirrors that: user and agent ids are different.
    const USER_TURN = "33333333-3333-7000-8000-333333333333";
    let t: AgentTranscript = emptyTranscript(AGENT_ID);
    t = appendUserTurn(t, USER_TURN, "what is 2+2?");
    t = reduce(t, { type: "turn_start", turn_id: TURN_A, started_at: ts(1) });
    t = reduce(t, { type: "content_chunk", turn_id: TURN_A, text: "4" });
    t = reduce(t, {
      type: "turn_end",
      turn_id: TURN_A,
      outcome: { status: "completed" },
      ended_at: ts(2),
    });

    expect(t.turns).toHaveLength(2);
    expect(expectTurn(t, 0).role).toBe("user");
    expect(expectTurn(t, 1).role).toBe("agent");
  });

  it("heartbeat_timeout for unknown turn is a no-op", () => {
    const t = emptyTranscript(AGENT_ID);
    const after = reduce(t, { type: "heartbeat_timeout", turn_id: TURN_A });
    expect(after).toEqual(t);
  });

  it("duplicate turn_start for the same turn_id appends only one agent turn", () => {
    // Defense-in-depth: a duplicate turn_start (dispatcher bug, late retry
    // delivery) must not produce two agent turns with the same id. Svelte's
    // keyed `{#each}` would silently collapse them and the reducer state
    // would diverge from what's rendered.
    let t = emptyTranscript(AGENT_ID);
    t = reduce(t, { type: "turn_start", turn_id: TURN_A, started_at: ts(1) });
    t = reduce(t, { type: "turn_start", turn_id: TURN_A, started_at: ts(2) });
    expect(t.turns).toHaveLength(1);
    // The original turn — including its startedAt — is preserved; the second
    // turn_start is dropped, not overwritten.
    const turn = expectAgentTurn(t, 0);
    expect(turn.startedAt).toBe(ts(1));
  });
});
