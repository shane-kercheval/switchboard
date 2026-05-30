import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import Harness from "./_TooltipHarness.svelte";

/// Tooltip wraps `bits-ui` with a 500ms `delayDuration`. Fake timers let
/// each `pointerEnter` resolve in microseconds instead of waiting 500ms
/// of wall time per test — without them the suite gets visibly slow as
/// tooltip coverage grows.
beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("Tooltip", () => {
  it("renders the label and shortcut in label mode (existing-caller regression)", async () => {
    render(Harness, { props: { mode: "label" } });
    await fireEvent.pointerEnter(screen.getByTestId("tt-trigger"));
    await vi.advanceTimersByTimeAsync(500);
    const content = await waitFor(() => screen.getByTestId("tooltip-content"));
    expect(content).toHaveTextContent("hello label");
    expect(content).toHaveTextContent("⌘K");
  });

  it("renders the slot content in children mode", async () => {
    render(Harness, { props: { mode: "children" } });
    await fireEvent.pointerEnter(screen.getByTestId("tt-trigger"));
    await vi.advanceTimersByTimeAsync(500);
    await waitFor(() => screen.getByTestId("tooltip-content"));
    const rich = screen.getByTestId("tt-rich-content");
    expect(rich).toHaveTextContent("row one");
    expect(rich).toHaveTextContent("row two");
    // Label-mode label-div must not appear when children are provided
    // (regression guard against accidentally rendering both).
    expect(screen.queryByText("hello label")).not.toBeInTheDocument();
  });

  it("opens on keyboard focus as well as pointer hover (a11y)", async () => {
    render(Harness, { props: { mode: "label" } });
    await fireEvent.focus(screen.getByTestId("tt-trigger"));
    await vi.advanceTimersByTimeAsync(500);
    const content = await waitFor(() => screen.getByTestId("tooltip-content"));
    expect(content).toHaveTextContent("hello label");
  });
});
