import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkflowProgressPayload, WorkflowRunInfo } from "$lib/types";

// Control the IPC + capture the channel callback so we can drive progress events.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
const channels = new Map<string, (e: { payload: WorkflowProgressPayload }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: WorkflowProgressPayload }) => void) => {
    channels.set(name, cb);
    return () => channels.delete(name);
  }),
}));

const { workflowRuns, subscribeProjectWorkflows, unsubscribeProjectWorkflows, _testing } =
  await import("./workflows.svelte");
const tracker = await import("./sendCompletion");

function emit(projectId: string, payload: WorkflowProgressPayload): void {
  const cb = channels.get(`workflow:${projectId}`);
  if (cb === undefined) throw new Error(`no workflow listener for ${projectId}`);
  cb({ payload });
}

function running(over: Partial<WorkflowProgressPayload> = {}): WorkflowProgressPayload {
  return {
    run_id: "r1",
    workflow: "review-and-recommend",
    step: 0,
    total: 3,
    status: "running",
    reason: null,
    ...over,
  };
}

afterEach(() => {
  _testing.reset();
  tracker._testing.reset();
  channels.clear();
  invokeMock.mockReset();
});

describe("workflow terminal → completion notification", () => {
  it("hands a terminal run to the completion tracker", async () => {
    // The wiring itself, which nothing else covers: the tracker's own tests call
    // `recordWorkflowTerminal` directly, so deleting this call would leave every
    // test green while workflows silently stopped being reported.
    invokeMock.mockResolvedValueOnce([]); // list_workflow_runs (seed)
    await subscribeProjectWorkflows("p1");
    invokeMock.mockClear();

    emit("p1", running({ status: "complete" }));

    const notified = invokeMock.mock.calls.filter(([command]) => command === "notify");
    expect(notified).toHaveLength(1);
    expect(notified[0]?.[1]).toMatchObject({
      title: "Workflow finished",
      body: expect.stringContaining("review-and-recommend"),
    });
  });

  it("stays silent for a cancelled run", async () => {
    invokeMock.mockResolvedValueOnce([]);
    await subscribeProjectWorkflows("p1");
    invokeMock.mockClear();

    emit("p1", running({ status: "cancelled" }));

    expect(invokeMock.mock.calls.some(([command]) => command === "notify")).toBe(false);
  });
});

describe("workflows store", () => {
  it("seeds runs on subscribe and advances a run on step events (not active-gated)", async () => {
    // Seed: one already-running run.
    const seeded: WorkflowRunInfo[] = [
      {
        run_id: "r1",
        workflow: "review-and-recommend",
        step: 0,
        total: 3,
        status: "running",
        reason: null,
        steps: [],
      },
    ];
    invokeMock.mockResolvedValueOnce(seeded); // list_workflow_runs
    await subscribeProjectWorkflows("p1");
    expect(workflowRuns["p1"]?.[0]?.step).toBe(0);

    // A step-transition event updates the run for a (possibly non-active) project.
    emit("p1", running({ step: 2 }));
    expect(workflowRuns["p1"]?.find((r) => r.run_id === "r1")?.step).toBe(2);
  });

  it("preserves a run's steps when a lean progress event updates it in place", async () => {
    const seeded: WorkflowRunInfo[] = [
      {
        run_id: "r1",
        workflow: "review-and-recommend",
        step: 0,
        total: 3,
        status: "running",
        reason: null,
        steps: [
          {
            kind: "send",
            label: "Send the review",
            description: null,
            prompt: { kind: "named", id: "builtin:code-review" },
            recipients: [],
            feeds_from: [],
          },
        ],
      },
    ];
    invokeMock.mockResolvedValueOnce(seeded); // list_workflow_runs
    await subscribeProjectWorkflows("p1");

    // The progress payload carries no steps; the update must keep the seeded ones.
    emit("p1", running({ step: 2 }));
    const row = workflowRuns["p1"]?.find((r) => r.run_id === "r1");
    expect(row?.step).toBe(2);
    expect(row?.steps).toHaveLength(1);
    expect(row?.steps[0]?.label).toBe("Send the review");
  });

  it("refreshes to fetch steps when a run is first seen via an event", async () => {
    invokeMock.mockResolvedValueOnce([]); // initial seed: empty
    await subscribeProjectWorkflows("p1");

    // The authoritative snapshot the triggered refresh will return.
    invokeMock.mockResolvedValueOnce([
      {
        run_id: "r2",
        workflow: "w",
        step: 0,
        total: 2,
        status: "running",
        reason: null,
        steps: [
          {
            kind: "send",
            label: "Go",
            description: null,
            prompt: null,
            recipients: [],
            feeds_from: [],
          },
        ],
      },
    ]); // list_workflow_runs (refresh)

    // An event for an unknown run appends a step-less row immediately...
    emit("p1", running({ run_id: "r2", total: 2 }));
    expect(workflowRuns["p1"]?.find((r) => r.run_id === "r2")?.steps).toHaveLength(0);

    // ...and the triggered refresh fills the steps.
    await vi.waitFor(() =>
      expect(workflowRuns["p1"]?.find((r) => r.run_id === "r2")?.steps).toHaveLength(1),
    );
  });

  it("drives terminals from the payload without a re-query", async () => {
    invokeMock.mockResolvedValueOnce([
      {
        run_id: "r1",
        workflow: "w",
        step: 0,
        total: 3,
        status: "running",
        reason: null,
        steps: [],
      },
    ]); // seed
    await subscribeProjectWorkflows("p1");
    const seedQueries = () =>
      invokeMock.mock.calls.filter(([c]) => c === "list_workflow_runs").length;
    const before = seedQueries();

    // A `failed` terminal builds the retained failed row from the payload — no re-query.
    emit("p1", running({ status: "failed", step: 1, reason: "boom" }));
    const r1 = workflowRuns["p1"]?.find((r) => r.run_id === "r1");
    expect(r1?.status).toBe("failed");
    expect(r1?.step).toBe(1);
    expect(r1?.reason).toBe("boom");
    expect(seedQueries()).toBe(before); // no extra IPC

    // A `complete` terminal drops the run outright.
    emit("p1", running({ status: "complete" }));
    expect(workflowRuns["p1"]?.some((r) => r.run_id === "r1")).toBe(false);
    expect(seedQueries()).toBe(before);
  });

  it("drops a project's runs and listener on unsubscribe", async () => {
    invokeMock.mockResolvedValueOnce([running()]);
    await subscribeProjectWorkflows("p1");
    expect(workflowRuns["p1"]).toBeDefined();

    unsubscribeProjectWorkflows(["p1"]);
    expect(workflowRuns["p1"]).toBeUndefined();
    expect(channels.has("workflow:p1")).toBe(false);
  });

  it("discards a stale seed when the project is unsubscribed mid-subscribe", async () => {
    // Hang the seed query so we can tear down while it's in flight.
    let resolveSeed!: (v: WorkflowRunInfo[]) => void;
    invokeMock.mockReturnValueOnce(
      new Promise<WorkflowRunInfo[]>((resolve) => {
        resolveSeed = resolve;
      }),
    );
    const sub = subscribeProjectWorkflows("p1");
    // Wait until the listener is installed (listen resolved) but the seed hangs.
    await vi.waitFor(() => expect(channels.has("workflow:p1")).toBe(true));

    unsubscribeProjectWorkflows(["p1"]);
    resolveSeed([
      {
        run_id: "r1",
        workflow: "w",
        step: 0,
        total: 3,
        status: "running",
        reason: null,
        steps: [],
      },
    ]);
    await sub;

    // The generation guard discards the stale seed and the listener is gone.
    expect(workflowRuns["p1"]).toBeUndefined();
    expect(channels.has("workflow:p1")).toBe(false);
  });
});

describe("a run list fetched around a run's end", () => {
  function runInfo(over: Partial<WorkflowRunInfo> = {}): WorkflowRunInfo {
    return {
      run_id: "r1",
      workflow: "review-and-recommend",
      step: 0,
      total: 3,
      status: "running",
      reason: null,
      steps: [
        {
          kind: "send",
          label: "draft",
          description: null,
          prompt: null,
          recipients: [],
          feeds_from: [],
        },
      ],
      ...over,
    };
  }

  /// `list_workflow_runs` answers from a queue the test releases one by one.
  function heldLists(): { release: (runs: WorkflowRunInfo[]) => void; held: () => number } {
    const pending: ((runs: WorkflowRunInfo[]) => void)[] = [];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "list_workflow_runs") {
        return await new Promise<WorkflowRunInfo[]>((resolve) => pending.push(resolve));
      }
      return undefined;
    });
    return {
      release: (runs) => {
        const next = pending.shift();
        if (next === undefined) throw new Error("no list fetch in flight");
        next(runs);
      },
      held: () => pending.length,
    };
  }

  async function settle(): Promise<void> {
    for (let i = 0; i < 10; i += 1) await new Promise((r) => setTimeout(r, 0));
  }

  it("a run that completes while its first fetch is in flight stays gone", async () => {
    const lists = heldLists();
    const subscribed = subscribeProjectWorkflows("p1");
    await settle();
    lists.release([]);
    await subscribed;

    emit("p1", running());
    await settle();
    expect(lists.held()).toBe(1);
    emit("p1", running({ status: "complete", step: 3 }));
    lists.release([runInfo()]);
    await settle();

    expect(workflowRuns["p1"]).toEqual([]);
  });

  it("a failed run keeps its failure over a fetch that still says running", async () => {
    const lists = heldLists();
    const subscribed = subscribeProjectWorkflows("p1");
    await settle();
    lists.release([]);
    await subscribed;

    emit("p1", running());
    await settle();
    emit("p1", running({ status: "failed", step: 1, reason: "agent busy" }));
    lists.release([runInfo()]);
    await settle();

    expect(workflowRuns["p1"]).toEqual([
      expect.objectContaining({ run_id: "r1", status: "failed", reason: "agent busy" }),
    ]);
    expect(workflowRuns["p1"]?.[0]?.steps).toHaveLength(1);
  });

  it("a fetch that starts after a run completed cannot bring it back", async () => {
    // The backend reports a run as running for a moment after announcing its
    // end, so a fetch in that gap answers "running" with no event left to
    // replay over it.
    const lists = heldLists();
    const subscribed = subscribeProjectWorkflows("p1");
    await settle();
    lists.release([]);
    await subscribed;
    emit("p1", running());
    await settle();
    lists.release([runInfo()]);
    await settle();
    emit("p1", running({ status: "complete", step: 3 }));

    const { refreshRuns } = await import("./workflows.svelte");
    const refreshed = refreshRuns("p1");
    await settle();
    lists.release([runInfo()]);
    await refreshed;

    expect(workflowRuns["p1"]).toEqual([]);
  });

  it("progress during the first fetch is kept over its answer", async () => {
    const lists = heldLists();
    const subscribed = subscribeProjectWorkflows("p1");
    await settle();
    emit("p1", running({ step: 2 }));
    lists.release([]);
    await settle();

    expect(workflowRuns["p1"]).toEqual([expect.objectContaining({ run_id: "r1", step: 2 })]);
    // The event asked for one more fetch, for the run's step names.
    lists.release([runInfo({ step: 2 })]);
    await subscribed;
    expect(workflowRuns["p1"]?.[0]?.steps).toHaveLength(1);
  });
});

describe("a run-list fetch around the project's first subscription", () => {
  async function settle(): Promise<void> {
    for (let i = 0; i < 10; i += 1) await new Promise((r) => setTimeout(r, 0));
  }

  function heldLists(): { release: (runs: WorkflowRunInfo[]) => void; held: () => number } {
    const pending: ((runs: WorkflowRunInfo[]) => void)[] = [];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "list_workflow_runs") {
        return await new Promise<WorkflowRunInfo[]>((resolve) => pending.push(resolve));
      }
      return undefined;
    });
    return {
      release: (runs) => {
        const next = pending.shift();
        if (next === undefined) throw new Error("no list fetch in flight");
        next(runs);
      },
      held: () => pending.length,
    };
  }

  const failedRun: WorkflowRunInfo = {
    run_id: "r-old",
    workflow: "review-and-recommend",
    step: 1,
    total: 3,
    status: "failed",
    reason: "agent busy",
    steps: [],
  };

  it("a fetch started before the first subscription still delivers the list", async () => {
    const { refreshRuns } = await import("./workflows.svelte");
    const lists = heldLists();
    const early = refreshRuns("p1");
    await settle();
    let subscribedDone = false;
    const subscribed = subscribeProjectWorkflows("p1").then(() => {
      subscribedDone = true;
    });
    await settle();

    // The early read may predate the listener, so it is read again.
    lists.release([]);
    await settle();
    expect(lists.held()).toBe(1);
    expect(subscribedDone).toBe(false);
    lists.release([failedRun]);
    await Promise.all([early, subscribed]);

    expect(workflowRuns["p1"]).toEqual([failedRun]);
  });

  it("a fetch whose project is torn down meanwhile commits nothing", async () => {
    const { refreshRuns } = await import("./workflows.svelte");
    const lists = heldLists();
    const subscribed = subscribeProjectWorkflows("p1");
    await settle();
    lists.release([]);
    await subscribed;

    const refreshed = refreshRuns("p1");
    await settle();
    unsubscribeProjectWorkflows(["p1"]);
    lists.release([failedRun]);
    await refreshed;

    expect(workflowRuns["p1"]).toBeUndefined();
  });
});
