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

function emit(projectId: string, payload: WorkflowProgressPayload): void {
  const cb = channels.get(`workflow:${projectId}`);
  if (cb === undefined) throw new Error(`no workflow listener for ${projectId}`);
  cb({ payload });
}

function running(over: Partial<WorkflowProgressPayload> = {}): WorkflowProgressPayload {
  return {
    run_id: "r1",
    workflow: "review-and-aggregate",
    step: 0,
    total: 3,
    status: "running",
    reason: null,
    ...over,
  };
}

afterEach(() => {
  _testing.reset();
  channels.clear();
  invokeMock.mockReset();
});

describe("workflows store", () => {
  it("seeds runs on subscribe and advances a run on step events (not active-gated)", async () => {
    // Seed: one already-running run.
    const seeded: WorkflowRunInfo[] = [
      {
        run_id: "r1",
        workflow: "review-and-aggregate",
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
        workflow: "review-and-aggregate",
        step: 0,
        total: 3,
        status: "running",
        reason: null,
        steps: [{ label: "Send the review", recipients: [], feeds_from: [] }],
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
        steps: [{ label: "Go", recipients: [], feeds_from: [] }],
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
