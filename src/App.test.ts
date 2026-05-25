import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type {
  AgentRecord,
  NormalizedEvent,
  ProjectConversation,
  ProjectListing,
  ProjectSummary,
} from "$lib/types";

// App.svelte tests focus on the workspace-level orchestration: eager registry
// load, lazy per-project activation (roster + hydration), display-only project
// switching (listeners persist), the not-persistable signal, and the
// post-restart merged-conversation render. Per-screen UI (ProjectsSidebar /
// Sidebar / UnifiedTranscript / ComposeBar) has its own component tests; these
// verify the glue.
//
// The IPC layer is mocked with a small **stateful fake backend** so flows read
// naturally (add directory → create project → activate → send) without
// asserting brittle call orders.

const listenCallbacks = new Map<string, (e: { payload: NormalizedEvent }) => void>();
function fireTo(channel: string, event: NormalizedEvent): void {
  const cb = listenCallbacks.get(channel);
  if (cb === undefined) throw new Error(`no listener registered for ${channel}`);
  cb({ payload: event });
}
const listenMock = vi.fn(async (event: string, handler: unknown): Promise<() => void> => {
  listenCallbacks.set(event, handler as (e: { payload: NormalizedEvent }) => void);
  return () => {
    listenCallbacks.delete(event);
  };
});
const openDialogMock = vi.fn(async (_options: unknown): Promise<string | null> => null);

// --- stateful fake backend ---
type Backend = {
  persistable: boolean;
  dirs: Map<string, { available: boolean }>;
  projects: ProjectListing[];
  rosters: Map<string, AgentRecord[]>;
  conversations: Map<string, ProjectConversation>;
  activeProjectId: string | null;
  probeFailures: Set<string>;
  agentQueue: AgentRecord[];
  sendMessageId: string;
  loadConvoCalls: string[];
  failOpenFor: Set<string>;
  failConvoFor: Set<string>;
};
let backend: Backend;
let agentSeq = 0;

function freshBackend(): Backend {
  return {
    persistable: true,
    dirs: new Map(),
    projects: [],
    rosters: new Map(),
    conversations: new Map(),
    activeProjectId: null,
    probeFailures: new Set(),
    agentQueue: [],
    sendMessageId: "77777777-7777-7000-8000-777777777777",
    loadConvoCalls: [],
    failOpenFor: new Set(),
    failConvoFor: new Set(),
  };
}

const PROBES = [
  "check_claude_binary",
  "check_codex_binary",
  "check_codex_auth",
  "check_gemini_binary",
  "check_gemini_auth",
  "check_antigravity_binary",
  "check_antigravity_auth",
];

function summaryFor(id: string): ProjectSummary {
  const row = backend.projects.find((p) => p.id === id);
  if (row === undefined) throw new Error(`open of unknown project ${id}`);
  return { id: row.id, name: row.name, created_at: row.created_at };
}

const invokeMock = vi.fn(async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
  if (PROBES.includes(cmd)) {
    if (backend.probeFailures.has(cmd)) throw new Error(`probe failed: ${cmd}`);
    return null;
  }
  switch (cmd) {
    case "list_workspace_directories":
      return {
        directories: [...backend.dirs].map(([path, d]) => ({ path, available: d.available })),
        persistable: backend.persistable,
      };
    case "list_projects":
      // Only projects in registered directories surface (matches the real
      // backend: the workspace registry gates which projects are listed).
      return backend.projects.filter((p) => backend.dirs.has(p.directory));
    case "init_directory": {
      const path = args?.path as string;
      backend.dirs.set(path, { available: true });
      return { path, has_switchboard: true, projects: [] };
    }
    case "remove_directory": {
      const path = args?.path as string;
      backend.dirs.delete(path);
      backend.projects = backend.projects.filter((p) => p.directory !== path);
      return null;
    }
    case "create_project": {
      const id = `00000000-0000-7000-8000-0000000c${(backend.projects.length + 1)
        .toString()
        .padStart(4, "0")}`;
      const row: ProjectListing = {
        id,
        name: args?.name as string,
        created_at: "2026-05-20T00:00:00Z",
        directory: args?.directory as string,
        available: true,
        last_activity: "2026-05-20T00:00:00Z",
      };
      backend.projects.push(row);
      backend.rosters.set(id, []);
      return summaryFor(id);
    }
    case "open_project": {
      const pid = args?.projectId as string;
      if (backend.failOpenFor.has(pid)) throw new Error("project is locked by another process");
      return summaryFor(pid);
    }
    case "set_active_project":
      backend.activeProjectId = args?.projectId as string;
      return null;
    case "list_agents": {
      const pid = (args?.projectId as string) ?? backend.activeProjectId;
      return backend.rosters.get(pid ?? "") ?? [];
    }
    case "create_agent":
    case "attach_agent": {
      const agent =
        backend.agentQueue.shift() ??
        ({
          id: `00000000-0000-7000-8000-00000000a${(++agentSeq).toString().padStart(3, "0")}`,
          project_id: backend.activeProjectId ?? "",
          name: args?.name as string,
          harness: args?.harness as AgentRecord["harness"],
          session_id: null,
          created_at: "2026-05-20T00:00:01Z",
        } satisfies AgentRecord);
      const pid = backend.activeProjectId ?? "";
      backend.rosters.set(pid, [...(backend.rosters.get(pid) ?? []), agent]);
      return agent;
    }
    case "load_transcript":
      return { turns: [], warnings: [] };
    case "load_project_conversation": {
      const pid = args?.projectId as string;
      backend.loadConvoCalls.push(pid);
      if (backend.failConvoFor.has(pid)) throw new Error("conversation load failed");
      return backend.conversations.get(pid) ?? { items: [], agents: [] };
    }
    case "send_message":
      return backend.sendMessageId;
    default:
      throw new Error(`unexpected invoke call: ${cmd}`);
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: unknown) => listenMock(event, handler),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (options: unknown) => openDialogMock(options),
}));

const DIR_A = "/tmp/sw-a";
const DIR_B = "/tmp/sw-b";

function listing(
  over: Partial<ProjectListing> & { id: string; directory: string },
): ProjectListing {
  return {
    name: "alpha",
    created_at: "2026-05-20T00:00:00Z",
    available: true,
    last_activity: "2026-05-20T00:00:00Z",
    ...over,
  };
}

function agent(over: Partial<AgentRecord> & { id: string; project_id: string }): AgentRecord {
  return {
    name: "assistant",
    harness: "claude_code",
    session_id: "33333333-3333-7000-8000-333333333333",
    created_at: "2026-05-20T00:00:01Z",
    ...over,
  };
}

/// Seed a directory with one project and (optionally) a roster + conversation,
/// so a test can mount straight into a populated workspace.
function seedProject(opts: {
  projectId: string;
  directory?: string;
  agents?: AgentRecord[];
  conversation?: ProjectConversation;
  available?: boolean;
  lastActivity?: string;
  name?: string;
}): void {
  const dir = opts.directory ?? DIR_A;
  backend.dirs.set(dir, { available: opts.available ?? true });
  backend.projects.push(
    listing({
      id: opts.projectId,
      directory: dir,
      name: opts.name ?? "alpha",
      available: opts.available ?? true,
      last_activity: opts.lastActivity ?? "2026-05-20T00:00:00Z",
    }),
  );
  backend.rosters.set(opts.projectId, opts.agents ?? []);
  if (opts.conversation !== undefined) backend.conversations.set(opts.projectId, opts.conversation);
}

async function mountApp() {
  const App = (await import("./App.svelte")).default;
  return render(App);
}

describe("App", () => {
  beforeEach(() => {
    backend = freshBackend();
    agentSeq = 0;
    listenCallbacks.clear();
    invokeMock.mockClear();
    listenMock.mockClear();
    openDialogMock.mockReset();
  });

  afterEach(async () => {
    const idx = await import("$lib/state/index.svelte");
    idx._testing.reset();
    const ws = await import("$lib/state/workspace.svelte");
    ws._testing.reset();
  });

  // --- harness availability banners (workspace empty → welcome) ---

  it("renders the welcome state (New / Add existing) with no banners when probes succeed and no projects exist", async () => {
    await mountApp();
    expect(screen.getByText("Switchboard")).toBeInTheDocument();
    expect(screen.getByTestId("welcome-new-project")).toBeInTheDocument();
    expect(screen.getByTestId("welcome-add-existing")).toBeInTheDocument();
    // The flat project sidebar is always present (no separate welcome phase).
    expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByTestId(/^banner-/)).not.toBeInTheDocument());
  });

  it("renders a Claude binary-missing banner when only the Claude probe fails", async () => {
    backend.probeFailures.add("check_claude_binary");
    await mountApp();
    await waitFor(() =>
      expect(screen.getByTestId("banner-binary_missing-claude_code")).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("banner-binary_missing-codex")).not.toBeInTheDocument();
  });

  it("renders a Codex auth-missing banner when only the Codex auth probe fails", async () => {
    backend.probeFailures.add("check_codex_auth");
    await mountApp();
    await waitFor(() =>
      expect(screen.getByTestId("banner-auth_missing-codex")).toBeInTheDocument(),
    );
  });

  it("renders a Gemini auth-missing banner when only the Gemini auth probe fails", async () => {
    backend.probeFailures.add("check_gemini_auth");
    await mountApp();
    await waitFor(() =>
      expect(screen.getByTestId("banner-auth_missing-gemini")).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("banner-auth_missing-codex")).not.toBeInTheDocument();
  });

  it("suppresses a harness's auth banner when its binary is also missing", async () => {
    backend.probeFailures.add("check_codex_binary");
    backend.probeFailures.add("check_codex_auth");
    await mountApp();
    await waitFor(() =>
      expect(screen.getByTestId("banner-binary_missing-codex")).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("banner-auth_missing-codex")).not.toBeInTheDocument();
  });

  it("renders two binary banners simultaneously when two binaries are missing", async () => {
    backend.probeFailures.add("check_claude_binary");
    backend.probeFailures.add("check_codex_binary");
    await mountApp();
    await waitFor(() => {
      expect(screen.getByTestId("banner-binary_missing-claude_code")).toBeInTheDocument();
      expect(screen.getByTestId("banner-binary_missing-codex")).toBeInTheDocument();
    });
  });

  // --- adding projects ---

  it("add existing project: pointing at a folder brings its existing projects into the flat list", async () => {
    // The folder already has a Switchboard project on disk; adding it surfaces
    // that project in the list (the directory is invisible plumbing).
    backend.projects.push(listing({ id: "p-x", directory: DIR_A, name: "existing-proj" }));
    backend.rosters.set("p-x", []);
    openDialogMock.mockResolvedValueOnce(DIR_A);
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-existing")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("welcome-add-existing"));
    await waitFor(() => expect(screen.getByText("existing-proj")).toBeInTheDocument());
    expect(
      invokeMock.mock.calls.some(([c, a]) => c === "init_directory" && a?.path === DIR_A),
    ).toBe(true);
    // No directory rows anywhere — folders are not a managed object.
    expect(screen.queryByTestId("directory-row")).not.toBeInTheDocument();
  });

  it("new project: choosing a folder + name creates the project and activates it", async () => {
    openDialogMock.mockResolvedValueOnce(DIR_A);
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-new-project")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("welcome-new-project"));
    await waitFor(() => expect(screen.getByTestId("new-project-form")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    const nameInput = screen.getByTestId("new-project-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "brand-new" } });
    await fireEvent.click(screen.getByTestId("new-project-submit"));

    // Created in the chosen folder, then activated → its (empty) roster prompts
    // for a first agent.
    await waitFor(() => expect(screen.getByText("brand-new")).toBeInTheDocument());
    const createCalls = invokeMock.mock.calls.filter(([c]) => c === "create_project");
    expect(createCalls).toHaveLength(1);
    expect(createCalls[0]?.[1]).toEqual({ name: "brand-new", directory: DIR_A });
  });

  it("surfaces the not-persistable state distinctly from a fresh install", async () => {
    backend.persistable = false;
    backend.dirs.set(DIR_A, { available: true });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("not-persistable-banner")).toBeInTheDocument());
    // The workspace shell renders (sidebar present), not a bare welcome with no warning.
    expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument();
  });

  // --- workspace: project list + lazy activation ---

  it("eager-loads the flat project list across directories without hydrating transcripts", async () => {
    seedProject({ projectId: "p-a", directory: DIR_A, name: "alpha" });
    seedProject({ projectId: "p-b", directory: DIR_B, name: "beta" });
    await mountApp();

    await waitFor(() => {
      expect(screen.getAllByTestId("project-row")).toHaveLength(2);
    });
    // Eager registry only — no project's conversation is hydrated at startup.
    expect(backend.loadConvoCalls).toEqual([]);
  });

  it("first activation loads the roster + hydrates once; re-activation does not re-hydrate", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({ projectId: "p-b", directory: DIR_B, name: "beta" });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row").length).toBe(2));

    const rowA = screen.getByText("alpha");
    await fireEvent.click(rowA);
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    // Hydration is fire-and-forget just after the roster renders.
    await waitFor(() => expect(backend.loadConvoCalls).toEqual(["p-a"]));

    // Switch to B then back to A — A must not re-hydrate.
    await fireEvent.click(screen.getByText("beta"));
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(backend.loadConvoCalls.filter((p) => p === "p-a")).toHaveLength(1);
  });

  it("switching projects is display-only: a backgrounded agent's listener still updates state", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-2", project_id: "p-b", name: "helper" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row").length).toBe(2));

    // Activate A (registers ag-1's listener), then switch to B.
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("beta"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    // A background turn for ag-1 (in directory A) arrives AFTER the switch.
    // Its listener must still be alive and update state — proving switching
    // didn't unregister it.
    const turnId = "88888888-8888-7000-8000-888888888888";
    fireTo(`agent:ag-1`, {
      type: "turn_start",
      turn_id: turnId,
      message_id: backend.sendMessageId,
      started_at: "2026-05-20T01:00:00Z",
    });
    fireTo(`agent:ag-1`, { type: "content_chunk", turn_id: turnId, kind: "text", text: "bg work" });

    // Switch back to A; the background turn's content is present.
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() =>
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("bg work"),
    );
  });

  // --- first agent + add agent ---

  it("a project with no agents shows the create-agent form; creating one renders the transcript", async () => {
    seedProject({ projectId: "p-a", directory: DIR_A, name: "alpha", agents: [] });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    await waitFor(() => {
      expect(screen.getByTestId("unified-transcript")).toBeInTheDocument();
      expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
      expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    });
    const createCalls = invokeMock.mock.calls.filter(([c]) => c === "create_agent");
    expect(createCalls).toHaveLength(1);
    expect(createCalls[0]?.[1]).toEqual({ name: "assistant", harness: "claude_code" });
  });

  it("add agent via the sidebar modal registers a listener and appends to the roster", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.agentQueue.push(
      agent({ id: "ag-2", project_id: "p-a", name: "second", harness: "codex", session_id: null }),
    );
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());

    const listenBefore = listenMock.mock.calls.length;
    await fireEvent.click(screen.getByTestId("sidebar-add-agent"));
    await waitFor(() => expect(screen.getByTestId("dialog-content")).toBeInTheDocument());
    const modal = screen.getByTestId("dialog-content");
    const nameInput = within(modal).getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "second" } });
    await fireEvent.click(within(modal).getByTestId("harness-codex"));
    await fireEvent.click(within(modal).getByTestId("confirm-create-agent"));

    await waitFor(() => {
      const names = screen.getAllByTestId("agent-name");
      expect(names.some((n) => n.textContent === "second")).toBe(true);
    });
    expect(listenMock.mock.calls.length).toBe(listenBefore + 1);
    expect(listenMock.mock.calls.at(-1)?.[0]).toBe("agent:ag-2");
  });

  // --- post-restart merged conversation ---

  it("renders the merged post-restart conversation: grouped user message, agent content, and failed/cancelled markers", async () => {
    const convo: ProjectConversation = {
      items: [
        {
          kind: "user_message",
          send_id: "s-1",
          agent_ids: ["ag-1"],
          text: "do the thing",
          at: "2026-05-20T00:00:00Z",
        },
        {
          kind: "agent_turn",
          turn_id: "t-done",
          agent_id: "ag-1",
          started_at: "2026-05-20T00:00:00Z",
          ended_at: "2026-05-20T00:00:02Z",
          status: "complete",
          items: [{ item_kind: "text", kind: "text", text: "did the thing" }],
          usage: null,
        },
        // A failed send and a cancelled send, each at its own send's instant.
        {
          kind: "user_message",
          send_id: "s-2",
          agent_ids: ["ag-1"],
          text: "second ask",
          at: "2026-05-20T00:01:00Z",
        },
        {
          kind: "outcome",
          turn_id: "t-fail",
          send_id: "s-2",
          agent_id: "ag-1",
          status: "failed",
          reason: "boom",
          at: "2026-05-20T00:01:00Z",
        },
        {
          kind: "user_message",
          send_id: "s-3",
          agent_ids: ["ag-1"],
          text: "third ask",
          at: "2026-05-20T00:02:00Z",
        },
        {
          kind: "outcome",
          turn_id: "t-cancel",
          send_id: "s-3",
          agent_id: "ag-1",
          status: "cancelled",
          reason: "user",
          at: "2026-05-20T00:02:00Z",
        },
      ],
      agents: [
        { agent_id: "ag-1", meta: null, last_rate_limit: null, warnings: [], load_error: null },
      ],
    };
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
      conversation: convo,
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));

    await waitFor(() => {
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("did the thing");
    });
    const transcript = screen.getByTestId("unified-transcript");
    expect(transcript).toHaveTextContent("do the thing");
    // The failed and cancelled markers render (from the journal overlay).
    expect(within(transcript).getByTestId("outcome-failed")).toBeInTheDocument();
    expect(within(transcript).getByTestId("outcome-cancelled")).toBeInTheDocument();

    // The user message renders once per send (grouped), not duplicated.
    const userRows = within(transcript)
      .getAllByTestId("turn")
      .filter((el) => el.getAttribute("data-role") === "user");
    expect(userRows).toHaveLength(3);

    // A marker never sorts above the prompt that caused it: within the
    // rendered order, each user message precedes its same-instant marker.
    const order = transcript.textContent ?? "";
    expect(order.indexOf("second ask")).toBeLessThan(order.indexOf("boom"));
    expect(order.indexOf("third ask")).toBeLessThan(order.indexOf("cancelled"));
  });

  // --- E2E send ---

  it("E2E send: send → turn_start → content_chunk → turn_end → agent_idle renders and unblocks Send", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const sendCalls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sendCalls).toHaveLength(1);
      expect(sendCalls[0]?.[1]).toEqual({ agentId: "ag-1", prompt: "hi" });
    });

    const turnId = "88888888-8888-7000-8000-888888888888";
    const channel = "agent:ag-1";
    fireTo(channel, {
      type: "turn_start",
      turn_id: turnId,
      message_id: backend.sendMessageId,
      started_at: "2026-05-20T00:00:00Z",
    });
    fireTo(channel, { type: "content_chunk", turn_id: turnId, kind: "text", text: "hello back" });
    fireTo(channel, {
      type: "turn_end",
      turn_id: turnId,
      outcome: { status: "completed" },
      ended_at: "2026-05-20T00:00:01Z",
      usage: null,
    });
    fireTo(channel, { type: "agent_idle", agent_id: "ag-1" });

    await waitFor(() =>
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("hello back"),
    );
    await fireEvent.input(textarea, { target: { value: "again" } });
    await waitFor(() => expect(screen.getByTestId("compose-send")).not.toBeDisabled());
  });

  // --- directory removal lifecycle (store-level: the `removeDirectory` +
  // teardown primitive that M8's project-delete will reuse; there is no
  // directory-removal UI in this milestone) ---

  it("removeDirectory drops the directory's projects and tears down its agents' listeners", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({ projectId: "p-b", directory: DIR_B, name: "beta" });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row").length).toBe(2));

    // Activate p-a so ag-1's listener is registered, then remove its directory.
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(listenCallbacks.has("agent:ag-1")).toBe(true);

    const ws = await import("$lib/state/workspace.svelte");
    await ws.removeDirectory(DIR_A);

    await waitFor(() => {
      const names = screen.getAllByTestId("project-row").map((r) => r.textContent);
      expect(names.some((n) => n?.includes("beta"))).toBe(true);
      expect(names.some((n) => n?.includes("alpha"))).toBe(false);
    });
    // Teardown: the removed agent's listener is gone (no leak), and the backend
    // drain command fired.
    expect(listenCallbacks.has("agent:ag-1")).toBe(false);
    expect(
      invokeMock.mock.calls.some(([c, a]) => c === "remove_directory" && a?.path === DIR_A),
    ).toBe(true);
  });

  it("remove-then-re-add reuses no stale state: re-activation re-opens the project", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const ws = await import("$lib/state/workspace.svelte");
    await ws.removeDirectory(DIR_A);
    await waitFor(() => expect(screen.queryByTestId("project-row")).not.toBeInTheDocument());

    // Re-add the same directory + project id (ids persist on disk). The stale
    // memoized load must NOT be reused: re-activation re-opens the project.
    const openCallsBefore = invokeMock.mock.calls.filter(([c]) => c === "open_project").length;
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await ws.loadWorkspace();

    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    const openCallsAfter = invokeMock.mock.calls.filter(([c]) => c === "open_project").length;
    expect(openCallsAfter).toBeGreaterThan(openCallsBefore);
  });

  it("a failed activation shows a center error + retry, not an endless loading state", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.failOpenFor.add("p-a");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("activation-error")).toBeInTheDocument());
    expect(screen.queryByTestId("project-loading")).not.toBeInTheDocument();

    // Recover and retry — the project opens.
    backend.failOpenFor.delete("p-a");
    await fireEvent.click(screen.getByTestId("activation-retry"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(screen.queryByTestId("activation-error")).not.toBeInTheDocument();
  });

  it("an unreadable workspace with no recovered directories shows the not-persistable banner, not welcome", async () => {
    backend.persistable = false; // empty dirs + not persistable
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("not-persistable-banner")).toBeInTheDocument());
    expect(screen.queryByText("Open working directory")).not.toBeInTheDocument();
  });

  it("a whole-project conversation load failure shows the load-failed banner; live sends still work", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.failConvoFor.add("p-a");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    await waitFor(() => expect(screen.getByTestId("transcript-load-failed")).toBeInTheDocument());
  });

  it("a per-agent transcript load error surfaces in the sidebar without blanking the project", async () => {
    const convo: ProjectConversation = {
      items: [
        {
          kind: "agent_turn",
          turn_id: "t-ok",
          agent_id: "ag-ok",
          started_at: "2026-05-20T00:00:00Z",
          ended_at: "2026-05-20T00:00:01Z",
          status: "complete",
          items: [{ item_kind: "text", kind: "text", text: "healthy history" }],
          usage: null,
        },
      ],
      agents: [
        { agent_id: "ag-ok", meta: null, last_rate_limit: null, warnings: [], load_error: null },
        {
          agent_id: "ag-bad",
          meta: null,
          last_rate_limit: null,
          warnings: [],
          load_error: "corrupt sidecar",
        },
      ],
    };
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-ok", project_id: "p-a", name: "healthy" }),
        agent({
          id: "ag-bad",
          project_id: "p-a",
          name: "broken",
          harness: "codex",
          session_id: null,
        }),
      ],
      conversation: convo,
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));

    await waitFor(() =>
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("healthy history"),
    );
    await waitFor(() =>
      expect(screen.getByTestId("agent-hydration-error")).toHaveTextContent("corrupt sidecar"),
    );
  });

  // --- sidebar collapse / expand ---

  it("projects sidebar: toggle collapses and re-opens; the control moves between sidebar and title bar", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    // The toggle is inside the sidebar when open.
    const sidebar = screen.getByTestId("projects-sidebar");
    expect(within(sidebar).getByTestId("projects-sidebar-toggle")).toBeInTheDocument();

    // Collapse — same testid, but now lives in the center title bar.
    await fireEvent.click(within(sidebar).getByTestId("projects-sidebar-toggle"));
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());
    // Re-open control is now in the DOM (in the title bar).
    expect(screen.getByTestId("projects-sidebar-toggle")).toBeInTheDocument();

    // Re-open.
    await fireEvent.click(screen.getByTestId("projects-sidebar-toggle"));
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
  });

  it("agents sidebar: toggle hides and re-shows the sidebar while the title bar persists", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());
    expect(screen.getByTestId("agents-sidebar-toggle")).toBeInTheDocument();

    // Hide.
    await fireEvent.click(screen.getByTestId("agents-sidebar-toggle"));
    await waitFor(() => expect(screen.queryByTestId("sidebar")).not.toBeInTheDocument());
    // Title bar (breadcrumb) and compose area remain usable.
    expect(screen.getByTestId("breadcrumb")).toBeInTheDocument();
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();

    // Re-show.
    await fireEvent.click(screen.getByTestId("agents-sidebar-toggle"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());
  });

  it("settings button toggles settings without changing the selected project", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");

    await fireEvent.click(
      within(screen.getByTestId("projects-sidebar")).getByTestId("settings-button"),
    );
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");
    expect(screen.getByText("Theme")).toBeInTheDocument();
    expect(screen.getByText("Shortcuts")).toBeInTheDocument();

    await fireEvent.click(
      within(screen.getByTestId("projects-sidebar")).getByTestId("settings-button"),
    );
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();

    await fireEvent.click(
      within(screen.getByTestId("projects-sidebar")).getByTestId("settings-button"),
    );
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("settings-close"));
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
  });

  it("settings button label reflects open/closed state for assistive technology", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const settingsBtn = within(screen.getByTestId("projects-sidebar")).getByTestId(
      "settings-button",
    );
    expect(settingsBtn).toHaveAttribute("aria-label", "Open settings");

    await fireEvent.click(settingsBtn);
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    expect(settingsBtn).toHaveAttribute("aria-label", "Close settings");

    await fireEvent.click(settingsBtn);
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
    expect(settingsBtn).toHaveAttribute("aria-label", "Open settings");
  });

  it("settings is reachable from the title bar when the projects sidebar is collapsed", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    // Collapse the sidebar — the settings button moves to the center title bar.
    const sidebar = screen.getByTestId("projects-sidebar");
    await fireEvent.click(within(sidebar).getByTestId("projects-sidebar-toggle"));
    await waitFor(() =>
      expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument(),
    );

    // Settings button is now in the title bar with the correct closed-state label.
    const titleBarBtn = screen.getByTestId("settings-button");
    expect(titleBarBtn).toHaveAttribute("aria-label", "Open settings");

    // Opening and closing works from the title bar.
    await fireEvent.click(titleBarBtn);
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    expect(titleBarBtn).toHaveAttribute("aria-label", "Close settings");

    await fireEvent.click(titleBarBtn);
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
  });

  it("global shortcuts toggle sidebars and open settings", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    await fireEvent.keyDown(window, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());
    await fireEvent.keyDown(window, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");

    await fireEvent.keyDown(window, { key: "B", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.queryByTestId("sidebar")).not.toBeInTheDocument());
    await fireEvent.keyDown(window, { key: "B", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());

    await fireEvent.keyDown(window, { key: ",", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");

    await fireEvent.keyDown(window, { key: ",", metaKey: true });
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
  });

  it("global shortcuts are suppressed when focus is inside an editable element", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const textarea = screen.getByTestId("compose-textarea");

    // ⌘B fired from the textarea must not toggle the projects sidebar.
    await fireEvent.keyDown(textarea, { key: "b", metaKey: true });
    expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument();

    // ⌘⇧B must not toggle the agents sidebar.
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    await fireEvent.keyDown(textarea, { key: "B", metaKey: true, shiftKey: true });
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();

    // ⌘, must not open settings.
    await fireEvent.keyDown(textarea, { key: ",", metaKey: true });
    expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument();
  });
});
