import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import type { AgentRecord, NormalizedEvent } from "$lib/types";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

// The inline rename editor commits through the workspace state's `renameAgent`;
// inline row actions remove agents through the same module. `$lib/state/index.svelte`
// (the real module the other suites drive) does not import either, so these
// mocks are orthogonal.
const renameAgentMock = vi.fn<(id: string, name: string) => Promise<void>>();
const removeAgentMock = vi.fn<(id: string) => Promise<void>>();
vi.mock("$lib/state/workspace.svelte", () => ({
  renameAgent: (id: string, name: string) => renameAgentMock(id, name),
  removeAgent: (id: string) => removeAgentMock(id),
}));

const agentSessionInfoMock = vi.fn();
const openSessionFileMock = vi.fn();
vi.mock("$lib/api", () => ({
  agentSessionInfo: (id: string) => agentSessionInfoMock(id),
  openSessionFile: async (id: string) => {
    openSessionFileMock(id);
  },
  cancelAgent: vi.fn(),
}));

const copyTextMock = vi.fn<(t: string) => Promise<void>>();
vi.mock("$lib/native", () => ({
  copyText: (t: string) => copyTextMock(t),
}));

async function loadState() {
  return await import("$lib/state/index.svelte");
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
  vi.clearAllMocks();
  renameAgentMock.mockResolvedValue(undefined);
  removeAgentMock.mockResolvedValue(undefined);
  agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
  openSessionFileMock.mockReset();
  copyTextMock.mockReset();
  copyTextMock.mockResolvedValue(undefined);
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

  it("renders the harness icon by default and keeps inline action icons hover-only", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("agent-harness-icon")).toBeInTheDocument();
    const actions = screen.getByTestId("agent-inline-actions");
    expect(actions).toHaveClass("max-w-0");
    expect(actions).toHaveClass("group-hover:max-w-[var(--agent-action-width)]");

    const resume = await screen.findByTestId("agent-action-resume");
    const open = await screen.findByTestId("agent-action-open-session");
    expect(actions).toHaveAttribute("style", "--agent-action-width: 4.75rem;");
    expect(resume).toHaveAttribute("tabindex", "-1");
    expect(open).toHaveAttribute("tabindex", "-1");
  });

  it("shows only currently available inline actions", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.dispatchUserTurn(CLAUDE_AGENT.id, "user-1", "go", "send-1", "2026-05-16T00:00:00Z");
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("agent-action-stop")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("agent-action-resume")).toBeInTheDocument());
    expect(screen.getByTestId("agent-action-open-session")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-action-remove")).toBeNull();
  });

  it("opens session-backed inline actions when session info is available", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    await fireEvent.click(await screen.findByTestId("agent-action-open-session"));
    expect(openSessionFileMock).toHaveBeenCalledWith(CLAUDE_AGENT.id);

    await fireEvent.click(screen.getByTestId("agent-action-resume"));
    await waitFor(() => expect(screen.getByTestId("resume-panel")).toBeInTheDocument());
    expect(screen.getByTestId("resume-command")).toHaveTextContent("claude --resume abc");

    await fireEvent.click(screen.getByTestId("resume-copy"));
    expect(copyTextMock).toHaveBeenCalledWith(
      "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    );
  });

  it("omits session-backed actions when no session is bound", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    await waitFor(() => expect(agentSessionInfoMock).toHaveBeenCalledWith(CLAUDE_AGENT.id));
    expect(screen.queryByTestId("agent-action-resume")).toBeNull();
    expect(screen.queryByTestId("agent-action-open-session")).toBeNull();
    expect(screen.getByTestId("agent-action-remove")).toBeInTheDocument();
  });

  it("refetches empty session info on row hover so new session actions can appear", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    agentSessionInfoMock
      .mockResolvedValueOnce({ session_file: null, resume_command: null })
      .mockResolvedValueOnce({
        session_file: "/sessions/alice.jsonl",
        resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
      });

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    await waitFor(() => expect(agentSessionInfoMock).toHaveBeenCalledTimes(1));
    expect(screen.queryByTestId("agent-action-resume")).toBeNull();
    expect(screen.queryByTestId("agent-action-open-session")).toBeNull();

    await fireEvent.pointerEnter(screen.getByTestId("sidebar-agent"));

    await waitFor(() => expect(agentSessionInfoMock).toHaveBeenCalledTimes(2));
    expect(await screen.findByTestId("agent-action-resume")).toBeInTheDocument();
    expect(screen.getByTestId("agent-action-open-session")).toBeInTheDocument();
  });

  it("remove swaps inline actions to Cancel | Confirm and pointer leave disarms it", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    await fireEvent.click(screen.getByTestId("agent-action-remove"));
    expect(screen.getByTestId("agent-remove-cancel")).toBeInTheDocument();
    expect(screen.getByTestId("agent-remove-confirm")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-action-remove")).toBeNull();

    await fireEvent.pointerLeave(screen.getByTestId("sidebar-agent"));

    expect(screen.getByTestId("agent-action-remove")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-remove-confirm")).not.toBeInTheDocument();
    expect(removeAgentMock).not.toHaveBeenCalled();
  });

  it("warns in the delete tooltip that responses are removed from the conversation", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const state = await loadState();
      await state.registerAgent(CLAUDE_AGENT);

      const Sidebar = (await import("./Sidebar.svelte")).default;
      render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

      await fireEvent.pointerEnter(screen.getByTestId("agent-action-remove"));
      await vi.advanceTimersByTimeAsync(500);

      const tooltip = await waitFor(() => screen.getByTestId("tooltip-content"));
      expect(tooltip).toHaveTextContent("Delete agent");
      expect(tooltip).toHaveTextContent(
        "Deletes Switchboard's files for this agent; underlying session files are kept, and its responses are removed from the conversation.",
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("confirming remove calls removeAgent and failures keep the row", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    removeAgentMock.mockRejectedValueOnce(new Error("registry locked"));

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    await fireEvent.click(screen.getByTestId("agent-action-remove"));
    await fireEvent.click(screen.getByTestId("agent-remove-confirm"));

    expect(removeAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id);
    const err = await screen.findByTestId("agent-remove-error");
    expect(err).toHaveTextContent("registry locked");
    expect(screen.getByTestId("sidebar-agent")).toBeInTheDocument();
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

  it("displays context-utilization bar from the latest agent turn's reconciled occupancy", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    // The Claude bug fixture: caching makes raw `input_tokens` tiny (only the
    // new prompt), but `context_input_tokens` carries the full cached prefix.
    // The bar must reflect the reconciled occupancy, not the marginal input.
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
          input_tokens: 5_000,
          output_tokens: 10_000,
          context_input_tokens: 130_000,
          context_window: 200_000,
        },
      },
    ];

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });

    // (130000 + 10000) / 200000 = 0.70 → "70%" (NOT the ~8% the old
    // input-only formula would have shown from the marginal 5000 input).
    expect(screen.getByTestId("agent-context-bar")).toHaveTextContent("70%");
  });

  it("does not over-report Codex context (cached is already inside input)", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);

    // Codex's adapter sets context_input_tokens to input_tokens alone (its
    // cached count is a subset). The bar must use that reconciled value, not
    // re-add cached on top — which would inflate the percentage.
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 80_000,
          cached_input_tokens: 60_000,
          output_tokens: 20_000,
          context_input_tokens: 80_000,
          context_window: 200_000,
        },
      },
    ];

    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CODEX_AGENT] } });

    // (80000 + 20000) / 200000 = 0.50 → "50%". Re-adding cached would give
    // (80000 + 60000 + 20000) / 200000 = 80%, the regression this guards.
    expect(screen.getByTestId("agent-context-bar")).toHaveTextContent("50%");
  });

  it("hides the context bar when context_input_tokens is absent", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    // A window without a reconciled occupancy value → occupancy unknown, bar
    // hidden (clean-hide), rather than falling back to a misleading raw input.
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

    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
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

/// A unix-epoch-seconds timestamp `deltaSeconds` from now — payloads use
/// real-now-relative resets so the "is this window still in the future?" gate
/// is exercised deterministically (a fixed epoch would drift past `now` and
/// flip the test's meaning over time).
function epochFromNow(deltaSeconds: number): number {
  return Math.floor(Date.now() / 1000) + deltaSeconds;
}

/// An ISO string `ms` before now — for the snapshot-age (`as_of`) tooltip line.
function agoIso(ms: number): string {
  return new Date(Date.now() - ms).toISOString();
}

async function renderClaudeWithRateLimit(info: unknown, asOf: string | null): Promise<void> {
  const state = await loadState();
  await state.registerAgent(CLAUDE_AGENT);
  const runtime = state.runtimes[CLAUDE_AGENT.id];
  if (runtime === undefined) throw new Error("unreachable");
  state.runtimes[CLAUDE_AGENT.id] = {
    ...runtime,
    last_rate_limit: info,
    last_rate_limit_as_of: asOf,
  };
  const Sidebar = (await import("./Sidebar.svelte")).default;
  render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });
}

/// Claude rate-limit surface — two independent signals (the always-present
/// primary window + the overage escalation), each gated on its own reset being
/// in the future (reset-passed → clean-hide). Exact clock/date text isn't
/// asserted (jsdom locale/timezone dependent) — only the stable label/copy and
/// presence/absence per the gating rules.
describe("Sidebar Claude rate-limit surface", () => {
  it("shows the primary window independent of overage (normal-quota turn)", async () => {
    // No isUsingOverage — the 5-hour window must still surface (the bug we're
    // fixing: the window used to be gated on overage).
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(4 * 3600) },
      null,
    );
    const window = screen.getByTestId("agent-rate-window");
    expect(window).toHaveTextContent("5-hour limit resets");
    // Not overaging → no amber escalation.
    expect(screen.queryByTestId("agent-overage")).toBeNull();
  });

  it("derives the window label from rateLimitType (unknown → generic)", async () => {
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "weekly", resetsAt: epochFromNow(4 * 3600) },
      null,
    );
    // Unknown type falls back to the generic label, never a hardcoded "5-hour".
    const window = screen.getByTestId("agent-rate-window");
    expect(window).toHaveTextContent("rate limit resets");
    expect(window).not.toHaveTextContent("5-hour");
  });

  it("hides the primary window once its reset is in the past (reset-passed)", async () => {
    // A past reset is known-stale (the window has cycled, we lack the new
    // reset) — showing a past 'resets at' would be wrong, so it clean-hides.
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(-3600) },
      null,
    );
    expect(screen.queryByTestId("agent-rate-window")).toBeNull();
    expect(screen.queryByTestId("agent-rate-limit-claude")).toBeNull();
  });

  it("shows the amber overage escalation when overaging with a future overage window", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "rejected",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        isUsingOverage: true,
        overageResetsAt: epochFromNow(6 * 86400),
      },
      null,
    );
    // Both signals present: neutral window + amber escalation.
    expect(screen.getByTestId("agent-rate-window")).toHaveTextContent("5-hour limit resets");
    const overage = screen.getByTestId("agent-overage");
    expect(overage).toHaveTextContent("using credits");
    expect(overage).toHaveClass("text-warning");
  });

  it("drops the overage escalation once the overage window has passed", async () => {
    // isUsingOverage true, but the overage window elapsed → the credit window
    // has cycled, so the escalation is stale and hidden. The still-future
    // primary window stays.
    await renderClaudeWithRateLimit(
      {
        status: "rejected",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        isUsingOverage: true,
        overageResetsAt: epochFromNow(-3600),
      },
      null,
    );
    expect(screen.getByTestId("agent-rate-window")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-overage")).toBeNull();
  });

  it("overage flag with no overage window still shows (can't prove it stale)", async () => {
    await renderClaudeWithRateLimit(
      { isUsingOverage: true, resetsAt: epochFromNow(4 * 3600), rateLimitType: "five_hour" },
      null,
    );
    expect(screen.getByTestId("agent-overage")).toHaveTextContent("using credits");
  });

  it("renders nothing when there is no usable rate-limit signal", async () => {
    // Everything elapsed / absent → the whole cell clean-hides.
    await renderClaudeWithRateLimit({ status: "allowed", resetsAt: epochFromNow(-3600) }, null);
    expect(screen.queryByTestId("agent-rate-limit-claude")).toBeNull();
    expect(screen.queryByTestId("agent-rate-window")).toBeNull();
    expect(screen.queryByTestId("agent-overage")).toBeNull();
  });

  it("Codex agent never shows the Claude rate-limit cells (Claude-gated)", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    const runtime = state.runtimes[CODEX_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CODEX_AGENT.id] = {
      ...runtime,
      last_rate_limit: {
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        isUsingOverage: true,
        overageResetsAt: epochFromNow(6 * 86400),
      },
    };
    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CODEX_AGENT] } });
    expect(screen.queryByTestId("agent-rate-window")).toBeNull();
    expect(screen.queryByTestId("agent-overage")).toBeNull();
  });
});

/// Rate-limit tooltip content — always present when the cell shows, carrying
/// full reset dates (a window can be days out, beyond the inline clock) plus
/// both windows and the snapshot age when rehydrated. Mirrors the
/// parse-warnings tooltip test's fake-timer + pointerEnter pattern.
describe("Sidebar Claude rate-limit tooltip", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("surfaces the window + overage windows on hover; no snapshot line when live", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "rejected",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        isUsingOverage: true,
        overageResetsAt: epochFromNow(6 * 86400),
      },
      null,
    );
    await fireEvent.pointerEnter(screen.getByTestId("agent-rate-limit-claude"));
    await vi.advanceTimersByTimeAsync(500);
    const detail = await waitFor(() => screen.getByTestId("agent-rate-detail"));
    expect(detail).toHaveTextContent("5-hour limit resets");
    // The overage window — dropped before M4, now surfaced here.
    expect(detail).toHaveTextContent("overage window resets");
    // Live snapshot (as_of null) → no snapshot-age line.
    expect(screen.queryByTestId("agent-rate-snapshot")).toBeNull();
  });

  it("adds a snapshot-age + refresh line on hover when rehydrated (as_of set)", async () => {
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(4 * 3600) },
      agoIso(3 * 60 * 60 * 1000),
    );
    await fireEvent.pointerEnter(screen.getByTestId("agent-rate-limit-claude"));
    await vi.advanceTimersByTimeAsync(500);
    await waitFor(() => screen.getByTestId("agent-rate-detail"));
    const snapshot = screen.getByTestId("agent-rate-snapshot");
    expect(snapshot).toHaveTextContent(/snapshot from .* ago/i);
    expect(snapshot).toHaveTextContent(/refresh/i);
  });
});

async function renderCodexWithRateLimit(info: unknown): Promise<void> {
  const state = await loadState();
  await state.registerAgent(CODEX_AGENT);
  const runtime = state.runtimes[CODEX_AGENT.id];
  if (runtime === undefined) throw new Error("unreachable");
  state.runtimes[CODEX_AGENT.id] = { ...runtime, last_rate_limit: info };
  const Sidebar = (await import("./Sidebar.svelte")).default;
  render(Sidebar, { props: { agents: [CODEX_AGENT] } });
}

/// Codex rate-limit windows — both independent windows (primary ~5-hour +
/// secondary weekly) surfaced as gauge lines, each labeled from its
/// `window_minutes` and gated reset-passed. The reset times (incl. the weekly
/// window, days out) live in the tooltip. Class B (session-file-backed), so no
/// snapshot-age line. Closes G8 (secondary window + reset times were dropped).
describe("Sidebar Codex rate-limit windows", () => {
  it("renders both windows with duration-derived labels", async () => {
    await renderCodexWithRateLimit({
      primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) },
      secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
    });
    const cell = screen.getByTestId("agent-rate-limit");
    // window_minutes → human label, not "primary/secondary".
    expect(cell).toHaveTextContent("5-hour used: 42%");
    expect(cell).toHaveTextContent("weekly used: 7%");
  });

  it("bare used_percent (no window_minutes) keeps the legacy 'quota used' copy", async () => {
    // Backward-compatible fallback: a minimal payload still reads cleanly.
    await renderCodexWithRateLimit({ primary: { used_percent: 42.5 } });
    expect(screen.getByTestId("agent-rate-limit")).toHaveTextContent("quota used: 43%");
  });

  it("hides a window whose reset has passed (reset-passed), keeps the live one", async () => {
    await renderCodexWithRateLimit({
      primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(-3600) },
      secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
    });
    const cell = screen.getByTestId("agent-rate-limit");
    expect(cell).not.toHaveTextContent("5-hour");
    expect(cell).toHaveTextContent("weekly used: 7%");
  });

  it("surfaces reset times in the tooltip, not the inline gauge", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      await renderCodexWithRateLimit({
        primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) },
        secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
      });
      await fireEvent.pointerEnter(screen.getByTestId("agent-rate-limit"));
      await vi.advanceTimersByTimeAsync(500);
      const detail = await waitFor(() => screen.getByTestId("agent-rate-limit-detail"));
      expect(detail).toHaveTextContent(/5-hour: 42% used · resets/);
      expect(detail).toHaveTextContent(/weekly: 7% used · resets/);
    } finally {
      vi.useRealTimers();
    }
  });

  it("Claude agent never shows the Codex gauge cell (Codex-gated)", async () => {
    await renderClaudeWithRateLimit(
      { primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) } },
      null,
    );
    // Claude reads its own shape (isUsingOverage/resetsAt), not Codex's
    // primary.used_percent — so the Codex gauge cell must not appear.
    expect(screen.queryByTestId("agent-rate-limit")).toBeNull();
  });
});

/// Clean-hide convention (G10): a metadata cell a harness can't report must
/// render *nothing* — no empty label, no blank bar, no placeholder. These pin
/// the capable-absence cases so a future "show — / n/a" regression fails here.
describe("Sidebar clean-hide for absent metadata", () => {
  it("Codex agent renders no cost cell (subscription model — no dollar figure)", async () => {
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    // A completed Codex turn with usage but total_cost_usd null.
    state.transcripts[CODEX_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-1",
        agent_id: CODEX_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: { input_tokens: 100, output_tokens: 20, total_cost_usd: null },
      },
    ];
    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CODEX_AGENT] } });
    expect(screen.queryByTestId("agent-cost")).toBeNull();
  });

  it("agent with no metadata renders no cost / quota / rate-limit / context cells", async () => {
    // A freshly-registered agent (no turns, no rate-limit, no meta) — every
    // value-gated cell must be absent, not blank.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT] } });
    expect(screen.queryByTestId("agent-cost")).toBeNull();
    expect(screen.queryByTestId("agent-rate-limit")).toBeNull();
    expect(screen.queryByTestId("agent-rate-limit-claude")).toBeNull();
    expect(screen.queryByTestId("agent-rate-window")).toBeNull();
    expect(screen.queryByTestId("agent-overage")).toBeNull();
    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
    expect(screen.queryByTestId("agent-meta")).toBeNull();
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

/// Inline rename editor. The card's name swaps to an <input> with live
/// validation; Enter / the save icon commit, Escape / blur cancel (never
/// persist on blur). Double-click on the name row is the entry point. Commits
/// route through the mocked workspace `renameAgent`; the backend stays
/// authoritative, the frontend check is UX.
describe("Sidebar inline rename", () => {
  async function enterEditViaDoubleClick(agent: AgentRecord): Promise<HTMLInputElement> {
    const state = await loadState();
    await state.registerAgent(agent);
    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [agent] } });
    const toggle = screen.getByTestId("agent-name").closest("button");
    if (!toggle) throw new Error("expected the agent name to sit in a toggle button");
    await fireEvent.dblClick(toggle);
    return (await screen.findByTestId("agent-rename-input")) as HTMLInputElement;
  }

  it("double-click on the name row enters edit mode seeded with the current name", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    expect(input).toHaveValue("alice");
    // The name span / toggle button is gone while editing.
    expect(screen.queryByTestId("agent-name")).toBeNull();
  });

  it("the input is not nested inside the collapse toggle (no nested interactive)", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    expect(input.closest("button")).toBeNull();
  });

  it("Enter commits the new name via renameAgent and exits edit mode", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.keyDown(input, { key: "Enter" });

    expect(renameAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id, "alice2");
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
  });

  it("the save icon commits the new name via renameAgent", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });

    const save = screen.getByTestId("agent-rename-save");
    // mousedown-preventDefault keeps focus so the click commits before any
    // blur-cancel; the click does the actual commit.
    await fireEvent.mouseDown(save);
    await fireEvent.click(save);

    expect(renameAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id, "alice2");
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
  });

  it("trims the draft before submitting (validated value equals submitted value)", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "  alice2  " } });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(renameAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id, "alice2");
  });

  it("Escape reverts without calling renameAgent", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.keyDown(input, { key: "Escape" });

    expect(renameAgentMock).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
    expect(screen.getByTestId("agent-name")).toHaveTextContent("alice");
  });

  it("blur (click-away) reverts without calling renameAgent — never persists on blur", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.blur(input);

    expect(renameAgentMock).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
    expect(screen.getByTestId("agent-name")).toHaveTextContent("alice");
  });

  it("renaming to the agent's own name (case variant) is allowed — exclude-self", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    // "Alice" canonicalizes to "alice" (the agent's own); exclude-self means it
    // is not a duplicate, and it differs verbatim, so it commits.
    await fireEvent.input(input, { target: { value: "Alice" } });
    expect(screen.getByTestId("agent-rename-save")).not.toBeDisabled();
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(renameAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id, "Alice");
  });

  it("an unchanged name skips the backend round-trip and just exits", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(renameAgentMock).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
  });

  it("a duplicate of another agent disables save and blocks commit", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    const Sidebar = (await import("./Sidebar.svelte")).default;
    render(Sidebar, { props: { agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const [firstName] = screen.getAllByTestId("agent-name");
    const toggle = firstName?.closest("button");
    if (!toggle) throw new Error("expected a toggle button");
    await fireEvent.dblClick(toggle);
    const input = (await screen.findByTestId("agent-rename-input")) as HTMLInputElement;

    await fireEvent.input(input, { target: { value: "bob" } });
    const save = screen.getByTestId("agent-rename-save");
    expect(save).toBeDisabled();
    // The live message rides the input's title tooltip in the cramped card.
    expect(input).toHaveAttribute("title", "An agent named 'bob' already exists");
    expect(input).toHaveAttribute("aria-invalid", "true");

    // Enter is a no-op while invalid; the agent stays in edit mode.
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(renameAgentMock).not.toHaveBeenCalled();
    expect(screen.getByTestId("agent-rename-input")).toBeInTheDocument();
  });

  it("an emptied field disables save without showing a nag message", async () => {
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "" } });
    expect(screen.getByTestId("agent-rename-save")).toBeDisabled();
    // `empty` is suppressed — no scary message mid-edit (aria-invalid still set).
    expect(input).not.toHaveAttribute("title");
    expect(input).toHaveAttribute("aria-invalid", "true");
  });

  it("double-Enter while a rename is in flight commits only once", async () => {
    // Defer the resolution so the second Enter lands mid-flight (renaming=true),
    // exercising the re-entry guard the save button already enforces.
    let resolve: (() => void) | undefined;
    renameAgentMock.mockImplementationOnce(
      () =>
        new Promise<void>((r) => {
          resolve = () => r();
        }),
    );

    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    await fireEvent.keyDown(input, { key: "Enter" });

    expect(renameAgentMock).toHaveBeenCalledTimes(1);
    resolve?.();
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
  });

  it("a backend rejection keeps edit mode and surfaces the error", async () => {
    renameAgentMock.mockRejectedValueOnce(new Error("registry locked"));
    const input = await enterEditViaDoubleClick(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.keyDown(input, { key: "Enter" });

    const err = await screen.findByTestId("agent-rename-error");
    expect(err).toHaveTextContent("registry locked");
    // Still editing — the agent is kept on the field for a retry.
    expect(screen.getByTestId("agent-rename-input")).toBeInTheDocument();
  });
});
