import { beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import PromptComposer from "./PromptComposer.svelte";
import type { Prompt } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const PROMPT: Prompt = {
  provider: "local",
  name: "review",
  title: "Code Review",
  description: "Review code",
  arguments: [
    { name: "focus", description: "What to focus on", required: true },
    { name: "tone", description: null, required: false },
  ],
  tags: [],
};

function setup(args: Record<string, string>, appendedText = "") {
  const onremove = vi.fn();
  render(PromptComposer, {
    props: { prompt: PROMPT, args, appendedText, onremove },
  });
  return { onremove };
}

beforeEach(() => {
  invokeMock.mockReset();
});

describe("PromptComposer", () => {
  it("renders an input per argument with required/optional markers and descriptions", () => {
    setup({ focus: "", tone: "" });
    expect(screen.getByTestId("prompt-arg-focus")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-arg-required-focus")).toHaveTextContent("required");
    // The optional arg has no required marker.
    expect(screen.queryByTestId("prompt-arg-required-tone")).toBeNull();
    expect(screen.getByText("What to focus on")).toBeInTheDocument();
  });

  it("renders the appended-text field and the chosen prompt", () => {
    setup({ focus: "", tone: "" });
    expect(screen.getByTestId("prompt-appended")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-selector")).toHaveTextContent("Code Review");
  });

  it("previews the combined message (rendered prompt + appended text) as markdown", async () => {
    invokeMock.mockResolvedValue({ text: "# RENDERED BODY" });
    setup({ focus: "tests", tone: "" }, "extra note");

    await fireEvent.click(screen.getByTestId("prompt-preview-button"));

    // Rendered as markdown in a dialog overlay (the heading becomes an <h1>),
    // not inline in the compose box.
    const previewEl = await screen.findByTestId("prompt-preview");
    expect(previewEl).toHaveTextContent("RENDERED BODY");
    expect(previewEl).toHaveTextContent("extra note");
    expect(previewEl.querySelector("h1")).not.toBeNull();
    expect(screen.getByTestId("dialog-content")).toBeInTheDocument();
    const call = invokeMock.mock.calls.find(([c]) => c === "render_prompt");
    // Blank optional `tone` is omitted, not sent as "".
    expect(call?.[1]).toMatchObject({
      provider: "local",
      name: "review",
      args: { focus: "tests" },
    });
    expect((call?.[1] as { args: Record<string, string> }).args).not.toHaveProperty("tone");
  });

  it("disables Preview until required arguments are filled", async () => {
    setup({ focus: "", tone: "" });
    expect((screen.getByTestId("prompt-preview-button") as HTMLButtonElement).disabled).toBe(true);
  });

  it("surfaces a preview render failure inline", async () => {
    invokeMock.mockRejectedValue(new Error("server is down"));
    setup({ focus: "tests", tone: "" });

    await fireEvent.click(screen.getByTestId("prompt-preview-button"));

    await waitFor(() =>
      expect(screen.getByTestId("prompt-preview-error")).toHaveTextContent("server is down"),
    );
  });

  it("removes the prompt via the Remove control", async () => {
    const { onremove } = setup({ focus: "", tone: "" });
    await fireEvent.click(screen.getByTestId("prompt-remove"));
    expect(onremove).toHaveBeenCalledTimes(1);
  });
});
