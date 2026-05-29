import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import Button from "./Button.svelte";

function label(text: string) {
  return createRawSnippet(() => ({ render: () => `<span>${text}</span>` }));
}

describe("Button", () => {
  it("renders children and defaults to the primary (accent) variant", () => {
    render(Button, { props: { children: label("Go") } });
    const btn = screen.getByRole("button", { name: "Go" });
    expect(btn).toHaveClass("bg-accent");
    expect(btn).toHaveClass("text-accent-fg");
    // Pill shape (matches the app's circular icon language).
    expect(btn).toHaveClass("rounded-full");
  });

  it("applies the secondary variant as an outline", () => {
    render(Button, { props: { variant: "secondary", children: label("Cancel") } });
    const btn = screen.getByRole("button");
    expect(btn).toHaveClass("border");
    expect(btn).toHaveClass("bg-transparent");
  });

  it("applies the destructive (danger) variant", () => {
    render(Button, { props: { variant: "danger", children: label("Delete") } });
    const btn = screen.getByRole("button");
    expect(btn).toHaveClass("bg-destructive");
    expect(btn).toHaveClass("text-destructive-fg");
  });

  it("forwards the disabled attribute", () => {
    render(Button, { props: { disabled: true, children: label("X") } });
    expect(screen.getByRole("button")).toBeDisabled();
  });

  it("fires onclick", async () => {
    const onclick = vi.fn();
    render(Button, { props: { onclick, children: label("Hit") } });
    await fireEvent.click(screen.getByRole("button"));
    expect(onclick).toHaveBeenCalledTimes(1);
  });
});
