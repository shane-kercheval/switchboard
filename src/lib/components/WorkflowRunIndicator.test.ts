import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import type { WorkflowRunInfo } from "$lib/types";

// Drive the indicator off a mocked store so we control the run list + actions.
const cancelRun = vi.fn();
const abandonRun = vi.fn();
let runs: { projectId: string; run: WorkflowRunInfo }[] = [];
vi.mock("$lib/state/workflows.svelte", () => ({
  allRuns: () => runs,
  cancelRun: (id: string) => cancelRun(id),
  abandonRun: (pid: string, id: string) => abandonRun(pid, id),
}));
// Project names for the per-row label.
vi.mock("$lib/state/workspace.svelte", () => ({
  projects: { list: [{ id: "p1", name: "alpha" }] },
}));

const { default: WorkflowRunIndicator } = await import("./WorkflowRunIndicator.svelte");

function run(over: Partial<WorkflowRunInfo> = {}): WorkflowRunInfo {
  return {
    run_id: "r1",
    workflow: "review-and-aggregate",
    step: 1,
    total: 3,
    status: "running",
    reason: null,
    ...over,
  };
}

afterEach(() => {
  runs = [];
  cancelRun.mockReset();
  abandonRun.mockReset();
});

describe("WorkflowRunIndicator", () => {
  it("is hidden when there are no runs", () => {
    runs = [];
    render(WorkflowRunIndicator);
    expect(screen.queryByTestId("workflow-run-indicator")).toBeNull();
  });

  it("shows the count, the per-run project label, and Cancel for a running run", async () => {
    cancelRun.mockResolvedValue(undefined);
    runs = [{ projectId: "p1", run: run() }];
    render(WorkflowRunIndicator);
    expect(screen.getByTestId("workflow-run-indicator-count")).toHaveTextContent("1 workflow");

    await fireEvent.click(screen.getByTestId("workflow-run-indicator-toggle"));
    expect(screen.getByTestId("workflow-run-r1")).toHaveTextContent("step 2/3");
    // The row names its project (app-global list disambiguation).
    expect(screen.getByTestId("workflow-run-project-r1")).toHaveTextContent("alpha");
    await fireEvent.click(screen.getByTestId("workflow-run-cancel-r1"));
    expect(cancelRun).toHaveBeenCalledWith("r1");
  });

  it("offers Abandon for a failed/interrupted run", async () => {
    abandonRun.mockResolvedValue(undefined);
    runs = [{ projectId: "p1", run: run({ run_id: "r2", status: "interrupted", step: 0 }) }];
    render(WorkflowRunIndicator);
    await fireEvent.click(screen.getByTestId("workflow-run-indicator-toggle"));
    await fireEvent.click(screen.getByTestId("workflow-run-abandon-r2"));
    expect(abandonRun).toHaveBeenCalledWith("p1", "r2");
  });

  it("surfaces an inline error when an abandon fails (not a silent dead button)", async () => {
    abandonRun.mockRejectedValue(new Error("run still active"));
    runs = [{ projectId: "p1", run: run({ run_id: "r3", status: "failed", step: 0 }) }];
    render(WorkflowRunIndicator);
    await fireEvent.click(screen.getByTestId("workflow-run-indicator-toggle"));
    await fireEvent.click(screen.getByTestId("workflow-run-abandon-r3"));
    await screen.findByTestId("workflow-run-action-error");
    expect(screen.getByTestId("workflow-run-action-error")).toHaveTextContent("run still active");
  });
});
