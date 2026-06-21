import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import type { AgentRecord, ConversationItem, NormalizedEvent } from "$lib/types";
import { HEARTBEAT_TIMEOUT_MS } from "$lib/types";
import { agentCopy } from "$lib/agentCopy.svelte";
// Static import so the component-tree transform happens at module collection,
// not inside the first test's timeout (cold CI transforms have no vite cache).
// `vi.mock` is hoisted above imports, so the mocks below still apply.
import UnifiedTranscript from "./UnifiedTranscript.svelte";
import {
  setProjectCompact,
  toggleKey,
  _testing as previewState,
} from "$lib/state/transcriptPreview.svelte";
import type { Turn } from "$lib/state/index.svelte";

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

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";

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
  // Default the suite to expanded rendering so behavioral tests state the mode
  // they exercise. Compact mode is on by default in the app, so the
  // compact/fan-out-control describes opt back into it explicitly.
  setProjectCompact(PROJECT_ID, false);
});

const SEND_1 = "00000000-0000-7000-8000-0000000000d1";

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
  previewState.reset();
});

describe("UnifiedTranscript", () => {
  it("renders empty-state message when no turns exist", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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

      render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("turn-error")).toHaveTextContent("rate limited");
  });
});

describe("UnifiedTranscript — fan-out groups", () => {
  function seedFanout(
    state: Awaited<ReturnType<typeof loadState>>,
    aliceResponse: {
      status: "streaming" | "complete" | "failed";
      text?: string;
      model?: string;
      effort?: string;
    } | null,
    bobResponse: {
      status: "streaming" | "complete" | "failed";
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

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

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

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

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

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

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

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

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

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    expect(screen.queryByTestId("fanout-card-cancel")).toBeNull();
  });

  // A turn cancelled after it produced output reopens with its partial content
  // and, for Claude, a `streaming` disk status (no terminal stop_reason). The
  // journal's cancelled Outcome marker is authoritative: the column reads
  // `cancelled` and shows none of the live affordances a `streaming` turn would
  // otherwise render (no "Working…", no live cancel button).
  it("reads a reopened cancelled-mid-turn column as cancelled with no live affordances", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // Alice cancelled after partial output (disk `streaming`); bob completed.
    seedFanout(
      state,
      { status: "streaming", text: "partial work" },
      { status: "complete", text: "bob reply" },
    );

    render(UnifiedTranscript, {
      props: {
        projectId: PROJECT_ID,
        agents: [CLAUDE_AGENT, CODEX_AGENT],
        overlay: [
          {
            kind: "outcome",
            turn_id: "o-alice",
            send_id: SEND_1,
            agent_id: CLAUDE_AGENT.id,
            status: "cancelled",
            reason: null,
            at: "2026-05-16T00:00:05Z",
          },
        ],
      },
    });

    const columns = screen.getAllByTestId("fanout-column");
    expect(columns[0]).toHaveAttribute("data-agent-id", CLAUDE_AGENT.id);
    expect(columns[0]).toHaveAttribute("data-state", "cancelled");
    // Partial content is preserved (reopen mirrors live), labelled cancelled…
    expect(columns[0]).toHaveTextContent("partial work");
    expect(screen.getByTestId("outcome-cancelled")).toBeInTheDocument();
    // …and the dead turn shows no live spinner or live cancel control.
    expect(screen.queryByTestId("turn-working")).toBeNull();
    expect(screen.queryByTestId("turn-live-control")).toBeNull();
    expect(screen.queryByTestId("fanout-card-cancel")).toBeNull();
  });

  it("renders a collapsed column's status chip last — after the indicator and body", async () => {
    // Consistency with the expanded view: the terminal status (cancelled/failed)
    // belongs on the LAST line, after the `n tool calls` indicator and the body —
    // not stacked on top of the indicator (the prior collapsed-only order).
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-alice",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "fan out",
      },
      {
        role: "agent",
        turn_id: "a-alice",
        agent_id: CLAUDE_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:01Z",
        status: "streaming", // cancelled-mid on disk; the marker below is authoritative
        items: [
          {
            item_kind: "tool",
            tool_use_id: "t1",
            kind: "builtin",
            name: "Bash",
            input: { command: "x" },
            started_at: "2026-05-16T00:00:01Z",
            completed_at: "2026-05-16T00:00:02Z",
            output: "ok",
          },
          { item_kind: "text", kind: "text", text: "partial work" },
        ],
      },
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
      {
        role: "agent",
        turn_id: "a-bob",
        agent_id: CODEX_AGENT.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:02Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "bob reply" }],
      },
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, {
      props: {
        projectId: PROJECT_ID,
        agents: [CLAUDE_AGENT, CODEX_AGENT],
        overlay: [
          {
            kind: "outcome",
            turn_id: "o-alice",
            send_id: SEND_1,
            agent_id: CLAUDE_AGENT.id,
            status: "cancelled",
            reason: null,
            at: "2026-05-16T00:00:05Z",
          },
        ],
      },
    });

    const column = screen.getAllByTestId("fanout-column")[0]!;
    await fireEvent.click(within(column).getByTestId("turn-preview-toggle"));
    const indicator = within(column).getByTestId("hidden-items-indicator");
    const chip = within(column).getByTestId("outcome-cancelled");
    // The chip follows the indicator in document order (it's last, not first).
    expect(indicator.compareDocumentPosition(chip) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // …and it follows the body text too.
    const body = within(column).getByText("partial work");
    expect(body.compareDocumentPosition(chip) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  // Codex/Gemini/Antigravity persist a cancelled-mid turn as `failed`, not
  // `streaming`. The cancelled marker still wins, so the column reads cancelled
  // rather than the mislabelled "failed" the bare disk status would give.
  it("reads a reopened cancelled column cancelled even when the disk turn is failed", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // The cancelled-mid recipient is the Codex agent — Codex (like Gemini /
    // Antigravity) persists a killed turn as `failed`, so this is the harness
    // shape where the marker must override a `failed` disk status.
    seedFanout(
      state,
      { status: "complete", text: "alice reply" },
      { status: "failed", text: "partial work" },
    );

    render(UnifiedTranscript, {
      props: {
        projectId: PROJECT_ID,
        agents: [CLAUDE_AGENT, CODEX_AGENT],
        overlay: [
          {
            kind: "outcome",
            turn_id: "o-bob",
            send_id: SEND_1,
            agent_id: CODEX_AGENT.id,
            status: "cancelled",
            reason: null,
            at: "2026-05-16T00:00:05Z",
          },
        ],
      },
    });

    const columns = screen.getAllByTestId("fanout-column");
    expect(columns[1]).toHaveAttribute("data-agent-id", CODEX_AGENT.id);
    expect(columns[1]).toHaveAttribute("data-state", "cancelled");
    expect(screen.getByTestId("outcome-cancelled")).toBeInTheDocument();
    expect(screen.queryByTestId("outcome-failed")).toBeNull();
    // The marker is authoritative: the turn's own "failed" status chip is
    // suppressed so it doesn't contradict the "cancelled" marker.
    expect(columns[1]).not.toHaveTextContent(/failed/i);
  });

  // A genuinely failed turn keeps its failed marker and its reason — the marker
  // is authoritative but not suppressed, so failure detail still surfaces.
  it("preserves a failed marker's reason on a reopened failed-mid-turn column", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    seedFanout(
      state,
      { status: "failed", text: "partial work" },
      { status: "complete", text: "bob reply" },
    );

    render(UnifiedTranscript, {
      props: {
        projectId: PROJECT_ID,
        agents: [CLAUDE_AGENT, CODEX_AGENT],
        overlay: [
          {
            kind: "outcome",
            turn_id: "o-alice",
            send_id: SEND_1,
            agent_id: CLAUDE_AGENT.id,
            status: "failed",
            reason: "process exited 1",
            at: "2026-05-16T00:00:05Z",
          },
        ],
      },
    });

    const columns = screen.getAllByTestId("fanout-column");
    expect(columns[0]).toHaveAttribute("data-state", "failed");
    expect(screen.getByTestId("outcome-failed")).toBeInTheDocument();
    expect(columns[0]).toHaveTextContent("process exited 1");
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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("fanout-group")).toBeNull();
    expect(screen.getAllByTestId("turn")).toHaveLength(2);
  });
});

describe("UnifiedTranscript — standalone cancelled turns", () => {
  // The single-recipient (non-fan-out) analog of the fan-out column fixes: a
  // cancelled-mid turn reopens as a standalone agent row plus a sibling
  // cancelled outcome marker. The marker is authoritative, so the dead turn must
  // not show the live "Working…" footer (Claude persists cancelled-mid as
  // `streaming`) and, for harnesses that persist it as `failed`, must not show a
  // contradictory `failed` chip beside the `cancelled` marker.
  function seedSolo(
    state: Awaited<ReturnType<typeof loadState>>,
    agent: AgentRecord,
    status: "streaming" | "failed",
  ): void {
    state.transcripts[agent.id] = [
      {
        role: "user",
        turn_id: "u-1",
        agent_id: agent.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:00Z",
        text: "do it",
      },
      {
        role: "agent",
        turn_id: "a-1",
        agent_id: agent.id,
        send_id: SEND_1,
        started_at: "2026-05-16T00:00:01Z",
        status,
        items: [{ item_kind: "text", kind: "text", text: "partial work" }],
      },
    ];
  }

  function cancelledOverlay(agent: AgentRecord): ConversationItem[] {
    return [
      {
        kind: "outcome",
        turn_id: "o-1",
        send_id: SEND_1,
        agent_id: agent.id,
        status: "cancelled",
        reason: null,
        at: "2026-05-16T00:00:05Z",
      },
    ];
  }

  it("shows no phantom live footer on a reopened streaming-on-disk cancelled turn", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedSolo(state, CLAUDE_AGENT, "streaming");

    render(UnifiedTranscript, {
      props: {
        projectId: PROJECT_ID,
        agents: [CLAUDE_AGENT],
        overlay: cancelledOverlay(CLAUDE_AGENT),
      },
    });

    expect(screen.queryByTestId("fanout-group")).toBeNull();
    expect(screen.getByText("partial work")).toBeInTheDocument();
    expect(screen.getByTestId("outcome-cancelled")).toBeInTheDocument();
    // The dead turn shows no live spinner and no live cancel control.
    expect(screen.queryByTestId("turn-working")).toBeNull();
    expect(screen.queryByTestId("turn-live-control")).toBeNull();
  });

  it("shows no contradictory failed chip on a reopened failed-on-disk cancelled turn", async () => {
    const state = await loadState();
    // CODEX_AGENT stands in for a harness that persists a killed turn as `failed`.
    await state.registerAgent(CODEX_AGENT);
    seedSolo(state, CODEX_AGENT, "failed");

    render(UnifiedTranscript, {
      props: {
        projectId: PROJECT_ID,
        agents: [CODEX_AGENT],
        overlay: cancelledOverlay(CODEX_AGENT),
      },
    });

    expect(screen.getByTestId("outcome-cancelled")).toBeInTheDocument();
    expect(screen.queryByTestId("outcome-failed")).toBeNull();
    // The agent row's own `failed` chip is suppressed — only the marker's
    // `cancelled` badge remains, no double/contradictory status.
    const agentTurn = screen
      .getAllByTestId("turn")
      .find((el) => el.getAttribute("data-role") === "agent");
    expect(agentTurn).toBeDefined();
    expect(agentTurn).not.toHaveTextContent(/failed/i);
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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const columns = screen.getAllByTestId("fanout-column");
    expect(columns[0]!.querySelector("strong")).toHaveTextContent("alice");
    expect(columns[1]!.querySelector("strong")).toHaveTextContent("bob");
  });

  it("renders streaming markdown without throwing on an intermediate partial fence", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

  it("holds the user's position when scrolled up instead of yanking to the bottom", async () => {
    // Same jsdom-stub approach: a content change while the user is scrolled up
    // should keep their distance from the bottom constant (no jump), rather than
    // re-pinning to the bottom.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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
      text: "first",
    });
    // Let the initial pin settle before simulating a user scroll.
    await waitFor(() => expect(container.scrollTop).toBe(1000));

    // User scrolls up: 200px from the bottom (1000 - 300 - 500), so not pinned.
    // A bare scroll event with UNCHANGED height is user-initiated — this is the
    // scrollbar-drag / keyboard case (no wheel or touch involved).
    container.scrollTop = 300;
    await fireEvent.scroll(container);

    // More content arrives and the document grows by 200px.
    Object.defineProperty(container, "scrollHeight", { configurable: true, value: 1200 });
    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "content_chunk",
      turn_id: "turn-1",
      kind: "text",
      text: " more arrives while reading history",
    });

    // Position held at 200px from the (new) bottom: 1200 - 500 - 200 = 500.
    await waitFor(() => {
      expect(container.scrollTop).toBe(500);
    });
  });

  it("does not unpin on a layout-induced scroll (content height changed)", async () => {
    // The browser clamps scrollTop (and fires `scroll`) when content shrinks on
    // collapse — that must NOT be read as the user scrolling away, or the next
    // re-anchor jumps to a stale position. A `scroll` whose height differs from
    // the last anchor is layout-induced and leaves `pinned` alone.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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
      text: "x",
    });
    await waitFor(() => expect(container.scrollTop).toBe(1000)); // pinned, lastScrollHeight=1000

    // A content change leaves scrollTop far from the bottom AND fires `scroll`
    // with a different height — the collapse-clamp shape.
    Object.defineProperty(container, "scrollHeight", { configurable: true, value: 2000 });
    container.scrollTop = 100; // gap of 2000 - 100 - 500 = 1400, well past the 32px threshold
    await fireEvent.scroll(container); // height (2000) !== lastScrollHeight (1000) → must not unpin

    // Still pinned: the next content change re-pins to the bottom (2000), not the
    // preserve-distance position (2000 - 500 - 1400 = 100) it would land on if unpinned.
    fireTo(`agent:${CLAUDE_AGENT.id}`, {
      type: "content_chunk",
      turn_id: "turn-1",
      kind: "text",
      text: "y",
    });
    await waitFor(() => expect(container.scrollTop).toBe(2000));
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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });
    copyTextMock.mockClear();

    const firstColumn = screen.getAllByTestId("fanout-column")[0]!;
    const copy = firstColumn.querySelector('[data-testid="message-copy"]');
    if (!copy) throw new Error("expected a copy button on the fan-out column");
    await fireEvent.click(copy);

    expect(copyTextMock).toHaveBeenCalledWith("final block");
  });

  it("copies all fan-out responses with agent labels", async () => {
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
          { item_kind: "text", kind: "text", text: "alice final" },
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
        started_at: "2026-05-16T00:00:02Z",
        status: "complete",
        items: [
          { item_kind: "text", kind: "thinking", text: "private reasoning" },
          { item_kind: "text", kind: "text", text: "bob final" },
        ],
      },
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });
    copyTextMock.mockClear();

    await fireEvent.click(screen.getByTestId("fanout-copy"));

    expect(copyTextMock).toHaveBeenCalledWith(
      'alice:\n\n"""\nalice final\n"""\n\nbob:\n\n"""\nbob final\n"""',
    );
  });

  it("omits queued recipients from fan-out copy", async () => {
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
        items: [{ item_kind: "text", kind: "text", text: "alice only" }],
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
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });
    copyTextMock.mockClear();

    await fireEvent.click(screen.getByTestId("fanout-copy"));

    expect(copyTextMock).toHaveBeenCalledWith('alice:\n\n"""\nalice only\n"""');
  });

  it("hides fan-out copy when no recipient has answer prose", async () => {
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
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    expect(screen.queryByTestId("fanout-copy")).toBeNull();
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
        items: [
          { item_kind: "text", kind: "text", text: "done" },
          {
            item_kind: "tool",
            tool_use_id: "tool-1",
            kind: "builtin",
            name: "Read",
            input: {},
            output: "ok",
            is_error: false,
            started_at: "2026-05-16T00:00:01Z",
            completed_at: "2026-05-16T00:00:02Z",
          },
        ],
        usage: { input_tokens: 10, output_tokens: 5, total_cost_usd: 0.0125 },
        spend: { real_spend: true, is_overage: true, overage_resets_at: null },
      },
    ];

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("message-cost")).toHaveTextContent("$0.0125");
    const overage = screen.getByTestId("message-overage");
    const toggle = screen.getByTestId("turn-preview-toggle");
    expect(overage).toBeInTheDocument();
    expect(overage.compareDocumentPosition(toggle) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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
        projectId: PROJECT_ID,
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
      props: {
        projectId: PROJECT_ID,
        agents: [CLAUDE_AGENT],
        loadStatus: "failed",
        loadError: "boom",
      },
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
    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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
    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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
    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
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
    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

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

describe("UnifiedTranscript compact mode", () => {
  type AgentTurn = Extract<Turn, { role: "agent" }>;
  type Item = AgentTurn["items"][number];

  const ANSWER = (text: string): Item => ({ item_kind: "text", kind: "text", text });
  const THINK = (text: string): Item => ({ item_kind: "text", kind: "thinking", text });
  const TOOL = (id: string): Item => ({
    item_kind: "tool",
    tool_use_id: id,
    kind: "builtin",
    name: "bash",
    input: {},
    output: "ok",
    is_error: false,
    started_at: "2026-05-16T00:00:01Z",
    completed_at: "2026-05-16T00:00:02Z",
  });

  function user(agent: AgentRecord, sendId: string, turnId: string, at: string): Turn {
    return {
      role: "user",
      turn_id: turnId,
      agent_id: agent.id,
      send_id: sendId,
      started_at: at,
      text: "prompt",
    };
  }
  function done(
    agent: AgentRecord,
    sendId: string,
    turnId: string,
    startedAt: string,
    endedAt: string,
    items: Item[],
  ): Turn {
    return {
      role: "agent",
      turn_id: turnId,
      agent_id: agent.id,
      send_id: sendId,
      started_at: startedAt,
      ended_at: endedAt,
      status: "complete",
      items,
    };
  }

  /// aria-label of the preview toggle inside an element ("Expand" when the unit
  /// is compact, "Collapse" when expanded), or null when there is no toggle.
  function toggleLabel(el: HTMLElement): string | null {
    return (
      el.querySelector('[data-testid="turn-preview-toggle"]')?.getAttribute("aria-label") ?? null
    );
  }
  function agentTurns(): HTMLElement[] {
    return screen.getAllByTestId("turn").filter((el) => el.getAttribute("data-role") === "agent");
  }

  it("renders an agent's latest response as full answer prose and an older one clipped", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        ANSWER("old first"),
        TOOL("t1"),
        ANSWER("old last"),
      ]),
      user(CLAUDE_AGENT, "send-b", "u-b", "2026-05-16T00:00:10Z"),
      done(CLAUDE_AGENT, "send-b", "a-b", "2026-05-16T00:00:11Z", "2026-05-16T00:00:12Z", [
        ANSWER("new first"),
        TOOL("t2"),
        ANSWER("new last"),
      ]),
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const [older, newest] = agentTurns();
    // Latest → expanded by default, including all answer prose and tool widgets.
    expect(within(newest!).getByText("new first")).toBeInTheDocument();
    expect(within(newest!).getByText("new last")).toBeInTheDocument();
    expect(within(newest!).getByTestId("tool-done")).toBeInTheDocument();
    // Older → clipped preview: all answer text, tools hidden.
    expect(within(older!).getByText("old first")).toBeInTheDocument();
    expect(within(older!).getByText("old last")).toBeInTheDocument();
    expect(within(older!).queryByTestId("tool-done")).toBeNull();
    // Older is collapsed by default; latest is expanded and can be collapsed.
    expect(toggleLabel(older!)).toBe("Expand");
    expect(toggleLabel(newest!)).toBe("Collapse");
  });

  it("keeps each agent's most-recent response full even when another agent replied later", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // CLAUDE's only response is globally older than CODEX's, but it is still
    // CLAUDE's latest → it gets the latest-response treatment, not a clipped preview.
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-1", "u-1", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-1", "a-1", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        ANSWER("claude first"),
        ANSWER("claude last"),
      ]),
    ];
    state.transcripts[CODEX_AGENT.id] = [
      user(CODEX_AGENT, "send-2", "u-2", "2026-05-16T00:00:09Z"),
      done(CODEX_AGENT, "send-2", "a-2", "2026-05-16T00:00:10Z", "2026-05-16T00:00:11Z", [
        ANSWER("codex first"),
        ANSWER("codex last"),
      ]),
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const [claudeTurn, codexTurn] = agentTurns();
    expect(within(claudeTurn!).getByText("claude first")).toBeInTheDocument();
    expect(within(claudeTurn!).getByText("claude last")).toBeInTheDocument();
    expect(within(codexTurn!).getByText("codex first")).toBeInTheDocument();
    expect(within(codexTurn!).getByText("codex last")).toBeInTheDocument();
  });

  it("picks each agent's latest response by completion recency, not rendered order", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    // send-a is anchored earlier (renders first) but finishes LAST (ended t5).
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:05Z", [
        ANSWER("a first"),
        ANSWER("a last"),
      ]),
      user(CLAUDE_AGENT, "send-b", "u-b", "2026-05-16T00:00:02Z"),
      done(CLAUDE_AGENT, "send-b", "a-b", "2026-05-16T00:00:03Z", "2026-05-16T00:00:04Z", [
        ANSWER("b first"),
        ANSWER("b last"),
      ]),
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const [first, second] = agentTurns();
    // send-a is the latest by recency → full answer prose.
    expect(within(first!).getByText("a first")).toBeInTheDocument();
    expect(within(first!).getByText("a last")).toBeInTheDocument();
    // send-b is older → clipped preview (all its answer text).
    expect(within(second!).getByText("b first")).toBeInTheDocument();
    expect(within(second!).getByText("b last")).toBeInTheDocument();
  });

  it("renders each latest fan-out column as full answer prose", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // Earlier standalone send, then a later fan-out (each agent's latest).
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        ANSWER("solo first"),
        ANSWER("solo last"),
      ]),
      user(CLAUDE_AGENT, "send-b", "u-bc", "2026-05-16T00:00:10Z"),
      done(CLAUDE_AGENT, "send-b", "a-bc", "2026-05-16T00:00:11Z", "2026-05-16T00:00:13Z", [
        ANSWER("alice first"),
        ANSWER("alice last"),
      ]),
    ];
    state.transcripts[CODEX_AGENT.id] = [
      user(CODEX_AGENT, "send-b", "u-bx", "2026-05-16T00:00:10Z"),
      done(CODEX_AGENT, "send-b", "a-bx", "2026-05-16T00:00:11Z", "2026-05-16T00:00:12Z", [
        ANSWER("bob first"),
        ANSWER("bob last"),
      ]),
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const [aliceCol, bobCol] = screen.getAllByTestId("fanout-column");
    expect(within(aliceCol!).getByText("alice first")).toBeInTheDocument();
    expect(within(aliceCol!).getByText("alice last")).toBeInTheDocument();
    expect(within(bobCol!).getByText("bob first")).toBeInTheDocument();
    expect(within(bobCol!).getByText("bob last")).toBeInTheDocument();
    // Older standalone → clipped preview (all answer text).
    const solo = agentTurns()[0];
    expect(within(solo!).getByText("solo first")).toBeInTheDocument();
    expect(within(solo!).getByText("solo last")).toBeInTheDocument();
  });

  it("lets a manual toggle expand only that unit", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    // Each response has a tool call, so each has something to expand.
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        TOOL("ta"),
        ANSWER("A"),
      ]),
      user(CLAUDE_AGENT, "send-b", "u-b", "2026-05-16T00:00:10Z"),
      done(CLAUDE_AGENT, "send-b", "a-b", "2026-05-16T00:00:11Z", "2026-05-16T00:00:12Z", [
        TOOL("tb"),
        ANSWER("B"),
      ]),
      user(CLAUDE_AGENT, "send-c", "u-c", "2026-05-16T00:00:20Z"),
      done(CLAUDE_AGENT, "send-c", "a-c", "2026-05-16T00:00:21Z", "2026-05-16T00:00:22Z", [
        TOOL("tc"),
        ANSWER("C"),
      ]),
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const [a, b, c] = agentTurns();
    // Older responses are collapsed by default; the latest response is expanded.
    expect(toggleLabel(a!)).toBe("Expand");
    expect(toggleLabel(b!)).toBe("Expand");
    expect(toggleLabel(c!)).toBe("Collapse");
    await fireEvent.click(a!.querySelector('[data-testid="turn-preview-toggle"]')!);
    expect(toggleLabel(a!)).toBe("Collapse"); // now expanded
    expect(toggleLabel(b!)).toBe("Expand"); // unchanged
    expect(toggleLabel(c!)).toBe("Collapse"); // unchanged
  });

  it("renders no compact toggle on a queued row", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.dispatchUserTurn(CLAUDE_AGENT.id, "u-q", "later", [], "send-q", "2026-05-16T00:00:00Z");
    state.recordSendAccepted(CLAUDE_AGENT.id, "u-q", "msg-q");
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const queuedRow = agentTurns().find((el) => el.querySelector('[data-testid="turn-queued"]'));
    expect(queuedRow).toBeDefined();
    expect(toggleLabel(queuedRow!)).toBeNull();
  });

  it("renders no compact toggle on an outcome-only row", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    setProjectCompact(PROJECT_ID, true);

    const overlay: ConversationItem[] = [
      {
        kind: "outcome",
        turn_id: "o-1",
        send_id: "send-a",
        agent_id: CLAUDE_AGENT.id,
        status: "failed",
        reason: "boom",
        at: "2026-05-16T00:00:05Z",
      },
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT], overlay },
    });

    const outcome = screen.getByTestId("turn-outcome");
    expect(toggleLabel(outcome)).toBeNull();
  });

  it("hides tool calls and thinking widgets in a compact completed response", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-1", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        THINK("private reasoning"),
        TOOL("t1"),
        ANSWER("the answer"),
      ]),
    ];
    // Force this single (latest) turn compact via an explicit override.
    toggleKey(PROJECT_ID, "agent:a-1", false);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("turn-thinking")).toBeNull();
    expect(screen.queryByTestId("tool-done")).toBeNull();
    expect(screen.getByText("the answer")).toBeInTheDocument();
    // Copy is unaffected by the compact preview.
    expect(agentTurns()[0]!.querySelector('[data-testid="message-copy"]')).not.toBeNull();
  });

  it("shows a hidden-items indicator with the tool-call count on a compact response", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-1", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        TOOL("t1"),
        TOOL("t2"),
        ANSWER("done"),
      ]),
      user(CLAUDE_AGENT, "send-b", "u-b", "2026-05-16T00:00:10Z"),
      done(CLAUDE_AGENT, "send-b", "a-2", "2026-05-16T00:00:11Z", "2026-05-16T00:00:12Z", [
        ANSWER("newest"),
      ]),
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const indicator = screen.getByTestId("hidden-items-indicator");
    expect(indicator).toHaveTextContent("2 tool calls");
    // Clicking it expands the response, revealing the tools.
    expect(screen.queryByTestId("tool-done")).toBeNull();
    await fireEvent.click(indicator);
    expect(screen.getAllByTestId("tool-done").length).toBe(2);
    expect(screen.queryByTestId("hidden-items-indicator")).toBeNull();
  });

  it("shows the hidden-items indicator on an older clipped response too", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-old", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        TOOL("t1"),
        ANSWER("older answer"),
      ]),
      user(CLAUDE_AGENT, "send-b", "u-b", "2026-05-16T00:00:10Z"),
      done(CLAUDE_AGENT, "send-b", "a-new", "2026-05-16T00:00:11Z", "2026-05-16T00:00:12Z", [
        ANSWER("newest"),
      ]),
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    // The older (clipped) response hid a tool → indicator present with the count.
    const older = agentTurns()[0]!;
    expect(within(older).getByTestId("hidden-items-indicator")).toHaveTextContent("1 tool call");
    // The newest (latest-response view, no tools) hides nothing non-text → no indicator.
    expect(within(agentTurns()[1]!).queryByTestId("hidden-items-indicator")).toBeNull();
  });

  it("shows no toggle when collapsing would hide nothing (short, no tools)", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    // A short user prompt and a single-block agent answer with no tools: the
    // collapsed and expanded views are identical, so neither shows a toggle.
    // (A long message that overflows the clip gets one via DOM measurement,
    // which jsdom can't exercise — verified in the real app.)
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-1", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        ANSWER("hi"),
      ]),
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const userRow = screen
      .getAllByTestId("turn")
      .find((el) => el.getAttribute("data-role") === "user");
    expect(toggleLabel(userRow!)).toBeNull();
    expect(toggleLabel(agentTurns()[0]!)).toBeNull();
    // The content still renders (just without a redundant toggle).
    expect(within(userRow!).getByText("prompt")).toBeInTheDocument();
    expect(screen.getByText("hi")).toBeInTheDocument();
  });

  it("does not give a streaming response a compact preview", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      {
        role: "agent",
        turn_id: "a-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "send-a",
        started_at: "2026-05-16T00:00:01Z",
        status: "streaming",
        items: [ANSWER("streaming…")],
      },
    ];
    setProjectCompact(PROJECT_ID, true);

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const [streaming] = agentTurns();
    expect(toggleLabel(streaming!)).toBeNull(); // no compact toggle while streaming
    expect(screen.getByTestId("turn-working")).toBeInTheDocument(); // live footer stays
  });

  it("offers a per-message toggle even with compact mode off, compacting only that unit", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    setProjectCompact(PROJECT_ID, false);
    state.transcripts[CLAUDE_AGENT.id] = [
      user(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      done(CLAUDE_AGENT, "send-a", "a-1", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z", [
        TOOL("t1"),
        ANSWER("the answer"),
      ]),
    ];
    // Compact mode is OFF (default).
    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const [turn] = agentTurns();
    expect(toggleLabel(turn!)).toBe("Collapse"); // expanded, but a toggle is offered
    expect(screen.getByTestId("tool-done")).toBeInTheDocument();

    await fireEvent.click(turn!.querySelector('[data-testid="turn-preview-toggle"]')!);
    expect(toggleLabel(turn!)).toBe("Expand"); // now compact
    expect(screen.queryByTestId("tool-done")).toBeNull(); // tool suppressed
  });
});

describe("UnifiedTranscript live streaming cap", () => {
  // jsdom has no layout, so the bottom-pin scroll math can't be exercised here
  // (verified in the real-app walkthrough). These cover the DOM contract: the
  // cap wraps streamed content, the live footer sits outside it, and the cap is
  // gone once the turn completes.
  const text = (t: string) => ({ item_kind: "text" as const, kind: "text" as const, text: t });
  function streaming(agent: AgentRecord, sendId: string, turnId: string, body: string): Turn {
    return {
      role: "agent",
      turn_id: turnId,
      agent_id: agent.id,
      send_id: sendId,
      started_at: "2026-05-16T00:00:01Z",
      status: "streaming",
      items: [text(body)],
    };
  }

  it("caps a streaming standalone response and keeps the footer outside the cap", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "send-1",
        started_at: "2026-05-16T00:00:00Z",
        text: "go",
      },
      streaming(CLAUDE_AGENT, "send-1", "a-1", "streaming output"),
    ];

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const scroll = screen.getByTestId("turn-live-scroll");
    expect(scroll).toHaveTextContent("streaming output"); // streamed content is inside the cap
    // The working/cancel footer is rendered as a sibling, never inside the cap.
    expect(scroll.querySelector('[data-testid="turn-working"]')).toBeNull();
    expect(screen.getByTestId("turn-working")).toBeInTheDocument();
  });

  it("caps each streaming fan-out column independently", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-a",
        agent_id: CLAUDE_AGENT.id,
        send_id: "send-f",
        started_at: "2026-05-16T00:00:00Z",
        text: "fan",
      },
      streaming(CLAUDE_AGENT, "send-f", "a-a", "alice streaming"),
    ];
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-b",
        agent_id: CODEX_AGENT.id,
        send_id: "send-f",
        started_at: "2026-05-16T00:00:00Z",
        text: "fan",
      },
      streaming(CODEX_AGENT, "send-f", "a-b", "bob streaming"),
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    expect(screen.getAllByTestId("turn-live-scroll")).toHaveLength(2);
    // Each column's footer stays outside its own cap.
    for (const scroll of screen.getAllByTestId("turn-live-scroll")) {
      expect(scroll.querySelector('[data-testid="turn-working"]')).toBeNull();
    }
    expect(screen.getAllByTestId("turn-working")).toHaveLength(2);
  });

  it("removes the cap once the turn completes, leaving the latest response expanded", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "send-1",
        started_at: "2026-05-16T00:00:00Z",
        text: "go",
      },
      {
        role: "agent",
        turn_id: "a-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "send-1",
        started_at: "2026-05-16T00:00:01Z",
        ended_at: "2026-05-16T00:00:02Z",
        status: "complete",
        items: [text("final answer")],
      },
    ];

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("turn-live-scroll")).toBeNull(); // cap gone
    expect(screen.getByText("final answer")).toBeInTheDocument(); // latest, expanded
    expect(screen.queryByTestId("turn-working")).toBeNull();
  });
});

describe("UnifiedTranscript fan-out group control", () => {
  // These cases lean on compact-on defaults (older groups collapsed, latest set
  // expanded), which is the app default; opt back in after the suite-wide off.
  beforeEach(() => {
    setProjectCompact(PROJECT_ID, true);
  });

  const text = (t: string) => ({ item_kind: "text" as const, kind: "text" as const, text: t });
  const tool = (id: string) => ({
    item_kind: "tool" as const,
    tool_use_id: id,
    kind: "builtin" as const,
    name: "bash",
    input: {},
    output: "ok",
    is_error: false,
    started_at: "2026-05-16T00:00:01Z",
    completed_at: "2026-05-16T00:00:02Z",
  });
  function u(agent: AgentRecord, sendId: string, turnId: string, at: string): Turn {
    return {
      role: "user",
      turn_id: turnId,
      agent_id: agent.id,
      send_id: sendId,
      started_at: at,
      text: "fan",
    };
  }
  // A response with a tool call so it has something to expand → a real toggle.
  function a(
    agent: AgentRecord,
    sendId: string,
    turnId: string,
    started: string,
    ended: string,
  ): Turn {
    return {
      role: "agent",
      turn_id: turnId,
      agent_id: agent.id,
      send_id: sendId,
      started_at: started,
      ended_at: ended,
      status: "complete",
      items: [tool(`${turnId}-t`), text(`${agent.name} reply`)],
    };
  }
  function columnLabels(group: HTMLElement): (string | null)[] {
    return [...group.querySelectorAll('[data-testid="fanout-column"]')].map(
      (col) =>
        col.querySelector('[data-testid="turn-preview-toggle"]')?.getAttribute("aria-label") ??
        null,
    );
  }
  function groupToggle(group: HTMLElement): HTMLElement {
    return group.querySelector('[data-testid="fanout-preview-toggle-all"]')!;
  }

  it("renders the group control with the responses, not inside the user message", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      u(CLAUDE_AGENT, "send-f", "u-a", "2026-05-16T00:00:00Z"),
      a(CLAUDE_AGENT, "send-f", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:03Z"),
    ];
    state.transcripts[CODEX_AGENT.id] = [
      u(CODEX_AGENT, "send-f", "u-b", "2026-05-16T00:00:00Z"),
      a(CODEX_AGENT, "send-f", "a-b", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z"),
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const group = screen.getByTestId("fanout-group");
    const userTurn = within(group)
      .getAllByTestId("turn")
      .find((el) => el.getAttribute("data-role") === "user")!;
    // The toggle-all must not live inside the user message's hover scope (that
    // was leaking the user prompt's meta onto response hover).
    expect(userTurn.querySelector('[data-testid="fanout-preview-toggle-all"]')).toBeNull();
    const footer = within(group).getByTestId("fanout-actions-footer");
    expect(footer).toContainElement(groupToggle(group));
  });

  it("shows a per-column hidden-items indicator and expands the column on click", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // `a()` includes a tool call, so each collapsed column hides a tool → indicator.
    state.transcripts[CLAUDE_AGENT.id] = [
      u(CLAUDE_AGENT, "send-f", "u-a", "2026-05-16T00:00:00Z"),
      a(CLAUDE_AGENT, "send-f", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:03Z"),
    ];
    state.transcripts[CODEX_AGENT.id] = [
      u(CODEX_AGENT, "send-f", "u-b", "2026-05-16T00:00:00Z"),
      a(CODEX_AGENT, "send-f", "a-b", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z"),
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const [aliceCol] = screen.getAllByTestId("fanout-column");
    await fireEvent.click(within(aliceCol!).getByTestId("turn-preview-toggle"));
    const indicator = within(aliceCol!).getByTestId("hidden-items-indicator");
    expect(indicator).toHaveTextContent("1 tool call");
    expect(within(aliceCol!).queryByTestId("tool-done")).toBeNull(); // tool hidden while collapsed
    await fireEvent.click(indicator);
    expect(within(aliceCol!).getByTestId("tool-done")).toBeInTheDocument(); // expanded
  });

  it("expands all columns when none is, and collapses all when any is", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // A single fan-out → both columns start expanded (each agent's latest response).
    state.transcripts[CLAUDE_AGENT.id] = [
      u(CLAUDE_AGENT, "send-f", "u-a", "2026-05-16T00:00:00Z"),
      a(CLAUDE_AGENT, "send-f", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:03Z"),
    ];
    state.transcripts[CODEX_AGENT.id] = [
      u(CODEX_AGENT, "send-f", "u-b", "2026-05-16T00:00:00Z"),
      a(CODEX_AGENT, "send-f", "a-b", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z"),
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const group = screen.getByTestId("fanout-group");
    expect(columnLabels(group)).toEqual(["Collapse", "Collapse"]); // both expanded
    expect(groupToggle(group)).toHaveAttribute("aria-label", "Collapse all responses above");

    await fireEvent.click(groupToggle(group));
    expect(columnLabels(group)).toEqual(["Expand", "Expand"]); // all collapsed
    expect(groupToggle(group)).toHaveAttribute("aria-label", "Expand all responses above");

    await fireEvent.click(groupToggle(group));
    expect(columnLabels(group)).toEqual(["Collapse", "Collapse"]); // all expanded again
  });

  it("affects only its own fan-out group", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    // Two fan-outs, plus a later standalone send so neither fan-out is the
    // latest set → both groups' columns start compact by default.
    state.transcripts[CLAUDE_AGENT.id] = [
      u(CLAUDE_AGENT, "send-a", "u-a", "2026-05-16T00:00:00Z"),
      a(CLAUDE_AGENT, "send-a", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z"),
      u(CLAUDE_AGENT, "send-b", "u-b", "2026-05-16T00:00:10Z"),
      a(CLAUDE_AGENT, "send-b", "a-b", "2026-05-16T00:00:11Z", "2026-05-16T00:00:12Z"),
      {
        role: "user",
        turn_id: "u-z",
        agent_id: CLAUDE_AGENT.id,
        send_id: "send-z",
        started_at: "2026-05-16T00:00:20Z",
        text: "solo",
      },
      {
        role: "agent",
        turn_id: "a-z",
        agent_id: CLAUDE_AGENT.id,
        send_id: "send-z",
        started_at: "2026-05-16T00:00:21Z",
        ended_at: "2026-05-16T00:00:22Z",
        status: "complete",
        items: [text("latest solo")],
      },
    ];
    state.transcripts[CODEX_AGENT.id] = [
      u(CODEX_AGENT, "send-a", "uc-a", "2026-05-16T00:00:00Z"),
      a(CODEX_AGENT, "send-a", "ac-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z"),
      u(CODEX_AGENT, "send-b", "uc-b", "2026-05-16T00:00:10Z"),
      a(CODEX_AGENT, "send-b", "ac-b", "2026-05-16T00:00:11Z", "2026-05-16T00:00:12Z"),
      {
        role: "user",
        turn_id: "uc-z",
        agent_id: CODEX_AGENT.id,
        send_id: "send-z-codex",
        started_at: "2026-05-16T00:00:20Z",
        text: "solo",
      },
      {
        role: "agent",
        turn_id: "ac-z",
        agent_id: CODEX_AGENT.id,
        send_id: "send-z-codex",
        started_at: "2026-05-16T00:00:21Z",
        ended_at: "2026-05-16T00:00:22Z",
        status: "complete",
        items: [text("latest solo codex")],
      },
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const [groupA, groupB] = screen.getAllByTestId("fanout-group");
    expect(columnLabels(groupA!)).toEqual(["Expand", "Expand"]); // both compact
    expect(columnLabels(groupB!)).toEqual(["Expand", "Expand"]);

    await fireEvent.click(groupToggle(groupA!));
    expect(columnLabels(groupA!)).toEqual(["Collapse", "Collapse"]); // group A expanded
    expect(columnLabels(groupB!)).toEqual(["Expand", "Expand"]); // group B untouched
  });

  it("leaves columns individually toggleable after a group toggle", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      u(CLAUDE_AGENT, "send-f", "u-a", "2026-05-16T00:00:00Z"),
      a(CLAUDE_AGENT, "send-f", "a-a", "2026-05-16T00:00:01Z", "2026-05-16T00:00:03Z"),
    ];
    state.transcripts[CODEX_AGENT.id] = [
      u(CODEX_AGENT, "send-f", "u-b", "2026-05-16T00:00:00Z"),
      a(CODEX_AGENT, "send-f", "a-b", "2026-05-16T00:00:01Z", "2026-05-16T00:00:02Z"),
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const group = screen.getByTestId("fanout-group");
    expect(columnLabels(group)).toEqual(["Collapse", "Collapse"]); // both expanded by default
    await fireEvent.click(groupToggle(group)); // collapse both
    expect(columnLabels(group)).toEqual(["Expand", "Expand"]);

    // Expand just the first column individually — the group toggle wrote
    // per-column overrides, so each column is still independently controllable.
    const firstColToggle = group
      .querySelectorAll('[data-testid="fanout-column"]')[0]!
      .querySelector('[data-testid="turn-preview-toggle"]')!;
    await fireEvent.click(firstColToggle);
    expect(columnLabels(group)).toEqual(["Collapse", "Expand"]);
  });
});

describe("UnifiedTranscript terminal-response collapse", () => {
  beforeEach(() => {
    setProjectCompact(PROJECT_ID, true);
  });

  function agentTurnEl(): HTMLElement {
    return screen.getAllByTestId("turn").find((el) => el.getAttribute("data-role") === "agent")!;
  }

  it("gives a cancelled response a compact toggle (it is terminal, with content)", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "s-1",
        started_at: "2026-05-16T00:00:00Z",
        text: "go",
      },
      {
        role: "agent",
        turn_id: "a-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "s-1",
        started_at: "2026-05-16T00:00:01Z",
        ended_at: "2026-05-16T00:00:02Z",
        status: "cancelled",
        items: [
          { item_kind: "text", kind: "text", text: "partial work before cancel" },
          {
            item_kind: "tool",
            tool_use_id: "ct",
            kind: "builtin",
            name: "bash",
            input: {},
            output: "ok",
            is_error: false,
            started_at: "2026-05-16T00:00:01Z",
            completed_at: "2026-05-16T00:00:02Z",
          },
        ],
      },
    ];

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    // Previously a cancelled response had no toggle and never collapsed; now it
    // collapses and (having a hidden tool call) offers a toggle.
    const toggle = agentTurnEl().querySelector('[data-testid="turn-preview-toggle"]');
    expect(toggle).not.toBeNull();
    expect(toggle).toHaveAttribute("aria-label", "Collapse"); // expanded by default (latest response)
    await fireEvent.click(toggle!);
    expect(agentTurnEl().querySelector('[data-testid="turn-preview-toggle"]')).toHaveAttribute(
      "aria-label",
      "Expand",
    ); // now collapsed
  });

  it("collapses a dangling streaming-on-disk turn closed by an outcome marker", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    // The exact dangling case: parser left status `streaming` (model owed a
    // continuation), but an outcome marker closed it → it is terminal, not live.
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "u-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "s-1",
        started_at: "2026-05-16T00:00:00Z",
        text: "go",
      },
      {
        role: "agent",
        turn_id: "a-1",
        agent_id: CLAUDE_AGENT.id,
        send_id: "s-1",
        started_at: "2026-05-16T00:00:01Z",
        status: "streaming",
        items: [
          { item_kind: "text", kind: "text", text: "dangling text" },
          {
            item_kind: "tool",
            tool_use_id: "dt",
            kind: "builtin",
            name: "bash",
            input: {},
            output: "ok",
            is_error: false,
            started_at: "2026-05-16T00:00:01Z",
            completed_at: "2026-05-16T00:00:02Z",
          },
        ],
      },
    ];
    const overlay: ConversationItem[] = [
      {
        kind: "outcome",
        turn_id: "a-1",
        send_id: "s-1",
        agent_id: CLAUDE_AGENT.id,
        status: "cancelled",
        reason: null,
        at: "2026-05-16T00:00:05Z",
      },
    ];

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT], overlay },
    });

    // It must not be treated as live (no working footer / live cap) but must be
    // collapsible (has a toggle) — the bug was neither toggle nor collapse.
    expect(screen.queryByTestId("turn-working")).toBeNull();
    expect(screen.queryByTestId("turn-live-scroll")).toBeNull();
    expect(agentTurnEl().querySelector('[data-testid="turn-preview-toggle"]')).not.toBeNull();
  });
});

describe("UnifiedTranscript loading state", () => {
  it("centers a large spinner with the title while loading with no rows yet", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT], loadStatus: "loading" },
    });

    const loading = screen.getByTestId("transcript-loading");
    expect(loading).toHaveTextContent("Loading history…");
    // The same centered spinner+title look as the project-loading EmptyState.
    expect(loading.querySelector(".animate-spin")).not.toBeNull();
    expect(loading).toHaveClass("items-center", "justify-center");
  });

  it("keeps the small inline note when rows are already on screen", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "user",
        turn_id: "user-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        text: "hi",
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT], loadStatus: "loading" },
    });

    // Visible content must not be covered by a centered takeover.
    const loading = screen.getByTestId("transcript-loading");
    expect(loading.querySelector(".animate-spin")).toBeNull();
    expect(screen.getByText("hi")).toBeInTheDocument();
  });
});

describe("model footer visibility", () => {
  it("shows the model only on completed turns, never on failed/cancelled ones", async () => {
    // Harnesses stamp placeholder models on terminal-failure events (Claude
    // records the literal "<synthetic>") — a non-complete turn's model is a
    // leaked sentinel, not information.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "agent-ok",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [{ item_kind: "text", kind: "text", text: "done" }],
        model: "claude-fable-5",
      },
      {
        role: "agent",
        turn_id: "agent-bad",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:02Z",
        status: "failed",
        items: [{ item_kind: "text", kind: "text", text: "partial" }],
        error: "boom",
        model: "<synthetic>",
      },
    ];

    const UnifiedTranscript = (await import("./UnifiedTranscript.svelte")).default;
    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const models = screen.getAllByTestId("message-model").map((el) => el.textContent);
    expect(models).toContain("claude-fable-5");
    expect(models.join(" ")).not.toContain("synthetic");
  });
});

describe("UnifiedTranscript — cross-agent forward", () => {
  async function loadHeld() {
    return await import("$lib/state/heldForwards.svelte");
  }

  afterEach(async () => {
    (await loadHeld())._testing.reset();
  });

  it("renders a held forward as a 'waiting for…' row, cancellable via cancel_forward", async () => {
    const state = await loadState();
    const held = await loadHeld();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    held.addHeldForward(PROJECT_ID, {
      forwardId: "fwd-1",
      sendId: "s-1",
      body: "please aggregate",
      sources: [{ kind: "agent", id: CODEX_AGENT.id, name: "bob" }],
      recipients: [CLAUDE_AGENT.id],
    });

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    const heldEl = await screen.findByTestId("held-forward");
    // Distinct copy from a busy recipient's "Queued…", and the body is verbatim.
    expect(heldEl).toHaveTextContent("waiting for bob");
    expect(heldEl).toHaveTextContent("please aggregate");
    expect(screen.queryByTestId("turn-queued")).toBeNull();

    await fireEvent.click(screen.getByTestId("held-forward-cancel"));
    expect(invokeMock).toHaveBeenCalledWith(
      "cancel_forward",
      expect.objectContaining({ forwardId: "fwd-1" }),
    );
    // Cancelling a held forward does NOT cancel the source agents' turns.
    expect(invokeMock.mock.calls.some(([c]) => c === "cancel_turn" || c === "cancel_send")).toBe(
      false,
    );
  });

  it("clears the held-forward indicator when the forward resolves (removed from store)", async () => {
    const state = await loadState();
    const held = await loadHeld();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    held.addHeldForward(PROJECT_ID, {
      forwardId: "fwd-1",
      sendId: "s-1",
      body: "please aggregate",
      sources: [{ kind: "agent", id: CODEX_AGENT.id, name: "bob" }],
      recipients: [CLAUDE_AGENT.id],
    });

    render(UnifiedTranscript, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    expect(await screen.findByTestId("held-forward")).toBeInTheDocument();

    // Simulate `forward_message` resolving: the compose-bar closure removes the
    // held entry. The indicator must disappear.
    held.removeHeldForward(PROJECT_ID, "fwd-1");
    await tick();
    await waitFor(() => {
      expect(screen.queryByTestId("held-forward")).toBeNull();
    });
  });

  it("shows a held forward only in the pane(s) that contain its recipient", async () => {
    // Each pane renders its own UnifiedTranscript scoped to its agents. A held
    // forward must show in the recipient's pane only — not leak into a pane that
    // doesn't contain the recipient (where the name would also resolve "unknown").
    const state = await loadState();
    const held = await loadHeld();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    held.addHeldForward(PROJECT_ID, {
      forwardId: "fwd-1",
      sendId: "s-1",
      body: "aggregate",
      sources: [{ kind: "agent", id: CODEX_AGENT.id, name: "bob" }],
      recipients: [CLAUDE_AGENT.id],
    });

    // This pane holds only the *source* (codex), not the recipient (claude).
    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });
    expect(screen.queryByTestId("held-forward")).toBeNull();
  });

  it("shows only this pane's recipients in the held row — resolvable, never 'unknown'", async () => {
    // The "Forward to unknown" regression: a fan-out forward whose recipients span
    // panes must, in each pane, name only the recipients that pane contains (so
    // the name always resolves) — not every recipient.
    const state = await loadState();
    const held = await loadHeld();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    held.addHeldForward(PROJECT_ID, {
      forwardId: "fwd-1",
      sendId: "s-1",
      body: "x",
      sources: [{ kind: "agent", id: CODEX_AGENT.id, name: "bob" }],
      recipients: [CLAUDE_AGENT.id, CODEX_AGENT.id],
    });

    // This pane holds only claude; the held row names claude, not the other
    // recipient (which lives in a different pane) and never "unknown".
    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    const heldEl = await screen.findByTestId("held-forward");
    expect(heldEl).toHaveTextContent("Forward to alice");
    expect(heldEl).not.toHaveTextContent("unknown");
  });

  it("renders a forward verbatim, marked via its body sentinel, with a partial-empty caption", async () => {
    const state = await loadState();
    const held = await loadHeld();
    await state.registerAgent(CLAUDE_AGENT);
    // A forward dispatches as a normal send; the marker is derived from the
    // body's sentinel (durable across reload), not a live store.
    const body = "=== START forwarded from bob ===\nthe review\n=== END forwarded from bob ===";
    state.dispatchUserTurn(CLAUDE_AGENT.id, "u-1", body, [], "s-2", "2026-05-16T00:00:00Z");
    held.setForwardCaption(PROJECT_ID, "s-2", { included: ["bob"], skipped: ["carol"] });

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const turn = await waitFor(() => {
      const userTurn = screen
        .getAllByTestId("turn")
        .find((t) => t.getAttribute("data-role") === "user");
      if (!userTurn) throw new Error("no user turn yet");
      return userTurn;
    });
    // Verbatim: the sentinel text is present, unchanged; marked as a forward.
    expect(turn).toHaveTextContent("START forwarded from bob");
    expect(turn).toHaveAttribute("data-forwarded", "true");

    // The partial-empty caption is separate from the wire body (it names the
    // included + skipped sources) and lives in the meta row.
    const caption = screen.getByTestId("forward-caption");
    expect(caption).toHaveTextContent("forwarded from bob");
    expect(caption).toHaveTextContent("carol had no output");
  });

  it("bands only the forwarded blocks, leaving the user's typed text plain", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const body =
      "Please aggregate these:\n\n=== START forwarded from bob ===\nthe review\n=== END forwarded from bob ===";
    state.dispatchUserTurn(CLAUDE_AGENT.id, "u-1", body, [], "s-2", "2026-05-16T00:00:00Z");

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const block = await screen.findByTestId("forward-block");
    // The band wraps the forwarded block (sentinel + content) verbatim…
    expect(block).toHaveTextContent("START forwarded from bob");
    expect(block).toHaveTextContent("the review");
    // …but NOT the user's own typed text (which renders plain, outside the band).
    expect(block).not.toHaveTextContent("Please aggregate these");
  });

  it("does not band a block whose START and END agents don't pair", async () => {
    // Defensive: the canonical backend shape always pairs START X / END X. Stray
    // or pasted sentinel-looking text with mismatched agents must not mis-band —
    // it falls through to plain rendering (no forward-block).
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const body = "=== START forwarded from alice ===\nx\n=== END forwarded from bob ===";
    state.dispatchUserTurn(CLAUDE_AGENT.id, "u-1", body, [], "s-1", "2026-05-16T00:00:00Z");

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    await waitFor(() => expect(screen.getAllByTestId("turn").length).toBeGreaterThan(0));
    expect(screen.queryByTestId("forward-block")).toBeNull();
  });

  it("does not mark an ordinary (non-forward) user message", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.dispatchUserTurn(CLAUDE_AGENT.id, "u-1", "just a normal message", [], "s-3");

    render(UnifiedTranscript, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const turn = await waitFor(() => {
      const userTurn = screen
        .getAllByTestId("turn")
        .find((t) => t.getAttribute("data-role") === "user");
      if (!userTurn) throw new Error("no user turn yet");
      return userTurn;
    });
    expect(turn).toHaveAttribute("data-forwarded", "false");
    expect(screen.queryByTestId("forward-caption")).toBeNull();
  });
});
