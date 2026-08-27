import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import ResizeHandle from "./ResizeHandle.svelte";

type Overrides = Partial<{
  value: () => number;
  min: number;
  max: () => number;
  edge: "start" | "end";
  onDraft: (value: number) => void;
  onCommit: (value: number) => void;
  onReset: () => void;
}>;

function mount(overrides: Overrides = {}): HTMLElement {
  render(ResizeHandle, {
    props: {
      value: () => 300,
      min: 200,
      max: () => 500,
      label: "Resize",
      testid: "handle",
      onCommit: () => {},
      ...overrides,
    },
  });
  return screen.getByTestId("handle");
}

async function drag(handle: HTMLElement, from: number, to: number[]): Promise<void> {
  await fireEvent.pointerDown(handle, { clientX: from });
  for (const x of to) await fireEvent.pointerMove(window, { clientX: x });
  await fireEvent.pointerUp(window);
}

describe("ResizeHandle", () => {
  it("drafts the adjusted value on every move and commits it on pointer-up", async () => {
    const onDraft = vi.fn();
    const onCommit = vi.fn();
    const handle = mount({ onDraft, onCommit });
    await drag(handle, 100, [140, 160]);
    expect(onDraft.mock.calls.map((c) => c[0])).toEqual([340, 360]);
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(360);
  });

  it("hides the resize tooltip throughout a drag and requires a fresh hover afterward", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const handle = mount();
      await fireEvent.pointerEnter(handle);
      await vi.advanceTimersByTimeAsync(700);
      const tooltip = await waitFor(() => screen.getByTestId("tooltip-content"));
      expect(tooltip).toHaveTextContent("Resize");
      expect(tooltip).toHaveTextContent("← →");
      expect(tooltip).toHaveTextContent("double-click to reset");
      expect(screen.getByText("Resize")).toHaveAttribute("aria-hidden", "true");

      await fireEvent.pointerDown(handle, { clientX: 100 });
      await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());
      await fireEvent.pointerMove(window, { clientX: 140 });
      await vi.advanceTimersByTimeAsync(1_000);
      expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

      await fireEvent.pointerUp(window);
      await fireEvent.pointerMove(handle);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

      await fireEvent.pointerLeave(handle);
      await fireEvent.pointerEnter(handle);
      await vi.advanceTimersByTimeAsync(700);
      expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
        "Resize",
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("stays suppressed when the pointer leaves and re-enters during a drag", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const handle = mount();
      await fireEvent.pointerEnter(handle);
      await vi.advanceTimersByTimeAsync(700);
      await waitFor(() => screen.getByTestId("tooltip-content"));

      await fireEvent.pointerDown(handle, { clientX: 100 });
      await fireEvent.pointerLeave(handle);
      await fireEvent.pointerEnter(handle);
      await fireEvent.pointerMove(handle);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

      await fireEvent.pointerLeave(handle);
      await fireEvent.pointerUp(window);
      await fireEvent.pointerEnter(handle);
      await vi.advanceTimersByTimeAsync(700);
      expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
        "Resize",
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it.each([
    ["end", "left"],
    ["start", "right"],
  ] as const)("places an %s-edge tooltip on the %s", async (edge, side) => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const handle = mount({ edge });
      await fireEvent.pointerEnter(handle);
      await vi.advanceTimersByTimeAsync(700);
      expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveAttribute(
        "data-side",
        side,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("inverts the axis for a start-edge handle", async () => {
    const onCommit = vi.fn();
    const handle = mount({ edge: "start", onCommit });
    await drag(handle, 100, [60]);
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(340);
  });

  it("clamps at min and max", async () => {
    const onDraft = vi.fn();
    const handle = mount({ onDraft, onCommit: () => {} });
    await drag(handle, 100, [-900, 900]);
    expect(onDraft.mock.calls.map((c) => c[0])).toEqual([200, 500]);
  });

  it("holds the midpoint when the live range inverts (container too small)", async () => {
    const onDraft = vi.fn();
    const handle = mount({ min: 400, max: () => 100, onDraft, onCommit: () => {} });
    await drag(handle, 100, [500]);
    expect(onDraft).toHaveBeenCalledExactlyOnceWith(250);
  });

  it("commits exactly once per drag, and not at all without movement", async () => {
    const onCommit = vi.fn();
    const handle = mount({ onCommit });
    await drag(handle, 100, [150]);
    expect(onCommit).toHaveBeenCalledTimes(1);
    await fireEvent.pointerDown(handle, { clientX: 100 });
    await fireEvent.pointerUp(window);
    expect(onCommit).toHaveBeenCalledTimes(1);
    await fireEvent.pointerMove(window, { clientX: 300 });
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("reads the live start value and max at drag time", async () => {
    let committed = 300;
    const onCommit = vi.fn((v: number) => {
      committed = v;
    });
    const handle = mount({ value: () => committed, max: () => 1000, onCommit });
    await drag(handle, 100, [200]);
    await drag(handle, 100, [200]);
    expect(onCommit).toHaveBeenNthCalledWith(1, 400);
    expect(onCommit).toHaveBeenNthCalledWith(2, 500);
  });

  it("double-click fires the reset callback", async () => {
    const onReset = vi.fn();
    const handle = mount({ onReset });
    await fireEvent.dblClick(handle);
    expect(onReset).toHaveBeenCalledOnce();
  });

  it("clamps the start value to the live bound: a drag begins from the rendered width, not the stored one", async () => {
    const onDraft = vi.fn();
    // Stored 480, live max 320 (the CSS-capped rendered width).
    const handle = mount({ value: () => 480, max: () => 320, onDraft, onCommit: () => {} });
    await drag(handle, 100, [60]);
    expect(onDraft).toHaveBeenCalledExactlyOnceWith(280);
  });

  it("pointercancel finalizes like pointer-up: commits once, disarms the drag", async () => {
    const onDraft = vi.fn();
    const onCommit = vi.fn();
    const handle = mount({ onDraft, onCommit });
    await fireEvent.pointerDown(handle, { clientX: 100 });
    await fireEvent.pointerMove(window, { clientX: 150 });
    await fireEvent.pointerCancel(window);
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(350);
    await fireEvent.pointerMove(window, { clientX: 300 });
    expect(onDraft).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("window blur finalizes a moved drag, and commits nothing without movement", async () => {
    const onCommit = vi.fn();
    const handle = mount({ onCommit });
    await fireEvent.pointerDown(handle, { clientX: 100 });
    await fireEvent.pointerMove(window, { clientX: 150 });
    await fireEvent.blur(window);
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(350);
    await fireEvent.pointerDown(handle, { clientX: 100 });
    await fireEvent.blur(window);
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("arrow keys draft steps from the live value and commit once on release", async () => {
    const onDraft = vi.fn();
    const onCommit = vi.fn();
    const handle = mount({ onDraft, onCommit });
    await fireEvent.keyDown(handle, { key: "ArrowRight" });
    await fireEvent.keyDown(handle, { key: "ArrowRight" });
    await fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(onDraft.mock.calls.map((c) => c[0])).toEqual([316, 332, 316]);
    expect(onCommit).not.toHaveBeenCalled();
    await fireEvent.keyUp(handle, { key: "ArrowLeft" });
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(316);
    expect(handle.getAttribute("aria-valuenow")).toBe("316");
  });

  it("hides the tooltip during keyboard resizing until the handle is focused again", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const handle = mount();
      await fireEvent.keyDown(window, { key: "Tab" });
      await fireEvent.focus(handle);
      await vi.advanceTimersByTimeAsync(700);
      expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
        "Resize",
      );

      await fireEvent.keyDown(handle, { key: "ArrowRight" });
      await waitFor(() => expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument());
      await fireEvent.keyUp(handle, { key: "ArrowRight" });
      await vi.advanceTimersByTimeAsync(1_000);
      expect(screen.queryByTestId("tooltip-content")).not.toBeInTheDocument();

      await fireEvent.blur(handle);
      await fireEvent.keyDown(window, { key: "Tab" });
      await fireEvent.focus(handle);
      await vi.advanceTimersByTimeAsync(700);
      expect(await waitFor(() => screen.getByTestId("tooltip-content"))).toHaveTextContent(
        "Resize",
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("keyboard steps clamp at the bounds and invert for a start-edge handle", async () => {
    const onDraft = vi.fn();
    const onCommit = vi.fn();
    const handle = mount({ edge: "start", value: () => 490, onDraft, onCommit });
    await fireEvent.keyDown(handle, { key: "ArrowLeft" });
    // start-edge inverts: ArrowLeft grows, clamped at max 500 (start 490).
    expect(onDraft).toHaveBeenLastCalledWith(500);
    await fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(onDraft).toHaveBeenLastCalledWith(500);
    await fireEvent.keyUp(handle, { key: "ArrowLeft" });
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(500);
  });

  it("blurring the handle mid-adjustment commits the keyboard draft", async () => {
    const onCommit = vi.fn();
    const handle = mount({ onCommit });
    await fireEvent.keyDown(handle, { key: "ArrowRight" });
    await fireEvent.blur(handle);
    expect(onCommit).toHaveBeenCalledExactlyOnceWith(316);
    await fireEvent.blur(handle);
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("prevents default on arrow keys and exposes the splitter to the accessibility tree", async () => {
    const handle = mount({});
    expect(handle).toHaveAttribute("tabindex", "0");
    expect(handle).toHaveAttribute("aria-valuemin", "200");
    await fireEvent.focus(handle);
    expect(handle).toHaveAttribute("aria-valuenow", "300");
    expect(handle).toHaveAttribute("aria-valuemax", "500");
    const notPrevented = await fireEvent.keyDown(handle, { key: "ArrowRight" });
    expect(notPrevented).toBe(false);
  });
});
