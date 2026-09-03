import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/svelte";
import SidebarToggleButton from "./SidebarToggleButton.svelte";

describe("SidebarToggleButton", () => {
  it.each(["left", "right"] as const)("fills the %s panel only while expanded", async (side) => {
    const view = render(SidebarToggleButton, {
      props: {
        side,
        expanded: false,
        label: `Show ${side} sidebar`,
        testid: `${side}-toggle`,
        onclick: vi.fn(),
      },
    });

    expect(screen.getByTestId(`${side}-toggle`)).toHaveAttribute("aria-expanded", "false");
    expect(view.container.querySelector("[data-sidebar-glyph-fill]")).toBeNull();

    await view.rerender({
      side,
      expanded: true,
      label: `Hide ${side} sidebar`,
      testid: `${side}-toggle`,
      onclick: vi.fn(),
    });

    expect(screen.getByTestId(`${side}-toggle`)).toHaveAttribute("aria-expanded", "true");
    expect(view.container.querySelector(`[data-sidebar-glyph-fill="${side}"]`)).toBeInTheDocument();
  });
});
