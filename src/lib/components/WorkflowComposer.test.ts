import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import WorkflowComposer from "./WorkflowComposer.svelte";
import ForwardHarness from "./_WorkflowComposerForwardHarness.svelte";
import type {
  AgentRecord,
  DerivedArgInfo,
  FormCompatibility,
  WorkflowFormDescriptor,
  WorkflowInputInfo,
} from "$lib/types";
import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
import type { ForwardSource } from "$lib/state/heldForwards.svelte";

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

function input(over: Partial<WorkflowInputInfo>): WorkflowInputInfo {
  return { name: "x", ty: "text", optional: false, description: null, ...over };
}

function arg(over: Partial<DerivedArgInfo>): DerivedArgInfo {
  return { name: "context", required: false, description: null, prompts: ["builtin:code-review"], ...over };
}

function descriptor(
  inputs: WorkflowInputInfo[],
  derived_args: DerivedArgInfo[] = [],
  over: Partial<WorkflowFormDescriptor> = {},
): WorkflowFormDescriptor {
  return {
    name: "review-and-aggregate",
    description: "d",
    is_builtin: true,
    invocable: true,
    inputs,
    derived_args,
    compatibility: { state: "ok" } as FormCompatibility,
    ...over,
  };
}

function setup(
  desc: WorkflowFormDescriptor,
  inputs: Record<string, string | string[]> = {},
  panes: TranscriptPane[] = [],
  loading = false,
  forwardSources: Record<string, ForwardSource[]> = {},
  syncSettled = false,
) {
  const onremove = vi.fn();
  render(WorkflowComposer, {
    props: {
      descriptor: desc,
      agents: AGENTS,
      panes,
      inputs,
      onremove,
      loading,
      forwardSources,
      syncSettled,
    },
  });
  return { onremove, forwardSources };
}

// Forward-picker tests need the component to *re-render* a chip after an
// in-component mutation, which requires a reactive `$bindable` proxy — so they go
// through a harness that owns `$state`, mirroring how `ComposeBar` drives it in
// production (see `_WorkflowComposerForwardHarness.svelte`).
function setupForward(
  desc: WorkflowFormDescriptor,
  inputs: Record<string, string | string[]> = {},
  initialForwardSources: Record<string, ForwardSource[]> = {},
) {
  let latest: Record<string, ForwardSource[]> = initialForwardSources;
  render(ForwardHarness, {
    props: {
      descriptor: desc,
      agents: AGENTS,
      panes: [],
      initialInputs: inputs,
      initialForwardSources,
      onForwardSources: (s: Record<string, ForwardSource[]>) => (latest = s),
    },
  });
  return { sources: () => latest };
}

describe("WorkflowComposer", () => {
  it("renders the right control per declared input plus auto-derived arg fields", () => {
    setup(
      descriptor(
        [
          input({ name: "reviewers", ty: "agent_list" }),
          input({ name: "worker", ty: "agent" }),
        ],
        [arg({ name: "context", required: false, description: "Optional background" })],
      ),
      { reviewers: [], worker: "", context: "" },
    );
    // agent + agent_list render per-agent chips.
    expect(screen.getByTestId("workflow-agent-worker-primary")).toBeInTheDocument();
    expect(screen.getByTestId("workflow-agent-reviewers-reviewer-1")).toBeInTheDocument();
    // The derived `context` arg renders a text field; no prompt-picker control exists.
    expect(screen.getByTestId("workflow-arg-input-context")).toBeInTheDocument();
    expect(screen.queryByTestId("workflow-prompt-review_prompt")).toBeNull();
    // The derived field labels which prompt it feeds.
    expect(screen.getByTestId("workflow-arg-feeds-context")).toHaveTextContent("builtin:code-review");
  });

  it("editing a derived arg field binds its value", async () => {
    const inputs: Record<string, string | string[]> = { context: "" };
    setup(descriptor([], [arg({ name: "context" })]), inputs);
    await fireEvent.input(screen.getByTestId("workflow-arg-input-context"), {
      target: { value: "watch the error paths" },
    });
    expect(inputs.context).toBe("watch the error paths");
  });

  it("selecting an agent chip binds the input value", async () => {
    const inputs: Record<string, string | string[]> = { worker: "" };
    setup(descriptor([input({ name: "worker", ty: "agent" })]), inputs);
    await fireEvent.click(screen.getByTestId("workflow-agent-worker-primary"));
    expect(inputs.worker).toBe("primary");
  });

  it("toggles a whole pane's members into an [agent] input", async () => {
    const inputs: Record<string, string | string[]> = { reviewers: [] };
    setup(descriptor([input({ name: "reviewers", ty: "agent_list" })]), inputs, PANES);
    await fireEvent.click(screen.getByTestId("workflow-pane-reviewers-pane-1"));
    expect(inputs.reviewers).toEqual(["primary"]);
    await fireEvent.click(screen.getByTestId("workflow-pane-reviewers-pane-1"));
    expect(inputs.reviewers).toEqual([]);
  });

  it("offers pane chips on both [agent] and single agent inputs", () => {
    setup(
      descriptor([
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
    setup(descriptor([input({ name: "worker", ty: "agent" })]), inputs, PANES);
    await fireEvent.click(screen.getByTestId("workflow-pane-worker-pane-1"));
    expect(inputs.worker).toBe("primary");
  });

  it("hides multi-member panes on single agent inputs, keeps them on lists", () => {
    const multi: TranscriptPane[] = [
      { id: "multi", name: "Both", members: ["a1", "a2"], hidden: [] },
      { id: "solo", name: "Solo", members: ["a1"], hidden: [] },
    ] as TranscriptPane[];
    setup(
      descriptor([
        input({ name: "worker", ty: "agent" }),
        input({ name: "reviewers", ty: "agent_list" }),
      ]),
      { worker: "", reviewers: [] },
      multi,
    );
    expect(screen.queryByTestId("workflow-pane-worker-multi")).toBeNull();
    expect(screen.getByTestId("workflow-pane-worker-solo")).toBeInTheDocument();
    expect(screen.getByTestId("workflow-pane-reviewers-multi")).toBeInTheDocument();
    expect(screen.getByTestId("workflow-pane-reviewers-solo")).toBeInTheDocument();
  });

  it("removes the workflow via the x button", async () => {
    const { onremove } = setup(descriptor([input({ name: "worker", ty: "agent" })]), { worker: "" });
    await fireEvent.click(screen.getByTestId("workflow-composer-remove"));
    expect(onremove).toHaveBeenCalledOnce();
  });

  it("shows the capability-gate message for a non-invocable workflow", () => {
    setup(descriptor([], [], { invocable: false }));
    expect(screen.getByTestId("workflow-not-supported")).toBeInTheDocument();
  });

  it("prompts to fill a required derived arg until provided", () => {
    setup(descriptor([], [arg({ name: "context", required: true })]), { context: "" });
    expect(screen.getByTestId("workflow-missing")).toBeInTheDocument();
  });

  it("shows a blocking error for an incompatible (drifted) workflow", () => {
    setup(
      descriptor([], [], {
        compatibility: {
          state: "incompatible",
          issues: [{ prompt: "local:bare", argument: "context", reason: "prompt `local:bare` has no argument `context`" }],
        },
      }),
    );
    const banner = screen.getByTestId("workflow-incompatible");
    expect(banner).toHaveTextContent("has no argument `context`");
    // The form fields are not rendered while incompatible.
    expect(screen.queryByTestId("workflow-arg-input-context")).toBeNull();
  });

  it("shows a non-error resolving affordance for an unresolved prompt before a sync settles", () => {
    setup(
      descriptor([], [], { compatibility: { state: "unresolved", prompts: ["tiddly:x"] } }),
      {},
      [],
      false,
      {},
      false, // no sync settled yet
    );
    expect(screen.getByTestId("workflow-resolving")).toBeInTheDocument();
    expect(screen.queryByTestId("workflow-incompatible")).toBeNull();
    expect(screen.queryByTestId("workflow-prompt-missing")).toBeNull();
  });

  it("escalates an unresolved prompt to a not-found error once a sync has settled", () => {
    setup(
      descriptor([], [], { compatibility: { state: "unresolved", prompts: ["tiddly:gone"] } }),
      {},
      [],
      false,
      {},
      true, // a sync settled and it's still unresolved → genuinely missing
    );
    expect(screen.getByTestId("workflow-prompt-missing")).toHaveTextContent("tiddly:gone");
    expect(screen.queryByTestId("workflow-resolving")).toBeNull();
  });

  it("attaches and removes a forward source on a derived arg field", async () => {
    const { sources } = setupForward(descriptor([], [arg({ name: "context" })]), { context: "" });

    // No picker on agent/list fields — only the derived single-text `context`.
    await fireEvent.click(screen.getByTestId("workflow-forward-picker-context"));
    await fireEvent.click(await screen.findByTestId("forward-picker-agent-a1"));

    const sourcesEl = await screen.findByTestId("workflow-forward-sources-context");
    expect(within(sourcesEl).getByTestId("forward-source-chip-primary")).toBeInTheDocument();
    expect(sources().context).toEqual([{ kind: "agent", id: "a1", name: "primary" }]);

    await fireEvent.click(screen.getByTestId("forward-source-remove-primary"));
    await waitFor(() => expect(screen.queryByTestId("forward-source-chip-primary")).toBeNull());
    expect(sources().context).toEqual([]);
  });

  it("attaches a forward source on a declared text input", async () => {
    const { sources } = setupForward(descriptor([input({ name: "note", ty: "text" })]), {
      note: "",
    });
    await fireEvent.click(screen.getByTestId("workflow-forward-picker-note"));
    await fireEvent.click(await screen.findByTestId("forward-picker-agent-a1"));
    await screen.findByTestId("forward-source-chip-primary");
    expect(sources().note).toEqual([{ kind: "agent", id: "a1", name: "primary" }]);
  });

  it("does not offer a forward picker on agent or list fields", () => {
    setup(
      descriptor([
        input({ name: "worker", ty: "agent" }),
        input({ name: "reviewers", ty: "agent_list" }),
        input({ name: "items", ty: "text_list" }),
      ]),
      { worker: "", reviewers: [], items: [] },
    );
    expect(screen.queryByTestId("workflow-forward-picker-worker")).toBeNull();
    expect(screen.queryByTestId("workflow-forward-picker-reviewers")).toBeNull();
    expect(screen.queryByTestId("workflow-forward-picker-items")).toBeNull();
  });

  it("treats a required text field with a forward source as filled", () => {
    setup(
      descriptor([input({ name: "note", ty: "text" })]),
      { note: "" },
      [],
      false,
      { note: [{ kind: "agent", id: "a1", name: "primary" }] },
    );
    expect(screen.queryByTestId("workflow-missing")).toBeNull();
  });

  it("shows the resolving affordance while the descriptor is loading", () => {
    setup(descriptor([input({ name: "worker", ty: "agent" })]), { worker: "" }, [], true);
    expect(screen.getByTestId("workflow-resolving")).toBeInTheDocument();
    // Fields are withheld until resolution settles.
    expect(screen.queryByTestId("workflow-agent-worker-primary")).toBeNull();
  });
});
