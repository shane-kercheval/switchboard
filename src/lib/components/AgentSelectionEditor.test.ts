import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type { AgentSelection } from "$lib/types";
import AgentSelectionEditor from "./AgentSelectionEditor.svelte";

const BASE: AgentSelection = {
  model: "opus",
  effort: "high",
  model_choices: ["opus", "sonnet"],
  effort_choices: ["high", "medium"],
};

describe("AgentSelectionEditor", () => {
  it("explains quick choices in dialogs and calls the starting assignment Default", () => {
    render(AgentSelectionEditor, {
      props: { harness: "claude_code", selection: BASE, context: "start", onChange: vi.fn() },
    });

    expect(screen.getByText("Model")).toBeInTheDocument();
    expect(screen.getByText("Reasoning Effort")).toBeInTheDocument();
    expect(screen.getByText(/available for quick switching from the Agents sidebar/)).toBeVisible();
    expect(screen.getAllByText("Default")).toHaveLength(2);
    expect(screen.queryByText(/Quick Choices/)).toBeNull();
  });

  it("removes the current choice when another choice can take over", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: { harness: "claude_code", selection: BASE, context: "current", onChange },
    });

    const opus = screen.getByTestId("agent-selection-model-choice-opus");
    expect(opus).toHaveAttribute("aria-pressed", "true");
    expect(opus).toHaveAttribute("aria-disabled", "false");
    await fireEvent.click(opus);
    expect(onChange).toHaveBeenCalledWith({
      ...BASE,
      model: "sonnet",
      model_choices: ["sonnet"],
    });
  });

  it("locks only the last selected choice on an axis", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: { ...BASE, model_choices: ["opus"] },
        context: "default",
        onChange,
      },
    });

    const opus = screen.getByTestId("agent-selection-model-choice-opus");
    expect(opus).toHaveAttribute("aria-disabled", "true");
    await fireEvent.click(opus);
    expect(screen.getByRole("status")).toHaveTextContent(
      "At least one quick choice is required once this axis is configured.",
    );
    expect(onChange).not.toHaveBeenCalled();
  });

  it("omits a redundant default for one choice and shows a selector for two", async () => {
    const onChange = vi.fn();
    const { rerender } = render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: { ...BASE, model_choices: ["opus"] },
        context: "default",
        onChange,
      },
    });

    expect(screen.queryByTestId("agent-selection-model-implicit")).toBeNull();
    expect(screen.queryByTestId("agent-selection-model-current")).toBeNull();

    await rerender({
      harness: "claude_code",
      selection: BASE,
      context: "default",
      onChange,
    });
    expect(screen.getByTestId("agent-selection-model-current")).toBeEnabled();
  });

  it("orders assignment options like the quick-choice toggles", () => {
    render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: {
          ...BASE,
          model: "fable",
          model_choices: ["haiku", "opus", "fable"],
        },
        context: "default",
        onChange: vi.fn(),
      },
    });

    const select = screen.getByTestId("agent-selection-model-current") as HTMLSelectElement;
    expect(Array.from(select.options, (option) => option.value)).toEqual([
      "fable",
      "opus",
      "haiku",
    ]);
  });

  it("promotes the first visible remaining choice when removing the assignment", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: {
          ...BASE,
          model: "haiku",
          model_choices: ["haiku", "opus", "fable"],
        },
        context: "default",
        onChange,
      },
    });

    await fireEvent.click(screen.getByTestId("agent-selection-model-choice-haiku"));
    expect(onChange).toHaveBeenCalledWith({
      ...BASE,
      model: "fable",
      model_choices: ["opus", "fable"],
    });
  });

  it("makes the first choice on an empty existing-agent axis current", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: { model: null, effort: null, model_choices: [], effort_choices: [] },
        context: "current",
        onChange,
      },
    });

    await fireEvent.click(screen.getByTestId("agent-selection-model-choice-haiku"));
    expect(onChange).toHaveBeenCalledWith({
      model: "haiku",
      effort: null,
      model_choices: ["haiku"],
      effort_choices: [],
    });
  });

  it("leaves current assignment out of the existing-agent editor", () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: { ...BASE, model: null, model_choices: ["sonnet"] },
        context: "current",
        onChange,
      },
    });

    expect(screen.queryByTestId("agent-selection-model-current")).toBeNull();
    expect(screen.queryByTestId("agent-selection-model-implicit")).toBeNull();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("preserves unknown choices and allows incompatible effort membership", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "antigravity",
        selection: {
          model: "gemini-3.1-pro",
          effort: "high",
          model_choices: ["retired-model", "gemini-3.1-pro"],
          effort_choices: ["high", "medium"],
        },
        context: "current",
        onChange,
      },
    });

    expect(screen.getByTestId("agent-selection-model-choice-retired-model")).toBeInTheDocument();
    const medium = screen.getByTestId("agent-selection-effort-choice-medium");
    expect(medium).toBeEnabled();
    await fireEvent.click(medium);
    expect(onChange).toHaveBeenCalledWith({
      model: "gemini-3.1-pro",
      effort: "high",
      model_choices: ["retired-model", "gemini-3.1-pro"],
      effort_choices: ["high"],
    });
  });

  it("can configure an effort before switching to a model that requires it", async () => {
    let current: AgentSelection = {
      model: "gemini-3.1-pro",
      effort: "high",
      model_choices: ["gemini-3.1-pro"],
      effort_choices: ["high"],
    };
    const onChange = vi.fn((next: AgentSelection) => {
      current = next;
    });
    const view = render(AgentSelectionEditor, {
      props: { harness: "antigravity", selection: current, context: "current", onChange },
    });

    await fireEvent.click(screen.getByTestId("agent-selection-effort-choice-medium"));
    expect(current.effort_choices).toEqual(["high", "medium"]);
    await view.rerender({
      harness: "antigravity",
      selection: current,
      context: "current",
      onChange,
    });
    await fireEvent.click(screen.getByTestId("agent-selection-model-choice-gpt-oss-120b"));
    expect(current.model_choices).toEqual(["gemini-3.1-pro", "gpt-oss-120b"]);
    await view.rerender({
      harness: "antigravity",
      selection: current,
      context: "current",
      onChange,
    });
    await fireEvent.click(screen.getByTestId("agent-selection-model-choice-gemini-3.1-pro"));
    expect(current).toMatchObject({ model: "gpt-oss-120b", effort: "medium" });
  });

  it("keeps effort membership available while a current model has no effort axis", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "antigravity",
        selection: {
          model: "claude-sonnet-4-6",
          effort: null,
          model_choices: ["claude-sonnet-4-6", "gemini-3.8-flash"],
          effort_choices: ["high", "medium"],
        },
        context: "current",
        onChange,
      },
    });

    expect(screen.getByTestId("agent-selection-effort-unavailable")).toHaveTextContent(
      "current model does not use reasoning effort",
    );
    expect(screen.queryByTestId("agent-selection-effort-current")).toBeNull();
    expect(screen.getByTestId("agent-selection-effort-choice-medium")).toBeEnabled();
  });

  it("asks for a model before assigning effort when no model is selected", () => {
    render(AgentSelectionEditor, {
      props: {
        harness: "antigravity",
        selection: {
          model: null,
          effort: null,
          model_choices: [],
          effort_choices: ["high"],
        },
        context: "current",
        onChange: vi.fn(),
      },
    });

    expect(screen.getByTestId("agent-selection-effort-unavailable")).toHaveTextContent(
      "Choose a model before assigning the reasoning effort.",
    );
  });

  it("explains an incompatible model choice within an assignment editor", async () => {
    render(AgentSelectionEditor, {
      props: {
        harness: "antigravity",
        selection: {
          model: "gemini-3.8-flash",
          effort: "medium",
          model_choices: ["gemini-3.8-flash", "gemini-3.1-pro"],
          effort_choices: ["medium"],
        },
        context: "default",
        onChange: vi.fn(),
      },
    });

    await fireEvent.change(screen.getByTestId("agent-selection-model-current"), {
      target: { value: "gemini-3.1-pro" },
    });
    expect(
      within(screen.getByTestId("agent-selection-model")).getByRole("status"),
    ).toHaveTextContent(
      "No configured reasoning effort is compatible with that model. Add a compatible reasoning effort quick choice, then try again.",
    );
  });

  it("keeps the independent effort default editable for a no-effort default model", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "antigravity",
        selection: {
          model: "claude-sonnet-4-6",
          effort: "high",
          model_choices: ["claude-sonnet-4-6", "gemini-3.8-flash"],
          effort_choices: ["high", "medium"],
        },
        context: "default",
        onChange,
      },
    });

    const selector = screen.getByTestId("agent-selection-effort-current");
    expect(within(selector).getByRole("option", { name: "Medium" })).toBeEnabled();
    await fireEvent.change(selector, { target: { value: "medium" } });
    expect(onChange).toHaveBeenCalledWith(expect.objectContaining({ effort: "medium" }));
  });

  it("preserves the independent effort default when choosing a no-effort default model", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "antigravity",
        selection: {
          model: "gemini-3.8-flash",
          effort: "high",
          model_choices: ["gemini-3.8-flash", "claude-sonnet-4-6"],
          effort_choices: ["high", "medium"],
        },
        context: "default",
        onChange,
      },
    });

    await fireEvent.change(screen.getByTestId("agent-selection-model-current"), {
      target: { value: "claude-sonnet-4-6" },
    });
    expect(onChange).toHaveBeenCalledWith(
      expect.objectContaining({ model: "claude-sonnet-4-6", effort: "high" }),
    );
  });

  it("clears interaction feedback when the controlled selection changes externally", async () => {
    const onChange = vi.fn();
    const view = render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: { ...BASE, model_choices: ["opus"] },
        context: "current",
        onChange,
      },
    });
    await fireEvent.click(screen.getByTestId("agent-selection-model-choice-opus"));
    expect(within(screen.getByTestId("agent-selection-model")).getByRole("status")).toBeVisible();

    await view.rerender({
      harness: "claude_code",
      selection: {
        model: "sonnet",
        effort: "high",
        model_choices: ["opus", "sonnet"],
        effort_choices: ["high", "medium"],
      },
      context: "current",
      onChange,
    });
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  });
});
