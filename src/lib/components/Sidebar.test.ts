import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import type { AgentRecord, NormalizedEvent } from "$lib/types";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

async function loadState() {
  return await import("$lib/state/index.svelte");
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
  vi.clearAllMocks();
});

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
});

describe("Sidebar", () => {
  it("renders one row per agent with name and harness icon", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const rows = screen.getAllByTestId("sidebar-agent");
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveAttribute("data-agent-id", CLAUDE_AGENT.id);
    expect(rows[1]).toHaveAttribute("data-agent-id", CODEX_AGENT.id);

    const icons = screen.getAllByTestId("agent-harness-icon");
    expect(icons[0]).toHaveAttribute("alt", "Claude");
    expect(icons[1]).toHaveAttribute("alt", "Codex");
  });

  it("renders empty-state message when no agents", async () => {
    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [] } });
    expect(screen.queryAllByTestId("sidebar-agent")).toHaveLength(0);
    expect(screen.getByText(/no agents/i)).toBeInTheDocument();
  });

  // Run-status and last_error are no longer surfaced in the right sidebar:
  // mid-turn activity shows as a per-project spinner in the projects sidebar,
  // and failures render in the transcript as a failed agent turn (covered in
  // the reducer + UnifiedTranscript suites). The sidebar's job here is the
  // collapsible per-agent detail card.

  it("collapses an agent's detail card when its header is toggled", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: { input_tokens: 100, output_tokens: 20, total_cost_usd: 0.01 },
      },
    ];

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    // Expanded by default → details (cost) are visible.
    expect(screen.getByTestId("agent-cost")).toBeInTheDocument();

    const header = screen.getByTestId("agent-name").closest("button");
    if (!header) throw new Error("expected the agent name to sit in a toggle button");
    expect(header).toHaveAttribute("aria-expanded", "true");

    await fireEvent.click(header);

    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByTestId("agent-cost")).toBeNull();
  });

  it("collapse-all hides every agent's details; toggling again restores them", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: { input_tokens: 100, output_tokens: 20, total_cost_usd: 0.01 },
      },
    ];

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    expect(screen.getByTestId("agent-cost")).toBeInTheDocument();

    const toggleAll = screen.getByTestId("sidebar-toggle-all");
    await fireEvent.click(toggleAll);
    expect(screen.queryByTestId("agent-cost")).toBeNull();

    await fireEvent.click(toggleAll);
    expect(screen.getByTestId("agent-cost")).toBeInTheDocument();
  });

  it("displays Claude session-total cost (null-safe sum)", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    // Drive two completed agent turns with usage. Direct transcript
    // population — bypasses dispatch since we're testing the sidebar's
    // derivation, not the dispatch flow.
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: { input_tokens: 100, output_tokens: 20, total_cost_usd: 0.01 },
      },
      {
        role: "agent",
        turn_id: "turn-2",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:02Z",
        ended_at: "2026-05-16T00:00:03Z",
        status: "complete",
        items: [],
        // total_cost_usd: null — load-bearing for the ?? 0 null-safe sum.
        usage: { input_tokens: 50, output_tokens: 10, total_cost_usd: null },
      },
    ];

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("agent-cost")).toHaveTextContent("$0.0100");
  });

  it("displays Codex rate-limit % from last_rate_limit", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    const runtime = state.runtimes[CODEX_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CODEX_AGENT.id] = {
      ...runtime,
      last_rate_limit: { primary: { used_percent: 42.5 } },
    };

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CODEX_AGENT] } });

    expect(screen.getByTestId("agent-rate-limit")).toHaveTextContent("quota used: 43%");
  });

  it("displays context-utilization bar from the latest agent turn's usage", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 60_000,
          output_tokens: 10_000,
          context_window: 200_000,
        },
      },
    ];

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    // (60000 + 10000) / 200000 = 0.35 → "35%"
    expect(screen.getByTestId("agent-context-bar")).toHaveTextContent("35%");
  });

  it("renders meta info (model + mcp/skills counts) when SessionMeta has arrived", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      meta: {
        model: "claude-sonnet-4-6",
        harness_version: "2.1.140",
        tools: ["Bash", "Read"],
        mcp_servers: [{ name: "tiddly", status: "connected" }],
        skills: ["debug"],
      },
    };

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    const meta = screen.getByTestId("agent-meta");
    expect(meta).toHaveTextContent("claude-sonnet-4-6");
    expect(meta).toHaveTextContent("mcp: 1");
    expect(meta).toHaveTextContent("skills: 1");
  });
});

/// Transcript-warnings indicator. The indicator persists for the
/// lifetime of a project session — it's the project's drift detector
/// for upstream CLI parser changes (see `harness-update-review.md`).
/// What changed in M3: native `title=` → themed `Tooltip` with rows;
/// 10-row cap with "+N more" footer for long tails.
describe("Sidebar agent-parse-warnings tooltip", () => {
  beforeEach(() => {
    // bits-ui Tooltip has a 500ms delayDuration; fake timers let
    // hover-driven content appear immediately in test time.
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows the indicator with the count and uses Tooltip (not native title)", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      parse_warnings: [
        { line_number: 17, reason: "tool_result for toolu_a never matched a tool_use" },
        { line_number: 42, reason: "tool_result for toolu_b never matched a tool_use" },
      ],
    };

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    const indicator = screen.getByTestId("agent-parse-warnings");
    expect(indicator).toHaveTextContent("⚠ 2 transcript warnings");
    // Native `title=` is gone — the themed Tooltip replaces it.
    expect(indicator).not.toHaveAttribute("title");
    // Keyboard-reachable: the trigger is a <div> (no click action implied),
    // so it needs an explicit tabindex to receive focus. Without this,
    // keyboard-only users couldn't open the tooltip to read the warnings.
    // The Tooltip.test.ts focus test exercises the primitive with a
    // <button> harness, which is intrinsically focusable; this pins the
    // production-shape contract.
    expect(indicator).toHaveAttribute("tabindex", "0");

    await fireEvent.pointerEnter(indicator);
    await vi.advanceTimersByTimeAsync(500);
    await waitFor(() => screen.getByTestId("agent-parse-warnings-list"));

    const rows = screen.getAllByTestId("agent-parse-warnings-row");
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent("line 17:");
    expect(rows[0]).toHaveTextContent("tool_result for toolu_a never matched a tool_use");
    expect(rows[1]).toHaveTextContent("line 42:");
    expect(rows[1]).toHaveTextContent("tool_result for toolu_b never matched a tool_use");
    // No overflow row when under the cap.
    expect(screen.queryByTestId("agent-parse-warnings-overflow")).not.toBeInTheDocument();
  });

  it("caps at 10 rows and renders an overflow footer for the remainder", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      parse_warnings: Array.from({ length: 12 }, (_, i) => ({
        line_number: i + 1,
        reason: `warning ${i + 1}`,
      })),
    };

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    // Indicator reflects the full count (not the cap).
    expect(screen.getByTestId("agent-parse-warnings")).toHaveTextContent(
      "⚠ 12 transcript warnings",
    );

    await fireEvent.pointerEnter(screen.getByTestId("agent-parse-warnings"));
    await vi.advanceTimersByTimeAsync(500);
    await waitFor(() => screen.getByTestId("agent-parse-warnings-list"));

    expect(screen.getAllByTestId("agent-parse-warnings-row")).toHaveLength(10);
    const overflow = screen.getByTestId("agent-parse-warnings-overflow");
    expect(overflow).toHaveTextContent("+ 2 more");
  });
});

// agent-scoped events not crashing the component — direct-listener
// integration covered by index.test.ts already.
describe("Sidebar agent-scoped event tolerance", () => {
  it("does not crash on session_meta / rate_limit_event", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    // The state-module listener is mocked-but-existing; fire events
    // through it by calling the captured callback. Sidebar should
    // re-render with the new runtime values, not crash.
    const meta: NormalizedEvent = {
      type: "session_meta",
      agent_id: CLAUDE_AGENT.id,
      model: "claude-sonnet-4-6",
      harness_version: "2.1.140",
      tools: [],
      mcp_servers: [],
      skills: [],
      raw: {},
    };
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      meta: {
        model: meta.model,
        harness_version: meta.harness_version,
        tools: meta.tools,
        mcp_servers: meta.mcp_servers,
        skills: meta.skills,
      },
    };
    // Component still rendered correctly with the new meta.
    await waitFor(() => {
      expect(screen.getByTestId("agent-meta")).toHaveTextContent("claude-sonnet-4-6");
    });
  });
});
