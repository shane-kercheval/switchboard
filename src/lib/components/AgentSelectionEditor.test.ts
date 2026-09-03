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
    await fireEvent.change(screen.getByTestId("agent-selection-model-current"), {
      target: { value: "gpt-oss-120b" },
    });
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
          model_choices: ["claude-sonnet-4-6", "gemini-3.7-flash"],
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

  it("keeps the independent effort default editable for a no-effort default model", async () => {
    const onChange = vi.fn();
    render(AgentSelectionEditor, {
      props: {
        harness: "antigravity",
        selection: {
          model: "claude-sonnet-4-6",
          effort: "high",
          model_choices: ["claude-sonnet-4-6", "gemini-3.7-flash"],
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
          model: "gemini-3.7-flash",
          effort: "high",
          model_choices: ["gemini-3.7-flash", "claude-sonnet-4-6"],
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

  it("clears interaction feedback when the edited context changes", async () => {
    const onChange = vi.fn();
    const view = render(AgentSelectionEditor, {
      props: { harness: "claude_code", selection: BASE, context: "current", onChange },
    });
    await fireEvent.click(screen.getByTestId("agent-selection-model-choice-opus"));
    expect(within(screen.getByTestId("agent-selection-model")).getByRole("status")).toBeVisible();

    await view.rerender({
      harness: "codex",
      selection: {
        model: "gpt-5.6-sol",
        effort: "high",
        model_choices: ["gpt-5.6-sol", "gpt-5.6-terra"],
        effort_choices: ["high", "medium"],
      },
      context: "current",
      onChange,
    });
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  });
});
