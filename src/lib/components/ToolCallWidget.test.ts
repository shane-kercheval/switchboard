import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, fireEvent } from "@testing-library/svelte";
import type { ToolCall } from "$lib/state/types";
import ToolCallWidget from "./ToolCallWidget.svelte";

const running: ToolCall = {
  item_kind: "tool",
  tool_use_id: "t1",
  kind: "builtin",
  name: "Bash",
  input: { command: "sleep 1" },
  started_at: "2026-05-16T00:00:01Z",
};
const done: ToolCall = {
  ...running,
  output: "hi\n",
  is_error: false,
  completed_at: "2026-05-16T00:00:02Z",
};
const cancelled: ToolCall = {
  ...running,
  stopped_at: "2026-05-16T00:00:02Z",
  stop_reason: "cancelled",
};
const stoppedFailed: ToolCall = {
  ...running,
  stopped_at: "2026-05-16T00:00:02Z",
  stop_reason: "failed",
};

function summaryOf(el: HTMLElement): HTMLElement {
  const summary = el.querySelector("summary");
  if (summary === null) throw new Error("expected a summary");
  return summary as HTMLElement;
}

describe("ToolCallWidget disclosure", () => {
  it("shows a one-line input preview in the collapsed row", () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: { ...done, input: { command: "git status --short --branch && git log --oneline -1" } },
    });

    expect(getByTestId("tool-input-preview")).toHaveTextContent(
      "git status --short --branch && git log --oneline -1",
    );
  });

  it("renders input and output as separate expanded sections", async () => {
    const { getByTestId } = render(ToolCallWidget, {
      tool: {
        ...done,
        input: { file_path: "/tmp/file.txt", old_string: "before", new_string: "after" },
      },
    });
    const tool = getByTestId("turn-tool");

    await fireEvent.click(summaryOf(tool));

    expect(getByTestId("tool-input")).toHaveTextContent('"file_path": "/tmp/file.txt"');
    expect(getByTestId("tool-input")).toHaveTextContent('"old_string": "before"');
    expect(getByTestId("tool-output")).toHaveTextContent("hi");
  });

  it("stays collapsed while running and after completion when untouched", async () => {
    const { getByTestId, rerender } = render(ToolCallWidget, { tool: running });
    expect(getByTestId("turn-tool")).not.toHaveAttribute("open");

    await rerender({ tool: done });
    expect(getByTestId("turn-tool")).not.toHaveAttribute("open");
  });

  it("does not yank the panel shut on completion once the user has toggled it", async () => {
    const { getByTestId, rerender } = render(ToolCallWidget, { tool: running });
    const tool = getByTestId("turn-tool");
    await fireEvent.click(summaryOf(tool));
    expect(tool).toHaveAttribute("open");

    await rerender({ tool: done });
    expect(tool).toHaveAttribute("open");
  });

  it("lets the user expand a completed tool and keeps it open", async () => {
    const { getByTestId } = render(ToolCallWidget, { tool: done });
    const tool = getByTestId("turn-tool");
    expect(tool).not.toHaveAttribute("open");

    await fireEvent.click(summaryOf(tool));
    expect(tool).toHaveAttribute("open");
  });

  it("shows a cancelled icon for a tool that was pending when the turn stopped", () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, { tool: cancelled });
    expect(queryByTestId("tool-running")).toBeNull();
    expect(getByTestId("tool-cancelled")).toBeInTheDocument();
    expect(getByTestId("turn-tool")).not.toHaveAttribute("open");
  });

  it("shows a failed icon for a tool that was pending when the turn failed", () => {
    const { getByTestId, queryByTestId } = render(ToolCallWidget, { tool: stoppedFailed });
    expect(queryByTestId("tool-running")).toBeNull();
    expect(getByTestId("tool-error")).toBeInTheDocument();
    expect(getByTestId("turn-tool")).not.toHaveAttribute("open");
  });
});
