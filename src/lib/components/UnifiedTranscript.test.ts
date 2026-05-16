import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/svelte";
import type { AgentRecord, NormalizedEvent } from "$lib/types";

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

function fireTo(channel: string, event: NormalizedEvent): void {
  const cb = listeners.get(channel);
  if (cb === undefined) throw new Error(`no listener for ${channel}`);
  cb({ payload: event });
}

const CLAUDE_AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "alice",
  harness: "claude_code",
  session_id: "00000000-0000-7000-8000-000000000001",
  created_at: "2026-05-16T00:00:00Z",
};
const CODEX_AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "bob",
  harness: "codex",
  session_id: null,
  created_at: "2026-05-16T00:00:01Z",
};

beforeEach(() => {
  listeners.clear();
});

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
});

describe("UnifiedTranscript", () => {
  it("renders empty-state message when no turns exist", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByText(/no messages yet/i)).toBeInTheDocument();
  });

  it("merges turns across multiple agents in chronological order", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);

    // Directly seed transcripts to pin the merge logic without going
    // through dispatch (which would require event ordering setup).
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        text: "hi alice",
      },
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "hello!" }],
      },
    ];
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-2",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:02Z",
        text: "hi bob",
      },
      {
        role: "agent",
        turn_id: "agent-2",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:03Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "hey!" }],
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const turns = screen.getAllByTestId("turn");
    expect(turns).toHaveLength(4);
    // Chronological: alice user → alice agent → bob user → bob agent.
    expect(turns[0]).toHaveAttribute("data-role", "user");
    expect(turns[0]).toHaveTextContent("hi alice");
    expect(turns[1]).toHaveAttribute("data-role", "agent");
    expect(turns[1]).toHaveTextContent("hello!");
    expect(turns[2]).toHaveAttribute("data-role", "user");
    expect(turns[2]).toHaveTextContent("hi bob");
    expect(turns[3]).toHaveAttribute("data-role", "agent");
    expect(turns[3]).toHaveTextContent("hey!");
  });

  it("preserves insertion order on same-timestamp ties (no v4-random tie-breaker)", async () => {
    // Same-agent same-timestamp: user turn + agent turn share started_at.
    // User-turn ids are crypto.randomUUID (v4); agent ids are UUID v7.
    // A lexicographic turn_id tie-breaker would order them randomly.
    // Stable sort + transcripts[agent.id] insertion order keeps user
    // first (it was appended first by dispatchUserTurn).
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "ffffffff-ffff-4fff-8fff-ffffffffffff", // v4-random shape; sorts AFTER v7 lex
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00.000Z",
        text: "prompt",
      },
      {
        role: "agent",
        turn_id: "00000000-0000-7000-8000-000000000001", // v7-time-ordered; sorts BEFORE the v4 above
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00.000Z", // identical to user's
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "response" }],
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const turns = screen.getAllByTestId("turn");
    // User must render before agent — preserved via stable sort over
    // the insertion order in transcripts[CLAUDE_AGENT.id], NOT by
    // turn_id comparison (which would put agent first because v7 < v4
    // lexicographically).
    expect(turns[0]).toHaveAttribute("data-role", "user");
    expect(turns[1]).toHaveAttribute("data-role", "agent");
  });

  it("attributes user turns to their recipient agent (You → agentname)", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        text: "test",
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    expect(screen.getByTestId("turn-recipient")).toHaveTextContent("bob");
  });

  it("renders interleaved text/tool/text items in order", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [
          { item_kind: "text", kind: "text", text: "Running… " },
          {
            item_kind: "tool",
            tool_use_id: "tool-1",
            kind: "builtin",
            name: "Bash",
            input: { command: "echo hi" },
            output: "hi\n",
            is_error: false,
            started_at: "2026-05-16T00:00:01Z",
            completed_at: "2026-05-16T00:00:02Z",
          },
          { item_kind: "text", kind: "text", text: "Done." },
        ],
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const turn = screen.getByTestId("turn");
    // The text chunks and tool are rendered in order — DOM children
    // sequence reflects items array order.
    const tool = screen.getByTestId("turn-tool");
    expect(tool).toHaveAttribute("data-tool-use-id", "tool-1");
    expect(tool).toHaveTextContent("Bash");
    expect(tool).toHaveTextContent("hi");
    // Both surrounding text chunks present.
    expect(turn).toHaveTextContent("Running…");
    expect(turn).toHaveTextContent("Done.");
  });

  it("shows processing… indicator for Codex turns with empty items array (streaming)", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "streaming",
        items: [], // Codex emits one whole agent_message at the end — empty until then
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    expect(screen.getByTestId("turn-processing")).toBeInTheDocument();
  });

  it("hides processing… indicator once items arrive", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "streaming",
        items: [{ item_kind: "text", kind: "text", text: "ack" }],
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    expect(screen.queryByTestId("turn-processing")).toBeNull();
    expect(screen.getByTestId("turn")).toHaveTextContent("ack");
  });

  it("live-streams an in-progress turn into the unified view", async () => {
    // Real listener path: register, fire turn_start, content_chunk, etc.
    // through the captured callback. The reducer + state-module + Svelte
    // reactivity should drive the rendered DOM.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "turn_start",
      turn_id: "turn-1",
      started_at: "2026-05-16T00:00:00Z",
    });
    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "content_chunk",
      turn_id: "turn-1",
      kind: "text",
      text: "hello",
    });
    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "content_chunk",
      turn_id: "turn-1",
      kind: "text",
      text: " world",
    });

    // State updates synchronously; Svelte's render flush is async — use
    // waitFor for the DOM assertion.
    expect(state.transcripts[CLAUDE_AGENT.id]?.length).toBe(1);
    await waitFor(() => {
      expect(screen.getByTestId("turn-streaming")).toBeInTheDocument();
    });
    expect(screen.getByTestId("turn")).toHaveTextContent("hello");
    expect(screen.getByTestId("turn")).toHaveTextContent("world");
  });

  it("renders failed turn with error message", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "failed",
        items: [],
        error: "rate limited",
        error_kind: "harness_error",
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("turn-error")).toHaveTextContent("rate limited");
  });
});
