import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import type { AgentRecord, ProjectListing } from "$lib/types";

const invokeMock = vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>(
  async () => undefined,
);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

async function loadState() {
  return await import("$lib/state/index.svelte");
}
async function loadWorkspace() {
  return await import("$lib/state/workspace.svelte");
}

const observerStops: (() => void)[] = [];

async function startActivityObserver(
  getNow: () => string = () => "2026-05-25T12:00:00.000Z",
): Promise<void> {
  const ws = await loadWorkspace();
  observerStops.push(ws.startProjectActivityObserver(getNow));
}

const PROJECT_1 = "00000000-0000-7000-8000-0000000000f1";
const PROJECT_2 = "00000000-0000-7000-8000-0000000000f2";
const AGENT_1 = "00000000-0000-7000-8000-00000000000a";

function project(id: string): ProjectListing {
  return {
    id,
    name: `proj-${id.slice(-2)}`,
    created_at: "2026-05-16T00:00:00Z",
    directory: `/work/${id.slice(-2)}`,
    available: true,
    last_activity: "2026-05-16T00:00:00Z",
  };
}

function agent(id: string, projectId: string): AgentRecord {
  return {
    id,
    project_id: projectId,
    name: `agent-${id.slice(-1)}`,
    harness: "claude_code",
    session_id: null,
    created_at: "2026-05-16T00:00:00Z",
  };
}

const noopProps = {
  onAddProject: () => {},
  onOpenSettings: () => {},
  onProjectSelect: () => {},
  onToggleSidebar: () => {},
};

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
});

afterEach(async () => {
  for (const stop of observerStops.splice(0)) stop();
  const state = await loadState();
  state._testing.reset();
  const ws = await loadWorkspace();
  ws._testing.reset();
});

/// Seed a project with one agent that has a live (pending) send.
async function seedBusyProject(projectId: string): Promise<void> {
  const state = await loadState();
  const ws = await loadWorkspace();
  ws.projects.list = [project(projectId)];
  const a = agent(AGENT_1, projectId);
  ws.agentsByProject[projectId] = [a];
  await state.registerAgent(a);
  state.dispatchUserTurn(AGENT_1, "user-1", "go", "send-1", "2026-05-16T00:00:00Z");
}

describe("ProjectsSidebar — background activity", () => {
  it("shows a cancel control for a project with live sends", async () => {
    await seedBusyProject(PROJECT_1);
    const ProjectsSidebar = (await import("./ProjectsSidebar.svelte")).default;
    render(ProjectsSidebar, { props: noopProps });

    expect(screen.getByTestId("project-cancel")).toBeInTheDocument();
    expect(screen.queryByTestId("project-completed")).toBeNull();
  });

  it("cancelling fires cancel_send with the send's grouped recipients", async () => {
    await seedBusyProject(PROJECT_1);
    const state = await loadState();
    // Record the backend receipt so cancelSend fires immediately (an
    // un-accepted entry would defer until recordSendAccepted).
    state.recordSendAccepted(AGENT_1, "user-1", "msg-1");

    const ProjectsSidebar = (await import("./ProjectsSidebar.svelte")).default;
    render(ProjectsSidebar, { props: noopProps });

    await fireEvent.click(screen.getByTestId("project-cancel"));

    expect(invokeMock).toHaveBeenCalledWith("cancel_send", {
      sendId: "send-1",
      recipients: [AGENT_1],
    });
  });

  it("marks a project completed when it goes busy → idle while not active", async () => {
    await seedBusyProject(PROJECT_1);
    const state = await loadState();
    const ws = await loadWorkspace();
    ws.selection.activeProjectId = PROJECT_2; // PROJECT_1 is not being viewed

    await startActivityObserver();
    const ProjectsSidebar = (await import("./ProjectsSidebar.svelte")).default;
    render(ProjectsSidebar, { props: noopProps });
    await tick(); // let the effect record PROJECT_1 as busy

    // The send finishes: clear the pending entry and settle to idle.
    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };

    await waitFor(() => expect(screen.getByTestId("project-completed")).toBeInTheDocument());
    expect(screen.queryByTestId("project-cancel")).toBeNull();
  });

  it("moves a completed background project to the top with a fresh timestamp", async () => {
    const state = await loadState();
    const ws = await loadWorkspace();
    const background = project(PROJECT_1);
    const foreground = {
      ...project(PROJECT_2),
      last_activity: "2026-05-20T00:00:00Z",
    };
    ws.projects.list = [foreground, background];
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a];
    await state.registerAgent(a);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", "send-1", background.last_activity);
    ws.selection.activeProjectId = PROJECT_2;

    await startActivityObserver();
    await tick();

    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };
    await tick();

    const ProjectsSidebar = (await import("./ProjectsSidebar.svelte")).default;
    render(ProjectsSidebar, { props: noopProps });
    await tick();

    const rows = screen.getAllByTestId("project-row");
    expect(rows[0]).toHaveAttribute("data-project-id", PROJECT_1);
    expect(screen.getByTestId("project-completed")).toBeInTheDocument();
    expect(ws.projects.list[0]).toMatchObject({
      id: PROJECT_1,
      last_activity: "2026-05-25T12:00:00.000Z",
    });
  });

  it("clears the completed marker when the project is selected", async () => {
    await seedBusyProject(PROJECT_1);
    const state = await loadState();
    const ws = await loadWorkspace();
    ws.selection.activeProjectId = PROJECT_2;

    await startActivityObserver();
    const ProjectsSidebar = (await import("./ProjectsSidebar.svelte")).default;
    render(ProjectsSidebar, { props: noopProps });
    await tick();
    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };
    await waitFor(() => expect(screen.getByTestId("project-completed")).toBeInTheDocument());

    const selectButton = screen.getByTestId("project-row").querySelector("button");
    if (!selectButton) throw new Error("expected a select button in the project row");
    await fireEvent.click(selectButton);

    await waitFor(() => expect(screen.queryByTestId("project-completed")).toBeNull());
  });

  it("updates active project activity without showing a completed marker", async () => {
    await seedBusyProject(PROJECT_1);
    const state = await loadState();
    const ws = await loadWorkspace();
    ws.selection.activeProjectId = PROJECT_1; // the user is viewing it

    await startActivityObserver();
    const ProjectsSidebar = (await import("./ProjectsSidebar.svelte")).default;
    render(ProjectsSidebar, { props: noopProps });
    await tick();
    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };
    await tick();

    expect(screen.queryByTestId("project-completed")).toBeNull();
    expect(ws.projects.list[0]).toMatchObject({
      id: PROJECT_1,
      last_activity: "2026-05-25T12:00:00.000Z",
    });
  });
});

function projectIn(id: string, name: string, directory: string): ProjectListing {
  return {
    id,
    name,
    created_at: "2026-05-16T00:00:00Z",
    directory,
    available: true,
    last_activity: "2026-05-16T00:00:00Z",
  };
}

/// Make `rename_project` echo back the renamed row (the backend's contract); all
/// other commands stay no-ops.
function mockRenameEchoes(): void {
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "rename_project") {
      const id = args?.projectId as string;
      return projectIn(id, args?.newName as string, "/work/a");
    }
    return undefined;
  });
}

async function renderWith(list: ProjectListing[]) {
  const ws = await loadWorkspace();
  ws.projects.list = list;
  const ProjectsSidebar = (await import("./ProjectsSidebar.svelte")).default;
  render(ProjectsSidebar, { props: noopProps });
}

function rowSelectButton(index = 0): HTMLButtonElement {
  const rows = screen.getAllByTestId("project-row");
  const btn = rows[index]?.querySelector("button");
  if (!btn) throw new Error("expected a select button in the project row");
  return btn as HTMLButtonElement;
}

describe("ProjectsSidebar — rename", () => {
  const A1 = "00000000-0000-7000-8000-0000000000a1";
  const A2 = "00000000-0000-7000-8000-0000000000a2";

  it("enters edit via double-click and via the kebab Rename item", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    // Double-click the row.
    await fireEvent.dblClick(rowSelectButton());
    expect(await screen.findByTestId("project-rename-input")).toBeInTheDocument();

    // Escape back out, then use the menu.
    await fireEvent.keyDown(screen.getByTestId("project-rename-input"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("project-rename-input")).toBeNull());

    await fireEvent.click(screen.getByTestId("project-actions-trigger"));
    await fireEvent.click(await screen.findByTestId("project-action-rename"));
    expect(await screen.findByTestId("project-rename-input")).toBeInTheDocument();
  });

  it("Enter commits the rename and updates the row", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    await fireEvent.dblClick(rowSelectButton());
    const input = await screen.findByTestId("project-rename-input");
    await fireEvent.input(input, { target: { value: "renamed" } });
    await fireEvent.keyDown(input, { key: "Enter" });

    expect(invokeMock).toHaveBeenCalledWith("rename_project", {
      projectId: A1,
      newName: "renamed",
    });
    await waitFor(() => expect(screen.queryByTestId("project-rename-input")).toBeNull());
    expect(screen.getByText("renamed")).toBeInTheDocument();
  });

  it("Escape and blur cancel without persisting", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    await fireEvent.dblClick(rowSelectButton());
    let input = await screen.findByTestId("project-rename-input");
    await fireEvent.input(input, { target: { value: "changed" } });
    await fireEvent.keyDown(input, { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("project-rename-input")).toBeNull());

    await fireEvent.dblClick(rowSelectButton());
    input = await screen.findByTestId("project-rename-input");
    await fireEvent.input(input, { target: { value: "changed-again" } });
    await fireEvent.blur(input);
    await waitFor(() => expect(screen.queryByTestId("project-rename-input")).toBeNull());

    expect(invokeMock).not.toHaveBeenCalledWith("rename_project", expect.anything());
    expect(screen.getByText("alpha")).toBeInTheDocument();
  });

  it("an unchanged name exits edit without a round-trip", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    await fireEvent.dblClick(rowSelectButton());
    const input = await screen.findByTestId("project-rename-input");
    await fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(screen.queryByTestId("project-rename-input")).toBeNull());
    expect(invokeMock).not.toHaveBeenCalledWith("rename_project", expect.anything());
  });

  it("disables save and flags the input on a same-directory duplicate", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a"), projectIn(A2, "beta", "/work/a")]);

    await fireEvent.dblClick(rowSelectButton()); // edits alpha (first row)
    const input = await screen.findByTestId("project-rename-input");
    await fireEvent.input(input, { target: { value: "BETA" } }); // canonical collision
    expect(screen.getByTestId("project-rename-save")).toBeDisabled();
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input).toHaveAttribute("title", "A project named 'beta' already exists");
  });

  it("allows a name that only collides in a different directory", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a"), projectIn(A2, "beta", "/work/b")]);

    await fireEvent.dblClick(rowSelectButton()); // edits alpha in /work/a
    const input = await screen.findByTestId("project-rename-input");
    await fireEvent.input(input, { target: { value: "beta" } }); // beta lives in /work/b
    expect(screen.getByTestId("project-rename-save")).not.toBeDisabled();
    expect(input).toHaveAttribute("aria-invalid", "false");
  });

  it("disables save on empty without showing a message", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    await fireEvent.dblClick(rowSelectButton());
    const input = await screen.findByTestId("project-rename-input");
    await fireEvent.input(input, { target: { value: "" } });
    expect(screen.getByTestId("project-rename-save")).toBeDisabled();
    expect(input).not.toHaveAttribute("title");
  });

  it("keeps the editor open and surfaces a backend rejection", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "rename_project") throw new Error("registry locked");
      return undefined;
    });
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    await fireEvent.dblClick(rowSelectButton());
    const input = await screen.findByTestId("project-rename-input");
    await fireEvent.input(input, { target: { value: "renamed" } });
    await fireEvent.keyDown(input, { key: "Enter" });

    const err = await screen.findByTestId("project-rename-error");
    expect(err).toHaveTextContent("registry locked");
    expect(screen.getByTestId("project-rename-input")).toBeInTheDocument();
  });

  it("double-Enter commits once", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    await fireEvent.dblClick(rowSelectButton());
    const input = await screen.findByTestId("project-rename-input");
    await fireEvent.input(input, { target: { value: "renamed" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    await fireEvent.keyDown(input, { key: "Enter" });

    const renameCalls = invokeMock.mock.calls.filter((c) => c[0] === "rename_project");
    expect(renameCalls).toHaveLength(1);
  });

  it("omits the kebab on unavailable rows (through M1–M2)", async () => {
    mockRenameEchoes();
    await renderWith([{ ...projectIn(A1, "alpha", "/work/a"), available: false }]);
    expect(screen.queryByTestId("project-actions-trigger")).toBeNull();
  });
});
