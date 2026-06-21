import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import WorkflowComposer from "./WorkflowComposer.svelte";
import type { AgentRecord, Prompt, WorkflowInputInfo, WorkflowListing } from "$lib/types";

const AGENTS: AgentRecord[] = [
  { id: "a1", project_id: "p", name: "primary", harness: "claude_code" },
  { id: "a2", project_id: "p", name: "reviewer-1", harness: "claude_code" },
] as AgentRecord[];

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

function setup(workflow: WorkflowListing, inputs: Record<string, string | string[]> = {}) {
  const onremove = vi.fn();
  render(WorkflowComposer, {
    props: { workflow, agents: AGENTS, prompts: PROMPTS, inputs, onremove },
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
