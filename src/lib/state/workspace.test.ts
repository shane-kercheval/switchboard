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
// Capture per-agent channel callbacks so a test can fire real backend events
// (the fork-history refresh is driven from the `turn_end` boundary, so a
// synthetic hook call would test the wiring rather than the behaviour).
const listeners = new Map<string, (e: { payload: unknown }) => void>();
/// Default: record the callback and succeed. A test can override this to make a
/// specific channel's subscription fail.
const listenMock = vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
  listeners.set(name, cb);
  return vi.fn();
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => listenMock(name, cb),
}));

function fireTo(channel: string, payload: unknown): void {
  const cb = listeners.get(channel);
  if (cb === undefined) throw new Error(`no listener for ${channel}`);
  cb({ payload });
}

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
  const layoutStore = await import("$lib/layout.svelte");
  layoutStore._testing.reset();
  const prefs = await import("$lib/preferences.svelte");
  prefs._testing.reset();
  const workflows = await import("$lib/state/workflows.svelte");
  workflows._testing.reset();
});

describe("workspace project activity", () => {
  it("sorts mixed-precision project activity chronologically", async () => {
    const ws = await loadWorkspaceState();
    ws.projects.list = [
      project(PROJECT_1, "2026-05-25T12:00:00Z"),
      project(PROJECT_2, "2026-05-25T12:00:00.500Z"),
    ];

    ws.recordProjectsActivityLocally([PROJECT_1], "2026-05-25T12:00:00Z");

    expect(ws.projects.list.map((item) => item.id)).toEqual([PROJECT_2, PROJECT_1]);
  });

  it("accepts a valid activity override but rejects an invalid one", async () => {
    const ws = await loadWorkspaceState();
    ws.projects.list = [project(PROJECT_1, "invalid")];

    ws.recordProjectsActivityLocally([PROJECT_1], "2026-05-25T12:00:00Z");
    expect(ws.projects.list[0]?.last_activity).toBe("2026-05-25T12:00:00Z");

    ws.recordProjectsActivityLocally([PROJECT_1], "still-invalid");
    expect(ws.projects.list[0]?.last_activity).toBe("2026-05-25T12:00:00Z");
  });

  it("selects the chronologically oldest unread project across mixed precision", async () => {
    const ws = await loadWorkspaceState();
    ws.projects.list = [
      project(PROJECT_2, "2026-05-25T12:00:00.500Z"),
      project(PROJECT_1, "2026-05-25T12:00:00Z"),
    ];
    ws.backgroundCompletedProjectIds[PROJECT_1] = true;
    ws.backgroundCompletedProjectIds[PROJECT_2] = true;

    expect(ws.nextUnreadCompletedProjectId()).toBe(PROJECT_1);
  });

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
    state.dispatchUserTurn(AGENT_1, "user-1", "go", [], "send-1", staleBackground.last_activity);
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
    state.dispatchUserTurn(AGENT_1, "user-1", "go", [], "send-1", p.last_activity);
    state.dispatchUserTurn(AGENT_2, "user-2", "go", [], "send-2", p.last_activity);
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
    const layoutStore = await import("$lib/layout.svelte");
    const busyProject = project(PROJECT_1, "2026-05-16T00:00:00Z");
    ws.projects.list = [busyProject];
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a];
    layoutStore.layout.setRightSidebarMode(PROJECT_1, "pins");
    layoutStore.layout.setPinsSortMode(PROJECT_1, "message_at");
    await state.registerAgent(a);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", [], "send-1", busyProject.last_activity);
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
    expect(layoutStore.layout.rightSidebarModeFor(PROJECT_1)).toBe("pins");
    expect(layoutStore.layout.pinsSortModeFor(PROJECT_1)).toBe("message_at");
  });
});

describe("project preference lifecycle", () => {
  it("clears project layout preferences after permanent deletion", async () => {
    const ws = await loadWorkspaceState();
    const layoutStore = await import("$lib/layout.svelte");
    ws.projects.list = [project(PROJECT_1, "2026-05-16T00:00:00Z")];
    layoutStore.layout.setRightSidebarMode(PROJECT_1, "pins");
    layoutStore.layout.setPinsSortMode(PROJECT_1, "message_at");
    invokeMock.mockResolvedValue(undefined);

    await ws.deleteProject(PROJECT_1);

    expect(layoutStore.layout.rightSidebarModeFor(PROJECT_1)).toBe("agents");
    expect(layoutStore.layout.pinsSortModeFor(PROJECT_1)).toBe("pinned_at");
  });

  it("shares one in-flight deletion per project", async () => {
    const ws = await loadWorkspaceState();
    ws.projects.list = [project(PROJECT_1, "2026-05-16T00:00:00Z")];
    let resolveDelete!: () => void;
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "delete_project"
        ? new Promise<void>((resolve) => {
            resolveDelete = resolve;
          })
        : undefined,
    );

    const first = ws.deleteProject(PROJECT_1);
    const second = ws.deleteProject(PROJECT_1);

    expect(second).toBe(first);
    expect(ws.projectDeletions.pending[PROJECT_1]).toBe(true);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "delete_project")).toHaveLength(1);

    resolveDelete();
    await Promise.all([first, second]);
    expect(ws.projectDeletions.pending[PROJECT_1]).toBeUndefined();
  });

  it("retains and dismisses a project-scoped deletion failure", async () => {
    const ws = await loadWorkspaceState();
    ws.projects.list = [project(PROJECT_1, "2026-05-16T00:00:00Z")];
    invokeMock.mockImplementation(async (cmd) => {
      if (cmd === "delete_project") throw new Error("disk busy");
      return undefined;
    });

    await expect(ws.deleteProject(PROJECT_1)).rejects.toThrow("disk busy");
    expect(ws.projectDeletions.pending[PROJECT_1]).toBeUndefined();
    expect(ws.projectDeletions.errors[PROJECT_1]).toBe("disk busy");

    ws.dismissProjectDeletionError(PROJECT_1);
    expect(ws.projectDeletions.errors[PROJECT_1]).toBeUndefined();
  });
});

describe("activation failure classification", () => {
  it.each([
    ["project_not_loaded", "project_not_loaded"],
    ["project_locked", "project_locked"],
    ["future_failure", "other"],
  ] as const)("maps %s to the safe frontend kind %s", async (wireType, expectedType) => {
    const ws = await loadWorkspaceState();
    invokeMock.mockImplementation(async (cmd) => {
      if (cmd === "open_project") throw { type: wireType, message: "activation failed" };
      return undefined;
    });

    expect(await ws.activateProject(PROJECT_1)).toBe("failed");
    expect(ws.selection.activationFailure).toEqual({
      type: expectedType,
      message: "activation failed",
    });
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
    model?: string,
    effort?: string,
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
      model: model ?? null,
      effort: effort ?? null,
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

  it("carries per-turn model/effort from agent_turn through the regroup", async () => {
    // The project-conversation regroup is hand-built (LoadedTurn assembled field
    // by field); a field not copied there is silently dropped — which is exactly
    // how the footer's model went missing on restart. Guards the frontend half of
    // that boundary (the Rust half is guarded in commands.rs).
    const ws = await loadWorkspaceState();
    const state = await loadAgentState();
    installBackend([agent(AGENT_1, PROJECT_1)]);
    fingerprints = [fp(AGENT_1, true, "2026-05-20T00:00:00Z", 100)];
    conversation = convo([
      agentTurnItem("t1", "K1", "2026-05-20T00:00:01Z", undefined, "claude-opus-4-8", "high"),
    ]);

    await ws.activateProject(PROJECT_1);
    await vi.waitFor(() => expect(state.transcripts[AGENT_1]?.length).toBe(1));

    const turn = state.transcripts[AGENT_1]![0]!;
    expect(turn.role).toBe("agent");
    if (turn.role === "agent") {
      expect(turn.model).toBe("claude-opus-4-8");
      expect(turn.effort).toBe("high");
    }
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
    state.dispatchUserTurn(AGENT_1, "u-live", "hi there", [], "send-L", "2026-05-20T00:03:00Z");

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

describe("agent model profiles", () => {
  it("setAgentProfiles updates both profiles in one call and replaces the roster record", async () => {
    const ws = await loadWorkspaceState();
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a, agent(AGENT_2, PROJECT_1)];
    const updated: AgentRecord = {
      ...a,
      model: "opus",
      effort: "high",
      profiles: {
        active: "primary",
        secondary: { model: "sonnet", effort: "medium" },
      },
    };
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "set_agent_profiles" ? updated : undefined,
    );

    await ws.setAgentProfiles(
      AGENT_1,
      { model: "opus", effort: "high" },
      { model: "sonnet", effort: "medium" },
    );

    expect(invokeMock).toHaveBeenCalledWith("set_agent_profiles", {
      agentId: AGENT_1,
      primary: { model: "opus", effort: "high" },
      secondary: { model: "sonnet", effort: "medium" },
    });
    expect(ws.agentsByProject[PROJECT_1]?.find((r) => r.id === AGENT_1)).toEqual(updated);
    expect(ws.agentsByProject[PROJECT_1]?.find((r) => r.id === AGENT_2)?.model).toBeUndefined();
  });

  it("setActiveAgentProfile persists the active slot and replaces the roster record", async () => {
    const ws = await loadWorkspaceState();
    const a: AgentRecord = {
      ...agent(AGENT_1, PROJECT_1),
      profiles: {
        active: "primary",
        secondary: { model: "sonnet", effort: "medium" },
      },
    };
    ws.agentsByProject[PROJECT_1] = [a];
    const switched: AgentRecord = { ...a, profiles: { ...a.profiles!, active: "secondary" } };
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "set_active_agent_profile" ? switched : undefined,
    );

    await ws.setActiveAgentProfile(AGENT_1, "secondary");

    expect(invokeMock).toHaveBeenCalledWith("set_active_agent_profile", {
      agentId: AGENT_1,
      active: "secondary",
    });
    expect(ws.agentsByProject[PROJECT_1]?.[0]).toEqual(switched);
  });
});

describe("agent reorder", () => {
  const AGENT_3 = "00000000-0000-7000-8000-00000000000c";

  it("applies the new order optimistically, then reconciles with the backend reply", async () => {
    const ws = await loadWorkspaceState();
    const a = agent(AGENT_1, PROJECT_1);
    const b = agent(AGENT_2, PROJECT_1);
    const c = agent(AGENT_3, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a, b, c];

    let resolveBackend!: (records: AgentRecord[]) => void;
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "reorder_agents"
        ? new Promise<AgentRecord[]>((r) => {
            resolveBackend = r;
          })
        : undefined,
    );

    const call = ws.reorderAgents(PROJECT_1, [c.id, a.id, b.id]);
    // Optimistic: the roster reorders before the backend replies.
    expect(ws.agentsByProject[PROJECT_1]?.map((r) => r.id)).toEqual([c.id, a.id, b.id]);

    resolveBackend([c, a, b]);
    await call;
    expect(invokeMock).toHaveBeenCalledWith("reorder_agents", {
      projectId: PROJECT_1,
      agentIds: [c.id, a.id, b.id],
    });
    expect(ws.agentsByProject[PROJECT_1]?.map((r) => r.id)).toEqual([c.id, a.id, b.id]);
  });

  it("reverts to the previous order and rethrows when the backend rejects", async () => {
    const ws = await loadWorkspaceState();
    const a = agent(AGENT_1, PROJECT_1);
    const b = agent(AGENT_2, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a, b];
    invokeMock.mockImplementation(async (cmd) => {
      if (cmd === "reorder_agents") throw new Error("roster changed");
      return undefined;
    });

    await expect(ws.reorderAgents(PROJECT_1, [b.id, a.id])).rejects.toThrow("roster changed");
    expect(ws.agentsByProject[PROJECT_1]?.map((r) => r.id)).toEqual([a.id, b.id]);
  });

  it("is a no-op for a project with no loaded roster", async () => {
    const ws = await loadWorkspaceState();
    await ws.reorderAgents(PROJECT_1, [AGENT_1]);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("drops a concurrent reorder for the same project and makes only one backend call", async () => {
    const ws = await loadWorkspaceState();
    const a = agent(AGENT_1, PROJECT_1);
    const b = agent(AGENT_2, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a, b];

    let resolveFirst!: (records: AgentRecord[]) => void;
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "reorder_agents"
        ? new Promise<AgentRecord[]>((r) => {
            resolveFirst = r;
          })
        : undefined,
    );

    // First call: in-flight.
    const first = ws.reorderAgents(PROJECT_1, [b.id, a.id]);
    // Optimistic order applied.
    expect(ws.agentsByProject[PROJECT_1]?.map((r) => r.id)).toEqual([b.id, a.id]);

    // Second call: dropped while first is still in-flight.
    await ws.reorderAgents(PROJECT_1, [a.id, b.id]);
    // Optimistic order unchanged (second call was a no-op).
    expect(ws.agentsByProject[PROJECT_1]?.map((r) => r.id)).toEqual([b.id, a.id]);

    resolveFirst([b, a]);
    await first;
    // Only one backend call was made.
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(ws.agentsByProject[PROJECT_1]?.map((r) => r.id)).toEqual([b.id, a.id]);
  });

  it("does not apply the optimistic update for a duplicate-id list", async () => {
    const ws = await loadWorkspaceState();
    const a = agent(AGENT_1, PROJECT_1);
    const b = agent(AGENT_2, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a, b];

    let resolveBackend!: (records: AgentRecord[]) => void;
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "reorder_agents"
        ? new Promise<AgentRecord[]>((r) => {
            resolveBackend = r;
          })
        : undefined,
    );

    // [b.id, b.id] passes the length checks but is not a valid permutation.
    const call = ws.reorderAgents(PROJECT_1, [b.id, b.id]);
    // Roster must not briefly show a duplicate entry while the call is in
    // flight — the optimistic gate is what's under test here. (The real
    // backend would reject the list; either way the store reconciles to a
    // backend-authoritative order on settle.)
    expect(ws.agentsByProject[PROJECT_1]?.map((r) => r.id)).toEqual([a.id, b.id]);

    resolveBackend([a, b]);
    await call;
    expect(ws.agentsByProject[PROJECT_1]?.map((r) => r.id)).toEqual([a.id, b.id]);
  });
});

describe("forkAgentIntoOwnPane", () => {
  const FORK_ID = "00000000-0000-7000-8000-00000000000f";

  // The file's shared `afterEach` doesn't reset pane layout, and these cases
  // assert on placement — without this they'd inherit the previous case's panes
  // and silently stop testing what they claim to.
  beforeEach(async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    panes._testing.reset();
  });

  function forkRecord(projectId: string): AgentRecord {
    return { ...agent(FORK_ID, projectId), name: "agent-a-fork" };
  }

  it("registers, rosters, and gives the branch its own visible pane", async () => {
    const ws = await loadWorkspaceState();
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const parent = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [parent];
    const fork = forkRecord(PROJECT_1);
    invokeMock.mockImplementation(async (cmd) => (cmd === "fork_agent" ? fork : undefined));

    const created = await ws.forkAgentIntoOwnPane(parent.id);

    expect(created.id).toBe(FORK_ID);
    expect(invokeMock).toHaveBeenCalledWith("fork_agent", { agentId: parent.id });
    // Rostered, so `lookup_agent` on the immediately-following send resolves it.
    expect(ws.agentsByProject[PROJECT_1]?.map((a) => a.id)).toEqual([parent.id, FORK_ID]);

    // Its own pane, not the parent's: they share history with identical
    // timestamps, so co-paning would render every inherited message twice.
    const rosterIds = [parent.id, FORK_ID];
    const forkPane = panes.paneOfAgent(PROJECT_1, rosterIds, FORK_ID);
    const parentPane = panes.paneOfAgent(PROJECT_1, rosterIds, parent.id);
    expect(forkPane).not.toBeNull();
    expect(forkPane?.id).not.toBe(parentPane?.id);
    // And the parent stayed where it was.
    expect(parentPane?.members).toContain(parent.id);
  });

  it("keeps only the fork source in an untouched all-agent pane", async () => {
    const ws = await loadWorkspaceState();
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const parent = agent(AGENT_1, PROJECT_1);
    const codex = { ...agent(AGENT_2, PROJECT_1), harness: "codex" as const };
    const antigravity = {
      ...agent("00000000-0000-7000-8000-00000000000c", PROJECT_1),
      harness: "antigravity" as const,
    };
    ws.agentsByProject[PROJECT_1] = [parent, codex, antigravity];
    const originalRoster = [parent.id, codex.id, antigravity.id];
    expect(panes.layoutFor(PROJECT_1, originalRoster).panes[0]?.members).toEqual(originalRoster);

    const fork = forkRecord(PROJECT_1);
    invokeMock.mockImplementation(async (cmd) => (cmd === "fork_agent" ? fork : undefined));
    await ws.forkAgentIntoOwnPane(parent.id);

    const rosterIds = [...originalRoster, FORK_ID];
    const parentPane = panes.paneOfAgent(PROJECT_1, rosterIds, parent.id);
    const forkPane = panes.paneOfAgent(PROJECT_1, rosterIds, FORK_ID);
    expect(parentPane?.members).toEqual([parent.id]);
    expect(forkPane?.members).toEqual([FORK_ID]);
    expect(panes.unassignedAgentIds(PROJECT_1, rosterIds)).toEqual([codex.id, antigravity.id]);
  });

  it("makes the branch visible when the user is in focus mode", async () => {
    // A fork-send has an immediate result to show. With another pane maximized,
    // placing the branch without revealing it would stream the reply into a
    // pane behind the focused one, with compose now addressed at an agent the
    // user cannot see — "I forked and my message vanished."
    const ws = await loadWorkspaceState();
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const parent = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [parent];
    const parentPane = panes.layoutFor(PROJECT_1, [parent.id]).panes[0];
    panes.maximizePane(PROJECT_1, [parent.id], parentPane!.id);

    const fork = forkRecord(PROJECT_1);
    invokeMock.mockImplementation(async (cmd) => (cmd === "fork_agent" ? fork : undefined));
    await ws.forkAgentIntoOwnPane(parent.id);

    const rosterIds = [parent.id, FORK_ID];
    const forkPane = panes.paneOfAgent(PROJECT_1, rosterIds, FORK_ID);
    const layout = panes.layoutFor(PROJECT_1, rosterIds);
    expect(forkPane).not.toBeNull();
    // Focus mode is preserved — the branch *becomes* the focused pane rather
    // than dropping the user out of focus.
    expect(layout.maximized).toBe(forkPane?.id);
    expect(layout.minimized).not.toContain(forkPane?.id);
  });

  it("does not roster or place anything when the fork is refused", async () => {
    const ws = await loadWorkspaceState();
    const parent = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [parent];
    invokeMock.mockImplementation(async (cmd) => {
      if (cmd === "fork_agent") throw new Error("alice is working");
      return undefined;
    });

    await expect(ws.forkAgentIntoOwnPane(parent.id)).rejects.toThrow("alice is working");

    expect(ws.agentsByProject[PROJECT_1]?.map((a) => a.id)).toEqual([parent.id]);
  });
});

describe("forked-agent inherited-history refresh", () => {
  /// Poll a few microtask/macrotask turns for `predicate`. This file has no
  /// testing-library helpers, and the refresh is fired from an event handler
  /// that awaits an IPC.
  async function until(predicate: () => boolean): Promise<void> {
    for (let i = 0; i < 50; i += 1) {
      if (predicate()) return;
      await tick();
      await new Promise((r) => setTimeout(r, 1));
    }
    expect(predicate()).toBe(true);
  }

  /// Give any fire-and-forget refresh enough turns to land. A bare `tick()`
  /// asserts before an awaited IPC could have completed, which makes a
  /// "did NOT re-read" assertion pass whether or not the guard exists.
  async function settle(): Promise<void> {
    for (let i = 0; i < 20; i += 1) {
      await tick();
      await new Promise((r) => setTimeout(r, 1));
    }
  }

  let turnSeq = 0;
  function fireTurnEnd(agentId: string, status: "completed" | "failed" | "cancelled"): void {
    turnSeq += 1;
    fireTo(`agent:${agentId}`, {
      type: "turn_end",
      turn_id: `turn-${agentId}-${status}-${turnSeq}`,
      outcome: status === "completed" ? { status: "completed" } : { status },
      ended_at: "2026-05-16T00:00:05Z",
    });
  }

  const FORK_ID = "00000000-0000-7000-8000-00000000000f";
  const PARENT_SESSION = "00000000-0000-7000-8000-0000000000aa";

  function forkAgent(): AgentRecord {
    return {
      ...agent(FORK_ID, PROJECT_1),
      name: "agent-a-fork",
      forked_from_session: PARENT_SESSION,
    };
  }

  /// A hydrated project holding `records`, with `load_project_conversation`
  /// call-counted so a test can see whether a refresh actually re-read.
  async function hydratedProjectWith(
    records: AgentRecord[],
  ): Promise<{ ws: Awaited<ReturnType<typeof loadWorkspaceState>>; loads: () => number }> {
    const ws = await loadWorkspaceState();
    let loads = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation") {
        loads += 1;
        return { items: [], agents: [] } satisfies ProjectConversation;
      }
      return undefined;
    });
    ws.agentsByProject[PROJECT_1] = records;
    await ws.hydrateProject(PROJECT_1);
    expect(ws.conversations[PROJECT_1]?.status).toBe("complete");
    return { ws, loads: () => loads };
  }

  it("re-reads once when a fork's first turn completes, and not again", async () => {
    // A fork's branch point only exists once its first turn has run, so the
    // inherited history has to be read back afterwards — exactly once.
    const state = await loadAgentState();
    const { ws, loads } = await hydratedProjectWith([agent(AGENT_1, PROJECT_1), forkAgent()]);
    await state.registerAgent(forkAgent());
    ws.installForkHistoryRefresh();
    const before = loads();

    fireTurnEnd(FORK_ID, "completed");
    await until(() => loads() === before + 1);

    // Second completed turn: history is already loaded, so no re-read.
    fireTurnEnd(FORK_ID, "completed");
    await settle();
    expect(loads()).toBe(before + 1);
  });

  it("re-arms after a cancelled or failed first turn", async () => {
    // Outcome is an approximation of "did this turn materialize the branch"
    // — a cancelled first turn can still leave a complete child file, so the
    // shot must not be spent on it.
    const state = await loadAgentState();
    const { ws, loads } = await hydratedProjectWith([agent(AGENT_1, PROJECT_1), forkAgent()]);
    await state.registerAgent(forkAgent());
    ws.installForkHistoryRefresh();
    const before = loads();

    fireTurnEnd(FORK_ID, "cancelled");
    fireTurnEnd(FORK_ID, "failed");
    await settle();
    expect(loads()).toBe(before);

    fireTurnEnd(FORK_ID, "completed");
    await until(() => loads() === before + 1);
  });

  it("marks a new fork as awaiting history and clears it only on a successful load", async () => {
    // The transcript's branch cue reads this. It has to arm at fork *creation*
    // — the branch does not exist until the send that follows — and survive the
    // whole first turn, since that is the interval it exists to explain.
    const state = await loadAgentState();
    const { ws } = await hydratedProjectWith([agent(AGENT_1, PROJECT_1)]);
    await state.registerAgent(agent(AGENT_1, PROJECT_1));
    ws.installForkHistoryRefresh();
    expect(ws.isAwaitingForkHistory(FORK_ID)).toBe(false);

    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "fork_agent") return forkAgent();
      if (cmd === "load_project_conversation")
        return { items: [], agents: [] } satisfies ProjectConversation;
      return undefined;
    });
    await ws.forkAgentIntoOwnPane(AGENT_1);
    expect(ws.isAwaitingForkHistory(FORK_ID)).toBe(true);

    // A failed first turn leaves the history still missing — the cue stays.
    fireTurnEnd(FORK_ID, "failed");
    await settle();
    expect(ws.isAwaitingForkHistory(FORK_ID)).toBe(true);

    fireTurnEnd(FORK_ID, "completed");
    await until(() => !ws.isAwaitingForkHistory(FORK_ID));
  });

  /// Open a project whose fingerprint probe reports `fp` for the fork agent.
  async function openProjectWithForkFingerprint(
    fp: { refresh_capable: boolean; fingerprint: unknown } | undefined,
  ): Promise<Awaited<ReturnType<typeof loadWorkspaceState>>> {
    const ws = await loadWorkspaceState();
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation")
        return { items: [], agents: [] } satisfies ProjectConversation;
      if (cmd === "project_session_fingerprints")
        return fp === undefined ? [] : [{ agent_id: FORK_ID, ...fp }];
      return undefined;
    });
    ws.agentsByProject[PROJECT_1] = [agent(AGENT_1, PROJECT_1), forkAgent()];
    await ws.hydrateProject(PROJECT_1);
    return ws;
  }

  it("re-arms the cue on open for a branch whose first turn never materialized", async () => {
    // Fork created, first turn failed, app restarted: no session file, so no
    // inherited history — the cue is as true as it was before the restart, and
    // the next successful turn is what resolves it.
    await loadAgentState();
    const ws = await openProjectWithForkFingerprint({
      refresh_capable: true,
      fingerprint: null,
    });

    expect(ws.isAwaitingForkHistory(FORK_ID)).toBe(true);
  });

  it("does not re-arm the cue for a materialized branch that parsed to no turns", async () => {
    // The trap in judging this by transcript contents: a real session can load
    // with zero agent turns (cancelled after the file was created, a parent
    // holding only a dangling prompt). Its history HAS arrived, and a cue keyed
    // on emptiness would sit there permanently.
    await loadAgentState();
    const ws = await openProjectWithForkFingerprint({
      refresh_capable: true,
      fingerprint: { source_path: "/s.jsonl", modified_at: "2026-05-16T00:00:00Z", byte_len: 10 },
    });

    expect(ws.isAwaitingForkHistory(FORK_ID)).toBe(false);
  });

  it("seeds nothing when the fingerprint probe says nothing", async () => {
    // Unknown must fall to no cue: a missing one is cosmetic, a false permanent
    // one is not.
    await loadAgentState();
    const ws = await openProjectWithForkFingerprint(undefined);

    expect(ws.isAwaitingForkHistory(FORK_ID)).toBe(false);
  });

  it("does not mark a fork carried over from a previous session as awaiting history", async () => {
    // A reopened fork already got its inherited history from the ordinary
    // project load. Arming on the record's `forked_from_session` alone — which
    // stays set forever as lineage — would brand every fork the project has
    // ever held with a cue that never clears.
    const state = await loadAgentState();
    const { ws } = await hydratedProjectWith([agent(AGENT_1, PROJECT_1), forkAgent()]);
    await state.registerAgent(forkAgent());
    ws.installForkHistoryRefresh();

    expect(ws.isAwaitingForkHistory(FORK_ID)).toBe(false);
  });

  it("retries when the refresh fails, instead of recording it as loaded", async () => {
    // `hydrateProject` preserves the previously loaded conversation when a
    // refresh throws — deliberately, so a transient hiccup never blanks a good
    // view. That makes `conversations[id].status` useless as a success signal:
    // it reads "complete" either way. Keying on it marked a fork loaded whose
    // history never arrived, and the one-shot then blocked every retry.
    const state = await loadAgentState();
    const { ws } = await hydratedProjectWith([agent(AGENT_1, PROJECT_1), forkAgent()]);
    await state.registerAgent(forkAgent());
    ws.installForkHistoryRefresh();

    let loads = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation") {
        loads += 1;
        if (loads === 1) throw new Error("read failed");
        return { items: [], agents: [] } satisfies ProjectConversation;
      }
      return undefined;
    });

    fireTurnEnd(FORK_ID, "completed");
    await until(() => loads === 1);
    await settle();
    expect(ws.conversations[PROJECT_1]?.status).toBe("complete");

    // The failure must not have consumed the one shot.
    fireTurnEnd(FORK_ID, "completed");
    await until(() => loads === 2);
  });

  it("treats a per-agent parse failure as a failed refresh", async () => {
    // The project load succeeded, but the one agent we were waiting on
    // contributed nothing — that is a fork with no inherited history, not a
    // completed refresh.
    const state = await loadAgentState();
    const { ws } = await hydratedProjectWith([agent(AGENT_1, PROJECT_1), forkAgent()]);
    await state.registerAgent(forkAgent());
    ws.installForkHistoryRefresh();

    let loads = 0;
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "load_project_conversation") {
        loads += 1;
        return {
          items: [],
          agents: [{ agent_id: FORK_ID, load_error: "corrupt sidecar" }],
        } as unknown as ProjectConversation;
      }
      return undefined;
    });

    fireTurnEnd(FORK_ID, "completed");
    await until(() => loads === 1);
    await settle();

    fireTurnEnd(FORK_ID, "completed");
    await until(() => loads === 2);
  });

  it("serializes two branches finishing together without dropping either load", async () => {
    // Both callers clear `hydrationStarted` synchronously before entering
    // `hydrateProject`, so neither sees the other's guard and both used to read
    // concurrently. Coalescing (losers return early) would be worse than the
    // duplicate work: this retries on the fork's OWN next completed turn, which
    // may never come, so a dropped load leaves that branch's history missing
    // behind a banner promising it. The loser must wait and then read.
    const state = await loadAgentState();
    const SECOND_FORK = "00000000-0000-7000-8000-0000000000f2";
    const secondFork: AgentRecord = { ...forkAgent(), id: SECOND_FORK, name: "agent-a-fork-2" };
    const { ws } = await hydratedProjectWith([agent(AGENT_1, PROJECT_1), forkAgent(), secondFork]);
    await state.registerAgent(forkAgent());
    await state.registerAgent(secondFork);
    ws.installForkHistoryRefresh();

    let concurrent = 0;
    let maxConcurrent = 0;
    const filters: (string[] | undefined)[] = [];
    invokeMock.mockImplementation(
      async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
        if (cmd === "load_project_conversation") {
          concurrent += 1;
          maxConcurrent = Math.max(maxConcurrent, concurrent);
          filters.push(args === undefined ? undefined : []);
          await new Promise((r) => setTimeout(r, 5));
          concurrent -= 1;
          return { items: [], agents: [] } satisfies ProjectConversation;
        }
        return undefined;
      },
    );

    fireTurnEnd(FORK_ID, "completed");
    fireTurnEnd(SECOND_FORK, "completed");
    await until(() => filters.length === 2);
    await settle();

    // Both branches read — neither was dropped — and never at the same time.
    expect(filters.length).toBe(2);
    expect(maxConcurrent).toBe(1);
  });

  it("keeps a fork visible when its event subscription fails, and never re-forks on retry", async () => {
    // The backend commits the fork before returning, so a subscription failure
    // afterwards is not a failed creation. Treating it as one hid a durable
    // agent: the user retried, got a second fork, and the first appeared out of
    // nowhere on the next restart.
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const parent = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [parent];
    await state.registerAgent(parent);

    let forkCalls = 0;
    listenMock.mockImplementation(async (channel: string) => {
      if (channel === `agent:${FORK_ID}`) throw new Error("channel unavailable");
      return vi.fn();
    });
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "fork_agent") {
        forkCalls += 1;
        return forkAgent();
      }
      return undefined;
    });

    const fork = await ws.forkAgentIntoOwnPane(AGENT_1);

    // Committed means visible: rostered, with the failure attached rather than
    // reported as "fork failed".
    expect(fork.id).toBe(FORK_ID);
    expect(ws.agentsByProject[PROJECT_1]?.map((a) => a.id)).toContain(FORK_ID);
    expect(state.runtimes[FORK_ID]?.listener_error).toContain("channel unavailable");

    // Retrying the subscription must not mint a second branch.
    listenMock.mockImplementation(async (name: string, cb) => {
      listeners.set(name, cb);
      return vi.fn();
    });
    await state.retryAgentSubscription(fork);
    expect(forkCalls).toBe(1);
    expect(state.runtimes[FORK_ID]?.listener_error).toBeUndefined();
  });

  it("opens a project when one agent's subscription fails, without taking the rest down", async () => {
    // `ensureProjectLoaded` awaits `Promise.all(agents.map(registerAgent))`, so
    // before the committed-vs-subscribed split a single flaky subscription
    // rejected the whole open and the project surfaced as an activation failure
    // — with every other agent's record having loaded fine. The all-or-nothing
    // semantics is exactly what the split neutralises, and nothing tested it.
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const healthy = agent(AGENT_1, PROJECT_1);
    const broken = { ...agent(AGENT_2, PROJECT_1), name: "agent-b" };
    listenMock.mockImplementation(async (name: string, cb) => {
      if (name === `agent:${AGENT_2}`) throw new Error("channel unavailable");
      listeners.set(name, cb);
      return vi.fn();
    });

    await state.registerAgent(healthy);
    await state.registerAgent(broken);
    ws.agentsByProject[PROJECT_1] = [healthy, broken];

    // Both rostered; only the broken one carries the failure.
    expect(ws.agentsByProject[PROJECT_1]?.map((a) => a.id)).toEqual([AGENT_1, AGENT_2]);
    expect(state.runtimes[AGENT_1]?.listener_error).toBeUndefined();
    expect(state.runtimes[AGENT_2]?.listener_error).toContain("channel unavailable");
  });

  it("never fires for a non-fork agent", async () => {
    const state = await loadAgentState();
    const { ws, loads } = await hydratedProjectWith([agent(AGENT_1, PROJECT_1)]);
    await state.registerAgent(agent(AGENT_1, PROJECT_1));
    ws.installForkHistoryRefresh();
    const before = loads();

    fireTurnEnd(AGENT_1, "completed");
    await settle();
    expect(loads()).toBe(before);
  });
});

describe("project idle predicate", () => {
  async function loadWorkflowState() {
    return await import("$lib/state/workflows.svelte");
  }

  function runningRun(over: Partial<import("$lib/types").WorkflowRunInfo> = {}) {
    return {
      run_id: "run-1",
      workflow: "review-and-recommend",
      step: 0,
      total: 3,
      status: "running" as const,
      reason: null,
      steps: [],
      ...over,
    };
  }

  it("treats a project with a running workflow and no live sends as busy", async () => {
    // The clause `liveProjectSends` alone gets wrong: a workflow between steps
    // dispatches nothing, so send liveness reads empty while work continues.
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    ws.agentsByProject[PROJECT_1] = [agent(AGENT_1, PROJECT_1)];
    wf.workflowRuns[PROJECT_1] = [runningRun()];

    expect(ws.liveProjectSends(PROJECT_1).size).toBe(0);
    expect(ws.projectIsIdle(PROJECT_1)).toBe(false);
  });

  it("treats a held failed workflow run as idle", async () => {
    // A failed run is retained until the user dismisses it, but nothing is
    // executing — the project is genuinely quiet.
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    ws.agentsByProject[PROJECT_1] = [agent(AGENT_1, PROJECT_1)];
    wf.workflowRuns[PROJECT_1] = [runningRun({ status: "failed", reason: "boom" })];

    expect(ws.projectIsIdle(PROJECT_1)).toBe(true);
  });

  it("treats a project with a live send as busy", async () => {
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a];
    await state.registerAgent(a);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", [], "send-1", "2026-05-16T00:00:00Z");

    expect(ws.projectIsIdle(PROJECT_1)).toBe(false);
  });

  it("marks a background project completed when its last live send drains", async () => {
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const p = project(PROJECT_1, "2026-05-16T00:00:00Z");
    ws.projects.list = [p];
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a];
    await state.registerAgent(a);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", [], "send-1", p.last_activity);
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();

    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBe(true);
  });

  it("marks a background project completed when its workflow terminalizes with no live sends", async () => {
    // The transition the old send-pair-only observer could not see at all.
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    ws.projects.list = [project(PROJECT_1, "2026-05-16T00:00:00Z")];
    ws.agentsByProject[PROJECT_1] = [agent(AGENT_1, PROJECT_1)];
    wf.workflowRuns[PROJECT_1] = [runningRun()];
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();

    wf.workflowRuns[PROJECT_1] = [];
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBe(true);
  });

  it("does not mark completed while a workflow still runs after the last send drains", async () => {
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    const p = project(PROJECT_1, "2026-05-16T00:00:00Z");
    ws.projects.list = [p];
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a];
    wf.workflowRuns[PROJECT_1] = [runningRun()];
    await state.registerAgent(a);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", [], "send-1", p.last_activity);
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();

    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBeUndefined();

    // …and the second half: once the workflow ends too, the mark appears. The
    // negative alone would also pass against a predicate that never fires.
    wf.workflowRuns[PROJECT_1] = [];
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBe(true);
  });

  it("does not mark completed while sends are live after the workflow terminalizes", async () => {
    const state = await loadAgentState();
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    const p = project(PROJECT_1, "2026-05-16T00:00:00Z");
    ws.projects.list = [p];
    const a = agent(AGENT_1, PROJECT_1);
    ws.agentsByProject[PROJECT_1] = [a];
    wf.workflowRuns[PROJECT_1] = [runningRun()];
    await state.registerAgent(a);
    state.dispatchUserTurn(AGENT_1, "user-1", "go", [], "send-1", p.last_activity);
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();

    wf.workflowRuns[PROJECT_1] = [];
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBeUndefined();

    // …and the second half: once the send drains too, the mark appears.
    const rt = state.runtimes[AGENT_1];
    if (rt === undefined) throw new Error("unreachable");
    state.runtimes[AGENT_1] = { ...rt, run_status: "idle", pending_sends: undefined };
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBe(true);
  });

  it("keeps last-activity stamping on send pairs, not on the idle transition", async () => {
    // The observer's two jobs must stay separate: `last_activity` tracks
    // individual send completions (and must fire while the project is still
    // busy), while background-completed tracks the whole project going quiet.
    // Collapsing them onto one loop breaks one direction or the other.
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    ws.projects.list = [project(PROJECT_1, "2026-05-16T00:00:00Z")];
    ws.agentsByProject[PROJECT_1] = [agent(AGENT_1, PROJECT_1)];
    wf.workflowRuns[PROJECT_1] = [runningRun()];
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();

    wf.workflowRuns[PROJECT_1] = [];
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBe(true);
    expect(ws.projectActivityOverrides[PROJECT_1]).toBeUndefined();
  });

  it("sees a project that has workflow runs but no roster entry", async () => {
    // The union in the activity snapshot exists for this candidate. It is not
    // reachable through today's load path (`ensureProjectLoaded` assigns the
    // roster before it subscribes to workflows), so this pins the union's intent
    // rather than a current occurrence: a plausible refactor — subscribing
    // earlier so no progress events are missed — makes it reachable, and without
    // the union the project would silently drop out of the observer.
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    ws.projects.list = [project(PROJECT_1, "2026-05-16T00:00:00Z")];
    wf.workflowRuns[PROJECT_1] = [runningRun()];

    expect(ws.agentsByProject[PROJECT_1]).toBeUndefined();
    expect(ws.projectIsIdle(PROJECT_1)).toBe(false);

    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();
    wf.workflowRuns[PROJECT_1] = [];
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBe(true);
  });

  it("removing a workflow-only-busy project clears the observer's non-idle memory", async () => {
    // Mirrors the send-busy removal test for the case the non-idle snapshot was
    // added for. Without the teardown filter, dropping the project from the
    // candidate set reads as a not-idle -> idle transition and re-marks it
    // completed after the teardown already cleared the flag — and because ids are
    // persisted on disk and survive removal, a re-add would show a green
    // "finished" checkmark on a project that never ran anything.
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    const p = project(PROJECT_1, "2026-05-16T00:00:00Z");
    ws.projects.list = [p];
    ws.agentsByProject[PROJECT_1] = [agent(AGENT_1, PROJECT_1)];
    wf.workflowRuns[PROJECT_1] = [runningRun()];
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

    await ws.removeDirectory(p.directory);
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBeUndefined();
    expect(ws.projects.list).toEqual([]);
  });

  it("never marks the project the user is looking at", async () => {
    const ws = await loadWorkspaceState();
    const wf = await loadWorkflowState();
    ws.projects.list = [project(PROJECT_1, "2026-05-16T00:00:00Z")];
    ws.agentsByProject[PROJECT_1] = [agent(AGENT_1, PROJECT_1)];
    ws.selection.activeProjectId = PROJECT_1;
    wf.workflowRuns[PROJECT_1] = [runningRun()];
    observerStops.push(ws.startProjectActivityObserver(() => "2026-05-25T12:00:00.000Z"));
    await tick();

    wf.workflowRuns[PROJECT_1] = [];
    await tick();

    expect(ws.backgroundCompletedProjectIds[PROJECT_1]).toBeUndefined();
  });
});
