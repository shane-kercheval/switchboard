import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/svelte";
import ExpandCollapseIcon from "./ExpandCollapseIcon.svelte";

describe("ExpandCollapseIcon", () => {
  it("shows the pending action and preserves its visual props", async () => {
    const view = render(ExpandCollapseIcon, {
      props: { expanded: true, size: 18, strokeWidth: 1.8, class: "custom-icon" },
    });

    const collapse = view.container.querySelector("svg");
    expect(collapse).toHaveAttribute("data-icon-action", "collapse");
    expect(collapse).toHaveAttribute("width", "18");
    expect(collapse).toHaveAttribute("height", "18");
    expect(collapse).toHaveAttribute("stroke-width", "1.8");
    expect(collapse).toHaveClass("custom-icon");

    await view.rerender({ expanded: false, size: 18, strokeWidth: 1.8, class: "custom-icon" });
    expect(view.container.querySelector("svg")).toHaveAttribute("data-icon-action", "expand");
  });
});
