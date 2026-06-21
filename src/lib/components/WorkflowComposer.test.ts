import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import WorkflowComposer from "./WorkflowComposer.svelte";
import type { AgentRecord, Prompt, WorkflowInputInfo, WorkflowListing } from "$lib/types";
import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";

const AGENTS: AgentRecord[] = [
  { id: "a1", project_id: "p", name: "primary", harness: "claude_code" },
  { id: "a2", project_id: "p", name: "reviewer-1", harness: "claude_code" },
] as AgentRecord[];

// Two non-empty panes — the threshold at which pane chips become a meaningful
// shortcut (a single pane == "every agent", which the agent chips already cover).
const PANES: TranscriptPane[] = [
  { id: "pane-1", name: "Left", members: ["a1"], hidden: [] },
  { id: "pane-2", name: "Right", members: ["a2"], hidden: [] },
] as TranscriptPane[];

const PROMPTS: Prompt[] = [
  {
    provider: "builtin",
    name: "code-review",
    title: null,
    description: "Review the diff",
    arguments: [],
    tags: [],
  },
];

function input(over: Partial<WorkflowInputInfo>): WorkflowInputInfo {
  return { name: "x", ty: "text", optional: false, description: null, ...over };
}

function listing(
  inputs: WorkflowInputInfo[],
  over: Partial<WorkflowListing> = {},
): WorkflowListing {
  return {
    name: "review-and-aggregate",
    is_builtin: true,
    description: "d",
    inputs,
    invocable: true,
    parse_error: null,
    recommended_prompts: {},
    ...over,
  };
}

function setup(
  workflow: WorkflowListing,
  inputs: Record<string, string | string[]> = {},
  panes: TranscriptPane[] = [],
) {
  const onremove = vi.fn();
  render(WorkflowComposer, {
    props: { workflow, agents: AGENTS, prompts: PROMPTS, panes, inputs, onremove },
  });
  return { onremove };
}

describe("WorkflowComposer", () => {
  it("renders the correct control per input type", () => {
    setup(
      listing([
        input({ name: "primary_agent", ty: "agent" }),
        input({ name: "reviewer_agents", ty: "agent_list" }),
        input({ name: "review_prompt", ty: "prompt_id" }),
        input({ name: "user_context", ty: "text", optional: true }),
      ]),
      { primary_agent: "", reviewer_agents: [], review_prompt: "", user_context: "" },
    );
    // agent + agent_list render per-agent chips.
    expect(screen.getByTestId("workflow-agent-primary_agent-primary")).toBeInTheDocument();
    expect(screen.getByTestId("workflow-agent-reviewer_agents-reviewer-1")).toBeInTheDocument();
    // prompt_id renders a picker button; text renders a textarea.
    expect(screen.getByTestId("workflow-prompt-review_prompt")).toBeInTheDocument();
    expect(screen.getByTestId("workflow-text-user_context")).toBeInTheDocument();
  });

  it("selecting an agent chip binds the input value", async () => {
    const inputs: Record<string, string | string[]> = { primary_agent: "" };
    setup(listing([input({ name: "primary_agent", ty: "agent" })]), inputs);
    await fireEvent.click(screen.getByTestId("workflow-agent-primary_agent-primary"));
    expect(inputs.primary_agent).toBe("primary");
  });

  it("toggles a whole pane's members into an [agent] input", async () => {
    const inputs: Record<string, string | string[]> = { reviewers: [] };
    setup(listing([input({ name: "reviewers", ty: "agent_list" })]), inputs, PANES);
    // Picking the pane adds its members; picking it again removes them.
    await fireEvent.click(screen.getByTestId("workflow-pane-reviewers-pane-1"));
    expect(inputs.reviewers).toEqual(["primary"]);
    await fireEvent.click(screen.getByTestId("workflow-pane-reviewers-pane-1"));
    expect(inputs.reviewers).toEqual([]);
  });

  it("offers pane chips on both [agent] and single agent inputs", () => {
    setup(
      listing([
        input({ name: "worker", ty: "agent" }),
        input({ name: "reviewers", ty: "agent_list" }),
      ]),
      { worker: "", reviewers: [] },
      PANES,
    );
    expect(screen.getByTestId("workflow-pane-reviewers-pane-1")).toBeInTheDocument();
    expect(screen.getByTestId("workflow-pane-worker-pane-1")).toBeInTheDocument();
  });

  it("selecting a single-member pane on a single agent input binds that member", async () => {
    const inputs: Record<string, string | string[]> = { worker: "" };
    setup(listing([input({ name: "worker", ty: "agent" })]), inputs, PANES);
    await fireEvent.click(screen.getByTestId("workflow-pane-worker-pane-1"));
    expect(inputs.worker).toBe("primary");
  });

  it("hides multi-member panes on single agent inputs, keeps them on lists", () => {
    // A pane that maps to >1 agent can't fill a single slot, so it's offered on
    // the [agent] input but not the single agent input.
    const multi: TranscriptPane[] = [
      { id: "multi", name: "Both", members: ["a1", "a2"], hidden: [] },
      { id: "solo", name: "Solo", members: ["a1"], hidden: [] },
    ] as TranscriptPane[];
    setup(
      listing([
        input({ name: "worker", ty: "agent" }),
        input({ name: "reviewers", ty: "agent_list" }),
      ]),
      { worker: "", reviewers: [] },
      multi,
    );
    // Single agent: only the single-member pane shows.
    expect(screen.queryByTestId("workflow-pane-worker-multi")).toBeNull();
    expect(screen.getByTestId("workflow-pane-worker-solo")).toBeInTheDocument();
    // List: both panes show.
    expect(screen.getByTestId("workflow-pane-reviewers-multi")).toBeInTheDocument();
    expect(screen.getByTestId("workflow-pane-reviewers-solo")).toBeInTheDocument();
  });

  it("removes the workflow via the x button", async () => {
    const { onremove } = setup(listing([input({ name: "worker", ty: "agent" })]), { worker: "" });
    await fireEvent.click(screen.getByTestId("workflow-composer-remove"));
    expect(onremove).toHaveBeenCalledOnce();
  });

  it("shows the capability-gate message for a non-invocable workflow", () => {
    setup(listing([], { invocable: false }));
    expect(screen.getByTestId("workflow-not-supported")).toBeInTheDocument();
  });

  it("prompts to fill required inputs until they are provided", () => {
    setup(listing([input({ name: "review_prompt", ty: "prompt_id" })]), { review_prompt: "" });
    // A required, unfilled input surfaces the hint.
    expect(screen.getByTestId("workflow-missing")).toBeInTheDocument();
  });
});
