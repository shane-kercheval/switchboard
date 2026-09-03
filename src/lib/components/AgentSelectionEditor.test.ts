import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import type { AgentSelection } from "$lib/types";
import AgentSelectionEditor from "./AgentSelectionEditor.svelte";

const BASE: AgentSelection = {
  model: "opus",
  effort: "high",
  model_choices: ["opus", "sonnet"],
  effort_choices: ["high", "medium"],
};

describe("AgentSelectionEditor", () => {
  it("uses pressed multi-select controls and explains why the current value is locked", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: { harness: "claude_code", selection: BASE, context: "current", onChange },
    });

    const opus = screen.getByTestId("agent-selection-model-choice-opus");
    expect(opus).toHaveAttribute("aria-pressed", "true");
    expect(opus).toHaveAttribute("aria-disabled", "true");
    await fireEvent.click(opus);
    expect(screen.getByRole("status")).toHaveTextContent(
      "Choose another current before removing this value.",
    );
    expect(onChange).not.toHaveBeenCalled();
  });

  it("shows an implicit default for one choice and an explicit selector for two", async () => {
    const onChange = vi.fn();
    const { rerender } = render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: { ...BASE, model_choices: ["opus"] },
        context: "default",
        onChange,
      },
    });

    expect(screen.getByTestId("agent-selection-model-implicit")).toHaveTextContent("Default: Opus");
    expect(screen.queryByTestId("agent-selection-model-current")).toBeNull();

    await rerender({
      harness: "claude_code",
      selection: BASE,
      context: "default",
      onChange,
    });
    expect(screen.getByTestId("agent-selection-model-current")).toBeEnabled();
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

  it("allows adopting a sole configured choice when current is null", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "claude_code",
        selection: { ...BASE, model: null, model_choices: ["sonnet"] },
        context: "current",
        onChange,
      },
    });

    await fireEvent.change(screen.getByTestId("agent-selection-model-current"), {
      target: { value: "sonnet" },
    });
    expect(onChange).toHaveBeenCalledWith({ ...BASE, model: "sonnet", model_choices: ["sonnet"] });
  });

  it("preserves unknown choices and disables known incompatible Antigravity effort", () => {
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
    expect(screen.getByTestId("agent-selection-effort-choice-medium")).toBeDisabled();
  });
});
