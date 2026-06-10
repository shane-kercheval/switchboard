import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import EmptyState from "./EmptyState.svelte";

describe("EmptyState", () => {
  it("renders the title and an optional description under a testid", () => {
    render(EmptyState, {
      props: { title: "Nothing here", description: "add something", testid: "empty" },
    });
    const root = screen.getByTestId("empty");
    expect(root).toHaveTextContent("Nothing here");
    expect(root).toHaveTextContent("add something");
  });

  it("colors the title with the failed token in the error tone", () => {
    render(EmptyState, { props: { title: "Broke", tone: "error" } });
    expect(screen.getByText("Broke")).toHaveClass("text-status-failed");
  });

  it("renders an action snippet", () => {
    const action = createRawSnippet(() => ({
      render: () => `<button data-testid="cta">Retry</button>`,
    }));
    render(EmptyState, { props: { title: "Failed", action } });
    expect(screen.getByTestId("cta")).toBeInTheDocument();
  });

  it("renders a large spinner above the title when requested", () => {
    render(EmptyState, { props: { title: "Loading…", spinner: true, testid: "empty" } });
    const ring = screen.getByTestId("empty").querySelector(".animate-spin");
    expect(ring).not.toBeNull();
    expect(ring).toHaveClass("h-8", "w-8");
  });

  it("renders no spinner by default", () => {
    render(EmptyState, { props: { title: "Nothing here", testid: "empty" } });
    expect(screen.getByTestId("empty").querySelector(".animate-spin")).toBeNull();
  });
});
