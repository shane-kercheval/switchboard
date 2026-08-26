import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import Harness from "./_TooltipHarness.svelte";

/// Tooltip wraps `bits-ui` with a 700ms `delayDuration`. Fake timers let
/// each `pointerEnter` resolve in microseconds instead of waiting 700ms
/// of wall time per test — without them the suite gets visibly slow as
/// tooltip coverage grows.
///
/// `shouldAdvanceTime` also moves the clock forward with real time, so never
/// assert on the exact edge of a delay: a single auto-advance tick under load
/// carries virtual time past the threshold and opens a tooltip a test expects
/// to still be closed. Straddle a boundary with a comfortable margin on each
/// side (e.g. 500ms then 200ms), not 699ms then 1ms.
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
    await vi.advanceTimersByTimeAsync(700);
    const content = await waitFor(() => screen.getByTestId("tooltip-content"));
    expect(content).toHaveTextContent("hello label");
    expect(content).toHaveTextContent("⌘K");
  });

  it("renders the slot content in children mode", async () => {
    render(Harness, { props: { mode: "children" } });
    await fireEvent.pointerEnter(screen.getByTestId("tt-trigger"));
    await vi.advanceTimersByTimeAsync(700);
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
    await vi.advanceTimersByTimeAsync(700);
    const content = await waitFor(() => screen.getByTestId("tooltip-content"));
    expect(content).toHaveTextContent("hello label");
  });

  it("keeps supplemental hover text out of the keyboard tab order", async () => {
    render(Harness, { props: { mode: "non-focusable" } });
    const trigger = screen.getByTestId("tt-trigger");
    expect(trigger).not.toHaveAttribute("tabindex");

    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(700);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "supplemental detail",
    );
  });

  it("can delegate details to keyboard focus without claiming nested hover", async () => {
    render(Harness, { props: { mode: "focus-only" } });
    const trigger = screen.getByTestId("tt-trigger");

    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(1000);
    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "Tab" });
    await fireEvent.focus(trigger);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "keyboard detail",
    );
  });

  it("makes content pointer-transparent by default and closes after leaving the trigger", async () => {
    render(Harness, { props: { mode: "label" } });
    const trigger = screen.getByTestId("tt-trigger");

    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(700);
    const content = await waitFor(() => screen.getByTestId("tooltip-content"));
    expect(content.className).toContain("pointer-events-none");

    await fireEvent.pointerLeave(trigger);
    await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());
  });

  it("keeps an activated state control quiet until a real pointer re-entry", async () => {
    render(Harness, { props: { mode: "fresh-hover" } });
    const trigger = screen.getByTestId("tt-trigger");

    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(700);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "toggle 0",
    );

    await fireEvent.click(trigger);
    await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());
    expect(trigger).toHaveTextContent("trigger");

    await fireEvent.pointerMove(trigger);
    await vi.advanceTimersByTimeAsync(1000);
    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

    await fireEvent.pointerLeave(trigger);
    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(500);
    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(200);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "toggle 1",
    );
  });

  it("cancels a pending state-control tooltip when the control is clicked", async () => {
    render(Harness, { props: { mode: "fresh-hover" } });
    const trigger = screen.getByTestId("tt-trigger");

    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(200);
    await fireEvent.click(trigger);
    await vi.advanceTimersByTimeAsync(1000);

    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();
  });

  it("cancels a pending state-control tooltip when the window blurs", async () => {
    render(Harness, { props: { mode: "fresh-hover" } });
    const trigger = screen.getByTestId("tt-trigger");

    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(200);
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(1000);

    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();
  });

  it("does not restore a state-control tooltip when the window regains focus", async () => {
    render(Harness, { props: { mode: "fresh-hover" } });
    const trigger = screen.getByTestId("tt-trigger");

    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(700);
    await waitFor(() => screen.getByTestId("tooltip-content"));

    window.dispatchEvent(new Event("blur"));
    await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());
    window.dispatchEvent(new Event("focus"));
    await fireEvent.focus(trigger);
    await vi.advanceTimersByTimeAsync(1000);

    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

    await fireEvent.pointerLeave(trigger);
    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(700);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "toggle 0",
    );
  });

  it("opens after real keyboard navigation following a window change", async () => {
    render(Harness, { props: { mode: "fresh-hover" } });
    const trigger = screen.getByTestId("tt-trigger");
    vi.spyOn(trigger, "matches").mockReturnValue(true);

    window.dispatchEvent(new Event("blur"));
    window.dispatchEvent(new Event("focus"));
    await fireEvent.focus(trigger);
    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

    await fireEvent.blur(trigger);
    await fireEvent.keyDown(window, { key: "Tab" });
    await fireEvent.focus(trigger);

    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "toggle 0",
    );
  });

  it("keeps separate state controls independent while both remain mounted", async () => {
    render(Harness, { props: { mode: "two" } });
    const first = screen.getByTestId("tt-first");
    const second = screen.getByTestId("tt-second");

    await fireEvent.pointerEnter(first);
    await vi.advanceTimersByTimeAsync(700);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "first tooltip",
    );
    await fireEvent.click(first);
    await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());

    await fireEvent.pointerLeave(first);
    await fireEvent.pointerEnter(second);
    await vi.advanceTimersByTimeAsync(500);
    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(200);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "second tooltip",
    );
  });

  it("keeps a remaining state control registered when its sibling unmounts", async () => {
    render(Harness, { props: { mode: "two" } });
    const second = screen.getByTestId("tt-second");

    await fireEvent.click(screen.getByTestId("tt-remove-first"));
    await fireEvent.pointerEnter(second);
    await vi.advanceTimersByTimeAsync(700);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "second tooltip",
    );
  });

  it("preserves the ordinary tooltip's 300ms recent-tooltip grace period", async () => {
    render(Harness, { props: { mode: "label" } });
    const trigger = screen.getByTestId("tt-trigger");

    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(700);
    await waitFor(() => screen.getByTestId("tooltip-content"));
    await fireEvent.click(trigger);
    await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());

    await fireEvent.pointerLeave(trigger);
    await fireEvent.pointerEnter(trigger);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "hello label",
    );

    await fireEvent.click(trigger);
    await vi.advanceTimersByTimeAsync(300);
    await fireEvent.pointerLeave(trigger);
    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(500);
    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(200);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toBeInTheDocument();
  });

  it("preserves external open bindings", async () => {
    render(Harness, { props: { mode: "bound" } });

    expect(screen.getByTestId("tt-open-state")).toHaveTextContent("closed");
    await fireEvent.click(screen.getByTestId("tt-set-open"));
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "bound tooltip",
    );
    expect(screen.getByTestId("tt-open-state")).toHaveTextContent("open");

    await fireEvent.click(screen.getByTestId("tt-trigger"));
    await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());
    expect(screen.getByTestId("tt-open-state")).toHaveTextContent("closed");
  });

  it("writes tether closure through a fresh-hover open binding", async () => {
    render(Harness, { props: { mode: "bound-fresh" } });

    await fireEvent.click(screen.getByTestId("tt-set-open"));
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "bound tooltip",
    );
    expect(screen.getByTestId("tt-open-state")).toHaveTextContent("open");

    await fireEvent.click(screen.getByTestId("tt-trigger"));
    await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());
    expect(screen.getByTestId("tt-open-state")).toHaveTextContent("closed");
  });

  it("lets an explicit focus-modality choice override the fresh-hover default", async () => {
    render(Harness, { props: { mode: "focus-override" } });

    await fireEvent.focus(screen.getByTestId("tt-trigger"));
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "focus override",
    );
  });

  it("reinitializes pointer state when fresh-hover behavior reattaches", async () => {
    render(Harness, { props: { mode: "dynamic" } });
    const trigger = screen.getByTestId("tt-trigger");
    const nativeMatches = trigger.matches.bind(trigger);
    vi.spyOn(trigger, "matches").mockImplementation((selector: string) =>
      selector === ":hover" ? true : nativeMatches(selector),
    );

    await fireEvent.pointerEnter(trigger);
    await fireEvent.click(screen.getByTestId("tt-set-default"));
    await fireEvent.click(screen.getByTestId("tt-set-fresh"));
    await fireEvent.click(trigger);
    await fireEvent.pointerMove(trigger);
    await vi.advanceTimersByTimeAsync(1000);
    expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

    await fireEvent.pointerLeave(trigger);
    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(700);
    expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
      "dynamic tooltip",
    );
  });
});
