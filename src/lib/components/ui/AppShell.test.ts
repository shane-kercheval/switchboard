import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import AppShell from "./AppShell.svelte";

function region(text: string) {
  return createRawSnippet(() => ({ render: () => `<div>${text}</div>` }));
}

describe("AppShell", () => {
  it("renders left, center (under the center testid), and right panes", () => {
    render(AppShell, {
      props: {
        left: region("L"),
        center: region("C"),
        right: region("R"),
        centerTestid: "center-pane",
      },
    });
    expect(screen.getByText("L")).toBeInTheDocument();
    expect(screen.getByTestId("center-pane")).toHaveTextContent("C");
    expect(screen.getByText("R")).toBeInTheDocument();
  });

  it("omits the right pane when no right snippet is given", () => {
    render(AppShell, { props: { left: region("L"), center: region("C") } });
    expect(screen.getByText("L")).toBeInTheDocument();
    expect(screen.queryByText("R")).not.toBeInTheDocument();
  });
});
