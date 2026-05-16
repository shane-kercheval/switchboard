import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type { AgentRecord, DirectoryInfo, ProjectSummary } from "$lib/types";

// App.svelte tests focus narrowly on the primary phase-transition routing
// of the M2.5 acceptance flow:
//
//   welcome → directory-selector → no-agent → loaded
//
// Plus the cancel-back-to-welcome path. Per-screen UI is already covered by
// Sidebar / UnifiedTranscript / ComposeBar / DirectorySelector tests; these
// tests verify the App-level orchestration that glues them together (which
// IPC commands fire, in what order, and which phase renders next).
//
// Edge cases (cancel error states, multi-project switcher, rebind UI
// state-reset across phases) are deliberately deferred to M4 when the
// project switcher exercises them harder.

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);
const listenMock = vi.fn(
  async (_event: string, _handler: unknown): Promise<() => void> =>
    () => {},
);
const openDialogMock = vi.fn(async (_options: unknown): Promise<string | null> => null);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: unknown) => listenMock(event, handler),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (options: unknown) => openDialogMock(options),
}));

const PATH = "/tmp/sw-test";
const PROJECT: ProjectSummary = {
  id: "11111111-1111-7000-8000-111111111111",
  name: "alpha",
  created_at: "2026-05-13T00:00:00Z",
};
const AGENT: AgentRecord = {
  id: "22222222-2222-7000-8000-222222222222",
  project_id: PROJECT.id,
  name: "assistant",
  harness: "claude_code",
  session_id: "33333333-3333-7000-8000-333333333333",
  created_at: "2026-05-13T00:00:01Z",
};

const INFO_NO_SWITCHBOARD: DirectoryInfo = {
  path: PATH,
  has_switchboard: false,
  projects: [],
};

/**
 * Drives `invokeMock` with a per-command lookup so tests can be declarative
 * about which commands succeed (and with what return value) without
 * assuming any specific call order — App.svelte may legitimately reorder
 * sub-steps within a phase transition.
 */
function setInvokeResponses(map: Record<string, unknown>): void {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (!(cmd in map)) {
      throw new Error(`unexpected invoke call: ${cmd}`);
    }
    return map[cmd];
  });
}

describe("App", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    openDialogMock.mockReset();
  });

  // The state-rune module is a singleton; tests that register agents
  // would otherwise leak state across runs. Drain via the test-only
  // reset helper.
  afterEach(async () => {
    const { _testing } = await import("$lib/state/index.svelte");
    _testing.reset();
  });

  it("mounts and renders the welcome screen when the binary check succeeds", async () => {
    setInvokeResponses({ check_claude_binary: null });
    const App = (await import("./App.svelte")).default;
    render(App);
    expect(screen.getByText("Switchboard")).toBeInTheDocument();
    expect(screen.getByText("Open working directory")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.queryByTestId("binary-not-found-banner")).not.toBeInTheDocument();
    });
  });

  it("renders the binary-not-found banner when the startup probe fails", async () => {
    invokeMock.mockRejectedValueOnce(new Error("harness probe failed: harness binary not found"));
    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => {
      expect(screen.getByTestId("binary-not-found-banner")).toBeInTheDocument();
    });
  });

  it("welcome → directory-selector: clicking Open working directory invokes pick_directory and renders the selector", async () => {
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => {
      expect(screen.getByText("Open working directory")).toBeInTheDocument();
    });

    await fireEvent.click(screen.getByText("Open working directory"));

    await waitFor(() => {
      expect(screen.getByText("Working directory")).toBeInTheDocument();
    });
    // Asserts on the IPC sequence — would catch a handler that rendered the
    // right phase but skipped the inspect call (or vice versa).
    expect(openDialogMock).toHaveBeenCalledOnce();
    const pickCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "pick_directory");
    expect(pickCalls).toHaveLength(1);
    expect(pickCalls[0]?.[1]).toEqual({ path: PATH });
  });

  it("directory-selector → no-agent: confirming Initialize fires init_directory + create_project + set_active_project + list_agents, then renders Create agent", async () => {
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("confirm-init"));

    await waitFor(() => {
      expect(screen.getByText("Create an agent")).toBeInTheDocument();
    });
    // The IPC sequence must include all four backend steps. A handler that
    // skipped any one (e.g., forgot to call set_active_project) would render
    // the right next-phase but leave AppState mis-configured.
    const cmds = invokeMock.mock.calls.map(([cmd]) => cmd);
    expect(cmds).toContain("init_directory");
    expect(cmds).toContain("create_project");
    expect(cmds).toContain("set_active_project");
    expect(cmds).toContain("list_agents");
  });

  it("no-agent → loaded: submitting Create agent invokes create_agent and renders the 3-pane layout", async () => {
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: AGENT,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-init"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("confirm-create-agent"));

    // The "loaded" phase renders Sidebar + UnifiedTranscript + ComposeBar.
    await waitFor(() => {
      expect(screen.getByTestId("loaded-layout")).toBeInTheDocument();
      expect(screen.getByTestId("sidebar")).toBeInTheDocument();
      expect(screen.getByTestId("unified-transcript")).toBeInTheDocument();
      expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
    });
    // Sidebar surfaces the agent's name.
    expect(screen.getByTestId("agent-name")).toHaveTextContent("assistant");

    const createAgentCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "create_agent");
    expect(createAgentCalls).toHaveLength(1);
    expect(createAgentCalls[0]?.[1]).toEqual({ name: "assistant", harness: "claude_code" });
  });

  it("directory-selector → welcome: clicking Cancel returns to the welcome screen", async () => {
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByText("Working directory")).toBeInTheDocument());

    // Cancel from the directory-selector phase. M1.5 user flow — not deferred
    // to M4 — so it gets a dedicated assertion.
    const cancels = screen.getAllByRole("button", { name: /cancel/i });
    await fireEvent.click(cancels[cancels.length - 1]!);

    await waitFor(() => {
      expect(screen.getByText("Open working directory")).toBeInTheDocument();
      expect(screen.queryByText("Working directory")).not.toBeInTheDocument();
    });
  });

  // M2.5 plan's "dynamic agent add" acceptance test. The first agent's
  // session is already loaded; the user opens the sidebar "+" entry point,
  // submits, and the new agent appears in the sidebar (and is registered
  // in the state module so its events would flow on dispatch). The
  // load-bearing property: the new agent's listener is wired before the
  // phase transition completes, so an immediate dispatch wouldn't lose
  // the first event.
  it("loaded → add agent via sidebar modal: appends to phase.agents and registers a listener", async () => {
    const SECOND_AGENT: AgentRecord = {
      id: "44444444-4444-7000-8000-444444444444",
      project_id: PROJECT.id,
      name: "second",
      harness: "codex",
      session_id: null,
      created_at: "2026-05-13T00:00:02Z",
    };
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: AGENT,
    });
    openDialogMock.mockResolvedValueOnce(PATH);

    // Walk through to the loaded phase with one agent already present.
    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => expect(screen.getByText("Open working directory")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("Open working directory"));
    await waitFor(() => expect(screen.getByTestId("confirm-init")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-init"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    await waitFor(() => expect(screen.getByTestId("loaded-layout")).toBeInTheDocument());

    // listen() count at the loaded boundary — registerAgent wires one
    // listener per agent. Captures the baseline so we can assert the
    // second-agent registration adds exactly one more.
    const listenCallsBeforeAdd = listenMock.mock.calls.length;

    // Now swap create_agent to return the second agent for the next call.
    // (setInvokeResponses replaces the dispatch table; preserve everything
    // else.)
    setInvokeResponses({
      check_claude_binary: null,
      pick_directory: INFO_NO_SWITCHBOARD,
      init_directory: { ...INFO_NO_SWITCHBOARD, has_switchboard: true },
      create_project: PROJECT,
      set_active_project: null,
      list_agents: [],
      create_agent: SECOND_AGENT,
    });

    // Sidebar's "+" opens the modal. The modal renders a CreateAgentForm
    // (embedded variant — same data-testid="confirm-create-agent").
    await fireEvent.click(screen.getByTestId("sidebar-add-agent"));
    await waitFor(() => expect(screen.getByTestId("dialog-content")).toBeInTheDocument());
    // Scope queries to the modal — Sidebar also uses `data-testid="agent-name"`
    // for each agent row, so a global getByTestId("agent-name") would match
    // multiple after the modal opens.
    const modal = screen.getByTestId("dialog-content");

    // Default form values (mode=create, harness=claude_code, name=assistant)
    // would collide with the existing agent's name. Change the name to
    // something unique before submitting.
    const nameInput = within(modal).getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "second" } });
    await fireEvent.click(within(modal).getByTestId("harness-codex"));
    await fireEvent.click(within(modal).getByTestId("confirm-create-agent"));

    // The new agent should land in the sidebar (immutable phase update).
    // Both agents render side-by-side; assert the second is now present.
    await waitFor(() => {
      const names = screen.getAllByTestId("agent-name");
      expect(names.some((n) => n.textContent === "second")).toBe(true);
    });

    // Modal closes after successful submission.
    expect(screen.queryByTestId("dialog-content")).not.toBeInTheDocument();

    // create_agent IPC fired with the form's values (proves the modal's
    // submission was correctly threaded through createOrAttachAndRegister).
    const createAgentCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "create_agent");
    expect(createAgentCalls).toHaveLength(2); // first agent + this one
    expect(createAgentCalls[1]?.[1]).toEqual({ name: "second", harness: "codex" });

    // Exactly one additional listener was registered — the load-bearing
    // property for "immediately dispatch and no events miss." A regression
    // that updated phase.agents without calling registerAgent would still
    // render the sidebar but silently drop events.
    expect(listenMock.mock.calls.length).toBe(listenCallsBeforeAdd + 1);
    expect(listenMock.mock.calls.at(-1)?.[0]).toBe(`agent:${SECOND_AGENT.id}`);
  });
});
