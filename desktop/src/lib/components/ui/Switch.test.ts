import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import Switch from "./Switch.svelte";

// Behavior and accessible semantics only — the track/knob classes are
// presentation, and pinning them here would turn a restyle into a test failure
// while proving nothing about the control working.

describe("Switch", () => {
  it("exposes switch semantics with its accessible name and test id", () => {
    render(Switch, {
      props: {
        checked: false,
        ariaLabel: "Notify me when agents finish",
        testid: "notify-toggle",
        onclick: vi.fn(),
      },
    });
    const toggle = screen.getByRole("switch", { name: "Notify me when agents finish" });
    expect(toggle).toHaveAttribute("data-testid", "notify-toggle");
    // Not a submit button — every call site lives inside other forms/dialogs.
    expect(toggle).toHaveAttribute("type", "button");
  });

  it("reports its state through aria-checked in both directions", () => {
    const { unmount } = render(Switch, {
      props: { checked: false, ariaLabel: "Toggle", onclick: vi.fn() },
    });
    expect(screen.getByRole("switch")).toHaveAttribute("aria-checked", "false");
    unmount();

    render(Switch, { props: { checked: true, ariaLabel: "Toggle", onclick: vi.fn() } });
    expect(screen.getByRole("switch")).toHaveAttribute("aria-checked", "true");
  });

  it("calls onclick once per click, leaving the state to the parent", async () => {
    // Controlled by design: the primitive never flips `checked` itself, so a
    // parent whose write fails cannot end up disagreeing with what is rendered.
    const onclick = vi.fn();
    render(Switch, { props: { checked: false, ariaLabel: "Toggle", onclick } });

    await fireEvent.click(screen.getByRole("switch"));
    expect(onclick).toHaveBeenCalledOnce();
    expect(screen.getByRole("switch")).toHaveAttribute("aria-checked", "false");
  });

  it("does not fire while disabled", async () => {
    const onclick = vi.fn();
    render(Switch, { props: { checked: false, disabled: true, ariaLabel: "Toggle", onclick } });

    const toggle = screen.getByRole("switch");
    expect(toggle).toBeDisabled();
    await fireEvent.click(toggle);
    expect(onclick).not.toHaveBeenCalled();
  });
});
