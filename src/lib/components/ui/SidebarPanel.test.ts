import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import SidebarPanel from "./SidebarPanel.svelte";

const body = createRawSnippet(() => ({ render: () => `<div>contents</div>` }));

describe("SidebarPanel", () => {
  it("renders children, the pixel width, and a left-edge border by default", () => {
    render(SidebarPanel, { props: { width: 288, testid: "panel", children: body } });
    const panel = screen.getByTestId("panel");
    expect(panel).toHaveTextContent("contents");
    expect(panel).toHaveStyle({ width: "288px" });
    expect(panel).toHaveClass("border-r");
  });

  it("puts the border on the right edge when side=right", () => {
    render(SidebarPanel, {
      props: { side: "right", width: 240, testid: "panel", children: body },
    });
    expect(screen.getByTestId("panel")).toHaveClass("border-l");
  });
});
