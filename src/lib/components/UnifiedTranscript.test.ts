import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import type { AgentRecord, NormalizedEvent } from "$lib/types";
import { HEARTBEAT_TIMEOUT_MS } from "$lib/types";
import { agentCopy } from "$lib/agentCopy.svelte";
// Static import so the component-tree transform happens at module collection,
// not inside the first test's timeout (cold CI transforms have no vite cache).
// `vi.mock` is hoisted above imports, so the mocks below still apply.
import UnifiedTranscript from "./UnifiedTranscript.svelte";

const listeners = new Map<string, (e: { payload: NormalizedEvent }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: NormalizedEvent }) => void) => {
    listeners.set(name, cb);
    return vi.fn();
  }),
}));

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));

const copyTextMock = vi.fn(async (_t: string): Promise<void> => undefined);
vi.mock("$lib/native", () => ({
  copyText: (t: string) => copyTextMock(t),
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
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-05-16T00:00:00Z",
};
const CODEX_AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "bob",
  harness: "codex",
  session_locator: null,
  created_at: "2026-05-16T00:00:01Z",
};

beforeEach(() => {
  listeners.clear();
  invokeMock.mockReset();
  agentCopy.set("last_answer_block");
});

const SEND_1 = "00000000-0000-7000-8000-0000000000d1";

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
});

describe("UnifiedTranscript", () => {
  it("renders empty-state message when no turns exist", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

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

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const turns = screen.getAllByTestId("turn");
    // User must render before agent — preserved via stable sort over
    // the insertion order in transcripts[CLAUDE_AGENT.id], NOT by
    // turn_id comparison (which would put agent first because v7 < v4
    // lexicographically).
    expect(turns[0]).toHaveAttribute("data-role", "user");
    expect(turns[1]).toHaveAttribute("data-role", "agent");
  });

  it("renders the user's own turn distinctly from agent turns (role attribution, no explicit label)", async () => {
    // The user message renders as just the prompt text — no "User" label —
    // because authorship is implicit (it's the only non-agent role).
    // Recipient agents are still labeled by name on their response turns.
    // Outer container carries data-role="user" for styling / queries.
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

    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    const turn = screen.getByTestId("turn");
    expect(turn).toHaveAttribute("data-role", "user");
    expect(turn).toHaveTextContent("test");
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

  it("suppresses empty tool output body while keeping the lifecycle badge", async () => {
    // Harness-agnostic rule: a completed tool with `output === ""` renders
    // the tool name + completed state but no `<pre>` output body.
    // Defends against the regression where Gemini's live stream emits
    // empty `output` for read-like tools, and the body block would
    // otherwise render as a visible blank pre.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-empty-tool",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [
          {
            item_kind: "tool",
            tool_use_id: "empty-tool",
            kind: "builtin",
            name: "read_file",
            input: { file_path: "MARKER.txt" },
            output: "",
            is_error: false,
            started_at: "2026-05-16T00:00:01Z",
            completed_at: "2026-05-16T00:00:02Z",
          },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const tool = screen.getByTestId("turn-tool");
    expect(tool).toHaveTextContent("read_file");
    // Lifecycle badge: tool is completed and not an error, so no
    // "running…" or "error" annotation should appear.
    expect(tool).not.toHaveTextContent("running…");
    expect(tool).not.toHaveTextContent("error");
    // Body block suppressed — no <pre> child.
    expect(tool.querySelector("pre")).toBeNull();
  });

  it("collapses completed tool output by default and hides the internal builtin label", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-tool",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [
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
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const tool = screen.getByTestId("turn-tool");
    expect(tool).not.toHaveAttribute("open");
    expect(tool.querySelector("summary")).toHaveTextContent("Tool");
    expect(tool).not.toHaveTextContent("builtin");
    // Success shows the quiet muted check, not the running/error indicators.
    expect(tool.querySelector('[data-testid="tool-done"]')).not.toBeNull();
    expect(tool.querySelector('[data-testid="tool-error"]')).toBeNull();
  });

  it("keeps completed tool errors collapsed while showing error in the header", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-tool-error",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [
          {
            item_kind: "tool",
            tool_use_id: "tool-error",
            kind: "builtin",
            name: "Read",
            input: { file_path: "missing.txt" },
            output: "File does not exist",
            is_error: true,
            started_at: "2026-05-16T00:00:01Z",
            completed_at: "2026-05-16T00:00:02Z",
          },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const tool = screen.getByTestId("turn-tool");
    expect(tool).not.toHaveAttribute("open");
    expect(tool.querySelector('[data-testid="tool-error"]')).not.toBeNull();
  });

  it("expands running tool calls by default", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-running-tool",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "streaming",
        items: [
          {
            item_kind: "tool",
            tool_use_id: "tool-running",
            kind: "builtin",
            name: "Bash",
            input: { command: "sleep 1" },
            started_at: "2026-05-16T00:00:01Z",
          },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const tool = screen.getByTestId("turn-tool");
    expect(tool).toHaveAttribute("open");
    expect(tool.querySelector('[data-testid="tool-running"]')).not.toBeNull();
  });

  it("shows a Working... footer for Codex turns with empty items array (streaming)", async () => {
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

    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    expect(screen.getByTestId("turn-working")).toHaveTextContent("Working...");
  });

  it("keeps the Working... footer at the bottom once items arrive", async () => {
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

    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    expect(screen.getByTestId("turn")).toHaveTextContent("ack");
    expect(screen.getByTestId("turn-working")).toHaveTextContent("Working...");
  });

  it("shows the soft, counting-up 'No response' variant when the in-flight turn is quiet", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "streaming",
        items: [],
      },
    ];
    state.runtimes[CODEX_AGENT.id] = {
      ...state.runtimes[CODEX_AGENT.id]!,
      quiet_since: new Date().toISOString(),
      in_flight_turn_id: "agent-1",
    };

    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    const footer = screen.getByTestId("turn-working");
    expect(footer).toHaveTextContent("No response");
    expect(footer).not.toHaveTextContent("Working...");
    expect(footer).toHaveAttribute("data-quiet", "true");
  });

  it("does NOT show quiet on a streaming turn that is not the in-flight turn", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "streaming",
        items: [],
      },
    ];
    // Agent is quiet, but the heartbeat is tracking a different turn — the
    // footer for agent-1 must stay "Working...".
    state.runtimes[CODEX_AGENT.id] = {
      ...state.runtimes[CODEX_AGENT.id]!,
      quiet_since: new Date().toISOString(),
      in_flight_turn_id: "some-other-turn",
    };

    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    const footer = screen.getByTestId("turn-working");
    expect(footer).toHaveTextContent("Working...");
    expect(footer).toHaveAttribute("data-quiet", "false");
  });

  it("flips Working… → No response → Working… across the live heartbeat timer", async () => {
    // End-to-end reactive wiring: the timer fires through the real listener
    // path, mutates runtime.quiet_since, and the footer re-renders — then
    // activity clears it. This is the seam pure-reducer tests can't cover.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const state = await loadState();
      await state.registerAgent(CODEX_AGENT);

      render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

      fireTo(`agent:${CODEX_AGENT.id}`, {
        type: "turn_start",
        turn_id: "agent-1",
        message_id: "00000000-0000-7000-8000-0000000000e1",
        started_at: "2026-05-16T00:00:00Z",
      });
      await tick();
      expect(screen.getByTestId("turn-working")).toHaveTextContent("Working...");

      // Past the silence threshold → the quiet counter appears.
      vi.advanceTimersByTime(HEARTBEAT_TIMEOUT_MS + 1000);
      await tick();
      expect(screen.getByTestId("turn-working")).toHaveTextContent("No response");
      expect(screen.getByTestId("turn-working")).toHaveAttribute("data-quiet", "true");

      // Activity resumes → back to Working...
      fireTo(`agent:${CODEX_AGENT.id}`, {
        type: "content_chunk",
        turn_id: "agent-1",
        kind: "text",
        text: "hi",
      });
      await tick();
      expect(screen.getByTestId("turn-working")).toHaveTextContent("Working...");
      expect(screen.getByTestId("turn-working")).toHaveAttribute("data-quiet", "false");
    } finally {
      vi.useRealTimers();
    }
  });

  it("live-streams an in-progress turn into the unified view", async () => {
    // Real listener path: register, fire turn_start, content_chunk, etc.
    // through the captured callback. The reducer + state-module + Svelte
    // reactivity should drive the rendered DOM.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "turn_start",
      turn_id: "turn-1",
      message_id: "msg-1",
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
      expect(screen.getByTestId("turn")).toHaveTextContent("hello");
    });
    expect(screen.queryByText("streaming…")).toBeNull();
    expect(screen.getByTestId("turn")).toHaveTextContent("hello");
    expect(screen.getByTestId("turn")).toHaveTextContent("world");
    expect(screen.getByTestId("turn-working")).toHaveTextContent("Working...");
  });

  it("shows a live cancel control for a streaming standalone turn (send-scoped)", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "send-1",
        started_at: "2026-05-16T00:00:00Z",
        status: "streaming",
        items: [{ item_kind: "text", kind: "text", text: "working" }],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    // Cancel is send-scoped (TOCTOU-safe), not turn-scoped: it targets the
    // turn's send_id, so a turn that completes mid-click can't be mis-cancelled.
    await fireEvent.click(screen.getByTestId("turn-live-control"));
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_send",
      expect.objectContaining({ sendId: "send-1", recipients: [CLAUDE_AGENT.id] }),
    );
  });

  it("shows a queued… indicator + cancel for a queued single-recipient send", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    // A dispatched-but-not-started send: optimistic user turn + pending entry,
    // backend-accepted (message_id recorded) but no turn_start yet (queued
    // behind some other work).
    state.dispatchUserTurn(
      CLAUDE_AGENT.id,
      "user-1",
      "later",
      [],
      "send-q",
      "2026-05-16T00:00:00Z",
    );
    state.recordSendAccepted(CLAUDE_AGENT.id, "user-1", "msg-q");

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("turn-queued")).toHaveTextContent("Queued...");
    // The cancel control targets the queued send (send-scoped).
    await fireEvent.click(screen.getByTestId("turn-live-control"));
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_send",
      expect.objectContaining({ sendId: "send-q", recipients: [CLAUDE_AGENT.id] }),
    );
  });

  it("hides the live cancel control for a streaming turn with no send_id", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "streaming",
        items: [{ item_kind: "text", kind: "text", text: "working" }],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("turn-live-control")).toBeNull();
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

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("turn-error")).toHaveTextContent("rate limited");
  });
});

describe("UnifiedTranscript — fan-out groups", () => {
  function seedFanout(
    state: Awaited<ReturnType<typeof loadState>>,
    aliceResponse: {
      status: "streaming" | "complete";
      text?: string;
      model?: string;
      effort?: string;
    } | null,
    bobResponse: {
      status: "streaming" | "complete";
      text?: string;
      model?: string;
      effort?: string;
    } | null,
  ): void {
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-alice",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "fan out",
      },
      ...(aliceResponse
        ? [
            {
              role: "agent" as const,
              turn_id: "a-alice",
              agent_id: CLAUDE_AGENT.id,
              send_id: SEND_1,
              started_at: "2026-05-16T00:00:01Z",
              status: aliceResponse.status,
              items: aliceResponse.text
                ? [{ item_kind: "text" as const, kind: "text" as const, text: aliceResponse.text }]
                : [],
              model: aliceResponse.model,
              effort: aliceResponse.effort,
            },
          ]
        : []),
    ];
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-bob",
        agent_id: CODEX_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "fan out",
      },
      ...(bobResponse
        ? [
            {
              role: "agent" as const,
              turn_id: "a-bob",
              agent_id: CODEX_AGENT.id,
              send_id: SEND_1,
              started_at: "2026-05-16T00:00:02Z",
              status: bobResponse.status,
              items: bobResponse.text
                ? [{ item_kind: "text" as const, kind: "text" as const, text: bobResponse.text }]
                : [],
              model: bobResponse.model,
              effort: bobResponse.effort,
            },
          ]
        : []),
    ];
  }

  it("renders one group with the user message once and a column per recipient", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    seedFanout(
      state,
      { status: "complete", text: "alice reply" },
      { status: "complete", text: "bob reply" },
    );

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    expect(screen.getByTestId("fanout-group")).toBeInTheDocument();
    // The user's message renders once at the group head — exactly one
    // user-role turn in the group. The "User" label is gone by design
    // (the user-role is implicit); recipients are conveyed by columns.
    const userTurns = screen
      .getAllByTestId("turn")
      .filter((el) => el.getAttribute("data-role") === "user");
    expect(userTurns).toHaveLength(1);
    // One column per recipient, in recipient order, each with its response.
    const columns = screen.getAllByTestId("fanout-column");
    expect(columns).toHaveLength(2);
    expect(columns[0]).toHaveAttribute("data-agent-id", CLAUDE_AGENT.id);
    expect(columns[0]).toHaveTextContent("alice reply");
    expect(columns[1]).toHaveAttribute("data-agent-id", CODEX_AGENT.id);
    expect(columns[1]).toHaveTextContent("bob reply");
  });

  it("renders each fan-out column's own model and effort in the footer", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    seedFanout(
      state,
      { status: "complete", text: "alice reply", model: "claude-opus-4-8" },
      { status: "complete", text: "bob reply", model: "gpt-5.5", effort: "high" },
    );

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const columns = screen.getAllByTestId("fanout-column");
    expect(columns[0]).toHaveTextContent("claude-opus-4-8");
    expect(columns[0]!.querySelector('[data-testid="message-model"]')).toHaveTextContent(
      "claude-opus-4-8",
    );
    expect(columns[0]!.querySelector('[data-testid="message-effort"]')).toBeNull();
    expect(columns[1]!.querySelector('[data-testid="message-model"]')).toHaveTextContent("gpt-5.5");
    expect(columns[1]!.querySelector('[data-testid="message-effort"]')).toHaveTextContent("high");
  });

  it("shows a queued indicator for a recipient with no response yet", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // Alice responded; bob is still queued (busy agent).
    seedFanout(state, { status: "complete", text: "alice reply" }, null);

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    expect(screen.getByTestId("fanout-queued")).toHaveTextContent("Queued...");
    const columns = screen.getAllByTestId("fanout-column");
    expect(columns[1]).toHaveAttribute("data-state", "queued");
  });

  it("offers per-recipient cancel controls only for live columns", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // Alice streaming, bob queued → group is live.
    seedFanout(state, { status: "streaming", text: "thinking" }, null);

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const queuedCancel = screen.getByTestId("fanout-card-cancel");
    const streamingCancel = screen.getByTestId("turn-live-control");
    expect(screen.getByTestId("turn-working")).toHaveTextContent("Working...");
    await fireEvent.click(queuedCancel);
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_send",
      expect.objectContaining({ sendId: SEND_1 }),
    );
    const call = invokeMock.mock.calls.find(([c]) => c === "cancel_send");
    expect((call?.[1] as { recipients: string[] }).recipients).toEqual([CODEX_AGENT.id]);

    invokeMock.mockClear();
    await fireEvent.click(streamingCancel);
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_send",
      expect.objectContaining({ sendId: SEND_1, recipients: [CLAUDE_AGENT.id] }),
    );
  });

  it("hides per-recipient cancel controls once every recipient has settled", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    seedFanout(state, { status: "complete", text: "a" }, { status: "complete", text: "b" });

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    expect(screen.queryByTestId("fanout-card-cancel")).toBeNull();
  });

  it("renders a single-recipient send as a normal row, not a fan-out group", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "solo",
      },
      {
        role: "agent",
        turn_id: "a-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "ok" }],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("fanout-group")).toBeNull();
    expect(screen.getAllByTestId("turn")).toHaveLength(2);
  });
});

describe("UnifiedTranscript — markdown rendering", () => {
  it("renders agent text as markdown with tool calls interleaved between segments", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-md",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [
          { item_kind: "text", kind: "text", text: "Running **before** the tool" },
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
          { item_kind: "text", kind: "text", text: "Done with `code` after" },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const turn = screen.getByTestId("turn");
    // Both text segments are formatted (not raw markdown).
    expect(turn.querySelector("strong")).toHaveTextContent("before");
    expect(turn.querySelector("code")).toHaveTextContent("code");
    // The tool box still renders *between* the two markdown segments — assert the
    // DOM order is [markdown, tool, markdown].
    const sequence = Array.from(
      turn.querySelectorAll(".markdown-body, [data-testid='turn-tool']"),
    ).map((el) => (el.getAttribute("data-testid") === "turn-tool" ? "tool" : "md"));
    expect(sequence).toEqual(["md", "tool", "md"]);
  });

  it("renders a user message as markdown", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-md",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        text: "# Heading\n\nwith **bold**",
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const turn = screen.getByTestId("turn");
    expect(turn.querySelector("h1")).toHaveTextContent("Heading");
    expect(turn.querySelector("strong")).toHaveTextContent("bold");
  });

  it("renders markdown in each fan-out column", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-a",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "fan out",
      },
      {
        role: "agent",
        turn_id: "a-a",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "**alice**" }],
      },
    ];
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-b",
        agent_id: CODEX_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "fan out",
      },
      {
        role: "agent",
        turn_id: "a-b",
        agent_id: CODEX_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:02Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "**bob**" }],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const columns = screen.getAllByTestId("fanout-column");
    expect(columns[0]!.querySelector("strong")).toHaveTextContent("alice");
    expect(columns[1]!.querySelector("strong")).toHaveTextContent("bob");
  });

  it("renders streaming markdown without throwing on an intermediate partial fence", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "turn_start",
      turn_id: "turn-1",
      message_id: "msg-1",
      started_at: "2026-05-16T00:00:00Z",
    });
    // Unclosed fence — marked renders a partial code block; must not throw.
    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "content_chunk",
      turn_id: "turn-1",
      kind: "text",
      text: "```rust\nlet x =",
    });
    await waitFor(() => {
      expect(screen.getByTestId("turn").querySelector("pre")).not.toBeNull();
    });
    // Closing chunk completes the block.
    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "content_chunk",
      turn_id: "turn-1",
      kind: "text",
      text: " 1;\n```",
    });
    await waitFor(() => {
      expect(screen.getByTestId("turn")).toHaveTextContent("let x = 1;");
    });
  });

  it("keeps the view pinned to the bottom as streaming text grows", async () => {
    // jsdom does no layout (scrollHeight/clientHeight are 0), so stub the
    // dimensions and assert the wiring: a content_chunk into an existing turn
    // grows item.text.length → scrollSignal changes → the pin effect re-assigns
    // scrollTop to the bottom. This guards the streaming-growth → re-pin chain;
    // the real height-jump-after-paint behavior is verified manually.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const container = screen.getByTestId("unified-transcript");
    Object.defineProperty(container, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(container, "clientHeight", { configurable: true, value: 500 });

    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "turn_start",
      turn_id: "turn-1",
      message_id: "msg-1",
      started_at: "2026-05-16T00:00:00Z",
    });
    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "content_chunk",
      turn_id: "turn-1",
      kind: "text",
      text: "hello",
    });
    // Growing the same turn's text (not adding a row) exercises the
    // item.text.length term of scrollSignal specifically.
    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "content_chunk",
      turn_id: "turn-1",
      kind: "text",
      text: " world, more streaming content arrives",
    });

    await waitFor(() => {
      expect(container.scrollTop).toBe(1000);
    });
  });

  it("intercepts a link click, routes it to open_external_url, and prevents navigation", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-link",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "see [site](https://example.com)" }],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const link = screen.getByTestId("turn").querySelector("a");
    if (!link) throw new Error("expected a rendered link");
    invokeMock.mockClear();

    const notCancelled = await fireEvent.click(link);

    // Routed to the backend opener (which validates the scheme), not the webview.
    expect(invokeMock).toHaveBeenCalledWith("open_external_url", { url: "https://example.com" });
    expect(notCancelled).toBe(false);
  });
});

describe("UnifiedTranscript — per-message copy", () => {
  it("copies a user message's text", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        text: "please do the thing",
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });
    copyTextMock.mockClear();

    const turn = screen.getByTestId("turn");
    const copy = turn.querySelector('[data-testid="message-copy"]');
    if (!copy) throw new Error("expected a copy button on the user message");
    await fireEvent.click(copy);

    expect(copyTextMock).toHaveBeenCalledWith("please do the thing");
  });

  it("copies an agent message's last answer block by default", async () => {
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
          { item_kind: "text", kind: "text", text: "Here is **step one**" },
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
          { item_kind: "text", kind: "text", text: "and step two." },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });
    copyTextMock.mockClear();

    const turn = screen.getByTestId("turn");
    const copy = turn.querySelector('[data-testid="message-copy"]');
    if (!copy) throw new Error("expected a copy button on the agent message");
    await fireEvent.click(copy);

    expect(copyTextMock).toHaveBeenCalledWith("and step two.");
  });

  it("copies full agent answer prose when that copy mode is selected", async () => {
    agentCopy.set("full_answer");
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
          { item_kind: "text", kind: "text", text: "\nHere is **step one**\n\n" },
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
          { item_kind: "text", kind: "thinking", text: "private reasoning" },
          { item_kind: "text", kind: "text", text: "\nand step two.\n" },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });
    copyTextMock.mockClear();

    const turn = screen.getByTestId("turn");
    const copy = turn.querySelector('[data-testid="message-copy"]');
    if (!copy) throw new Error("expected a copy button on the agent message");
    await fireEvent.click(copy);

    // Prose segments joined with normalized spacing; tool output and reasoning are omitted.
    expect(copyTextMock).toHaveBeenCalledWith("Here is **step one**\n\nand step two.");
  });

  it("renders a thinking item as a distinct collapsed widget, separate from the answer", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-thinking",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [
          { item_kind: "text", kind: "thinking", text: "secret reasoning" },
          { item_kind: "text", kind: "text", text: "Final answer" },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    // Both render, via distinct containers.
    const thinking = screen.getByTestId("turn-thinking");
    expect(thinking).toBeInTheDocument();
    expect(screen.getByText("Final answer")).toBeInTheDocument();

    // Collapsed by default.
    expect((thinking as HTMLDetailsElement).open).toBe(false);

    // Reasoning lives in the thinking widget's body, not the answer container.
    expect(screen.getByTestId("thinking-body").textContent).toContain("secret reasoning");
    expect(thinking.textContent).not.toContain("Final answer");
    // The body renders through the muted Markdown variant so opened reasoning
    // reads as subordinate (the bare `.markdown-body` color matches the answer).
    expect(
      screen.getByTestId("thinking-body").querySelector(".markdown-body.markdown-thinking"),
    ).not.toBeNull();
  });

  it("excludes reasoning from the copied answer text", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-copy-thinking",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        status: "complete",
        items: [
          { item_kind: "text", kind: "thinking", text: "secret reasoning" },
          { item_kind: "text", kind: "text", text: "Answer text" },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });
    copyTextMock.mockClear();

    const turn = screen.getByTestId("turn");
    const copy = turn.querySelector('[data-testid="message-copy"]');
    if (!copy) throw new Error("expected a copy button on the agent message");
    await fireEvent.click(copy);

    // Only the answer is copied; the reasoning is omitted.
    expect(copyTextMock).toHaveBeenCalledWith("Answer text");
  });

  it("shows a timestamp (titled with the ISO start) on each message", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T08:30:00Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "done" }],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const time = screen.getByTestId("turn").querySelector('[data-testid="message-time"]');
    if (!time) throw new Error("expected a timestamp on the message");
    expect(time).toHaveAttribute("title", "2026-05-16T08:30:00Z");
    expect(time.textContent?.trim()).not.toBe("");
  });

  it("shows no copy button on a tool-only agent turn", async () => {
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
          {
            item_kind: "tool",
            tool_use_id: "tool-1",
            kind: "builtin",
            name: "read_file",
            input: { file_path: "x" },
            output: "",
            is_error: false,
            started_at: "2026-05-16T00:00:01Z",
            completed_at: "2026-05-16T00:00:02Z",
          },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("turn").querySelector('[data-testid="message-copy"]')).toBeNull();
  });

  it("applies last-block copy mode to fan-out columns", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-claude",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "fan out",
      },
      {
        role: "agent",
        turn_id: "agent-claude",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [
          { item_kind: "text", kind: "text", text: "first block" },
          { item_kind: "text", kind: "text", text: "final block" },
        ],
      },
    ];
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-codex",
        agent_id: CODEX_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "fan out",
      },
      {
        role: "agent",
        turn_id: "agent-codex",
        agent_id: CODEX_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "codex final" }],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });
    copyTextMock.mockClear();

    const firstColumn = screen.getAllByTestId("fanout-column")[0]!;
    const copy = firstColumn.querySelector('[data-testid="message-copy"]');
    if (!copy) throw new Error("expected a copy button on the fan-out column");
    await fireEvent.click(copy);

    expect(copyTextMock).toHaveBeenCalledWith("final block");
  });
});

describe("UnifiedTranscript — per-message cost + overage", () => {
  it("renders cost and the using-credits marker on an overage Claude turn", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "done" }],
        usage: { input_tokens: 10, output_tokens: 5, total_cost_usd: 0.0125 },
        spend: { real_spend: true, is_overage: true, overage_resets_at: null },
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("message-cost")).toHaveTextContent("$0.0125");
    expect(screen.getByTestId("message-overage")).toBeInTheDocument();
  });

  it("renders neither cost nor marker on a normal-quota Claude turn", async () => {
    // Cost is present in usage but spend.real_spend is false (notional cost,
    // not actual spend) → the message shows nothing cost-related.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "done" }],
        usage: { input_tokens: 10, output_tokens: 5, total_cost_usd: 0.0125 },
        spend: { real_spend: false, is_overage: false },
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("message-cost")).toBeNull();
    expect(screen.queryByTestId("message-overage")).toBeNull();
  });

  it("renders no cost on a Codex turn (no spend signal)", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "done" }],
        usage: { input_tokens: 10, output_tokens: 5 },
        // No `spend` — Codex carries no real-spend attribution.
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    expect(screen.queryByTestId("message-cost")).toBeNull();
    expect(screen.queryByTestId("message-overage")).toBeNull();
  });
});

describe("UnifiedTranscript hydration failures", () => {
  it("renders the project-load-failed block with Retry + Details and the verbatim error", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const onRetryLoad = vi.fn();

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, {
      props: {
        agents: [CLAUDE_AGENT],
        loadStatus: "failed",
        loadError: "journal read failed at /work/journal.jsonl",
        onRetryLoad,
      },
    });

    expect(screen.getByTestId("transcript-load-failed")).toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("transcript-load-failed-retry"));
    expect(onRetryLoad).toHaveBeenCalledTimes(1);

    // Details opens the dialog with the exact error; Copy is wired.
    await fireEvent.click(screen.getByTestId("transcript-load-failed-details"));
    await tick();
    expect(screen.getByTestId("error-details-text")).toHaveTextContent(
      "journal read failed at /work/journal.jsonl",
    );
    await fireEvent.click(screen.getByTestId("error-details-copy"));
    expect(copyTextMock).toHaveBeenCalledWith("journal read failed at /work/journal.jsonl");
  });

  it("suppresses the empty-state message when the project load failed", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, {
      props: { agents: [CLAUDE_AGENT], loadStatus: "failed", loadError: "boom" },
    });

    expect(screen.getByTestId("transcript-load-failed")).toBeInTheDocument();
    // No contradictory "this project is empty" copy beneath the failure.
    expect(screen.queryByText(/no messages yet/i)).toBeNull();
  });

  it("suppresses the empty-state message when an agent's history failed", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const rt = state.runtimes[CLAUDE_AGENT.id];
    if (rt === undefined) throw new Error("runtime missing");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...rt,
      hydration_status: "failed",
      hydration_error: "boom",
    };

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("agent-hydration-failed")).toBeInTheDocument();
    expect(screen.queryByText(/no messages yet/i)).toBeNull();
  });

  it("renders a per-agent failure banner naming the agent, with Details showing the verbatim error", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const rt = state.runtimes[CLAUDE_AGENT.id];
    if (rt === undefined) throw new Error("runtime missing");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...rt,
      hydration_status: "failed",
      hydration_error: "I/O error reading session file /x.jsonl: permission denied",
    };

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    const banner = screen.getByTestId("agent-hydration-failed");
    expect(banner).toHaveTextContent("Couldn't load alice's history");

    await fireEvent.click(screen.getByTestId("agent-hydration-failed-details"));
    await tick();
    expect(screen.getByTestId("error-details-text")).toHaveTextContent(
      "I/O error reading session file /x.jsonl: permission denied",
    );
  });

  it("Retry on a per-agent banner re-invokes load_transcript and clears the banner on success", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const rt = state.runtimes[CLAUDE_AGENT.id];
    if (rt === undefined) throw new Error("runtime missing");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...rt,
      hydration_status: "failed",
      hydration_error: "permission denied",
    };

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });
    expect(screen.getByTestId("agent-hydration-failed")).toBeInTheDocument();

    // Retry re-runs hydration; stage a successful load.
    invokeMock.mockResolvedValueOnce({
      turns: [],
      meta: null,
      last_rate_limit: null,
      warnings: [],
    });
    await fireEvent.click(screen.getByTestId("agent-hydration-failed-retry"));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("load_transcript", { agentId: CLAUDE_AGENT.id }),
    );
    await waitFor(() => expect(screen.queryByTestId("agent-hydration-failed")).toBeNull());
    expect(state.runtimes[CLAUDE_AGENT.id]?.hydration_status).toBe("complete");
    expect(state.runtimes[CLAUDE_AGENT.id]?.hydration_error).toBeUndefined();
  });

  it("keeps the per-agent banner when Retry fails again", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const rt = state.runtimes[CLAUDE_AGENT.id];
    if (rt === undefined) throw new Error("runtime missing");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...rt,
      hydration_status: "failed",
      hydration_error: "first error",
    };

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    invokeMock.mockRejectedValueOnce(new Error("still broken"));
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await fireEvent.click(screen.getByTestId("agent-hydration-failed-retry"));

    await waitFor(() =>
      expect(state.runtimes[CLAUDE_AGENT.id]?.hydration_error).toBe("still broken"),
    );
    expect(screen.getByTestId("agent-hydration-failed")).toBeInTheDocument();
    warnSpy.mockRestore();
  });

  // --- Per-turn model/effort footer (history, independent of the sidebar) -----

  it("agent-turn footer renders the turn's model and effort", async () => {
    const state = await loadState();
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "hi" }],
        model: "gpt-5.5",
        effort: "high",
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    expect(screen.getByTestId("message-model")).toHaveTextContent("gpt-5.5");
    expect(screen.getByTestId("message-effort")).toHaveTextContent("high");
  });

  it("agent-turn footer shows model with no effort when the turn has only a model", async () => {
    const state = await loadState();
    // The Claude case: per-turn model is exposed, per-turn effort is not.
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "hi" }],
        model: "claude-opus-4-8",
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("message-model")).toHaveTextContent("claude-opus-4-8");
    expect(screen.queryByTestId("message-effort")).toBeNull();
  });

  it("agent-turn footer omits model/effort when the turn carries neither", async () => {
    const state = await loadState();
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "hi" }],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("turn")).toBeInTheDocument();
    expect(screen.queryByTestId("message-model")).toBeNull();
    expect(screen.queryByTestId("message-effort")).toBeNull();
  });
});

describe("UnifiedTranscript — attachments", () => {
  it("renders an image thumbnail and a file chip under a user message, never a raw path", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    const imgPath = "/proj/.switchboard/projects/p/attachments/uuid__diagram.png";
    const filePath = "/proj/.switchboard/projects/p/attachments/uuid__data.bin";
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        text: "compare these",
        attachments: [
          { label: "image-1", kind: "image", path: imgPath, original_name: "diagram.png" },
          { label: "file-1", kind: "file", path: filePath, original_name: "data.bin" },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { agents: [CODEX_AGENT] } });

    const thumb = (await screen.findByTestId("attachment-thumb-image-1")) as HTMLImageElement;
    // The thumbnail uses convertFileSrc (asset:// URL), not the raw filesystem path.
    expect(thumb.getAttribute("src")).toContain("asset://");
    expect(thumb.getAttribute("src")).not.toBe(imgPath);
    expect(screen.getByTestId("attachment-file-file-1")).toHaveTextContent("data.bin");

    // The user bubble shows the prose + display names, never the staged paths.
    const turn = screen.getByTestId("turn");
    expect(turn.textContent).not.toContain(imgPath);
    expect(turn.textContent).not.toContain(filePath);
  });
});
