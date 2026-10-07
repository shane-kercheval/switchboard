import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MockInstance } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import Textarea from "./Textarea.svelte";
import TextareaHarness from "./_TextareaHarness.svelte";
import TextareaNonReactiveHarness from "./_TextareaNonReactiveHarness.svelte";

// The autosize contract is per-keystroke layout cost, not just final geometry:
// exactly one measure (`scrollHeight` read) per value change, and the
// max-height cap read once per instance — never per keystroke, never shared
// across instances (consumers use different caps).

let measure: MockInstance<() => number>;
let styles: MockInstance<typeof window.getComputedStyle>;

beforeEach(() => {
  measure = vi.spyOn(HTMLTextAreaElement.prototype, "scrollHeight", "get");
  styles = vi.spyOn(window, "getComputedStyle");
});

afterEach(() => {
  vi.restoreAllMocks();
});

function textarea(testid: string): HTMLTextAreaElement {
  return screen.getByTestId(testid) as HTMLTextAreaElement;
}

function asStyle(maxHeight: string): CSSStyleDeclaration {
  return { maxHeight } as CSSStyleDeclaration;
}

describe("Textarea autosize", () => {
  it("measures exactly once per value change, with no per-change getComputedStyle", async () => {
    measure.mockReturnValue(96);
    styles.mockReturnValue(asStyle("192px"));
    render(Textarea, { props: { autosize: true, value: "", "data-testid": "ta" } });
    await tick();
    // The cap is read once, on the first (mount-time) resize…
    expect(styles).toHaveBeenCalledTimes(1);
    measure.mockClear();
    styles.mockClear();

    await fireEvent.input(textarea("ta"), { target: { value: "hello" } });
    await tick();
    // …so a keystroke costs one measure and zero computed-style reads.
    expect(measure).toHaveBeenCalledTimes(1);
    expect(styles).not.toHaveBeenCalled();
    expect(textarea("ta").style.height).toBe("96px");

    // A second, DISTINCT value measures again — the dedup guard suppresses the
    // same-value overlap, never the next change (the guard's failure mode is
    // latching after the first resize, which this pins).
    await fireEvent.input(textarea("ta"), { target: { value: "hello world" } });
    await tick();
    expect(measure).toHaveBeenCalledTimes(2);
    expect(styles).not.toHaveBeenCalled();
  });

  it("forwards oninput to the consumer regardless of autosize", async () => {
    measure.mockReturnValue(96);
    styles.mockReturnValue(asStyle("192px"));
    const withAutosize = vi.fn();
    render(Textarea, {
      props: { autosize: true, value: "", oninput: withAutosize, "data-testid": "ta" },
    });
    await fireEvent.input(textarea("ta"), { target: { value: "a" } });
    expect(withAutosize).toHaveBeenCalledTimes(1);

    const withoutAutosize = vi.fn();
    render(Textarea, { props: { value: "", oninput: withoutAutosize, "data-testid": "tb" } });
    await fireEvent.input(textarea("tb"), { target: { value: "b" } });
    expect(withoutAutosize).toHaveBeenCalledTimes(1);
  });

  it("typing still resizes when the consumer's binding is not reactive", async () => {
    // With a non-reactive binding the value effect never re-runs, so the
    // input-path resize is the only live trigger — this test fails if that
    // path is removed (e.g. "simplified" to effect-only).
    measure.mockImplementation(function (this: HTMLTextAreaElement): number {
      return this.value.length > 0 ? 120 : 40;
    });
    styles.mockReturnValue(asStyle("192px"));
    render(TextareaNonReactiveHarness, { props: { args: { value: "" } } });
    await tick();
    expect(textarea("ta").style.height).toBe("40px");

    await fireEvent.input(textarea("ta"), { target: { value: "hello" } });
    expect(textarea("ta").style.height).toBe("120px");
  });

  it("resizes on a programmatic clear (the send path)", async () => {
    measure.mockImplementation(function (this: HTMLTextAreaElement): number {
      return this.value === "" ? 40 : 96;
    });
    styles.mockReturnValue(asStyle("192px"));
    const { component } = render(TextareaHarness, { props: { initial: "draft text" } });
    await tick();
    expect(textarea("ta").style.height).toBe("96px");

    (component as { setValue: (next: string) => void }).setValue("");
    await waitFor(() => expect(textarea("ta").style.height).toBe("40px"));
  });

  it("does not measure or set height when autosize is off", async () => {
    styles.mockReturnValue(asStyle("192px"));
    render(Textarea, { props: { value: "", "data-testid": "ta" } });
    await tick();
    measure.mockClear();

    await fireEvent.input(textarea("ta"), { target: { value: "hello" } });
    await tick();
    expect(measure).not.toHaveBeenCalled();
    expect(textarea("ta").style.height).toBe("");
  });

  it("caps two concurrently-mounted instances at their own max-heights", async () => {
    // Fails specifically on a module-level cap cache: the second instance would
    // inherit the first's cap instead of reading its own.
    measure.mockReturnValue(500);
    styles.mockImplementation(
      (el: Element): CSSStyleDeclaration =>
        asStyle((el as HTMLElement).getAttribute("data-cap") ?? ""),
    );
    render(Textarea, {
      props: { autosize: true, value: "x", "data-testid": "a", "data-cap": "160px" },
    });
    render(Textarea, {
      props: { autosize: true, value: "x", "data-testid": "b", "data-cap": "320px" },
    });
    await tick();
    expect(textarea("a").style.height).toBe("160px");
    expect(textarea("b").style.height).toBe("320px");

    // Subsequent changes use each instance's cached cap — no re-read, no cross-talk.
    styles.mockClear();
    await fireEvent.input(textarea("a"), { target: { value: "xx" } });
    await fireEvent.input(textarea("b"), { target: { value: "yy" } });
    await tick();
    expect(textarea("a").style.height).toBe("160px");
    expect(textarea("b").style.height).toBe("320px");
    expect(textarea("a").style.overflowY).toBe("auto");
    expect(styles).not.toHaveBeenCalled();
  });
});
