import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import ComposeBar from "./ComposeBar.svelte";

describe("ComposeBar", () => {
  it("Cmd+Enter triggers submit with the trimmed prompt", async () => {
    const onSubmit = vi.fn();
    render(ComposeBar, { props: { onSubmit } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    await fireEvent.input(textarea, { target: { value: "  hello world  " } });
    await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });

    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit).toHaveBeenCalledWith("hello world");
  });

  it("clicking Send triggers submit", async () => {
    const onSubmit = vi.fn();
    render(ComposeBar, { props: { onSubmit } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    await fireEvent.input(textarea, { target: { value: "ping" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    expect(onSubmit).toHaveBeenCalledWith("ping");
  });

  it("empty or whitespace-only input does not submit", async () => {
    const onSubmit = vi.fn();
    render(ComposeBar, { props: { onSubmit } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    // Cmd+Enter on empty input does nothing.
    await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSubmit).not.toHaveBeenCalled();

    // Whitespace-only input also does nothing.
    await fireEvent.input(textarea, { target: { value: "   " } });
    await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("disabled prop blocks submission via keyboard and button", async () => {
    const onSubmit = vi.fn();
    render(ComposeBar, { props: { onSubmit, disabled: true } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;

    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSubmit).not.toHaveBeenCalled();

    const sendButton = screen.getByTestId("compose-send") as HTMLButtonElement;
    expect(sendButton.disabled).toBe(true);
  });

  it("plain Enter (no Cmd) does not submit — allows newlines", async () => {
    const onSubmit = vi.fn();
    render(ComposeBar, { props: { onSubmit } });
    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "line one" } });
    await fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
