import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/svelte";
import WorkflowSteps from "./WorkflowSteps.svelte";
import type { RecipientRef, WorkflowStepInfo, WorkflowInputValue } from "$lib/types";

function slot(input: string): RecipientRef {
  return { kind: "slot", input };
}
function lit(name: string): RecipientRef {
  return { kind: "literal", name };
}
function step(
  label: string,
  recipients: RecipientRef[] = [],
  feeds_from: RecipientRef[] = [],
): WorkflowStepInfo {
  return { label, recipients, feeds_from };
}

const STEPS: WorkflowStepInfo[] = [
  step("Send the review", [slot("reviewers")]),
  step("Wait for reviews", [slot("reviewers")]),
  step("Hand off", [slot("worker")], [slot("reviewers")]),
];

function recipientsText(i: number): string | null {
  return screen.queryByTestId(`workflow-step-recipients-${i}`)?.textContent?.trim() ?? null;
}

describe("WorkflowSteps — preview mode", () => {
  it("renders rows in order with labels", () => {
    render(WorkflowSteps, { steps: STEPS, mode: "preview", inputs: {} });
    const rows = screen.getAllByTestId(/^workflow-step-\d+$/);
    expect(rows).toHaveLength(3);
    expect(rows[0]).toHaveTextContent("Send the review");
    expect(rows[2]).toHaveTextContent("Hand off");
  });

  it("shows the slot name when unbound and the resolved name(s) once bound", async () => {
    const inputs: Record<string, WorkflowInputValue> = {};
    const { rerender } = render(WorkflowSteps, { steps: STEPS, mode: "preview", inputs });
    // Unbound → slot name.
    expect(recipientsText(0)).toContain("reviewers");

    // Bind the list slot → resolves to the agent names, in order.
    await rerender({ steps: STEPS, mode: "preview", inputs: { reviewers: ["alice", "bob"] } });
    expect(recipientsText(0)).toContain("alice, bob");
    expect(recipientsText(0)).not.toContain("reviewers");

    // Clear it → back to the slot name.
    await rerender({ steps: STEPS, mode: "preview", inputs: { reviewers: [] } });
    expect(recipientsText(0)).toContain("reviewers");
  });

  it("resolves a single agent slot to one name", async () => {
    const { rerender } = render(WorkflowSteps, { steps: STEPS, mode: "preview", inputs: {} });
    expect(recipientsText(2)).toContain("worker");
    await rerender({ steps: STEPS, mode: "preview", inputs: { worker: "carol" } });
    expect(recipientsText(2)).toContain("carol");
  });

  it("shows the feeds-from hint when present", () => {
    render(WorkflowSteps, { steps: STEPS, mode: "preview", inputs: { reviewers: ["alice"] } });
    expect(screen.getByTestId("workflow-step-feeds-2")).toHaveTextContent("feeds from alice");
    expect(screen.queryByTestId("workflow-step-feeds-0")).toBeNull();
  });

  it("renders a literal recipient verbatim", () => {
    render(WorkflowSteps, { steps: [step("Ping", [lit("ops")])], mode: "preview", inputs: {} });
    expect(recipientsText(0)).toContain("ops");
  });
});

describe("WorkflowSteps — live mode", () => {
  function stateOf(i: number): string | null {
    return screen.getByTestId(`workflow-step-${i}`).getAttribute("data-step-state");
  }

  it("derives done / active / pending from the current index", () => {
    render(WorkflowSteps, { steps: STEPS, mode: "live", current: 1, status: "running" });
    expect(stateOf(0)).toBe("done");
    expect(stateOf(1)).toBe("active");
    expect(stateOf(2)).toBe("pending");
    // The active row carries the spinner.
    expect(screen.getByTestId("workflow-step-1").querySelector(".animate-spin")).not.toBeNull();
  });

  it("marks the failing step failed and shows the reason", () => {
    render(WorkflowSteps, {
      steps: STEPS,
      mode: "live",
      current: 1,
      status: "failed",
      reason: "agent reviewer-1 is busy",
    });
    expect(stateOf(0)).toBe("done");
    expect(stateOf(1)).toBe("failed");
    expect(stateOf(2)).toBe("pending");
    expect(screen.getByTestId("workflow-step-reason-1")).toHaveTextContent(
      "agent reviewer-1 is busy",
    );
  });

  it("treats an interrupted run's current step as failed", () => {
    render(WorkflowSteps, { steps: STEPS, mode: "live", current: 2, status: "interrupted" });
    expect(stateOf(0)).toBe("done");
    expect(stateOf(1)).toBe("done");
    expect(stateOf(2)).toBe("failed");
  });
});
