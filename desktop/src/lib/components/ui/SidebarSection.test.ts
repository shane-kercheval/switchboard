import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import SidebarSection from "./SidebarSection.svelte";

const body = createRawSnippet(() => ({ render: () => `<div>rows</div>` }));

describe("SidebarSection", () => {
  it("renders the title and body", () => {
    render(SidebarSection, { props: { title: "Projects", children: body } });
    expect(screen.getByText("Projects")).toBeInTheDocument();
    expect(screen.getByText("rows")).toBeInTheDocument();
  });

  it("renders an action snippet in the header", () => {
    const action = createRawSnippet(() => ({
      render: () => `<button data-testid="add">+</button>`,
    }));
    render(SidebarSection, { props: { title: "Agents", action, children: body } });
    expect(screen.getByTestId("add")).toBeInTheDocument();
  });
});
