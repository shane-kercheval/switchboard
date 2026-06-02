import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import type { AgentRecord, ProjectListing } from "$lib/types";

const invokeMock = vi.fn(async (_cmd: string, _args?: Record<string, unknown>) => undefined);
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
