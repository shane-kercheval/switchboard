import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import Badge from "./Badge.svelte";

function label(text: string) {
  return createRawSnippet(() => ({ render: () => `<span>${text}</span>` }));
}

describe("Badge", () => {
  it("renders a neutral chip with the panel/muted token classes", () => {
    const { container } = render(Badge, { props: { children: label("unavailable") } });
    const badge = container.querySelector("span");
    expect(badge).toHaveClass("bg-panel");
    expect(badge).toHaveClass("text-muted");
  });

  it("merges a caller-supplied class and testid", () => {
    render(Badge, { props: { class: "ml-2", testid: "kind-badge", children: label("?") } });
    const badge = screen.getByTestId("kind-badge");
    expect(badge).toHaveClass("ml-2");
  });
});
