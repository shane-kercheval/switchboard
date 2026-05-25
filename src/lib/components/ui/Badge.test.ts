import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import Badge from "./Badge.svelte";

function label(text: string) {
  return createRawSnippet(() => ({ render: () => `<span>${text}</span>` }));
}

describe("Badge", () => {
  it("renders the harness variant with the harness token classes", () => {
    render(Badge, {
      props: { variant: "harness", harness: "claude_code", children: label("Claude") },
    });
    const badge = screen.getByText("Claude").closest("span[class*='harness']");
    expect(badge).toHaveClass("bg-harness-claude-soft");
    expect(badge).toHaveClass("text-harness-claude");
  });

  it("renders the status variant with the status token classes", () => {
    const { container } = render(Badge, {
      props: { variant: "status", status: "processing", children: label("processing") },
    });
    const badge = container.querySelector("span");
    expect(badge).toHaveClass("bg-status-processing-soft");
    expect(badge).toHaveClass("text-status-processing");
  });

  it("defaults to the neutral variant", () => {
    const { container } = render(Badge, { props: { children: label("unavailable") } });
    const badge = container.querySelector("span");
    expect(badge).toHaveClass("bg-panel");
    expect(badge).toHaveClass("text-muted");
  });
});
