import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/svelte";
import type { AgentRecord, AgentSelection, NormalizedEvent } from "$lib/types";
// Static import so the component-tree transform happens at module collection,
// not inside the first test's timeout (cold CI transforms have no vite cache).
// `vi.mock` is hoisted above imports, so the mocks below still apply.
import Sidebar from "./Sidebar.svelte";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

// The inline rename editor commits through the workspace state's `renameAgent`;
// inline row actions remove agents through the same module. `$lib/state/index.svelte`
// (the real module the other suites drive) does not import either, so these
// mocks are orthogonal.
const renameAgentMock = vi.fn<(id: string, name: string) => Promise<void>>();
const removeAgentMock = vi.fn<(id: string) => Promise<void>>();
const setAgentSelectionMock = vi.fn<(id: string, selection: AgentSelection) => Promise<void>>();
const reorderAgentsMock = vi.fn<(projectId: string, orderedIds: string[]) => Promise<void>>();
vi.mock("$lib/state/workspace.svelte", () => ({
  renameAgent: (id: string, name: string) => renameAgentMock(id, name),
  removeAgent: (id: string) => removeAgentMock(id),
  setAgentSelection: (id: string, selection: AgentSelection) =>
    setAgentSelectionMock(id, selection),
  reorderAgents: (projectId: string, orderedIds: string[]) =>
    reorderAgentsMock(projectId, orderedIds),
}));

const agentSessionInfoMock = vi.fn();
const openSessionFileMock = vi.fn();
const resumeAgentInTerminalMock = vi.fn<(id: string) => Promise<void>>();
const compactAgentMock = vi.fn<(agentId: string, sendId: string) => Promise<string>>();
const contextReportAgentMock = vi.fn<(agentId: string, sendId: string) => Promise<string>>();
vi.mock("$lib/api", () => ({
  agentSessionInfo: (id: string) => agentSessionInfoMock(id),
  openSessionFile: async (id: string) => {
    openSessionFileMock(id);
  },
  resumeAgentInTerminal: (id: string) => resumeAgentInTerminalMock(id),
  cancelAgent: vi.fn(),
  cancelSend: vi.fn(),
  cancelTurn: vi.fn(),
  compactAgent: (agentId: string, sendId: string) => compactAgentMock(agentId, sendId),
  contextReportAgent: (agentId: string, sendId: string) => contextReportAgentMock(agentId, sendId),
  loadTranscript: vi.fn(),
}));

const copyTextMock = vi.fn<(t: string) => Promise<void>>();
vi.mock("$lib/native", () => ({
  copyText: (t: string) => copyTextMock(t),
}));

function deferred<T = void>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((r, fail) => {
    resolve = r;
    reject = fail;
  });
  return { promise, resolve, reject };
}

async function loadState() {
  return await import("$lib/state/index.svelte");
}

async function openAgentActions(index = 0): Promise<HTMLElement> {
  const triggers = await screen.findAllByTestId("agent-actions-trigger");
  const trigger = triggers.at(index);
  if (trigger === undefined) throw new Error("expected agent actions trigger");
  await fireEvent.click(trigger);
  const menus = await screen.findAllByTestId("agent-actions-menu");
  const menu = menus.at(-1);
  if (menu === undefined) throw new Error("expected agent actions menu");
  return menu;
}

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";

const CLAUDE_AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "alice",
  harness: "claude_code",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  model: null,
  effort: null,
  model_choices: [],
  effort_choices: [],
  created_at: "2026-05-16T00:00:00Z",
};
const CODEX_AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "bob",
  harness: "codex",
  session_locator: null,
  model: null,
  effort: null,
  model_choices: [],
  effort_choices: [],
  created_at: "2026-05-16T00:00:01Z",
};

const ANTIGRAVITY_AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000ddd",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "ada",
  harness: "antigravity",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000003" },
  model: null,
  effort: null,
  model_choices: [],
  effort_choices: [],
  created_at: "2026-05-16T00:00:03Z",
};

beforeEach(() => {
  vi.clearAllMocks();
  renameAgentMock.mockResolvedValue(undefined);
  removeAgentMock.mockResolvedValue(undefined);
  setAgentSelectionMock.mockResolvedValue(undefined);
  reorderAgentsMock.mockResolvedValue(undefined);
  agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
  openSessionFileMock.mockReset();
  resumeAgentInTerminalMock.mockReset();
  resumeAgentInTerminalMock.mockResolvedValue(undefined);
  copyTextMock.mockReset();
  copyTextMock.mockResolvedValue(undefined);
  compactAgentMock.mockReset();
  compactAgentMock.mockResolvedValue("00000000-0000-7000-8000-00000000c003");
});

beforeEach(async () => {
  // Reset up front, not in afterEach — vitest afterEach hooks run LIFO, ahead
  // of testing-library's auto-cleanup, so a teardown reset would mutate pane
  // state under the still-mounted previous component.
  (await import("$lib/state/transcriptPanes.svelte"))._testing.reset();
  (await import("$lib/state/recipientSelection.svelte"))._testing.reset();
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

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const rows = screen.getAllByTestId("sidebar-agent");
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveAttribute("data-agent-id", CLAUDE_AGENT.id);
    expect(rows[1]).toHaveAttribute("data-agent-id", CODEX_AGENT.id);

    const icons = screen.getAllByTestId("agent-harness-icon");
    expect(icons[0]).toHaveAttribute("alt", "Claude");
    expect(icons[1]).toHaveAttribute("alt", "Codex");
  });

  it("outlines only the agents selected to receive the draft", async () => {
    const selection = await import("$lib/state/recipientSelection.svelte");
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const cards = screen.getAllByTestId("sidebar-agent");
    expect(cards[0]).toHaveAttribute("data-recipient-selected", "false");
    expect(cards[1]).toHaveAttribute("data-recipient-selected", "false");

    selection.setRecipients(PROJECT_ID, [CLAUDE_AGENT.id]);
    await waitFor(() => expect(cards[0]).toHaveAttribute("data-recipient-selected", "true"));
    expect(cards[0]).toHaveClass("ring-accent", "ring-1", "hover:ring-accent");
    expect(cards[1]).toHaveAttribute("data-recipient-selected", "false");
    expect(cards[1]).not.toHaveClass("ring-accent");

    selection.setRecipients(PROJECT_ID, [CODEX_AGENT.id]);
    await waitFor(() => expect(cards[1]).toHaveAttribute("data-recipient-selected", "true"));
    expect(cards[0]).toHaveAttribute("data-recipient-selected", "false");
  });

  it("renders empty-state message when no agents", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [] } });
    expect(screen.queryAllByTestId("sidebar-agent")).toHaveLength(0);
    expect(screen.getByText(/no agents/i)).toBeInTheDocument();
  });

  // Run-status and last_error are no longer surfaced in the right sidebar:
  // mid-turn activity shows as a per-project spinner in the projects sidebar,
  // and failures render in the transcript as a failed agent turn (covered in
  // the reducer + UnifiedTranscript suites). The sidebar's job here is the
  // collapsible per-agent detail card.

  it("collapses an agent's detail card when its non-control surface is clicked", async () => {
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
          input_tokens: 100,
          output_tokens: 20,
          context_input_tokens: 100,
          context_tokens_after_turn: 120,
          context_window: 200_000,
        },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    // Expanded by default → details (the context bar) are visible.
    expect(screen.getByTestId("agent-context-bar")).toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("sidebar-agent"));
    expect(screen.queryByTestId("agent-context-bar")).toBeNull();

    await openAgentActions();
    await fireEvent.click(await screen.findByTestId("agent-action-collapse"));
    expect(screen.getByTestId("agent-context-bar")).toBeInTheDocument();

    const card = screen.getByTestId("sidebar-agent");
    card.focus();
    await fireEvent.keyDown(card, { key: " " });
    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
  });

  it("does not collapse the card when a click completes text selection", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    const selection = vi.spyOn(window, "getSelection").mockReturnValue({
      isCollapsed: false,
    } as Selection);

    const card = screen.getByTestId("sidebar-agent");
    await fireEvent.click(card);
    expect(card).toHaveAttribute("data-collapsed", "false");
    selection.mockRestore();
  });

  it("renders the harness icon and a hover-revealed actions menu trigger", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("agent-harness-icon")).toBeInTheDocument();
    const trigger = screen.getByTestId("agent-actions-trigger");
    // `hidden`, not `opacity-0`: a transparent icon still reserves its width,
    // and that gutter is what truncated names. Reveal is display-based.
    expect(trigger).toHaveClass("hidden");
    expect(trigger).toHaveClass("group-hover:inline-flex");

    const menu = await openAgentActions();
    expect(await screen.findByTestId("agent-action-resume")).toBeInTheDocument();
    expect(screen.getByTestId("agent-action-open-session")).toBeInTheDocument();
    expect(screen.getByTestId("agent-selection-settings")).toBeInTheDocument();
    expect(
      Array.from(menu.querySelectorAll('[role="menuitem"]')).map((item) =>
        item.textContent?.trim(),
      ),
    ).toEqual([
      "Rename",
      "Collapse",
      "Context breakdown…",
      "Compact context",
      "Resume in terminal",
      "Open session file",
      "Model settings…",
      "Delete agent",
    ]);
    expect(menu.querySelectorAll('[role="menuitem"] svg')).toHaveLength(8);
  });

  it("shows only currently available menu actions", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.dispatchUserTurn(CLAUDE_AGENT.id, "user-1", "go", [], "send-1", "2026-05-16T00:00:00Z");
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await openAgentActions();
    expect(screen.getByTestId("agent-action-stop")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("agent-action-resume")).toBeInTheDocument());
    expect(screen.getByTestId("agent-action-open-session")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-action-remove")).toBeNull();
  });

  it("opens session-backed menu actions when session info is available", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await openAgentActions();
    await fireEvent.click(await screen.findByTestId("agent-action-open-session"));
    expect(openSessionFileMock).toHaveBeenCalledWith(CLAUDE_AGENT.id);

    await openAgentActions();
    await fireEvent.click(screen.getByTestId("agent-action-resume"));
    await waitFor(() => expect(screen.getByTestId("resume-panel")).toBeInTheDocument());
    expect(screen.getByTestId("resume-command")).toHaveTextContent("claude --resume abc");

    await fireEvent.click(screen.getByTestId("resume-copy"));
    expect(copyTextMock).toHaveBeenCalledWith(
      "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    );

    await fireEvent.click(screen.getByTestId("resume-run-terminal"));
    await waitFor(() => expect(resumeAgentInTerminalMock).toHaveBeenCalledWith(CLAUDE_AGENT.id));
    await waitFor(() => expect(screen.queryByTestId("resume-panel")).toBeNull());
  });

  it("keeps the resume dialog open and shows a terminal launch failure", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });
    resumeAgentInTerminalMock.mockRejectedValue(new Error("Terminal automation was denied"));

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    await openAgentActions();
    await fireEvent.click(await screen.findByTestId("agent-action-resume"));
    await fireEvent.click(screen.getByTestId("resume-run-terminal"));

    expect(await screen.findByTestId("resume-launch-error")).toHaveTextContent(
      "Terminal automation was denied",
    );
    expect(screen.getByTestId("resume-panel")).toBeInTheDocument();
  });

  it("does not let a closed dialog's pending launch affect another agent", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    agentSessionInfoMock.mockImplementation(async (id: string) => ({
      session_file: `/sessions/${id}.jsonl`,
      resume_command: id === CLAUDE_AGENT.id ? "resume alice" : "resume bob",
    }));
    let rejectLaunch!: (reason: unknown) => void;
    const pendingLaunch = new Promise<void>((_resolve, reject) => {
      rejectLaunch = reject;
    });
    resumeAgentInTerminalMock.mockReturnValue(pendingLaunch);

    render(Sidebar, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] },
    });

    await openAgentActions(0);
    await fireEvent.click(await screen.findByTestId("agent-action-resume"));
    await fireEvent.click(screen.getByTestId("resume-run-terminal"));
    expect(resumeAgentInTerminalMock).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByTestId("dialog-close"));
    await openAgentActions(1);
    await fireEvent.click(await screen.findByTestId("agent-action-resume"));
    expect(screen.getByTestId("resume-command")).toHaveTextContent("resume bob");
    expect(screen.getByTestId("resume-run-terminal")).toBeDisabled();

    rejectLaunch(new Error("alice launch failed late"));
    await waitFor(() => expect(screen.getByTestId("resume-run-terminal")).toBeEnabled());
    expect(screen.getByTestId("resume-panel")).toBeInTheDocument();
    expect(screen.getByTestId("resume-command")).toHaveTextContent("resume bob");
    expect(screen.queryByTestId("resume-launch-error")).toBeNull();
    expect(resumeAgentInTerminalMock).toHaveBeenCalledTimes(1);
  });

  it("omits session-backed actions when no session is bound", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await waitFor(() => expect(agentSessionInfoMock).toHaveBeenCalledWith(CLAUDE_AGENT.id));
    await openAgentActions();
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

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await waitFor(() => expect(agentSessionInfoMock).toHaveBeenCalledTimes(1));
    await openAgentActions();
    expect(screen.queryByTestId("agent-action-resume")).toBeNull();
    expect(screen.queryByTestId("agent-action-open-session")).toBeNull();

    await fireEvent.pointerEnter(screen.getByTestId("sidebar-agent"));

    await waitFor(() => expect(agentSessionInfoMock).toHaveBeenCalledTimes(2));
    expect(await screen.findByTestId("agent-action-resume")).toBeInTheDocument();
    expect(screen.getByTestId("agent-action-open-session")).toBeInTheDocument();
  });

  it("remove swaps menu actions to Cancel | Confirm and pointer leave disarms it", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await openAgentActions();
    await fireEvent.click(screen.getByTestId("agent-action-remove"));
    expect(screen.getByTestId("agent-remove-cancel")).toBeInTheDocument();
    expect(screen.getByTestId("agent-remove-confirm")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-action-remove")).toBeNull();

    await fireEvent.pointerLeave(screen.getByTestId("sidebar-agent"));

    expect(screen.getByTestId("agent-action-remove")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-remove-confirm")).not.toBeInTheDocument();
    expect(removeAgentMock).not.toHaveBeenCalled();
  });

  it("warns on delete that responses are removed from the conversation", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await openAgentActions();
    const deleteItem = screen.getByTestId("agent-action-remove");
    expect(deleteItem).not.toHaveAttribute("title");
    await fireEvent.pointerEnter(deleteItem);
    expect(
      await screen.findByText(
        "Deletes Switchboard's files for this agent; underlying session files are kept, and its responses are removed from the conversation.",
        {},
        { timeout: 1_500 },
      ),
    ).toBeInTheDocument();
  });

  it("confirming remove calls removeAgent and failures keep the row", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    removeAgentMock.mockRejectedValueOnce(new Error("registry locked"));

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await openAgentActions();
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
        usage: {
          input_tokens: 100,
          output_tokens: 20,
          context_input_tokens: 100,
          context_tokens_after_turn: 120,
          context_window: 200_000,
        },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    expect(screen.getByTestId("agent-context-bar")).toBeInTheDocument();

    const toggleAll = screen.getByTestId("sidebar-toggle-all");
    expect(toggleAll).toHaveAccessibleName("Collapse all agents");
    await fireEvent.click(toggleAll);
    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
    expect(toggleAll).toHaveAccessibleName("Expand all agents");

    await fireEvent.click(toggleAll);
    expect(screen.getByTestId("agent-context-bar")).toBeInTheDocument();
    expect(toggleAll).toHaveAccessibleName("Collapse all agents");
  });

  it("does not render a per-agent cost total on the card (cost moved to the message)", async () => {
    // Per-turn cost now renders inline in the transcript on real-spend turns;
    // the card carries no accumulating `$` total (system-design §2). Even with
    // a turn that reports a dollar cost, no `agent-cost` cell appears.
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
        spend: { real_spend: true, is_overage: true },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("agent-cost")).toBeNull();
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

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

    const cell = screen.getByTestId("agent-rate-limit");
    expect(cell).toHaveTextContent("Quota");
    expect(cell).toHaveTextContent("43%");
  });

  it("displays context-utilization bar from the latest agent turn's reconciled occupancy", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    // The Claude bug fixture: caching makes raw `input_tokens` tiny (only the
    // new prompt), while `context_tokens_after_turn` carries the full parent
    // call occupancy. The bar must reflect that value, not marginal input or
    // aggregate billing output.
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
          context_tokens_after_turn: 140_000,
          context_window: 200_000,
        },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

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
          context_tokens_after_turn: 100_000,
          context_window: 200_000,
        },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });

    // (80000 + 20000) / 200000 = 0.50 → "50%". Re-adding cached would give
    // (80000 + 60000 + 20000) / 200000 = 80%, the regression this guards.
    expect(screen.getByTestId("agent-context-bar")).toHaveTextContent("50%");
  });

  it("hides the context bar when post-turn occupancy is absent", async () => {
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

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
  });

  it("does not fall back to an older percentage when the latest terminal window is absent", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-old",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 90_000,
          output_tokens: 10_000,
          context_input_tokens: 90_000,
          context_tokens_after_turn: 100_000,
          context_window: 200_000,
        },
      },
      {
        role: "agent",
        turn_id: "turn-latest",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:01:00Z",
        ended_at: "2026-05-16T00:01:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 120_000,
          output_tokens: 10_000,
          context_input_tokens: 120_000,
          context_tokens_after_turn: 130_000,
        },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
  });

  it("keeps the last completed percentage while a newer turn is streaming", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-complete",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 90_000,
          output_tokens: 10_000,
          context_input_tokens: 90_000,
          context_tokens_after_turn: 100_000,
          context_window: 200_000,
        },
      },
      {
        role: "agent",
        turn_id: "turn-streaming",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:01:00Z",
        status: "streaming",
        items: [],
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("agent-context-bar")).toHaveTextContent("50%");
  });

  it("uses parent-call occupancy instead of aggregate billing output", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-latest",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:01:00Z",
        ended_at: "2026-05-16T00:01:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 600_000,
          output_tokens: 300_000,
          context_input_tokens: 140_000,
          context_tokens_after_turn: 150_000,
          context_window: 200_000,
        },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("agent-context-bar")).toHaveTextContent("75%");
  });

  it("hides an impossible utilization instead of clamping it to 100%", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.transcripts[CLAUDE_AGENT.id] = [
      {
        role: "agent",
        turn_id: "turn-old",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 90_000,
          output_tokens: 10_000,
          context_input_tokens: 90_000,
          context_tokens_after_turn: 100_000,
          context_window: 200_000,
        },
      },
      {
        role: "agent",
        turn_id: "turn-latest",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:01:00Z",
        ended_at: "2026-05-16T00:01:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 2,
          output_tokens: 2_223,
          context_input_tokens: 594_747,
          context_tokens_after_turn: 596_970,
          context_window: 200_000,
        },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
  });

  it("renders the environment inventory without replacing selected model intent", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      meta: {
        model: "claude-sonnet-4-6",
        harness_version: "2.1.140",
        inventory: {
          tools: ["Bash", "Read"],
          mcp_servers: [{ name: "tiddly", status: "connected" }],
          skills: [{ name: "debug" }],
        },
      },
    };

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.getByTestId("agent-selection-default")).toHaveTextContent(
      "Harness/session default",
    );
    expect(screen.queryByTestId("agent-observed-model")).toBeNull();
    expect(screen.getByTestId("agent-env-summary")).toHaveTextContent("MCP 1 · Skills 1");
  });

  // --- Environment row wiring ------------------------------------------------
  //
  // The row's own behavior — the collapsed counts, the status dots, the
  // count-line expansions — is covered in `AgentEnvironment.test.ts`. These
  // pin the wiring: which runtime fields the card feeds it, and that it
  // clean-hides for an agent with nothing to show.

  it("hides the environment row for an agent that reported no inventory", async () => {
    // A fresh agent that has never run: nothing loaded, so no row at all
    // rather than a disclosure that opens onto nothing.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    expect(screen.queryByTestId("agent-meta")).toBeNull();
  });

  it("feeds the environment row the rehydrated snapshot time", async () => {
    // Claude's inventory is stream-only, so after a restart the card shows
    // what the last turn loaded and says so.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      meta: {
        model: "claude-fable-5-1",
        harness_version: "2.1.274",
        inventory: { agents: ["Explore"] },
      },
      meta_as_of: "2026-09-17T12:00:00Z",
    };

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    await fireEvent.click(screen.getByTestId("agent-env-toggle"));

    expect(screen.getByTestId("agent-env-as-of")).toHaveTextContent(/^as of /);
  });

  it("renders a Codex card's rollout inventory with no status dot", async () => {
    // Codex records no MCP status anywhere, so its servers come from
    // `config.toml` — a configured name is not a runtime status and must not
    // render as one.
    const state = await loadState();
    await state.registerAgent(CODEX_AGENT);
    const runtime = state.runtimes[CODEX_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CODEX_AGENT.id] = {
      ...runtime,
      meta: {
        model: "gpt-5.6-terra",
        harness_version: "0.154.0",
        inventory: {
          mcp_servers: [{ name: "tiddly", status: "configured" }],
          skills: [{ name: "build-report", description: "Build reports." }],
          approved_commands: ["ls"],
          settings: [{ label: "Sandbox", value: "read-only" }],
        },
      },
    };

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });
    await fireEvent.click(screen.getByTestId("agent-env-toggle"));

    expect(screen.getByTestId("agent-env-mcp")).toHaveTextContent("tiddly");
    expect(screen.queryByTestId("agent-env-mcp-dot")).toBeNull();
    expect(screen.getByTestId("agent-env-list-approved_commands")).toBeInTheDocument();
    expect(screen.getByTestId("agent-env-settings")).toHaveTextContent("Sandbox: read-only");
  });

  // --- Model / effort: change actions + intent display -----------------------

  it("Claude exposes one model settings action", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await openAgentActions();
    await waitFor(() => expect(screen.getByTestId("agent-selection-settings")).toBeInTheDocument());
    expect(screen.getByTestId("agent-selection-settings")).toHaveTextContent("Model settings");
  });

  it("Antigravity exposes the selection action like every other harness", async () => {
    // It had none while its model was harness-owned global config; `agy` 1.1.x
    // made both axes per-invocation, so the action is no longer withheld.
    const state = await loadState();
    await state.registerAgent(ANTIGRAVITY_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [ANTIGRAVITY_AGENT] } });

    await openAgentActions();
    expect(screen.queryByTestId("agent-selection-settings")).not.toBeNull();
  });

  it("model settings saves the complete independent selection atomically", async () => {
    const state = await loadState();
    const agent = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus"],
      effort_choices: ["high"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await openAgentActions();
    await waitFor(() => expect(screen.getByTestId("agent-selection-settings")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("agent-selection-settings"));

    await screen.findByTestId("change-selection-model");
    await fireEvent.click(screen.getByTestId("change-selection-model-choice-sonnet"));
    await fireEvent.click(screen.getByTestId("change-selection-model-choice-opus"));
    await fireEvent.click(screen.getByTestId("change-save"));

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "sonnet",
      effort: "high",
      model_choices: ["sonnet"],
      effort_choices: ["high"],
    });
  });

  it("Model settings preserves an untouched unpinned agent", async () => {
    const state = await loadState();
    const agent = { ...CLAUDE_AGENT, model: null, effort: null };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await openAgentActions();
    await waitFor(() => expect(screen.getByTestId("agent-selection-settings")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("agent-selection-settings"));

    await screen.findByTestId("change-selection-model");
    expect(screen.queryByTestId("change-selection-model-current")).toBeNull();
    expect(screen.queryByTestId("change-selection-effort-current")).toBeNull();
    await fireEvent.click(screen.getByTestId("change-save"));

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: null,
      effort: null,
      model_choices: [],
      effort_choices: [],
    });
  });

  it("requires a compatible effort before saving a newly configured attached Antigravity agent", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...ANTIGRAVITY_AGENT,
      model: null,
      effort: null,
      model_choices: [],
      effort_choices: [],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await openAgentActions();
    await fireEvent.click(await screen.findByTestId("agent-selection-settings"));
    await fireEvent.click(screen.getByTestId("change-selection-model-choice-gemini-3.8-flash"));

    expect(screen.getByTestId("change-selection-invalid")).toHaveTextContent(
      "Choose a reasoning effort supported by the current model before saving.",
    );
    expect(screen.getByTestId("change-save")).toBeDisabled();
    expect(screen.getByTestId("change-cancel")).toBeEnabled();

    await fireEvent.click(screen.getByTestId("change-selection-effort-choice-high"));
    expect(screen.getByTestId("change-save")).toBeEnabled();
  });

  it("changing only effort leaves an unpinned model unset", async () => {
    const state = await loadState();
    const agent = { ...CLAUDE_AGENT, model: null, effort: null };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await openAgentActions();
    await waitFor(() => expect(screen.getByTestId("agent-selection-settings")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("agent-selection-settings"));
    await fireEvent.click(screen.getByTestId("change-selection-effort-choice-medium"));
    await fireEvent.click(screen.getByTestId("change-save"));

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: null,
      effort: "medium",
      model_choices: [],
      effort_choices: ["medium"],
    });
  });

  it("Change model includes an unknown persisted value so Save preserves it", async () => {
    const state = await loadState();
    const agent = { ...CLAUDE_AGENT, model: "future-opus", model_choices: ["future-opus"] };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await openAgentActions();
    await waitFor(() => expect(screen.getByTestId("agent-selection-settings")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("agent-selection-settings"));

    expect(await screen.findByTestId("change-selection-model-choice-future-opus")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.queryByTestId("change-selection-model-implicit")).toBeNull();
    await fireEvent.click(screen.getByTestId("change-save"));

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "future-opus",
      effort: null,
      model_choices: ["future-opus"],
      effort_choices: [],
    });
  });

  it("Change model uses the segmented toggle for an in-catalog model", async () => {
    const state = await loadState();
    const agent = { ...CLAUDE_AGENT, model: "opus", model_choices: ["opus"] };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await openAgentActions();
    await waitFor(() => expect(screen.getByTestId("agent-selection-settings")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("agent-selection-settings"));

    expect(await screen.findByTestId("change-selection-model-choice-opus")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("keeps the change dialog open and non-dismissible while saving", async () => {
    const state = await loadState();
    const agent = { ...CLAUDE_AGENT, model: "opus", model_choices: ["opus"] };
    await state.registerAgent(agent);
    const pending = deferred();
    setAgentSelectionMock.mockReturnValueOnce(pending.promise);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await openAgentActions();
    await waitFor(() => expect(screen.getByTestId("agent-selection-settings")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("agent-selection-settings"));
    await fireEvent.click(screen.getByTestId("change-save"));

    expect(screen.getByTestId("change-selection-panel")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByTestId("dialog-close")).toBeNull());

    pending.resolve(undefined);
    await waitFor(() => expect(screen.queryByTestId("change-selection-panel")).toBeNull());
  });

  it("configures independent additional choices in the shared settings dialog", async () => {
    const state = await loadState();
    const agent = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus"],
      effort_choices: ["high"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await openAgentActions();
    await fireEvent.click(await screen.findByTestId("agent-selection-settings"));
    await fireEvent.click(screen.getByTestId("change-selection-model-choice-sonnet"));
    await fireEvent.click(screen.getByTestId("change-selection-effort-choice-medium"));
    await fireEvent.click(screen.getByTestId("change-save"));

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "opus",
      effort: "high",
      model_choices: ["opus", "sonnet"],
      effort_choices: ["high", "medium"],
    });
  });

  it("quick-switches model and effort independently when each has two choices", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus", "sonnet"],
      effort_choices: ["high", "medium"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const modelChip = screen.getByTestId("agent-model-chip");
    expect(modelChip).toHaveAccessibleName(/Current model: Opus.*Switch to Sonnet/);
    await fireEvent.click(modelChip);

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "sonnet",
      effort: "high",
      model_choices: ["opus", "sonnet"],
      effort_choices: ["high", "medium"],
    });
  });

  it("opens a direct menu for three model choices", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus", "sonnet", "haiku"],
      effort_choices: ["high"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await fireEvent.click(screen.getByTestId("agent-model-chip"));
    const menu = await screen.findByTestId("agent-model-menu");
    expect(menu).toHaveClass("min-w-28", "p-0.5", "text-xs");
    expect(await screen.findByTestId("agent-model-option-sonnet")).toHaveClass(
      "px-2",
      "py-1",
      "leading-4",
    );
    expect(screen.queryByTestId("agent-model-option-opus")).toBeNull();
    expect(screen.getByTestId("agent-model-option-sonnet")).toHaveTextContent("Sonnet");
    await fireEvent.click(await screen.findByTestId("agent-model-option-haiku"));

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "haiku",
      effort: "high",
      model_choices: ["opus", "sonnet", "haiku"],
      effort_choices: ["high"],
    });
  });

  it("opens a direct menu for three effort choices without changing the model", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus"],
      effort_choices: ["high", "medium", "low"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await fireEvent.click(screen.getByTestId("agent-effort-chip"));
    await fireEvent.click(await screen.findByTestId("agent-effort-option-low"));

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "opus",
      effort: "low",
      model_choices: ["opus"],
      effort_choices: ["high", "medium", "low"],
    });
  });

  it("announces and atomically applies an Antigravity model switch that adjusts effort", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...ANTIGRAVITY_AGENT,
      model: "gemini-3.8-flash",
      effort: "medium",
      model_choices: ["gemini-3.8-flash", "gemini-3.1-pro"],
      effort_choices: ["medium", "high"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const modelChip = screen.getByTestId("agent-model-chip");
    expect(modelChip).toHaveAccessibleName(/Switch to Gemini 3.1 Pro — effort becomes High/);
    await fireEvent.click(modelChip);

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "gemini-3.1-pro",
      effort: "high",
      model_choices: ["gemini-3.8-flash", "gemini-3.1-pro"],
      effort_choices: ["medium", "high"],
    });
  });

  // `gemini-3.7-flash` is deliberately NOT updated to 3.8 here: it is a real
  // model retired from the picker but still valid at dispatch, so this pins the
  // promise that retiring an entry doesn't strand an agent already using it.
  it("preserves effort when switching to an unknown Antigravity model", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...ANTIGRAVITY_AGENT,
      model: "gemini-3.7-flash",
      effort: "medium",
      model_choices: ["gemini-3.7-flash", "future-model"],
      effort_choices: ["medium"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await fireEvent.click(screen.getByTestId("agent-model-chip"));

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "future-model",
      effort: "medium",
      model_choices: ["gemini-3.7-flash", "future-model"],
      effort_choices: ["medium"],
    });
  });

  it("hides effort only for an explicitly known no-effort Antigravity model", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...ANTIGRAVITY_AGENT,
      model: "claude-sonnet-4-6",
      effort: null,
      model_choices: ["claude-sonnet-4-6"],
      effort_choices: ["high"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    expect(screen.getByTestId("agent-model-chip")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-effort-chip")).toBeNull();
  });

  it("disables an Antigravity model target with no compatible configured effort", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...ANTIGRAVITY_AGENT,
      model: "gemini-3.8-flash",
      effort: "medium",
      model_choices: ["gemini-3.8-flash", "gemini-3.1-pro"],
      effort_choices: ["medium"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const modelChip = screen.getByTestId("agent-model-chip");
    expect(modelChip).toBeDisabled();
    expect(modelChip).not.toHaveAttribute("title");
    expect(modelChip).toHaveAttribute("data-tooltip-trigger");
  });

  it("blocks both quick controls while a complete selection write is pending", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus", "sonnet"],
      effort_choices: ["high", "medium", "low"],
    };
    await state.registerAgent(agent);
    const pending = deferred();
    setAgentSelectionMock.mockReturnValueOnce(pending.promise);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    await fireEvent.click(screen.getByTestId("agent-model-chip"));
    expect(screen.getByTestId("agent-model-chip")).toHaveClass("cursor-default");
    expect(screen.getByTestId("agent-model-chip")).not.toHaveClass("cursor-not-allowed");
    expect(screen.getByTestId("agent-effort-chip")).toBeDisabled();
    await fireEvent.click(screen.getByTestId("agent-effort-chip"));
    expect(setAgentSelectionMock).toHaveBeenCalledTimes(1);
    pending.resolve(undefined);
    await waitFor(() => expect(screen.getByTestId("agent-effort-chip")).toBeEnabled());
  });

  it("tracks concurrent selection saves and failures independently per agent", async () => {
    const state = await loadState();
    const claude: AgentRecord = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus", "sonnet"],
      effort_choices: ["high", "medium"],
    };
    const codex: AgentRecord = {
      ...CODEX_AGENT,
      model: "gpt-5.6-sol",
      effort: "high",
      model_choices: ["gpt-5.6-sol", "gpt-5.6-terra"],
      effort_choices: ["high", "medium"],
    };
    await state.registerAgent(claude);
    await state.registerAgent(codex);
    const claudeSave = deferred();
    const codexSave = deferred();
    setAgentSelectionMock.mockImplementation((id) =>
      id === claude.id ? claudeSave.promise : codexSave.promise,
    );
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [claude, codex] } });
    const cards = screen.getAllByTestId("sidebar-agent");

    await fireEvent.click(within(cards[0]!).getByTestId("agent-model-chip"));
    await fireEvent.click(within(cards[1]!).getByTestId("agent-model-chip"));
    expect(within(cards[0]!).getByTestId("agent-effort-chip")).toBeDisabled();
    expect(within(cards[1]!).getByTestId("agent-effort-chip")).toBeDisabled();

    claudeSave.reject(new Error("claude registry failed"));
    await waitFor(() =>
      expect(within(cards[0]!).getByTestId("agent-selection-save-error")).toHaveTextContent(
        "claude registry failed",
      ),
    );
    expect(within(cards[0]!).getByTestId("agent-effort-chip")).toBeEnabled();
    expect(within(cards[1]!).getByTestId("agent-effort-chip")).toBeDisabled();

    codexSave.resolve(undefined);
    await waitFor(() => expect(within(cards[1]!).getByTestId("agent-effort-chip")).toBeEnabled());
    expect(within(cards[0]!).getByTestId("agent-selection-save-error")).toBeInTheDocument();
  });

  it("disables a two-choice effort target that the current Antigravity model rejects", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...ANTIGRAVITY_AGENT,
      model: "gemini-3.1-pro",
      effort: "high",
      model_choices: ["gemini-3.1-pro"],
      effort_choices: ["high", "medium"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const effortChip = screen.getByTestId("agent-effort-chip");
    expect(effortChip).toBeDisabled();
    expect(effortChip).not.toHaveAttribute("title");
    expect(effortChip).toHaveAttribute("data-tooltip-trigger");
  });

  it("a sole choice can be adopted when the current value is null", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...CLAUDE_AGENT,
      model: null,
      effort: null,
      model_choices: ["sonnet"],
      effort_choices: ["medium"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const modelChip = screen.getByTestId("agent-model-chip");
    expect(modelChip).toHaveAccessibleName(/Current model: Harness default.*Switch to Sonnet/);
    await fireEvent.click(modelChip);

    expect(setAgentSelectionMock).toHaveBeenCalledExactlyOnceWith(agent.id, {
      model: "sonnet",
      effort: null,
      model_choices: ["sonnet"],
      effort_choices: ["medium"],
    });
  });

  it("keeps the quick switch usable and surfaces a persistence failure", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus", "sonnet"],
      effort_choices: ["high", "medium"],
    };
    await state.registerAgent(agent);
    setAgentSelectionMock.mockRejectedValueOnce(new Error("registry is read-only"));
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const toggle = screen.getByTestId("agent-model-chip");
    await fireEvent.click(toggle);

    await waitFor(() =>
      expect(screen.getByTestId("agent-selection-save-error")).toHaveTextContent(
        "registry is read-only",
      ),
    );
    expect(toggle).toBeEnabled();
  });

  it("sidebar shows the SELECTED model even when the observed model differs (post-turn state)", async () => {
    const state = await loadState();
    const agent = { ...CLAUDE_AGENT, model: "opus", model_choices: ["opus"] };
    await state.registerAgent(agent);
    const runtime = state.runtimes[agent.id];
    if (runtime === undefined) throw new Error("unreachable");
    // A turn has run, so the harness reported a (resolved) observed model that
    // differs from the durable-alias selection.
    state.runtimes[agent.id] = {
      ...runtime,
      meta: {
        model: "claude-opus-4-8",
        harness_version: "2.1.140",
        inventory: {},
      },
    };

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const modelChip = screen.getByTestId("agent-model-chip");
    expect(modelChip).toHaveTextContent("Opus");
    expect(modelChip.tagName).toBe("SPAN");
    expect(modelChip).toHaveClass("cursor-default");
    expect(modelChip).not.toHaveAttribute("title");
    expect(modelChip).toHaveAttribute("data-tooltip-trigger");
    // The selection wins — the observed line is not shown when intent exists.
    expect(screen.queryByTestId("agent-observed-model")).toBeNull();
  });

  it("sidebar does not turn an observed model into future-send intent when unpinned", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT); // no selected model
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      meta: {
        model: "claude-sonnet-4-6",
        harness_version: "2.1.140",
        inventory: {},
      },
    };

    render(Sidebar, {
      props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] },
    });
    expect(screen.getByTestId("agent-selection-default")).toHaveTextContent(
      "Harness/session default",
    );
    expect(screen.queryByTestId("agent-observed-model")).toBeNull();
  });

  it("shows the harness default for an unset model beside an explicit effort", async () => {
    const state = await loadState();
    const agent: AgentRecord = {
      ...CLAUDE_AGENT,
      effort: "medium",
      effort_choices: ["medium"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    expect(screen.getByTestId("agent-model-default")).toHaveTextContent(
      "Model: Harness/session default",
    );
    expect(screen.getByTestId("agent-effort-chip")).toHaveTextContent("Medium");
  });

  it("sidebar shows the selected effort", async () => {
    const state = await loadState();
    const agent = {
      ...CLAUDE_AGENT,
      model: "opus",
      effort: "high",
      model_choices: ["opus"],
      effort_choices: ["high"],
    };
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    expect(screen.getByTestId("agent-effort-chip")).toHaveTextContent("High");
    expect(screen.getByTestId("agent-effort-chip").tagName).toBe("SPAN");
  });

  it("gives the name the full row until hover: shared-prefix names stay distinguishable at rest", async () => {
    const state = await loadState();
    const a = {
      ...CLAUDE_AGENT,
      id: "00000000-0000-7000-8000-000000000aa1",
      name: "gpt-5-5-minimal",
    };
    const b = {
      ...CLAUDE_AGENT,
      id: "00000000-0000-7000-8000-000000000aa2",
      name: "gpt-5-5-minimal-2",
    };
    await state.registerAgent(a);
    await state.registerAgent(b);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [a, b] } });

    const names = screen.getAllByTestId("agent-name").map((el) => el.textContent?.trim());
    expect(names).toEqual(["gpt-5-5-minimal", "gpt-5-5-minimal-2"]);
    // The room comes from the action icons being `hidden` (display: none —
    // zero width) until hover/focus, rather than `opacity-0` (invisible but
    // still reserving a two-icon gutter). The name truncates only while the
    // icons are revealed.
    for (const toggle of screen.getAllByTestId("agent-visibility-toggle")) {
      expect(toggle).toHaveClass("hidden");
    }
    for (const trigger of screen.getAllByTestId("agent-actions-trigger")) {
      expect(trigger).toHaveClass("hidden");
    }
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

async function renderClaudeWithRateLimit(
  info: unknown,
  asOf: string | null,
  model?: string,
): Promise<void> {
  const state = await loadState();
  await state.registerAgent(CLAUDE_AGENT);
  const runtime = state.runtimes[CLAUDE_AGENT.id];
  if (runtime === undefined) throw new Error("unreachable");
  state.runtimes[CLAUDE_AGENT.id] = {
    ...runtime,
    last_rate_limit: info,
    last_rate_limit_as_of: asOf,
    last_rate_limit_model: model,
  };
  render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
}

/// The window keys the probe saw on a Fable turn, each with a used fraction and
/// a future reset. The offsets carry deliberate slack past the hour and day
/// boundaries: the countdown floors, and `epochFromNow` truncates to whole
/// seconds, so an exactly-4-hour reset renders "in 3 h" and would flip on
/// clock jitter.
function unifiedWindows(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    five_hour: { utilization: 0.33, resetsAt: epochFromNow(4 * 3600 + 60) },
    seven_day: { utilization: 0.27, resetsAt: epochFromNow(5 * 86400 + 3600) },
    ...overrides,
  };
}

/// Claude rate-limit surface with **no `unifiedWindows`** — an older CLI, or a
/// future one that drops the undocumented field. Every payload here exercises
/// the fallback (a bare reset line, no percentage, because there is no
/// percentage to show) plus the overage escalation, which is a separate signal
/// orthogonal to which window shape arrived. Each is gated on its own reset
/// being in the future (reset-passed → clean-hide). Exact clock/date text isn't
/// asserted (jsdom locale/timezone dependent) — only the stable label/copy and
/// presence/absence per the gating rules.
describe("Sidebar Claude rate-limit fallback (no unifiedWindows)", () => {
  it("shows the fallback window independent of overage (normal-quota turn)", async () => {
    // No isUsingOverage — the window must still surface (the bug this fixed:
    // the window used to be gated on overage).
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(4 * 3600) },
      null,
    );
    const window = screen.getByTestId("agent-rate-window");
    expect(window).toHaveTextContent("5-hour limit resets");
    // Not overaging → no amber escalation.
    expect(screen.queryByTestId("agent-overage")).toBeNull();
  });

  it("derives the fallback label from rateLimitType (unknown → generic)", async () => {
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "weekly", resetsAt: epochFromNow(4 * 3600) },
      null,
    );
    // Unknown type falls back to the generic label, never a hardcoded "5-hour".
    const window = screen.getByTestId("agent-rate-window");
    expect(window).toHaveTextContent("rate limit resets");
    expect(window).not.toHaveTextContent("5-hour");
  });

  it("hides the fallback window once its reset is in the past (reset-passed)", async () => {
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
    // Both signals present: neutral fallback line + amber escalation.
    expect(screen.getByTestId("agent-rate-window")).toHaveTextContent("5-hour limit resets");
    const overage = screen.getByTestId("agent-overage");
    expect(overage).toHaveTextContent("using credits");
    expect(overage).toHaveClass("text-warning");
  });

  it("drops the overage escalation once the overage window has passed", async () => {
    // isUsingOverage true, but the overage window elapsed → the credit window
    // has cycled, so the escalation is stale and hidden. The still-future
    // fallback window stays.
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

  it("shows no meter and no percentage on the fallback path", async () => {
    // The point of the fallback: the top-level pair carries a reset but no
    // utilization, and a bar drawn without a value would read as 0% used.
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(4 * 3600) },
      null,
    );
    expect(screen.queryAllByTestId("agent-usage-window")).toHaveLength(0);
    expect(screen.getByTestId("agent-rate-limit-claude")).not.toHaveTextContent("%");
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
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });
    expect(screen.queryByTestId("agent-rate-window")).toBeNull();
    expect(screen.queryByTestId("agent-overage")).toBeNull();
  });
});

/// Claude usage windows — the primary path. `unifiedWindows` carries every
/// window the desktop app shows, each with a 0-1 used fraction, and is
/// authoritative whenever present.
describe("Sidebar Claude usage windows", () => {
  it("renders one meter per window, with the fraction as a percentage", async () => {
    await renderClaudeWithRateLimit({ status: "allowed", unifiedWindows: unifiedWindows() }, null);

    const meters = screen.getAllByTestId("agent-usage-window");
    expect(meters).toHaveLength(2);
    expect(meters[0]).toHaveTextContent("5-hour limit");
    expect(meters[0]).toHaveTextContent("33%");
    expect(meters[1]).toHaveTextContent("Weekly · all models");
    expect(meters[1]).toHaveTextContent("27%");
  });

  it("shows each window's countdown to its own reset", async () => {
    await renderClaudeWithRateLimit({ status: "allowed", unifiedWindows: unifiedWindows() }, null);

    const meters = screen.getAllByTestId("agent-usage-window");
    // Two windows resetting at different distances must not share one
    // countdown; the 5-hour one is hours out and the weekly one days.
    expect(meters[0]).toHaveTextContent("in 4 h");
    expect(meters[1]).toHaveTextContent("in 5 d");
  });

  it("ignores the top-level fallback pair when unifiedWindows is present", async () => {
    // `unifiedWindows` is authoritative, so the bare reset line must not
    // double-render the same window beneath the meters.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        unifiedWindows: unifiedWindows(),
      },
      null,
    );
    expect(screen.getAllByTestId("agent-usage-window")).toHaveLength(2);
    expect(screen.queryByTestId("agent-rate-window")).toBeNull();
  });

  it("labels the per-model weekly window with the model that delivered it", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        unifiedWindows: unifiedWindows({
          seven_day_overage_included: { utilization: 0.79, resetsAt: epochFromNow(5 * 86400) },
        }),
      },
      null,
      "claude-fable-5-1",
    );
    const meters = screen.getAllByTestId("agent-usage-window");
    expect(meters).toHaveLength(3);
    // Order is fixed by the key list, not by object iteration order.
    // The family name the user selected by, not the raw stream id — the label
    // shares a column with a countdown and a percentage.
    expect(meters[2]).toHaveTextContent("Weekly · Fable");
    expect(meters[2]).toHaveTextContent("79%");
  });

  it("falls back to a generic per-model label when no model was observed", async () => {
    // After a reload the sidecar restores the payload but deliberately not the
    // model, so the window must not name one it cannot vouch for.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        unifiedWindows: unifiedWindows({
          seven_day_overage_included: { utilization: 0.79, resetsAt: epochFromNow(5 * 86400) },
        }),
      },
      null,
      undefined,
    );
    const meters = screen.getAllByTestId("agent-usage-window");
    expect(meters[2]).toHaveTextContent("Weekly · model-specific");
  });

  it("fills only the flagged window amber when the CLI reports a threshold", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed_warning",
        rateLimitType: "seven_day_overage_included",
        surpassedThreshold: 0.75,
        unifiedWindows: unifiedWindows({
          seven_day_overage_included: { utilization: 0.79, resetsAt: epochFromNow(5 * 86400) },
        }),
      },
      null,
      "claude-fable-5-1",
    );
    const fills = screen.getAllByTestId("agent-usage-window-fill");
    expect(fills).toHaveLength(3);
    // The tone comes from the harness naming that window, not from a
    // percentage we chose — the 5-hour window at 33% stays calm.
    expect(fills[0]).toHaveClass("bg-fg");
    expect(fills[1]).toHaveClass("bg-fg");
    expect(fills[2]).toHaveClass("bg-warning");
  });

  it("drops a window whose reset has passed and keeps its siblings", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        unifiedWindows: unifiedWindows({
          five_hour: { utilization: 0.33, resetsAt: epochFromNow(-3600) },
        }),
      },
      null,
    );
    const meters = screen.getAllByTestId("agent-usage-window");
    expect(meters).toHaveLength(1);
    expect(meters[0]).toHaveTextContent("Weekly · all models");
  });

  it("drops a window key it does not recognize", async () => {
    // The CLI binary lists keys that are not Claude Code windows on any plan we
    // can probe. A junk label is worse than a dropped window.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        unifiedWindows: unifiedWindows({
          seven_day_cowork: { utilization: 0.5, resetsAt: epochFromNow(5 * 86400) },
        }),
      },
      null,
    );
    expect(screen.getAllByTestId("agent-usage-window")).toHaveLength(2);
    expect(screen.getByTestId("agent-rate-limit-claude")).not.toHaveTextContent("cowork");
  });

  // The input-validation matrix — malformed fractions, missing resets,
  // non-object entries — moved to `usageWindows.test.ts`, which tests the
  // derivation directly. A rendered card can only show those as an absent
  // meter; what stays here is what only a render can prove.

  it("treats an empty window container as absent and falls back", async () => {
    // Nothing reported means the top-level pair is still the best signal
    // available; a blank cell would withhold a reset time we have.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600 + 60),
        unifiedWindows: {},
      },
      null,
    );
    expect(screen.queryAllByTestId("agent-usage-window")).toHaveLength(0);
    expect(screen.getByTestId("agent-rate-window")).toHaveTextContent("5-hour limit resets");
  });

  it("stays authoritative when a non-empty container's windows were all dropped", async () => {
    // The windows were filtered on purpose — this one's reset has passed — so
    // the container still spoke. Falling back to the top-level pair here would
    // override the per-window rule rather than fill a gap.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600 + 60),
        unifiedWindows: { five_hour: { utilization: 0.33, resetsAt: epochFromNow(-3600) } },
      },
      null,
    );
    expect(screen.queryAllByTestId("agent-usage-window")).toHaveLength(0);
    expect(screen.queryByTestId("agent-rate-window")).toBeNull();
    expect(screen.queryByTestId("agent-rate-limit-claude")).toBeNull();
  });

  it("keeps the overage escalation beneath the meters", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        isUsingOverage: true,
        overageResetsAt: epochFromNow(6 * 86400),
        unifiedWindows: unifiedWindows(),
      },
      null,
    );
    expect(screen.getAllByTestId("agent-usage-window")).toHaveLength(2);
    const overage = screen.getByTestId("agent-overage");
    expect(overage).toHaveTextContent("using credits");
    expect(overage).toHaveClass("text-warning");
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
    // The overage window is surfaced here.
    expect(detail).toHaveTextContent("overage window resets");
    // Live snapshot (as_of null) → no snapshot-age line.
    expect(screen.queryByTestId("agent-rate-snapshot")).toBeNull();
  });

  it("spells out each window's percentage and full reset date on hover", async () => {
    // The inline countdown is compressed to fit the card column; the tooltip is
    // where the absolute date and the word "used" have room.
    await renderClaudeWithRateLimit({ status: "allowed", unifiedWindows: unifiedWindows() }, null);
    await fireEvent.pointerEnter(screen.getByTestId("agent-rate-limit-claude"));
    await vi.advanceTimersByTimeAsync(500);
    const detail = await waitFor(() => screen.getByTestId("agent-rate-detail"));
    expect(detail).toHaveTextContent(/5-hour limit: 33% used · resets/);
    expect(detail).toHaveTextContent(/Weekly · all models: 27% used · resets/);
  });

  it("names the threshold the harness flagged, on that window's line only", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed_warning",
        rateLimitType: "seven_day",
        surpassedThreshold: 0.75,
        unifiedWindows: unifiedWindows({
          seven_day: { utilization: 0.79, resetsAt: epochFromNow(5 * 86400 + 3600) },
        }),
      },
      null,
    );
    await fireEvent.pointerEnter(screen.getByTestId("agent-rate-limit-claude"));
    await vi.advanceTimersByTimeAsync(500);
    const detail = await waitFor(() => screen.getByTestId("agent-rate-detail"));
    // Per line, not over the whole tooltip: `textContent` concatenates the
    // paragraphs, so a cross-line regex would match a threshold clause that
    // belongs to the *other* window.
    expect(
      within(detail).getByText(
        /^Weekly · all models: 79% used · resets .+ · above 75% of this limit$/,
      ),
    ).toBeInTheDocument();
    // The unflagged window's line ends at its reset date.
    expect(within(detail).getByText(/^5-hour limit: 33% used · resets .+$/)).toBeInTheDocument();
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
  render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });
}

/// Codex rate-limit windows — both independent windows (primary ~5-hour +
/// secondary weekly) surfaced as gauge lines, each labeled from its
/// `window_minutes` and gated reset-passed. The reset times (incl. the weekly
/// window, days out) live in the tooltip. Class B (session-file-backed), so no
/// snapshot-age line. Closes G8 (secondary window + reset times were dropped).
describe("Sidebar Codex rate-limit windows", () => {
  it("renders both windows as meters carrying the harness-shared labels", async () => {
    await renderCodexWithRateLimit({
      primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) },
      secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
    });
    const meters = screen.getAllByTestId("agent-usage-window");
    expect(meters).toHaveLength(2);
    // window_minutes → the same strings the Claude cell uses, not
    // "primary/secondary" and not a Codex-only vocabulary.
    expect(meters[0]).toHaveTextContent("5-hour limit");
    expect(meters[0]).toHaveTextContent("42%");
    expect(meters[1]).toHaveTextContent("Weekly · all models");
    expect(meters[1]).toHaveTextContent("7%");
  });

  it("renders a bare used_percent as a 'Quota' meter", async () => {
    // A minimal payload — no duration to name the window — still reads as a
    // real gauge rather than disappearing.
    await renderCodexWithRateLimit({ primary: { used_percent: 42.5 } });
    const meter = screen.getByTestId("agent-usage-window");
    expect(meter).toHaveTextContent("Quota");
    expect(meter).toHaveTextContent("43%");
  });

  it("converts Codex's 0-100 percentage to the meter's used fraction", async () => {
    // The conversion happens at the derivation boundary so the meter only ever
    // sees a 0-1 fraction; a missed division would fill the bar at 4200%.
    await renderCodexWithRateLimit({
      primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) },
    });
    expect(screen.getByTestId("agent-usage-window-fill")).toHaveStyle({ width: "42.0%" });
  });

  it("hides a window whose reset has passed (reset-passed), keeps the live one", async () => {
    await renderCodexWithRateLimit({
      primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(-3600) },
      secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
    });
    const meters = screen.getAllByTestId("agent-usage-window");
    expect(meters).toHaveLength(1);
    expect(meters[0]).toHaveTextContent("Weekly · all models");
    expect(meters[0]).toHaveTextContent("7%");
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
      expect(detail).toHaveTextContent(/5-hour limit: 42% used · resets/);
      expect(detail).toHaveTextContent(/Weekly · all models: 7% used · resets/);
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
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CODEX_AGENT] } });
    expect(screen.queryByTestId("agent-cost")).toBeNull();
  });

  it("agent with no metadata renders no cost / quota / rate-limit / context cells", async () => {
    // A freshly-registered agent (no turns, no rate-limit, no meta) — every
    // value-gated cell must be absent, not blank.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    expect(screen.queryByTestId("agent-cost")).toBeNull();
    expect(screen.queryByTestId("agent-rate-limit")).toBeNull();
    expect(screen.queryByTestId("agent-rate-limit-claude")).toBeNull();
    expect(screen.queryByTestId("agent-rate-window")).toBeNull();
    expect(screen.queryByTestId("agent-overage")).toBeNull();
    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
    expect(screen.queryByTestId("agent-meta")).toBeNull();
  });
});

describe("Sidebar transcript diagnostics", () => {
  it("never renders an aggregate transcript-warning indicator", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    expect(screen.queryByTestId("agent-parse-warnings")).not.toBeInTheDocument();
  });
});

// agent-scoped events not crashing the component — direct-listener
// integration covered by index.test.ts already.
describe("Sidebar agent-scoped event tolerance", () => {
  it("does not crash on session_meta / rate_limit_event", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    // The state-module listener is mocked-but-existing; fire events
    // through it by calling the captured callback. Sidebar should
    // re-render with the new runtime values, not crash.
    const meta: NormalizedEvent = {
      type: "session_meta",
      agent_id: CLAUDE_AGENT.id,
      model: "claude-sonnet-4-6",
      harness_version: "2.1.140",
      inventory: {},
      raw: {},
    };
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("unreachable");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      meta: {
        model: meta.model,
        harness_version: meta.harness_version,
        inventory: meta.inventory,
      },
    };
    // Runtime metadata never becomes future-send intent.
    await waitFor(() => {
      expect(screen.getByTestId("agent-selection-default")).toHaveTextContent(
        "Harness/session default",
      );
      expect(screen.queryByTestId("agent-observed-model")).toBeNull();
    });
  });
});

/// Inline rename editor. The card's name swaps to an <input> with live
/// validation; Enter / the save icon commit, Escape / blur cancel (never
/// persist on blur). Entry points are the "Rename" action in the ⋯ menu and a
/// name-only double-click. Commits route through the mocked workspace
/// `renameAgent`; the backend stays authoritative, the frontend check is UX.
describe("Sidebar inline rename", () => {
  async function enterEditViaMenu(agent: AgentRecord): Promise<HTMLInputElement> {
    const state = await loadState();
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });
    await openAgentActions();
    await fireEvent.click(await screen.findByTestId("agent-action-rename"));
    return (await screen.findByTestId("agent-rename-input")) as HTMLInputElement;
  }

  it("the Rename action enters edit mode seeded with the current name", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    expect(input).toHaveValue("alice");
    // The name span is gone while editing.
    expect(screen.queryByTestId("agent-name")).toBeNull();
  });

  it("double-clicking the name text enters rename without changing collapse state", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    const name = screen.getByTestId("agent-name");
    expect(name).not.toHaveAttribute("title");
    await fireEvent.click(name);
    expect(screen.getByTestId("sidebar-agent")).toHaveAttribute("data-collapsed", "false");
    await fireEvent.dblClick(name);
    expect(await screen.findByTestId("agent-rename-input")).toBeInTheDocument();
    await fireEvent.keyDown(screen.getByTestId("agent-rename-input"), { key: "Escape" });
    expect(screen.getByTestId("sidebar-agent")).toHaveAttribute("data-collapsed", "false");
  });

  it("the input is not nested inside the collapse toggle (no nested interactive)", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    expect(input.closest("button")).toBeNull();
  });

  it("Enter commits the new name via renameAgent and exits edit mode", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.keyDown(input, { key: "Enter" });

    expect(renameAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id, "alice2");
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
  });

  it("the save icon commits the new name via renameAgent", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
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
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "  alice2  " } });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(renameAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id, "alice2");
  });

  it("Escape reverts without calling renameAgent", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.keyDown(input, { key: "Escape" });

    expect(renameAgentMock).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
    expect(screen.getByTestId("agent-name")).toHaveTextContent("alice");
  });

  it("blur (click-away) reverts without calling renameAgent — never persists on blur", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.blur(input);

    expect(renameAgentMock).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
    expect(screen.getByTestId("agent-name")).toHaveTextContent("alice");
  });

  it("renaming to the agent's own name (case variant) is allowed — exclude-self", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    // "Alice" canonicalizes to "alice" (the agent's own); exclude-self means it
    // is not a duplicate, and it differs verbatim, so it commits.
    await fireEvent.input(input, { target: { value: "Alice" } });
    expect(screen.getByTestId("agent-rename-save")).not.toBeDisabled();
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(renameAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id, "Alice");
  });

  it("an unchanged name skips the backend round-trip and just exits", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(renameAgentMock).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
  });

  it("a duplicate of another agent disables save and blocks commit", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(CODEX_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    await openAgentActions(0);
    await fireEvent.click(await screen.findByTestId("agent-action-rename"));
    const input = (await screen.findByTestId("agent-rename-input")) as HTMLInputElement;

    await fireEvent.input(input, { target: { value: "bob" } });
    const save = screen.getByTestId("agent-rename-save");
    expect(save).toBeDisabled();
    // The live message rides the input's title tooltip in the cramped card.
    expect(input).not.toHaveAttribute("title");
    expect(input).toHaveAttribute("aria-invalid", "true");

    // Enter is a no-op while invalid; the agent stays in edit mode.
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(renameAgentMock).not.toHaveBeenCalled();
    expect(screen.getByTestId("agent-rename-input")).toBeInTheDocument();
  });

  it("an emptied field disables save without showing a nag message", async () => {
    const input = await enterEditViaMenu(CLAUDE_AGENT);
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

    const input = await enterEditViaMenu(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    await fireEvent.keyDown(input, { key: "Enter" });

    expect(renameAgentMock).toHaveBeenCalledTimes(1);
    resolve?.();
    await waitFor(() => expect(screen.queryByTestId("agent-rename-input")).toBeNull());
  });

  it("a backend rejection keeps edit mode and surfaces the error", async () => {
    renameAgentMock.mockRejectedValueOnce(new Error("registry locked"));
    const input = await enterEditViaMenu(CLAUDE_AGENT);
    await fireEvent.input(input, { target: { value: "alice2" } });
    await fireEvent.keyDown(input, { key: "Enter" });

    const err = await screen.findByTestId("agent-rename-error");
    expect(err).toHaveTextContent("registry locked");
    // Still editing — the agent is kept on the field for a retry.
    expect(screen.getByTestId("agent-rename-input")).toBeInTheDocument();
  });
});

describe("Sidebar pane visibility + assignment", () => {
  async function importPanes() {
    return await import("$lib/state/transcriptPanes.svelte");
  }

  it("eye toggle hides and shows an agent within its pane", async () => {
    const panes = await importPanes();
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const toggles = screen.getAllByTestId("agent-visibility-toggle");
    await fireEvent.click(toggles[0]!);
    expect(
      panes.isAgentHidden(PROJECT_ID, [CLAUDE_AGENT.id, CODEX_AGENT.id], CLAUDE_AGENT.id),
    ).toBe(true);
    await fireEvent.click(toggles[0]!);
    expect(
      panes.isAgentHidden(PROJECT_ID, [CLAUDE_AGENT.id, CODEX_AGENT.id], CLAUDE_AGENT.id),
    ).toBe(false);
  });

  it("alt-click solos the agent; alt-click again restores", async () => {
    const panes = await importPanes();
    const roster = [CLAUDE_AGENT.id, CODEX_AGENT.id];
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const toggles = screen.getAllByTestId("agent-visibility-toggle");
    await fireEvent.click(toggles[0]!, { altKey: true });
    expect(panes.isAgentHidden(PROJECT_ID, roster, CLAUDE_AGENT.id)).toBe(false);
    expect(panes.isAgentHidden(PROJECT_ID, roster, CODEX_AGENT.id)).toBe(true);

    await fireEvent.click(toggles[0]!, { altKey: true });
    expect(panes.hiddenCount(PROJECT_ID, roster)).toBe(0);
  });

  it("shows the hidden count with a Show-all reset only while something is hidden", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    expect(screen.queryByTestId("sidebar-show-all-agents")).not.toBeInTheDocument();

    await fireEvent.click(screen.getAllByTestId("agent-visibility-toggle")[1]!);
    const reset = screen.getByTestId("sidebar-show-all-agents");
    expect(reset).toHaveTextContent("1 hidden");

    await fireEvent.click(reset);
    expect(screen.queryByTestId("sidebar-show-all-agents")).not.toBeInTheDocument();
  });

  it("offers Move to new pane for multi-agent projects and moves the agent", async () => {
    const panes = await importPanes();
    const selection = await import("$lib/state/recipientSelection.svelte");
    const roster = [CLAUDE_AGENT.id, CODEX_AGENT.id];
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    const menu = await openAgentActions(1);
    await fireEvent.click(within(menu).getByTestId("agent-move-to-new-pane"));

    const layout = panes.layoutFor(PROJECT_ID, roster);
    expect(layout.panes).toHaveLength(2);
    expect(layout.panes[1]!.members).toEqual([CODEX_AGENT.id]);
    expect(layout.panes[0]!.members).toEqual([CLAUDE_AGENT.id]);
    // Adding the agent to a pane selects its compose chip.
    expect(selection.selectionFor(PROJECT_ID)).toEqual([CODEX_AGENT.id]);
  });

  it("lists other panes as move targets once split, excluding the agent's own", async () => {
    const panes = await importPanes();
    const selection = await import("$lib/state/recipientSelection.svelte");
    const roster = [CLAUDE_AGENT.id, CODEX_AGENT.id];
    const newPane = panes.moveAgentToNewPane(PROJECT_ID, roster, CODEX_AGENT.id);
    const pane1 = panes.layoutFor(PROJECT_ID, roster).panes[0]!.id;
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, CODEX_AGENT] } });

    // alice (pane 1) can move to pane 2 — but not to her own pane.
    const menu = await openAgentActions(0);
    expect(within(menu).queryByTestId(`agent-move-to-pane-${pane1}`)).not.toBeInTheDocument();
    await fireEvent.click(within(menu).getByTestId(`agent-move-to-pane-${newPane}`));

    expect(panes.layoutFor(PROJECT_ID, roster).panes[1]!.members).toEqual([
      CODEX_AGENT.id,
      CLAUDE_AGENT.id,
    ]);
    // The direct move-to-new-pane in setup is a pure layout op (no selection
    // change); only the menu gesture selects the moved agent's compose chip.
    expect(selection.selectionFor(PROJECT_ID)).toEqual([CLAUDE_AGENT.id]);
  });

  it("hides Move to new pane for a single-agent project", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    const menu = await openAgentActions(0);
    expect(within(menu).queryByTestId("agent-move-to-new-pane")).not.toBeInTheDocument();
  });
});

describe("Sidebar — agent reordering", () => {
  // A completed or cancelled drag arms a capture-phase `click` listener on
  // `window` to swallow the synthesized click, and removes it on a `setTimeout(0)`
  // macrotask. A test that ends right after `pointerUp` never lets that macrotask
  // run, so the listener outlives the test and eats the first click of the next
  // one — which manifests as an unrelated menu that mysteriously won't open.
  // Draining one macrotask here lets the guard disarm itself.
  afterEach(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  const THREE_AGENTS = [CLAUDE_AGENT, CODEX_AGENT, ANTIGRAVITY_AGENT];

  function grip(index: number): HTMLElement {
    const grips = screen.getAllByTestId("agent-drag-grip");
    const el = grips.at(index);
    if (el === undefined) throw new Error("expected a drag grip");
    return el;
  }

  it("keeps reorder actions out of the agent menu", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const menu = await openAgentActions(1);
    expect(within(menu).queryByTestId("agent-move-up")).not.toBeInTheDocument();
    expect(within(menu).queryByTestId("agent-move-down")).not.toBeInTheDocument();
  });

  it("hides the drag grip for a single-agent roster", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    expect(screen.queryAllByTestId("agent-drag-grip")).toHaveLength(0);
  });

  it("a plain drag-grip click does not toggle the card", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const card = screen.getAllByTestId("sidebar-agent")[0]!;
    expect(card).toHaveAttribute("data-collapsed", "false");
    await fireEvent.click(grip(0));
    expect(card).toHaveAttribute("data-collapsed", "false");
  });

  it("Alt+ArrowDown with focus inside a card moves that agent down", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const names = screen.getAllByTestId("agent-name");
    await fireEvent.keyDown(names[1]!, { key: "ArrowDown", altKey: true });
    expect(reorderAgentsMock).toHaveBeenCalledWith(PROJECT_ID, [
      CLAUDE_AGENT.id,
      ANTIGRAVITY_AGENT.id,
      CODEX_AGENT.id,
    ]);
  });

  it("Alt+ArrowUp at the top is a no-op", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const names = screen.getAllByTestId("agent-name");
    await fireEvent.keyDown(names[0]!, { key: "ArrowUp", altKey: true });
    expect(reorderAgentsMock).not.toHaveBeenCalled();
  });

  // macOS WebKit does not focus buttons on click, so the chord must also work
  // scoped to the card under the pointer, with no focus involved.
  it("Alt+ArrowDown moves the hovered card when nothing in it has focus", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const cards = screen.getAllByTestId("sidebar-agent");
    await fireEvent.pointerEnter(cards[1]!);
    await fireEvent.keyDown(window, { key: "ArrowDown", altKey: true });
    expect(reorderAgentsMock).toHaveBeenCalledWith(PROJECT_ID, [
      CLAUDE_AGENT.id,
      ANTIGRAVITY_AGENT.id,
      CODEX_AGENT.id,
    ]);
  });

  it("Alt+Arrow does nothing when no card is hovered or focused", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const cards = screen.getAllByTestId("sidebar-agent");
    await fireEvent.pointerEnter(cards[1]!);
    await fireEvent.pointerLeave(cards[1]!);
    await fireEvent.keyDown(window, { key: "ArrowDown", altKey: true });
    expect(reorderAgentsMock).not.toHaveBeenCalled();
  });

  it("surfaces a rejected reorder as an inline error on the moved card", async () => {
    reorderAgentsMock.mockRejectedValue(new Error("roster changed"));
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    await fireEvent.keyDown(screen.getAllByTestId("agent-name")[0]!, {
      key: "ArrowDown",
      altKey: true,
    });
    const error = await screen.findByTestId("agent-reorder-error");
    expect(error).toHaveTextContent("roster changed");
    // The error renders under the card that was moved.
    expect(error.closest("[data-agent-id]")).toHaveAttribute("data-agent-id", CLAUDE_AGENT.id);
  });

  // jsdom has no real layout (every rect is zero-height), so a drag past the
  // slop threshold deterministically resolves to "after every other card" —
  // enough to exercise the gesture wiring (slop gate, commit, cancel). The
  // midpoint math itself is covered by the agentReorder unit tests.
  it("grip drag past the slop threshold commits an order on release", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const handle = grip(0);
    await fireEvent.pointerDown(handle, { pointerId: 1, button: 0, clientX: 0, clientY: 0 });
    await fireEvent.pointerMove(handle, { pointerId: 1, clientX: 0, clientY: 100 });
    await fireEvent.pointerUp(handle, { pointerId: 1 });
    expect(reorderAgentsMock).toHaveBeenCalledWith(PROJECT_ID, [
      CODEX_AGENT.id,
      ANTIGRAVITY_AGENT.id,
      CLAUDE_AGENT.id,
    ]);
  });

  it("a grip press below the slop threshold commits nothing", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const handle = grip(0);
    await fireEvent.pointerDown(handle, { pointerId: 1, button: 0, clientX: 0, clientY: 0 });
    await fireEvent.pointerMove(handle, { pointerId: 1, clientX: 1, clientY: 2 });
    await fireEvent.pointerUp(handle, { pointerId: 1 });
    expect(reorderAgentsMock).not.toHaveBeenCalled();
  });

  it("Escape cancels an in-flight drag without committing", async () => {
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: THREE_AGENTS } });
    const handle = grip(0);
    await fireEvent.pointerDown(handle, { pointerId: 1, button: 0, clientX: 0, clientY: 0 });
    await fireEvent.pointerMove(handle, { pointerId: 1, clientX: 0, clientY: 100 });
    await fireEvent.keyDown(window, { key: "Escape" });
    await fireEvent.pointerUp(handle, { pointerId: 1 });
    expect(reorderAgentsMock).not.toHaveBeenCalled();
  });
});

describe("compact context action", () => {
  it("is offered for a Claude agent", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const menu = await openAgentActions();
    expect(within(menu).getByTestId("agent-action-compact")).toBeInTheDocument();
  });

  it.each([
    ["codex", CODEX_AGENT],
    ["antigravity", ANTIGRAVITY_AGENT],
  ])("is withheld from a %s agent", async (_harness, agent) => {
    // The capability gate, on the surface a user actually touches. Offering it
    // here would produce a refusal at best — and the reason the refusal has to
    // be real is that a `/compact` *prompt* to these harnesses returns a
    // model-authored claim of success while nothing compacted.
    const state = await loadState();
    await state.registerAgent(agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const menu = await openAgentActions();
    expect(within(menu).queryByTestId("agent-action-compact")).toBeNull();
    // The menu did render — an empty one would pass the assertion above for the
    // wrong reason.
    expect(within(menu).getByTestId("agent-action-rename")).toBeInTheDocument();
  });

  it("dispatches a compaction and registers its pending entry", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    await openAgentActions();
    await fireEvent.click(await screen.findByTestId("agent-action-compact"));

    await waitFor(() => expect(compactAgentMock).toHaveBeenCalledTimes(1));
    const [agentId, sendId] = compactAgentMock.mock.calls[0]!;
    expect(agentId).toBe(CLAUDE_AGENT.id);
    // The send id is minted here so the queued row has something to cancel with
    // before any `turn_start` carries it back.
    expect(sendId).toMatch(/^[0-9a-f-]{36}$/);
    await waitFor(() => {
      const pending = state.runtimes[CLAUDE_AGENT.id]?.pending_sends ?? [];
      expect(pending).toHaveLength(1);
      expect(pending[0]?.kind).toBe("compaction");
      expect(pending[0]?.send_id).toBe(sendId);
      expect(pending[0]?.queued_at).toBeDefined();
    });
  });

  it("is still offered while the agent is busy, because a compaction queues", async () => {
    // Decision 1. Greying it out would refuse something the backend accepts, and
    // would push the user into watching the agent to catch it idle.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    state.dispatchUserTurn(
      CLAUDE_AGENT.id,
      "00000000-0000-7000-8000-000000000001",
      "go",
      [],
      "00000000-0000-7000-8000-0000000000d1",
      "2026-05-16T00:00:00Z",
    );

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    const menu = await openAgentActions();
    const item = within(menu).getByTestId("agent-action-compact");
    expect(item).toBeInTheDocument();
    expect(item).not.toHaveAttribute("aria-disabled", "true");
  });

  it("moves the context bar to the compaction's post-compaction occupancy", async () => {
    // The user-visible payoff of the whole feature: the bar has to drop. It
    // reads the latest terminal turn carrying usage, and a compaction is exactly
    // that — no sidebar change was needed, which this pins.
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
          input_tokens: 10,
          output_tokens: 5,
          context_input_tokens: 120_000,
          context_tokens_after_turn: 120_000,
          context_window: 200_000,
        },
      },
      {
        role: "agent",
        turn_id: "turn-compaction",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:02Z",
        ended_at: "2026-05-16T00:00:03Z",
        status: "complete",
        kind: "compaction",
        items: [],
        usage: {
          input_tokens: 0,
          output_tokens: 0,
          context_input_tokens: 120_000,
          context_tokens_after_turn: 20_000,
          context_window: 200_000,
        },
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const bar = screen.getByTestId("agent-context-bar");
    expect(bar).toHaveTextContent("Context used");
    expect(bar).toHaveTextContent("20k / 200k");
    expect(bar).toHaveTextContent("10%");
  });

  it("leaves the bar unchanged after a compaction that carried no usage", async () => {
    // A *refused* compaction withholds usage entirely, so the previous turn's
    // number is still the truth. Blanking the bar would tell the user their
    // context is unknown when nothing about it changed.
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
          input_tokens: 10,
          output_tokens: 5,
          context_input_tokens: 120_000,
          context_tokens_after_turn: 120_000,
          context_window: 200_000,
        },
      },
      {
        role: "agent",
        turn_id: "turn-compaction",
        agent_id: CLAUDE_AGENT.id,
        started_at: "2026-05-16T00:00:02Z",
        ended_at: "2026-05-16T00:00:03Z",
        status: "failed",
        kind: "compaction",
        items: [],
        error: "Not enough messages to compact.",
      },
    ];

    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const bar = screen.getByTestId("agent-context-bar");
    expect(bar).toHaveTextContent("120k / 200k");
    expect(bar).toHaveTextContent("60%");
  });
});

describe("context breakdown", () => {
  function seedContextBar(state: Awaited<ReturnType<typeof loadState>>, agent: AgentRecord): void {
    state.transcripts[agent.id] = [
      {
        role: "agent",
        turn_id: `turn-1-${agent.id}`,
        agent_id: agent.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 10,
          output_tokens: 5,
          context_input_tokens: 120_000,
          context_tokens_after_turn: 120_000,
          context_window: 200_000,
        },
      },
    ];
  }

  it("offers the chevron beside the meter it explains, for a Claude agent", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedContextBar(state, CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const bar = await screen.findByTestId("agent-context-bar");
    expect(within(bar).getByTestId("agent-context-breakdown-button")).toBeInTheDocument();
  });

  it.each([
    ["codex", CODEX_AGENT],
    ["antigravity", ANTIGRAVITY_AGENT],
  ])("withholds the chevron and the menu item from a %s agent", async (_harness, agent) => {
    // Both harnesses report context, so the bar renders and only the
    // affordances must be absent — which is what makes this a gate test rather
    // than an absent-bar test. What stands behind the gate is a `/context`
    // prompt answered with invented figures.
    const state = await loadState();
    await state.registerAgent(agent);
    seedContextBar(state, agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const bar = await screen.findByTestId("agent-context-bar");
    expect(within(bar).queryByTestId("agent-context-breakdown-button")).toBeNull();
    const menu = await openAgentActions();
    expect(within(menu).queryByTestId("agent-action-context-breakdown")).toBeNull();
  });

  it("opens the panel from the chevron, with the agent's own report", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedContextBar(state, CLAUDE_AGENT);
    const runtime = state.runtimes[CLAUDE_AGENT.id];
    if (runtime === undefined) throw new Error("expected a runtime");
    state.runtimes[CLAUDE_AGENT.id] = {
      ...runtime,
      last_context_report: {
        model: "claude-fable-5-1",
        total_tokens: 48_000,
        max_tokens: 200_000,
        categories: [{ name: "Messages", tokens: 48_000, kind: "used" }],
        raw: "## Context Usage",
      },
    };
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await fireEvent.click(await screen.findByTestId("agent-context-breakdown-button"));

    const panel = await screen.findByTestId("context-breakdown");
    expect(within(panel).getByTestId("context-breakdown-usage")).toHaveTextContent("48k / 200k");
    expect(within(panel).queryByTestId("context-breakdown-empty")).toBeNull();
  });

  it("opens from the agent menu with an empty state before anything has measured it", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const menu = await openAgentActions();
    await fireEvent.click(within(menu).getByTestId("agent-action-context-breakdown"));

    expect(await screen.findByTestId("context-breakdown-empty")).toBeInTheDocument();
  });

  it("dispatches a report when the panel asks for one", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    contextReportAgentMock.mockResolvedValue("00000000-0000-7000-8000-00000000d0aa");
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });
    const menu = await openAgentActions();
    await fireEvent.click(within(menu).getByTestId("agent-action-context-breakdown"));

    await fireEvent.click(await screen.findByTestId("context-breakdown-refresh"));

    await waitFor(() =>
      expect(contextReportAgentMock).toHaveBeenCalledWith(CLAUDE_AGENT.id, expect.any(String)),
    );
  });
});

describe("compact button on the context bar", () => {
  /// The button lives inside the context bar, which only renders once a turn
  /// has reported occupancy — so every test here needs one.
  function seedContextBar(state: Awaited<ReturnType<typeof loadState>>, agent: AgentRecord): void {
    state.transcripts[agent.id] = [
      {
        role: "agent",
        turn_id: `turn-1-${agent.id}`,
        agent_id: agent.id,
        started_at: "2026-05-16T00:00:00Z",
        ended_at: "2026-05-16T00:00:01Z",
        status: "complete",
        items: [],
        usage: {
          input_tokens: 10,
          output_tokens: 5,
          context_input_tokens: 120_000,
          context_tokens_after_turn: 120_000,
          context_window: 200_000,
        },
      },
    ];
  }

  it("is offered for a Claude agent, beside the bar it acts on", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedContextBar(state, CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const bar = await screen.findByTestId("agent-context-bar");
    expect(within(bar).getByTestId("agent-compact-button")).toBeInTheDocument();
  });

  it.each([
    ["codex", CODEX_AGENT],
    ["antigravity", ANTIGRAVITY_AGENT],
  ])("is withheld from a %s agent", async (_harness, agent) => {
    // The same capability gate the menu item carries. Both harnesses report
    // context, so the bar renders and only the button must be absent — which is
    // what makes this a real gate test rather than an absent-bar test.
    const state = await loadState();
    await state.registerAgent(agent);
    seedContextBar(state, agent);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [agent] } });

    const bar = await screen.findByTestId("agent-context-bar");
    expect(within(bar).queryByTestId("agent-compact-button")).toBeNull();
  });

  it("does not render for a Claude agent whose context is unknown", async () => {
    // No bar, no button: the affordance is anchored to the measurement, and a
    // fresh agent pre-first-turn has none.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    await screen.findAllByTestId("agent-actions-trigger");
    expect(screen.queryByTestId("agent-context-bar")).toBeNull();
    expect(screen.queryByTestId("agent-compact-button")).toBeNull();
  });

  it("arms on the first click without dispatching", async () => {
    // The safety property the whole two-step exists for: a compaction spends a
    // turn's tokens and cannot be undone, so one stray click must cost nothing.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedContextBar(state, CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const button = await screen.findByTestId("agent-compact-button");
    expect(button).toHaveAttribute("data-armed", "false");
    await fireEvent.click(button);

    await waitFor(() =>
      expect(screen.getByTestId("agent-compact-button")).toHaveAttribute("data-armed", "true"),
    );
    expect(compactAgentMock).not.toHaveBeenCalled();
    // The label moves with the state, so a screen-reader user is told what the
    // next click does rather than being handed the same button twice.
    expect(screen.getByTestId("agent-compact-button")).toHaveAccessibleName("Compact now");
  });

  it("dispatches on the second click and returns to rest", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedContextBar(state, CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const button = await screen.findByTestId("agent-compact-button");
    await fireEvent.click(button);
    await fireEvent.click(await screen.findByTestId("agent-compact-button"));

    await waitFor(() => expect(compactAgentMock).toHaveBeenCalledTimes(1));
    const [agentId, sendId] = compactAgentMock.mock.calls[0]!;
    expect(agentId).toBe(CLAUDE_AGENT.id);
    expect(sendId).toMatch(/^[0-9a-f-]{36}$/);
    await waitFor(() => {
      const pending = state.runtimes[CLAUDE_AGENT.id]?.pending_sends ?? [];
      expect(pending).toHaveLength(1);
      expect(pending[0]?.kind).toBe("compaction");
      expect(pending[0]?.send_id).toBe(sendId);
    });
    // Disarmed after firing, so the next click starts the two-step over rather
    // than queueing a second compaction on one more click.
    await waitFor(() =>
      expect(screen.getByTestId("agent-compact-button")).toHaveAttribute("data-armed", "false"),
    );
  });

  it("disarms when the pointer leaves, so a later click re-arms instead of firing", async () => {
    // Walking away is the undo. Without it the button stays armed indefinitely
    // and a click minutes later — on what looks like an ordinary icon — spends
    // a turn.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedContextBar(state, CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const button = await screen.findByTestId("agent-compact-button");
    await fireEvent.click(button);
    await fireEvent.pointerLeave(screen.getByTestId("agent-compact-button"));

    await waitFor(() =>
      expect(screen.getByTestId("agent-compact-button")).toHaveAttribute("data-armed", "false"),
    );
    await fireEvent.click(screen.getByTestId("agent-compact-button"));
    await waitFor(() =>
      expect(screen.getByTestId("agent-compact-button")).toHaveAttribute("data-armed", "true"),
    );
    expect(compactAgentMock).not.toHaveBeenCalled();
  });

  it("disarms on blur, so a keyboard user who tabs away doesn't leave it armed", async () => {
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedContextBar(state, CLAUDE_AGENT);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const button = await screen.findByTestId("agent-compact-button");
    await fireEvent.click(button);
    await fireEvent.blur(screen.getByTestId("agent-compact-button"));

    await waitFor(() =>
      expect(screen.getByTestId("agent-compact-button")).toHaveAttribute("data-armed", "false"),
    );
    expect(compactAgentMock).not.toHaveBeenCalled();
  });

  it("arms one agent at a time", async () => {
    // Two Claude agents: arming one must not arm the other, or a confirm click
    // aimed at the first would fire the second.
    const state = await loadState();
    const second: AgentRecord = {
      ...CLAUDE_AGENT,
      id: "00000000-0000-7000-8000-00000000ca02",
      name: "claude-two",
    };
    await state.registerAgent(CLAUDE_AGENT);
    await state.registerAgent(second);
    seedContextBar(state, CLAUDE_AGENT);
    seedContextBar(state, second);
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT, second] } });

    const buttons = await screen.findAllByTestId("agent-compact-button");
    expect(buttons).toHaveLength(2);
    await fireEvent.click(buttons[0]!);

    await waitFor(() => {
      const [first, other] = screen.getAllByTestId("agent-compact-button");
      expect(first).toHaveAttribute("data-armed", "true");
      expect(other).toHaveAttribute("data-armed", "false");
    });

    // Clicking the other one arms it — it does not inherit the first's confirm.
    await fireEvent.click(screen.getAllByTestId("agent-compact-button")[1]!);
    await waitFor(() =>
      expect(screen.getAllByTestId("agent-compact-button")[1]!).toHaveAttribute(
        "data-armed",
        "true",
      ),
    );
    expect(compactAgentMock).not.toHaveBeenCalled();
  });

  it("queues like the menu action while the agent is busy", async () => {
    // Decision 1 on this surface too: never disabled mid-turn, because the
    // backend accepts it and queues it.
    const state = await loadState();
    await state.registerAgent(CLAUDE_AGENT);
    seedContextBar(state, CLAUDE_AGENT);
    state.dispatchUserTurn(
      CLAUDE_AGENT.id,
      "00000000-0000-7000-8000-000000000001",
      "go",
      [],
      "00000000-0000-7000-8000-0000000000d1",
      "2026-05-16T00:00:00Z",
    );
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [CLAUDE_AGENT] } });

    const button = await screen.findByTestId("agent-compact-button");
    expect(button).not.toBeDisabled();
    await fireEvent.click(button);
    await fireEvent.click(await screen.findByTestId("agent-compact-button"));

    await waitFor(() => expect(compactAgentMock).toHaveBeenCalledTimes(1));
  });
});
