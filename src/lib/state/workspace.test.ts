import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { tick } from "svelte";
import type { AgentRecord, ProjectListing } from "$lib/types";

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => undefined,
);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

const PROJECT_1 = "00000000-0000-7000-8000-0000000000f1";
const PROJECT_2 = "00000000-0000-7000-8000-0000000000f2";
const PROJECT_3 = "00000000-0000-7000-8000-0000000000f3";
const AGENT_1 = "00000000-0000-7000-8000-00000000000a";

function project(id: string, lastActivity: string): ProjectListing {
  return {
    id,
    name: `proj-${id.slice(-2)}`,
    created_at: "2026-05-16T00:00:00Z",
    directory: `/work/${id.slice(-2)}`,
    available: true,
    last_activity: lastActivity,
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

async function loadWorkspaceState() {
  return await import("./workspace.svelte");
}

async function loadAgentState() {
  return await import("./index.svelte");
}

const observerStops: (() => void)[] = [];

beforeEach(() => {
  invokeMock.mockReset();
});

afterEach(async () => {
  for (const stop of observerStops.splice(0)) stop();
  const state = await loadAgentState();
  state._testing.reset();
  const ws = await loadWorkspaceState();
  ws._testing.reset();
});

describe("workspace project activity", () => {
  it("records a shared local activity timestamp and preserves stable order within the batch", async () => {
    const ws = await loadWorkspaceState();
    ws.projects.list = [
      project(PROJECT_2, "2026-05-20T00:00:00Z"),
      project(PROJECT_1, "2026-05-16T00:00:00Z"),
      project(PROJECT_3, "2026-05-16T00:00:00Z"),
    ];

    ws.recordProjectsActivityLocally([PROJECT_1, PROJECT_3], "2026-05-25T12:00:00.000Z");

    expect(ws.projects.list.map((p) => p.id)).toEqual([PROJECT_1, PROJECT_3, PROJECT_2]);
    expect(ws.projectActivityOverrides[PROJECT_1]).toBe("2026-05-25T12:00:00.000Z");
    expect(ws.projectActivityOverrides[PROJECT_3]).toBe("2026-05-25T12:00:00.000Z");
  });

  it("ignores unknown project ids", async () => {
    const ws = await loadWorkspaceState();
    const known = project(PROJECT_1, "2026-05-16T00:00:00Z");
    ws.projects.list = [known];

    ws.recordProjectsActivityLocally(
      ["00000000-0000-7000-8000-00000000dead"],
      "2026-05-25T12:00:00.000Z",
    );

    expect(ws.projects.list).toEqual([known]);
    expect(ws.projectActivityOverrides["00000000-0000-7000-8000-00000000dead"]).toBeUndefined();
  });

  it("keeps local activity overrides when the backend project registry refreshes", async () => {
    const ws = await loadWorkspaceState();
    const staleBackground = project(PROJECT_1, "2026-05-16T00:00:00Z");
    const foreground = project(PROJECT_2, "2026-05-20T00:00:00Z");
    ws.projects.list = [foreground, staleBackground];
    ws.recordProjectsActivityLocally([PROJECT_1], "2026-05-25T12:00:00.000Z");
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workspace_directories") {
        return { directories: [], persistable: true };
      }
      if (cmd === "list_projects") {
        return [foreground, staleBackground];
      }
      return undefined;
    });

    await ws.loadWorkspace();

    expect(ws.projects.list[0]).toMatchObject({
      id: PROJECT_1,
      last_activity: "2026-05-25T12:00:00.000Z",
    });
  });

  it("lets fresher backend activity win over an older local override", async () => {
    const ws = await loadWorkspaceState();
    const background = project(PROJECT_1, "2026-05-16T00:00:00Z");
    const foreground = project(PROJECT_2, "2026-05-20T00:00:00Z");
    const fresherBackground = project(PROJECT_1, "2026-05-30T12:00:00Z");
    ws.projects.list = [foreground, background];
    ws.recordProjectsActivityLocally([PROJECT_1], "2026-05-25T12:00:00.000Z");
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workspace_directories") {
        return { directories: [], persistable: true };
      }
      if (cmd === "list_projects") {
        return [foreground, fresherBackground];
      }
      return undefined;
    });

    await ws.loadWorkspace();

    expect(ws.projects.list[0]).toMatchObject({
      id: PROJECT_1,
      last_activity: "2026-05-30T12:00:00Z",
    });
  });

  it("preserves observer-stamped activity when the backend project registry refreshes", async () => {
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const staleBackground = project(PROJECT_1, "2026-05-16T00:00:00Z");
    const foreground = project(PROJECT_2, "2026-05-20T00:00:00Z");
    ws.projects.list = [foreground, staleBackground];
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a];
    await state.registerAgent(a);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", "send-1", staleBackground.last_activity);
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();
    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };
    await tick();
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workspace_directories") {
        return { directories: [], persistable: true };
      }
      if (cmd === "list_projects") {
        return [foreground, staleBackground];
      }
      return undefined;
    });

    await ws.loadWorkspace();

    expect(ws.projects.list[0]).toMatchObject({
      id: PROJECT_1,
      last_activity: "2026-05-25T12:00:00.000Z",
    });
  });

  it("removing a busy project clears activity observer memory and local markers", async () => {
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const busyProject = project(PROJECT_1, "2026-05-16T00:00:00Z");
    ws.projects.list = [busyProject];
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a];
    await state.registerAgent(a);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", "send-1", busyProject.last_activity);
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "list_workspace_directories") {
        return { directories: [], persistable: true };
      }
      if (cmd === "list_projects") {
        return [];
      }
      return undefined;
    });

    await ws.removeDirectory(busyProject.directory);
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBeUndefined();
    expect(ws.projectActivityOverrides[PROJECT_1]).toBeUndefined();
    expect(ws.projects.list).toEqual([]);
  });
});
