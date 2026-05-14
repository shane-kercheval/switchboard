import { vi, describe, it, expect, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import type { AgentRecord, DirectoryInfo, ProjectSummary } from "$lib/types";

// App.svelte tests focus narrowly on the primary phase-transition routing
// of the M1 acceptance flow:
//
//   welcome → directory-selector → no-agent → active
//
// Plus the cancel-back-to-welcome path. Per-screen UI is already covered by
// AgentPane / DirectorySelector / ComposeBar / reducer tests; these tests
// verify the App-level orchestration that glues them together (which IPC
// commands fire, in what order, and which phase renders next).
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

  it("no-agent → active: submitting Create agent invokes create_agent and renders AgentPane", async () => {
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

    await waitFor(() => {
      // AgentPane renders the agent's name in its header.
      expect(screen.getByText("assistant")).toBeInTheDocument();
      // ComposeBar renders inside AgentPane — proves the active phase is mounted.
      expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
    });
    const createAgentCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "create_agent");
    expect(createAgentCalls).toHaveLength(1);
    expect(createAgentCalls[0]?.[1]).toEqual({ name: "assistant" });
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
});
