import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/svelte";
import WorkflowSteps from "./WorkflowSteps.svelte";
import type { RecipientRef, StepPrompt, WorkflowStepInfo, WorkflowInputValue } from "$lib/types";

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
  return { kind: "send", label, description: null, prompt: null, recipients, feeds_from };
}
function kstep(
  kind: WorkflowStepInfo["kind"],
  label: string,
  recipients: RecipientRef[] = [],
  feeds_from: RecipientRef[] = [],
  description?: string,
  prompt: StepPrompt | null = null,
): WorkflowStepInfo {
  return { kind, label, description: description ?? null, prompt, recipients, feeds_from };
}
function rowCount(): number {
  return screen.getAllByTestId(/^workflow-step-\d+$/).length;
}
function stateAt(i: number): string | null {
  return screen.getByTestId(`workflow-step-${i}`).getAttribute("data-step-state");
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

    // Bind the list slot → resolves to the agent names, in order (one chip each).
    await rerender({ steps: STEPS, mode: "preview", inputs: { reviewers: ["alice", "bob"] } });
    expect(recipientsText(0)).toContain("alice");
    expect(recipientsText(0)).toContain("bob");
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

describe("WorkflowSteps — collapse (send + its wait into one deliverable node)", () => {
  it("folds a send and its matching wait into a single node, keeping the send's label", () => {
    const steps = [
      kstep("send", "Code review", [slot("reviewers")]),
      kstep("wait", "Reviews received", [slot("reviewers")]),
      kstep("send", "Recommendations", [slot("worker")]),
    ];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(rowCount()).toBe(2);
    const rows = screen.getAllByTestId(/^workflow-step-\d+$/);
    expect(rows[0]).toHaveTextContent("Code review");
    // The wait label is absorbed, not shown.
    expect(rows[0]).not.toHaveTextContent("Reviews received");
    expect(rows[1]).toHaveTextContent("Recommendations");
  });

  it("shows recipients as chips (produced-by), not an arrow", () => {
    const steps = [kstep("send", "Code review", [lit("alice"), lit("bob")])];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    // One chip per agent, in order.
    expect(screen.getByTestId("workflow-step-agent-0-0")).toHaveTextContent("alice");
    expect(screen.getByTestId("workflow-step-agent-0-1")).toHaveTextContent("bob");
    expect(recipientsText(0) ?? "").not.toContain("→");
  });

  it("keeps a collapsed node active across both dispatch and the absorbed wait", () => {
    const steps = [
      kstep("send", "Code review", [lit("a")]),
      kstep("wait", "x", [lit("a")]), // absorbed into node [0,1]
      kstep("send", "Recs", [lit("b")]),
      kstep("wait", "y", [lit("b")]), // absorbed into node [2,3]
    ];
    // current sits on the wait (physical step 1) — the node must still read active,
    // not done: its work isn't finished until the wait clears.
    const { rerender } = render(WorkflowSteps, {
      steps,
      mode: "live",
      current: 1,
      status: "running",
    });
    expect(rowCount()).toBe(2);
    expect(stateAt(0)).toBe("active");
    expect(stateAt(1)).toBe("pending");
    // Advance past the first wait → first node done, second active.
    rerender({ steps, mode: "live", current: 2, status: "running" });
    expect(stateAt(0)).toBe("done");
    expect(stateAt(1)).toBe("active");
  });

  it("renders the diamond as two concurrent nodes, both active at once", () => {
    // send sec @0, send code @1, wait sec @2, wait code @3 → nodes [0,2] and [1,3].
    const steps = [
      kstep("send", "Security review", [lit("sec")]),
      kstep("send", "Code review", [lit("code")]),
      kstep("wait", "x", [lit("sec")]),
      kstep("wait", "y", [lit("code")]),
    ];
    render(WorkflowSteps, { steps, mode: "live", current: 2, status: "running" });
    expect(rowCount()).toBe(2);
    // current=2 falls inside both ranges → two spinners.
    expect(stateAt(0)).toBe("active");
    expect(stateAt(1)).toBe("active");
    expect(screen.getByTestId("workflow-step-0").querySelector(".animate-spin")).not.toBeNull();
    expect(screen.getByTestId("workflow-step-1").querySelector(".animate-spin")).not.toBeNull();
  });

  it("collapses a wait_for_all over multiple single sends via set-cover", () => {
    // Two heterogeneous single sends barriered by one wait_for_all [sec, code].
    const steps = [
      kstep("send", "Security review", [lit("sec")]),
      kstep("send", "Code review", [lit("code")]),
      kstep("wait", "both", [lit("sec"), lit("code")]),
    ];
    render(WorkflowSteps, { steps, mode: "live", current: 2, status: "running" });
    expect(rowCount()).toBe(2); // both sends closed by the union wait
    expect(stateAt(0)).toBe("active");
    expect(stateAt(1)).toBe("active");
  });

  it("leaves an unmatched wait as its own honest row (no silent mis-collapse)", () => {
    // List send [a,b] barriered by split single waits — recipient grouping differs
    // across the send/wait boundary, so nothing matches; render raw rows, never a
    // send mislabeled by absorbing a non-matching wait.
    const steps = [
      kstep("send", "Fan out", [lit("a"), lit("b")]),
      kstep("wait", "Wait a", [lit("a")]),
      kstep("wait", "Wait b", [lit("b")]),
    ];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(rowCount()).toBe(3);
    const rows = screen.getAllByTestId(/^workflow-step-\d+$/);
    expect(rows[1]).toHaveTextContent("Wait a");
    expect(rows[2]).toHaveTextContent("Wait b");
  });

  it("renders a pause as its own row", () => {
    const steps = [
      kstep("send", "Code review", [lit("a")]),
      kstep("wait", "x", [lit("a")]),
      kstep("pause", "Your input", [lit("b")]),
    ];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(rowCount()).toBe(2);
    const rows = screen.getAllByTestId(/^workflow-step-\d+$/);
    expect(rows[1]).toHaveTextContent("Your input");
  });

  it("shows a named prompt as a chip with its full provider:name id", () => {
    const steps = [
      kstep("send", "Code review", [lit("a")], [], undefined, {
        kind: "named",
        id: "builtin:code-review",
      }),
    ];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    // The full id (provider prefix included) is on the chip itself.
    expect(screen.getByTestId("workflow-step-prompt-0")).toHaveTextContent("builtin:code-review");
  });

  it("shows an inline-text send as an 'inline prompt' chip", () => {
    const steps = [kstep("send", "Hand off", [lit("a")], [], undefined, { kind: "inline" })];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(screen.getByTestId("workflow-step-prompt-0")).toHaveTextContent("inline prompt");
  });

  it("omits the prompt chip when a step runs no prompt", () => {
    const steps = [kstep("wait", "Reviews received", [lit("a")])];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(screen.queryByTestId("workflow-step-prompt-0")).toBeNull();
  });

  it("a collapsed node shows the send's prompt (the wait carries none)", () => {
    const steps = [
      kstep("send", "Code review", [lit("a")], [], undefined, {
        kind: "named",
        id: "builtin:code-review",
      }),
      kstep("wait", "x", [lit("a")]),
    ];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(rowCount()).toBe(1);
    expect(screen.getByTestId("workflow-step-prompt-0")).toHaveTextContent("builtin:code-review");
  });

  // Diamond: nodes A [0,2] and B [1,3] have *overlapping* ranges. On failure the
  // run's `current` is the one failed step; only the node that owns that step
  // (its send or its wait) must turn red — a range-based check would falsely fail
  // both branches.
  const DIAMOND = [
    kstep("send", "Security review", [lit("sec")]), // node A start (0)
    kstep("send", "Code review", [lit("code")]), //    node B start (1)
    kstep("wait", "x", [lit("sec")]), //               node A end   (2)
    kstep("wait", "y", [lit("code")]), //              node B end   (3)
  ];

  it("on a branch's send failure, only that branch's node is failed", () => {
    // Code's send (step 1) fails. Step 1 is inside A's range [0,2] but A doesn't
    // own it; it's B's start.
    render(WorkflowSteps, { steps: DIAMOND, mode: "live", current: 1, status: "failed" });
    expect(stateAt(0)).toBe("pending"); // Security — cut short, not failed
    expect(stateAt(1)).toBe("failed"); // Code — the one that actually failed
    expect(screen.queryByTestId("workflow-step-reason-0")).toBeNull();
  });

  it("on a branch's wait failure, only that branch's node is failed", () => {
    // Security's wait (step 2) fails. Step 2 is inside B's range [1,3] but B
    // doesn't own it; it's A's end.
    render(WorkflowSteps, {
      steps: DIAMOND,
      mode: "live",
      current: 2,
      status: "failed",
      reason: "agent sec is busy",
    });
    expect(stateAt(0)).toBe("failed"); // Security — the one that actually failed
    expect(stateAt(1)).toBe("pending"); // Code — cut short, not failed
    // The reason renders only on the failed (owning) node.
    expect(screen.getByTestId("workflow-step-reason-0")).toHaveTextContent("agent sec is busy");
    expect(screen.queryByTestId("workflow-step-reason-1")).toBeNull();
  });

  it("renders a step's description as a sub-line", () => {
    const steps = [kstep("send", "Code review", [lit("a")], [], "Each reviewer reviews the diff.")];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(screen.getByTestId("workflow-step-description-0")).toHaveTextContent(
      "Each reviewer reviews the diff.",
    );
  });

  it("a collapsed node shows the send's description (the wait's is absorbed)", () => {
    const steps = [
      kstep("send", "Code review", [lit("a")], [], "Reviewers review the diff."),
      kstep("wait", "x", [lit("a")]),
    ];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(rowCount()).toBe(1);
    expect(screen.getByTestId("workflow-step-description-0")).toHaveTextContent(
      "Reviewers review the diff.",
    );
  });

  it("omits the description sub-line when a step has none", () => {
    const steps = [kstep("send", "Code review", [lit("a")])];
    render(WorkflowSteps, { steps, mode: "preview", inputs: {} });
    expect(screen.queryByTestId("workflow-step-description-0")).toBeNull();
  });

  it("a shared wait_for_all failing marks every branch it barriered (set-cover)", () => {
    // send sec @0, send code @1, wait_for_all [sec, code] @2 → nodes [0,2] and
    // [1,2]; both own endStep 2, so a barrier failure fails both branches.
    const steps = [
      kstep("send", "Security review", [lit("sec")]),
      kstep("send", "Code review", [lit("code")]),
      kstep("wait", "both", [lit("sec"), lit("code")]),
    ];
    render(WorkflowSteps, { steps, mode: "live", current: 2, status: "failed" });
    expect(stateAt(0)).toBe("failed");
    expect(stateAt(1)).toBe("failed");
  });
});
