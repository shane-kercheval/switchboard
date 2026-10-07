import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import AppShell from "./AppShell.svelte";

function region(text: string) {
  return createRawSnippet(() => ({ render: () => `<div>${text}</div>` }));
}

describe("AppShell", () => {
  it("renders left and center panes", () => {
    render(AppShell, {
      props: {
        left: region("L"),
        center: region("C"),
        centerTestid: "center-pane",
      },
    });
    expect(screen.getByText("L")).toBeInTheDocument();
    expect(screen.getByTestId("center-pane")).toHaveTextContent("C");
  });
});
