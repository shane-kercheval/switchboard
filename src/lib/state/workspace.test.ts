import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { tick } from "svelte";
import type {
  AgentRecord,
  AgentSessionFingerprint,
  ConversationItem,
  ProjectConversation,
  ProjectListing,
} from "$lib/types";

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
const AGENT_2 = "00000000-0000-7000-8000-00000000000b";

function project(id: string, lastActivity: string): ProjectListing {
  return {
    id,
    name: `proj-${id.slice(-2)}`,
    created_at: "2026-05-16T00:00:00Z",
    directory: `/work/${id.slice(-2)}`,
    available: true,
    last_activity: lastActivity,
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

  it("records activity when any agent's live work ends even if the project stays busy", async () => {
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const p = project(PROJECT_1, "2026-05-16T00:00:00Z");
    ws.projects.list = [p];
    const agentA = agent(AGENT_1, PROJECT_1);
    const agentB = agent(AGENT_2, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [agentA, agentB];
    await state.registerAgent(agentA);
    await state.registerAgent(agentB);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", "send-1", p.last_activity);
    state.dispatchUserTurn(AGENT_2, "user-2", "go", "send-2", p.last_activity);
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();

    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };
    await tick();

    expect(ws.projectActivityOverrides[PROJECT_1]).toBe("2026-05-25T12:00:00.000Z");
    expect(ws.projects.list[0]).toMatchObject({
      id: PROJECT_1,
      last_activity: "2026-05-25T12:00:00.000Z",
    });
    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBeUndefined();
  });

  it("retains the error text on the conversation state when hydration fails", async () => {
    const ws = await loadWorkspaceState();
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation") throw new Error("journal read failed");
      return undefined;
    });
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    await ws.hydrateProject(PROJECT_1);

    expect(ws.conversations[PROJECT_1]?.status).toBe("failed");
    expect(ws.conversations[PROJECT_1]?.error).toBe("journal read failed");
    warnSpy.mockRestore();
  });

  it("is sticky on failure; retryProjectHydration clears the guard and re-runs", async () => {
    const ws = await loadWorkspaceState();
    let calls = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation") {
        calls += 1;
        throw new Error("still broken");
      }
      return undefined;
    });
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    await ws.hydrateProject(PROJECT_1);
    // Second call is a no-op — the per-project guard is sticky across failure.
    await ws.hydrateProject(PROJECT_1);
    expect(calls).toBe(1);

    // Retry clears the guard so the load actually re-runs.
    await ws.retryProjectHydration(PROJECT_1);
    expect(calls).toBe(2);
    expect(ws.conversations[PROJECT_1]?.status).toBe("failed");
    warnSpy.mockRestore();
  });

  it("ignores a concurrent project retry while one is already in flight", async () => {
    const ws = await loadWorkspaceState();
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation") throw new Error("boom");
      return undefined;
    });
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await ws.hydrateProject(PROJECT_1);
    expect(ws.conversations[PROJECT_1]?.status).toBe("failed");

    // Slow success; fire two retries before it resolves. The second must see
    // the in-flight "loading" status and no-op — `hydrateProject` feeds agent
    // turns through the per-agent append-merge, so a second concurrent run
    // would duplicate them.
    let resolveLoad: (v: unknown) => void = () => {};
    invokeMock.mockImplementation((cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation") {
        return new Promise((r) => {
          resolveLoad = r;
        });
      }
      return Promise.resolve(undefined);
    });
    const p1 = ws.retryProjectHydration(PROJECT_1);
    const p2 = ws.retryProjectHydration(PROJECT_1);
    // `hydrateProject` now fetches the freshness fingerprint before the load, so
    // wait until the (single) retry actually reaches `load_project_conversation`
    // before resolving it. p2 is guarded out, so the count settles at 2.
    await vi.waitFor(() =>
      expect(
        invokeMock.mock.calls.filter((c) => c[0] === "load_project_conversation"),
      ).toHaveLength(2),
    );
    resolveLoad({ items: [], agents: [] });
    await Promise.all([p1, p2]);

    const convoCalls = invokeMock.mock.calls.filter((c) => c[0] === "load_project_conversation");
    // Initial failed load + exactly one retry load = 2 (not 3).
    expect(convoCalls).toHaveLength(2);
    expect(ws.conversations[PROJECT_1]?.status).toBe("complete");
    warnSpy.mockRestore();
  });

  it("retry that succeeds clears the failed state and applies the overlay", async () => {
    const ws = await loadWorkspaceState();
    let attempt = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation") {
        attempt += 1;
        if (attempt === 1) throw new Error("boom");
        return { items: [], agents: [] };
      }
      return undefined;
    });
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    await ws.hydrateProject(PROJECT_1);
    expect(ws.conversations[PROJECT_1]?.status).toBe("failed");

    await ws.retryProjectHydration(PROJECT_1);
    expect(ws.conversations[PROJECT_1]?.status).toBe("complete");
    expect(ws.conversations[PROJECT_1]?.error).toBeUndefined();
    warnSpy.mockRestore();
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

describe("project staleness refresh", () => {
  function fp(
    agentId: string,
    refreshCapable: boolean,
    modifiedAt: string | null,
    byteLen = 100,
  ): AgentSessionFingerprint {
    return {
      agent_id: agentId,
      refresh_capable: refreshCapable,
      fingerprint:
        modifiedAt === null
          ? null
          : { source_path: `/s/${agentId}.jsonl`, modified_at: modifiedAt, byte_len: byteLen },
    };
  }

  function agentTurnItem(
    turnId: string,
    hydrationKey: string,
    startedAt: string,
    sendId?: string,
  ): ConversationItem {
    return {
      kind: "agent_turn",
      turn_id: turnId,
      agent_id: AGENT_1,
      send_id: sendId ?? null,
      started_at: startedAt,
      ended_at: startedAt,
      status: "complete",
      items: [{ item_kind: "text", kind: "text", text: "hi" }],
      hydration_key: hydrationKey,
    };
  }

  function userMessageItem(sendId: string, text: string, at: string): ConversationItem {
    return { kind: "user_message", id: sendId, send_id: sendId, agent_ids: [AGENT_1], text, at };
  }

  function hasUserMessage(
    ws: { conversations: Record<string, { items: ConversationItem[] }> },
    sendId: string,
  ): boolean {
    return (ws.conversations[PROJECT_1]?.items ?? []).some(
      (i) => i.kind === "user_message" && i.send_id === sendId,
    );
  }

  function convo(items: ConversationItem[]): ProjectConversation {
    return {
      items,
      agents: [
        { agent_id: AGENT_1, meta: null, last_rate_limit: null, warnings: [], load_error: null },
      ],
    };
  }

  // Stateful fake backend the tests mutate between activations.
  let fingerprints: AgentSessionFingerprint[] = [];
  let conversation: ProjectConversation = { items: [], agents: [] };
  let loadCount = 0;

  function installBackend(roster: AgentRecord[]): void {
    loadCount = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      switch (cmd) {
        case "open_project":
          return { id: PROJECT_1, name: "p", created_at: "2026-05-16T00:00:00Z" };
        case "list_agents":
          return roster;
        case "set_active_project":
          return undefined;
        case "project_session_fingerprints":
          return fingerprints;
        case "load_project_conversation":
          loadCount += 1;
          return conversation;
        default:
          return undefined;
      }
    });
  }

  function agentKeys(state: { transcripts: Record<string, unknown> }): (string | undefined)[] {
    const turns = (state.transcripts as Record<string, import("./types").Turn[]>)[AGENT_1] ?? [];
    return turns.map((t) => (t.role === "agent" ? t.hydration_key : undefined)).filter(Boolean);
  }

  it("re-reads on reactivation when a refresh-capable file advanced; the new turn appears exactly once", async () => {
    const ws = await loadWorkspaceState();
    const state = await loadAgentState();
    installBackend([agent(AGENT_1, PROJECT_1)]); // claude_code → refresh-capable

    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:00:00Z", 100)];
    conversation = convo([agentTurnItem("t1", "K1", "2026-05-20T00:00:01Z")]);
    expect(await ws.activateProject(PROJECT_1)).toBe("activated");
    await vi.waitFor(() => {
      expect(ws.conversations[PROJECT_1]?.status).toBe("complete");
      expect(state.transcripts[AGENT_1]?.length).toBe(1);
    });
    expect(loadCount).toBe(1);

    // The session file grew (a TUI-continued turn). Reactivate.
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:05:00Z", 250)];
    conversation = convo([
      agentTurnItem("t1-reparse", "K1", "2026-05-20T00:00:01Z"),
      agentTurnItem("t2", "K2", "2026-05-20T00:06:00Z"),
    ]);
    expect(await ws.activateProject(PROJECT_1)).toBe("activated");

    expect(loadCount).toBe(2); // a re-read happened
    expect(state.transcripts[AGENT_1]?.length).toBe(2); // K1 deduped, K2 added — no dup
    expect(agentKeys(state).sort()).toEqual(["K1", "K2"]);
  });

  it("does NOT re-read when the fingerprint is unchanged", async () => {
    const ws = await loadWorkspaceState();
    const state = await loadAgentState();
    installBackend([agent(AGENT_1, PROJECT_1)]);
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:00:00Z", 100)];
    conversation = convo([agentTurnItem("t1", "K1", "2026-05-20T00:00:01Z")]);

    await ws.activateProject(PROJECT_1);
    await vi.waitFor(() => expect(state.transcripts[AGENT_1]?.length).toBe(1));
    expect(loadCount).toBe(1);

    // Same fingerprint → reactivation must NOT call load_project_conversation.
    expect(await ws.activateProject(PROJECT_1)).toBe("activated");
    expect(loadCount).toBe(1);
  });

  it("never refreshes a non-refresh-capable agent, even if its file changed", async () => {
    const ws = await loadWorkspaceState();
    const state = await loadAgentState();
    const codex = agent(AGENT_1, PROJECT_1);
    codex.harness = "codex";
    installBackend([codex]);
    // Backend reports refresh_capable:false (and never stats the file).
    fingerprints = [fp(AGENT_1, false, null)];
    conversation = convo([agentTurnItem("t1", "K1", "2026-05-20T00:00:01Z")]);

    await ws.activateProject(PROJECT_1);
    await vi.waitFor(() => expect(state.transcripts[AGENT_1]?.length).toBe(1));
    expect(loadCount).toBe(1);

    // Even a (defensively) "changed" fingerprint must not trigger a re-read for a
    // non-refresh-capable harness.
    fingerprints = [fp(AGENT_1, false, "2026-05-20T00:05:00Z", 999)];
    expect(await ws.activateProject(PROJECT_1)).toBe("activated");
    expect(loadCount).toBe(1);
  });

  it("preserves a live in-flight turn across a refresh", async () => {
    const ws = await loadWorkspaceState();
    const state = await loadAgentState();
    installBackend([agent(AGENT_1, PROJECT_1)]);
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:00:00Z", 100)];
    conversation = convo([agentTurnItem("t1", "K1", "2026-05-20T00:00:01Z")]);
    await ws.activateProject(PROJECT_1);
    await vi.waitFor(() => expect(state.transcripts[AGENT_1]?.length).toBe(1));

    // A live streaming turn arrives (no hydration_key yet — keyed by turn_id).
    state.transcripts[AGENT_1] = [
      ...(state.transcripts[AGENT_1] ?? []),
      {
        role: "agent",
        turn_id: "live-1",
        agent_id: AGENT_1,
        started_at: "2026-05-20T00:07:00Z",
        status: "streaming",
        items: [],
      },
    ];

    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:05:00Z", 250)];
    conversation = convo([
      agentTurnItem("t1-reparse", "K1", "2026-05-20T00:00:01Z"),
      agentTurnItem("t2", "K2", "2026-05-20T00:06:00Z"),
    ]);
    await ws.activateProject(PROJECT_1);

    const live = (state.transcripts[AGENT_1] ?? []).find((t) => t.turn_id === "live-1");
    expect(live).toBeDefined();
    expect(live?.role === "agent" && live.status).toBe("streaming");
    // The new disk turn appeared exactly once, alongside the preserved live turn.
    expect(agentKeys(state).sort()).toEqual(["K1", "K2"]);
  });

  it("preserves a historical journaled prompt across refresh (overlay not over-filtered)", async () => {
    const ws = await loadWorkspaceState();
    installBackend([agent(AGENT_1, PROJECT_1)]);
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:00:00Z", 100)];
    // A journaled send: its prompt is an overlay user_message; its reply is a
    // slice agent_turn carrying the SAME send_id (the journal join stamps it).
    conversation = convo([
      userMessageItem("send-H", "historical prompt", "2026-05-20T00:00:00Z"),
      agentTurnItem("t1", "K1", "2026-05-20T00:00:01Z", "send-H"),
    ]);
    await ws.activateProject(PROJECT_1);
    await vi.waitFor(() => expect(ws.conversations[PROJECT_1]?.status).toBe("complete"));
    expect(hasUserMessage(ws, "send-H")).toBe(true);

    // Refresh (file grew). The historical agent turn's send_id must NOT cause the
    // historical user_message to be dropped from the overlay.
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:05:00Z", 250)];
    conversation = convo([
      userMessageItem("send-H", "historical prompt", "2026-05-20T00:00:00Z"),
      agentTurnItem("t1-reparse", "K1", "2026-05-20T00:00:01Z", "send-H"),
      agentTurnItem("t2", "K2", "2026-05-20T00:06:00Z"),
    ]);
    await ws.activateProject(PROJECT_1);

    expect(hasUserMessage(ws, "send-H")).toBe(true);
  });

  it("suppresses a this-session send's user_message from the overlay on refresh", async () => {
    const ws = await loadWorkspaceState();
    const state = await loadAgentState();
    installBackend([agent(AGENT_1, PROJECT_1)]);
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:00:00Z", 100)];
    conversation = convo([]);
    await ws.activateProject(PROJECT_1);
    await vi.waitFor(() => expect(ws.conversations[PROJECT_1]?.status).toBe("complete"));

    // Dispatch a send THIS session → a user turn lands in the slice with send_id.
    state.dispatchUserTurn(AGENT_1, "u-live", "hi there", "send-L", "2026-05-20T00:03:00Z");

    // The re-read journal now also carries that send. After refresh, the overlay
    // must NOT contain its user_message — it renders from the live slice instead.
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:05:00Z", 250)];
    conversation = convo([
      userMessageItem("send-L", "hi there", "2026-05-20T00:03:00Z"),
      agentTurnItem("t-live", "K-live", "2026-05-20T00:04:00Z", "send-L"),
    ]);
    await ws.activateProject(PROJECT_1);

    expect(hasUserMessage(ws, "send-L")).toBe(false);
  });

  it("keeps the loaded conversation intact when a refresh re-read fails", async () => {
    const ws = await loadWorkspaceState();
    const state = await loadAgentState();
    installBackend([agent(AGENT_1, PROJECT_1)]);
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:00:00Z", 100)];
    conversation = convo([
      userMessageItem("send-H", "hello", "2026-05-20T00:00:00Z"),
      agentTurnItem("t1", "K1", "2026-05-20T00:00:01Z", "send-H"),
    ]);
    await ws.activateProject(PROJECT_1);
    await vi.waitFor(() => expect(ws.conversations[PROJECT_1]?.status).toBe("complete"));
    const itemsBefore = ws.conversations[PROJECT_1]?.items.length;

    // Refresh: fingerprint advanced, but the re-read now throws (transient).
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:05:00Z", 250)];
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      switch (cmd) {
        case "open_project":
          return { id: PROJECT_1, name: "p", created_at: "2026-05-16T00:00:00Z" };
        case "list_agents":
          return [agent(AGENT_1, PROJECT_1)];
        case "set_active_project":
          return undefined;
        case "project_session_fingerprints":
          return fingerprints;
        case "load_project_conversation":
          throw new Error("transient re-read failure");
        default:
          return undefined;
      }
    });
    expect(await ws.activateProject(PROJECT_1)).toBe("activated");

    // The known-good view survives a failed best-effort refresh.
    expect(ws.conversations[PROJECT_1]?.status).toBe("complete");
    expect(ws.conversations[PROJECT_1]?.items.length).toBe(itemsBefore);
    expect(ws.conversations[PROJECT_1]?.error).toBeUndefined();
    expect(hasUserMessage(ws, "send-H")).toBe(true);
    warnSpy.mockRestore();

    // The baseline was left unchanged, so the next switch-back retries and succeeds.
    installBackend([agent(AGENT_1, PROJECT_1)]);
    conversation = convo([
      userMessageItem("send-H", "hello", "2026-05-20T00:00:00Z"),
      agentTurnItem("t1", "K1", "2026-05-20T00:00:01Z", "send-H"),
      agentTurnItem("t2", "K2", "2026-05-20T00:06:00Z"),
    ]);
    await ws.activateProject(PROJECT_1);
    expect(loadCount).toBe(1); // installBackend reset the counter; the retry re-read ran once
    expect(state.transcripts[AGENT_1]?.length).toBe(2);
  });
});
