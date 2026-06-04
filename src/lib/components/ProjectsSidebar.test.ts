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
    archived: false,
  };
}

function agent(id: string, projectId: string): AgentRecord {
  return {
    id,
    project_id: projectId,
    name: `agent-${id.slice(-1)}`,
    harness: "claude_code",
    session_locator: null,
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

  it("hides project actions behind the completed marker until the project is selected", async () => {
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
    expect(screen.queryByTestId("project-action-archive")).toBeNull();
    expect(screen.queryByTestId("project-action-delete")).toBeNull();

    const selectButton = screen.getByTestId("project-row").querySelector("button");
    if (!selectButton) throw new Error("expected a select button in the project row");
    await fireEvent.click(selectButton);

    await waitFor(() => expect(screen.queryByTestId("project-completed")).toBeNull());
    expect(screen.getByTestId("project-action-archive")).toBeInTheDocument();
    expect(screen.getByTestId("project-action-delete")).toBeInTheDocument();
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

function projectIn(id: string, name: string, directory: string, archived = false): ProjectListing {
  return {
    id,
    name,
    created_at: "2026-05-16T00:00:00Z",
    directory,
    available: true,
    last_activity: "2026-05-16T00:00:00Z",
    archived,
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

describe("ProjectsSidebar — relative activity labels", () => {
  const A1 = "00000000-0000-7000-8000-0000000000r1";

  it("refreshes relative timestamps while the sidebar is mounted", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-05-25T12:00:00Z"));
    try {
      await renderWith([
        {
          ...projectIn(A1, "alpha", "/work/a"),
          last_activity: "2026-05-25T11:59:30Z",
        },
      ]);

      expect(screen.getByText("just now")).toBeInTheDocument();

      await vi.advanceTimersByTimeAsync(60_000);

      await waitFor(() => expect(screen.getByText("1m ago")).toBeInTheDocument());
    } finally {
      vi.useRealTimers();
    }
  });

  it("refreshes relative timestamps when the app becomes visible again", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-05-25T12:00:00Z"));
    try {
      await renderWith([
        {
          ...projectIn(A1, "alpha", "/work/a"),
          last_activity: "2026-05-25T11:59:30Z",
        },
      ]);
      expect(screen.getByText("just now")).toBeInTheDocument();

      vi.setSystemTime(new Date("2026-05-25T13:00:00Z"));
      fireEvent(document, new Event("visibilitychange"));

      await waitFor(() => expect(screen.getByText("1h ago")).toBeInTheDocument());
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("ProjectsSidebar — rename", () => {
  const A1 = "00000000-0000-7000-8000-0000000000a1";
  const A2 = "00000000-0000-7000-8000-0000000000a2";

  it("enters edit via double-click", async () => {
    mockRenameEchoes();
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    await fireEvent.dblClick(rowSelectButton());
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

  it("omits project action icons while the project is busy so the cancel control owns the right slot", async () => {
    await seedBusyProject(PROJECT_1);
    const ProjectsSidebar = (await import("./ProjectsSidebar.svelte")).default;
    render(ProjectsSidebar, { props: noopProps });

    expect(screen.getByTestId("project-cancel")).toBeInTheDocument();
    expect(screen.queryByTestId("project-action-archive")).toBeNull();
    expect(screen.queryByTestId("project-action-delete")).toBeNull();
  });
});

describe("ProjectsSidebar — delete", () => {
  const A1 = "00000000-0000-7000-8000-0000000000d1";
  const A2 = "00000000-0000-7000-8000-0000000000d2";

  it("Delete swaps Archive | Delete to Cancel | Confirm without calling the backend", async () => {
    await renderWith([projectIn(A1, "alpha", "/work/a")]);
    await fireEvent.click(screen.getByTestId("project-action-delete"));

    expect(screen.getByTestId("project-delete-cancel")).toBeInTheDocument();
    expect(screen.getByTestId("project-delete-confirm")).toBeInTheDocument();
    expect(screen.queryByTestId("project-action-archive")).not.toBeInTheDocument();
    expect(screen.queryByTestId("project-action-delete")).not.toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("delete_project", expect.anything());
  });

  it("keeps hover-only project actions out of the tab order", async () => {
    await renderWith([projectIn(A1, "alpha", "/work/a")]);

    expect(screen.getByTestId("project-action-archive")).toHaveAttribute("tabindex", "-1");
    expect(screen.getByTestId("project-action-delete")).toHaveAttribute("tabindex", "-1");

    await fireEvent.click(screen.getByTestId("project-action-delete"));

    expect(screen.getByTestId("project-delete-cancel")).toHaveAttribute("tabindex", "-1");
    expect(screen.getByTestId("project-delete-confirm")).toHaveAttribute("tabindex", "-1");
  });

  it("confirming deletes through the backend and removes the row", async () => {
    await renderWith([projectIn(A1, "alpha", "/work/a")]);
    await fireEvent.click(screen.getByTestId("project-action-delete"));
    await fireEvent.click(screen.getByTestId("project-delete-confirm"));

    expect(invokeMock).toHaveBeenCalledWith("delete_project", { projectId: A1 });
    await waitFor(() => expect(screen.queryByTestId("project-row")).toBeNull());
  });

  it("cancel restores the menu without deleting", async () => {
    await renderWith([projectIn(A1, "alpha", "/work/a")]);
    await fireEvent.click(screen.getByTestId("project-action-delete"));
    await fireEvent.click(screen.getByTestId("project-delete-cancel"));

    expect(screen.getByTestId("project-action-archive")).toBeInTheDocument();
    expect(screen.getByTestId("project-action-delete")).toBeInTheDocument();
    expect(screen.queryByTestId("project-delete-confirm")).not.toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("delete_project", expect.anything());
    expect(screen.getByTestId("project-row")).toBeInTheDocument();
  });

  it("pointer leave cancels an armed delete", async () => {
    await renderWith([projectIn(A1, "alpha", "/work/a")]);
    await fireEvent.click(screen.getByTestId("project-action-delete"));

    await fireEvent.pointerLeave(screen.getByTestId("project-row"));

    expect(screen.getByTestId("project-action-archive")).toBeInTheDocument();
    expect(screen.getByTestId("project-action-delete")).toBeInTheDocument();
    expect(screen.queryByTestId("project-delete-confirm")).not.toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("delete_project", expect.anything());
  });

  it("arming another row disarms the previous row", async () => {
    await renderWith([projectIn(A1, "alpha", "/work/a"), projectIn(A2, "beta", "/work/b")]);

    const firstDelete = screen.getAllByTestId("project-action-delete").at(0);
    if (firstDelete === undefined) throw new Error("expected first row delete button");
    await fireEvent.click(firstDelete);
    expect(screen.getAllByTestId("project-delete-confirm")).toHaveLength(1);

    const secondDelete = screen.getAllByTestId("project-action-delete").at(0);
    if (secondDelete === undefined) throw new Error("expected second row delete button");
    await fireEvent.click(secondDelete);
    expect(screen.getAllByTestId("project-delete-confirm")).toHaveLength(1);
    expect(screen.getAllByTestId("project-row")[1]).toContainElement(
      screen.getByTestId("project-delete-confirm"),
    );
  });

  it("surfaces a backend failure and keeps the project", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "delete_project") throw new Error("disk busy");
      return undefined;
    });
    await renderWith([projectIn(A1, "alpha", "/work/a")]);
    await fireEvent.click(screen.getByTestId("project-action-delete"));
    await fireEvent.click(screen.getByTestId("project-delete-confirm"));

    const err = await screen.findByTestId("project-delete-error");
    expect(err).toHaveTextContent("disk busy");
    expect(screen.getByTestId("project-row")).toBeInTheDocument();
    expect(screen.getByTestId("project-action-delete")).toBeInTheDocument();
  });

  it("uses the themed delete tooltip for the Switchboard-only deletion copy", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      await renderWith([projectIn(A1, "alpha", "/work/a")]);
      const button = screen.getByTestId("project-action-delete");
      expect(button).not.toHaveAttribute("title");

      await fireEvent.pointerEnter(button);
      await vi.advanceTimersByTimeAsync(1000);

      const tooltip = await waitFor(() => screen.getByTestId("tooltip-content"));
      expect(tooltip).toHaveTextContent("Delete project");
      expect(tooltip).toHaveTextContent("Removes Switchboard's files for this project");
      expect(tooltip).toHaveTextContent("your code and agent session files are kept");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("ProjectsSidebar — archive + view toggle", () => {
  const A1 = "00000000-0000-7000-8000-0000000000b1";
  const A2 = "00000000-0000-7000-8000-0000000000b2";

  it("Active view hides archived projects; Archived view shows only them", async () => {
    await renderWith([
      projectIn(A1, "active-one", "/work/a"),
      projectIn(A2, "archived-one", "/work/a", true),
    ]);

    // Default Active view: only the non-archived row.
    let rows = screen.getAllByTestId("project-row");
    expect(rows).toHaveLength(1);
    expect(rows[0]).toHaveAttribute("data-project-id", A1);

    // Switch to Archived: only the archived row.
    await fireEvent.click(screen.getByTestId("project-view-archived"));
    rows = screen.getAllByTestId("project-row");
    expect(rows).toHaveLength(1);
    expect(rows[0]).toHaveAttribute("data-project-id", A2);
  });

  it("empty-state copy is view-aware", async () => {
    await renderWith([projectIn(A1, "active-one", "/work/a")]);
    // No archived projects → Archived view shows the archived empty message.
    await fireEvent.click(screen.getByTestId("project-view-archived"));
    expect(screen.getByText("No archived projects.")).toBeInTheDocument();
  });

  it("Archive item calls through and the row leaves the Active view", async () => {
    await renderWith([projectIn(A1, "alpha", "/work/a")]);
    await fireEvent.click(await screen.findByTestId("project-action-archive"));

    expect(invokeMock).toHaveBeenCalledWith("set_project_archived", {
      projectId: A1,
      archived: true,
    });
    // The (now-archived) row drops out of the default Active view.
    await waitFor(() => expect(screen.queryByTestId("project-row")).toBeNull());
  });

  it("archive button becomes Unarchive for an archived project", async () => {
    await renderWith([projectIn(A1, "alpha", "/work/a", true)]);
    await fireEvent.click(screen.getByTestId("project-view-archived"));

    const button = await screen.findByTestId("project-action-archive");
    expect(button).toHaveAttribute("aria-label", "Unarchive project");
    await fireEvent.click(button);
    expect(invokeMock).toHaveBeenCalledWith("set_project_archived", {
      projectId: A1,
      archived: false,
    });
  });

  it("on an unavailable row Archive stays enabled and Delete is disabled", async () => {
    await renderWith([{ ...projectIn(A1, "alpha", "/work/a"), available: false }]);

    expect(await screen.findByTestId("project-action-archive")).not.toBeDisabled();
    expect(screen.getByTestId("project-action-delete")).toBeDisabled();
  });
});

describe("ProjectsSidebar — search", () => {
  const S1 = "00000000-0000-7000-8000-0000000000c1";
  const S2 = "00000000-0000-7000-8000-0000000000c2";

  function rowIds(): string[] {
    return screen
      .queryAllByTestId("project-row")
      .map((r) => r.getAttribute("data-project-id") ?? "");
  }

  async function type(value: string): Promise<void> {
    await fireEvent.input(screen.getByTestId("project-search"), { target: { value } });
  }

  it("filters by project name, case-insensitively", async () => {
    await renderWith([projectIn(S1, "Alpha", "/work/one"), projectIn(S2, "Beta", "/work/two")]);
    await type("alp");
    expect(rowIds()).toEqual([S1]);
  });

  it("filters by directory basename (not the full path)", async () => {
    await renderWith([
      projectIn(S1, "Alpha", "/home/me/frontend"),
      projectIn(S2, "Beta", "/home/me/backend"),
    ]);
    // "me" is in both full paths but neither basename → no matches.
    await type("me");
    expect(rowIds()).toEqual([]);
    // Basename match.
    await type("front");
    expect(rowIds()).toEqual([S1]);
  });

  it("clearing the query (and the clear button) restores the list", async () => {
    await renderWith([projectIn(S1, "Alpha", "/work/one"), projectIn(S2, "Beta", "/work/two")]);
    await type("alpha");
    expect(rowIds()).toEqual([S1]);

    await fireEvent.click(screen.getByTestId("project-search-clear"));
    expect(rowIds()).toEqual([S1, S2]);
    expect(screen.queryByTestId("project-search-clear")).toBeNull(); // hidden when empty
  });

  it("composes with the Archived view", async () => {
    await renderWith([
      projectIn(S1, "alpha-active", "/work/one"),
      projectIn(S2, "alpha-archived", "/work/two", true),
    ]);
    await type("alpha");
    // Active view: only the non-archived "alpha".
    expect(rowIds()).toEqual([S1]);
    // Same query, Archived view: only the archived "alpha".
    await fireEvent.click(screen.getByTestId("project-view-archived"));
    expect(rowIds()).toEqual([S2]);
  });

  it("shows a distinct no-match state", async () => {
    await renderWith([projectIn(S1, "Alpha", "/work/one")]);
    await type("zzz");
    expect(rowIds()).toEqual([]);
    expect(screen.getByText("No projects match.")).toBeInTheDocument();
  });

  it("an empty Active view (all projects archived) says 'No active projects', not 'No projects yet'", async () => {
    // Projects exist, but all are archived → the Active view is empty without
    // being a query miss or a truly-empty workspace.
    await renderWith([projectIn(S1, "alpha", "/work/one", true)]);
    expect(rowIds()).toEqual([]);
    expect(screen.getByText("No active projects.")).toBeInTheDocument();
    expect(screen.queryByText("No projects yet.")).toBeNull();
  });

  it("an empty Archived view says 'No archived projects'", async () => {
    await renderWith([projectIn(S1, "alpha", "/work/one")]);
    await fireEvent.click(screen.getByTestId("project-view-archived"));
    expect(rowIds()).toEqual([]);
    expect(screen.getByText("No archived projects.")).toBeInTheDocument();
  });
});
