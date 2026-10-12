import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type { AgentRecord, ProjectConversation } from "$lib/types";
// Static imports so the component-tree transform happens at module collection,
// not inside the first test's timeout. `vi.mock` is hoisted above imports.
import ComposeBar from "./ComposeBar.svelte";
import Sidebar from "./Sidebar.svelte";

/// A Codex agent card's Fork item, end to end through the real stores: only the
/// backend calls are faked. `Sidebar.test.ts` covers when the item is offered;
/// this file covers what choosing it does to the rest of the app.

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async () => vi.fn(),
  }),
}));

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";
const PARENT_ID = "00000000-0000-7000-8000-000000000bbb";
const FORK_ID = "00000000-0000-7000-8000-000000000bbc";

function codexAgent(id: string, name: string): AgentRecord {
  return {
    id,
    project_id: PROJECT_ID,
    name,
    harness: "codex",
    session_locator: {
      codex: { thread_id: `thread-${id.slice(-3)}`, partition_date: "2026-10-11" },
    },
    model: null,
    effort: null,
    model_choices: [],
    effort_choices: [],
    created_at: "2026-05-16T00:00:00Z",
  };
}

const PARENT = codexAgent(PARENT_ID, "cx");
const FORK = codexAgent(FORK_ID, "cx-fork");

/// The project as the backend merges it: the parent's one exchange, and, once
/// the fork exists, the fork's inherited copy of it (an imported prompt with no
/// send, and the reply under the parent's hydration key).
function conversation(withFork: boolean): ProjectConversation {
  const exchange = (agentId: string) => [
    {
      kind: "user_message" as const,
      id: `imported-${agentId}`,
      send_id: null,
      agent_ids: [agentId],
      text: `earlier prompt seen by ${agentId}`,
      at: "2026-05-15T00:00:00Z",
    },
    {
      kind: "agent_turn" as const,
      turn_id: `reply-${agentId}`,
      agent_id: agentId,
      send_id: null,
      started_at: "2026-05-15T00:00:01Z",
      ended_at: "2026-05-15T00:00:02Z",
      status: "complete" as const,
      items: [{ item_kind: "text" as const, kind: "text" as const, text: "earlier reply" }],
      hydration_key: "parent-key",
    },
  ];
  const agents = withFork ? [PARENT_ID, FORK_ID] : [PARENT_ID];
  return {
    items: agents.flatMap(exchange),
    agents: agents.map((agentId) => ({ agent_id: agentId, warnings: [] })),
  };
}

type Backend = {
  forkAgent: () => Promise<AgentRecord>;
  loadConversation: () => Promise<ProjectConversation>;
};

let backend: Backend;
let calls: string[];

async function workspace() {
  return await import("$lib/state/workspace.svelte");
}

async function agentState() {
  return await import("$lib/state/index.svelte");
}

/// An open project holding the Codex parent (and any `others`) with its history
/// loaded, which is what the Fork item waits for. Its panes have never been
/// touched, so it shows the unified view.
async function openProject(others: AgentRecord[] = []): Promise<void> {
  const ws = await workspace();
  const state = await agentState();
  ws.agentsByProject[PROJECT_ID] = [PARENT, ...others];
  for (const agent of [PARENT, ...others]) await state.registerAgent(agent);
  await ws.hydrateProject(PROJECT_ID);
  expect(ws.conversations[PROJECT_ID]?.status).toBe("complete");
  calls.length = 0;
}

async function chooseFork(): Promise<void> {
  const trigger = (await screen.findAllByTestId("agent-actions-trigger"))[0];
  if (trigger === undefined) throw new Error("expected the parent's menu");
  await fireEvent.click(trigger);
  const menu = await screen.findByTestId("agent-actions-menu");
  await fireEvent.click(within(menu).getByTestId("agent-action-fork"));
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
} {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((r, fail) => {
    resolve = r;
    reject = fail;
  });
  return { promise, resolve, reject };
}

beforeEach(async () => {
  (await import("$lib/state/transcriptPanes.svelte"))._testing.reset();
  (await import("$lib/state/recipientSelection.svelte"))._testing.reset();
  (await import("$lib/state/composeOperations.svelte"))._testing.reset();
  (await import("$lib/layout.svelte"))._testing.reset();
  calls = [];
  let forked = false;
  backend = {
    forkAgent: async () => {
      forked = true;
      return FORK;
    },
    loadConversation: async () => conversation(forked),
  };
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    calls.push(cmd);
    if (cmd === "fork_agent") return await backend.forkAgent();
    if (cmd === "load_project_conversation") return await backend.loadConversation();
    if (cmd === "existing_attachment_paths") return [];
    if (cmd === "send_message") return "msg-1";
    return null;
  });
});

afterEach(async () => {
  (await agentState())._testing.reset();
  (await workspace())._testing.reset();
  (await import("$lib/state/composeStore"))._testing.reset();
});

describe("a Codex agent card's Fork item", () => {
  it("creates an idle fork in its own pane, with its earlier conversation, and sends nothing", async () => {
    await openProject();
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [PARENT] } });

    await chooseFork();

    const ws = await workspace();
    const state = await agentState();
    const panes = await import("$lib/state/transcriptPanes.svelte");
    await waitFor(() =>
      expect(ws.agentsByProject[PROJECT_ID]?.map((a) => a.id)).toEqual([PARENT_ID, FORK_ID]),
    );
    const roster = [PARENT_ID, FORK_ID];
    await waitFor(() => expect(panes.paneOfAgent(PROJECT_ID, roster, FORK_ID)).not.toBeNull());
    expect(panes.paneOfAgent(PROJECT_ID, roster, FORK_ID)?.id).not.toBe(
      panes.paneOfAgent(PROJECT_ID, roster, PARENT_ID)?.id,
    );
    await waitFor(() =>
      expect(state.transcripts[FORK_ID]?.filter((turn) => turn.role === "agent")).toHaveLength(1),
    );
    expect(calls.filter((cmd) => cmd === "fork_agent")).toHaveLength(1);
    expect(calls).not.toContain("send_message");
    expect(state.runtimes[FORK_ID]?.run_status).toBe("idle");
    expect(state.runtimes[PARENT_ID]?.run_status).toBe("idle");
  });

  it("disables compose sends while the fork is being created, then enables them", async () => {
    // The original's session is locked while Codex creates the fork, so a
    // compose send to it in that second would be refused with "resend".
    await openProject();
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [PARENT] } });
    render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [PARENT] } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "for the original" } });
    const send = screen.getByTestId("compose-send") as HTMLButtonElement;
    expect(send.disabled).toBe(false);

    const succeeds = deferred<AgentRecord>();
    backend.forkAgent = () => succeeds.promise;
    await chooseFork();
    await waitFor(() => expect(send.disabled).toBe(true));
    succeeds.resolve(FORK);
    await waitFor(() => expect(send.disabled).toBe(false));

    const fails = deferred<AgentRecord>();
    backend.forkAgent = () => fails.promise;
    await chooseFork();
    await waitFor(() => expect(send.disabled).toBe(true));
    fails.reject(new Error("cx is busy"));
    await waitFor(() => expect(send.disabled).toBe(false));
    expect(await screen.findByTestId("agent-fork-error")).toHaveTextContent(
      "Couldn't fork cx: cx is busy",
    );
    expect(calls).not.toContain("send_message");
  });

  it("keeps every agent the unified view showed when the fork takes its own pane", async () => {
    // A fork isn't a layout gesture, so splitting the unified view must not
    // hide the agents it was showing.
    const claudeAgent = (id: string, name: string): AgentRecord => ({
      ...codexAgent(id, name),
      harness: "claude_code",
      session_locator: { uuid: id },
    });
    const others = [
      claudeAgent("00000000-0000-7000-8000-000000000aa1", "claude-a"),
      claudeAgent("00000000-0000-7000-8000-000000000aa2", "claude-b"),
    ];
    await openProject(others);
    const originals = [PARENT_ID, ...others.map((a) => a.id)];
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [PARENT, ...others] } });

    await chooseFork();

    const panes = await import("$lib/state/transcriptPanes.svelte");
    const roster = [...originals, FORK_ID];
    await waitFor(() =>
      expect(panes.paneOfAgent(PROJECT_ID, roster, FORK_ID)?.members).toEqual([FORK_ID]),
    );
    expect(panes.paneOfAgent(PROJECT_ID, roster, PARENT_ID)?.members).toEqual(originals);
    expect(panes.unassignedAgentIds(PROJECT_ID, roster)).toEqual([]);
  });

  it("frees the compose bar once the fork exists, while only the fork waits for its history", async () => {
    // The compose bar is held only for the original's lock during creation.
    // The fork's earlier conversation rereads the whole project, so until it
    // arrives only sends to the fork wait, which also keeps that read from
    // overlapping the fork's own first turn.
    await openProject();
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [PARENT] } });
    const compose = render(ComposeBar, { props: { projectId: PROJECT_ID, agents: [PARENT] } });
    const history = deferred<ProjectConversation>();
    backend.loadConversation = () => history.promise;

    await chooseFork();

    const ops = await import("$lib/state/composeOperations.svelte");
    const state = await agentState();
    await waitFor(() => expect(ops.operationFor(PROJECT_ID)).toBeUndefined());
    expect(screen.queryByTestId("agent-fork-pending")).toBeNull();
    expect(state.runtimes[FORK_ID]?.hydration_status).toBe("loading");

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "for the original" } });
    await fireEvent.click(screen.getByTestId("compose-send"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "send_message",
        expect.objectContaining({ agentId: PARENT_ID, prompt: "for the original" }),
      ),
    );

    const selection = await import("$lib/state/recipientSelection.svelte");
    await compose.rerender({ projectId: PROJECT_ID, agents: [PARENT, FORK] });
    selection.setRecipients(PROJECT_ID, [FORK_ID]);
    await fireEvent.input(textarea, { target: { value: "for the fork" } });
    const send = screen.getByTestId("compose-send") as HTMLButtonElement;
    await waitFor(() => expect(send.disabled).toBe(true));

    history.resolve(conversation(true));

    await waitFor(() => expect(state.runtimes[FORK_ID]?.hydration_status).toBe("complete"));
    await waitFor(() => expect(send.disabled).toBe(false));
    expect(state.transcripts[FORK_ID]?.filter((turn) => turn.role === "agent")).toHaveLength(1);
    expect(invokeMock).not.toHaveBeenCalledWith(
      "send_message",
      expect.objectContaining({ agentId: FORK_ID }),
    );
  });

  it("never leaves the fork loading when the project is reloading as its read comes up", async () => {
    // The fork's read is skipped then, because the reload rereads every agent,
    // the fork included. The fork must not be left loading, with every send to
    // it refused.
    await openProject();
    render(Sidebar, { props: { projectId: PROJECT_ID, agents: [PARENT] } });
    const ws = await workspace();
    backend.forkAgent = async () => {
      ws.conversations[PROJECT_ID] = { items: [], status: "loading" };
      return FORK;
    };

    await chooseFork();

    const state = await agentState();
    await waitFor(() => expect(state.runtimes[FORK_ID]).toBeDefined());
    await waitFor(() => expect(state.runtimes[FORK_ID]?.hydration_status).not.toBe("loading"));
    expect(state.runtimes[FORK_ID]?.hydration_status).toBe("complete");
    expect(state.runtimes[FORK_ID]?.hydration_error).toBeUndefined();
  });

  it("shows a failed history load on the fork's card, and Retry loads it without a turn", async () => {
    await openProject();
    const view = render(Sidebar, { props: { projectId: PROJECT_ID, agents: [PARENT] } });
    const load = backend.loadConversation;
    backend.loadConversation = async () => {
      throw new Error("disk went away");
    };

    await chooseFork();

    const ws = await workspace();
    const state = await agentState();
    await waitFor(() => expect(state.runtimes[FORK_ID]?.hydration_status).toBe("failed"));
    await view.rerender({ projectId: PROJECT_ID, agents: [PARENT, FORK] });
    const forkCard = (await screen.findAllByTestId("sidebar-agent")).find(
      (card) => card.getAttribute("data-agent-id") === FORK_ID,
    );
    if (forkCard === undefined) throw new Error("expected the fork's card");
    expect(within(forkCard).getByTestId("agent-hydration-error")).toHaveTextContent(
      "disk went away",
    );

    backend.loadConversation = load;
    await fireEvent.click(within(forkCard).getByTestId("agent-hydration-retry"));

    await waitFor(() => expect(state.runtimes[FORK_ID]?.hydration_status).toBe("complete"));
    expect(within(forkCard).queryByTestId("agent-hydration-error")).toBeNull();
    const inherited = (ws.conversations[PROJECT_ID]?.items ?? []).filter(
      (item) => item.kind === "user_message" && item.agent_ids.includes(FORK_ID),
    );
    expect(inherited).toHaveLength(1);
    expect(state.transcripts[FORK_ID]?.filter((turn) => turn.role === "agent")).toHaveLength(1);
    expect(calls.filter((cmd) => cmd === "fork_agent")).toHaveLength(1);
    expect(calls).not.toContain("send_message");
  });
});
