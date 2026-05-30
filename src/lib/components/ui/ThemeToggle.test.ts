import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import { tick } from "svelte";
import ThemeToggle from "./ThemeToggle.svelte";
import { theme } from "$lib/theme.svelte";

beforeEach(() => {
  theme.set("system");
});

afterEach(() => {
  document.documentElement.classList.remove("dark");
});

describe("ThemeToggle", () => {
  it("cycles system → light → dark → system on click", async () => {
    render(ThemeToggle);
    const button = screen.getByTestId("theme-toggle");
    expect(button).toHaveAttribute("title", "Theme: system");

    await fireEvent.click(button);
    await tick();
    expect(button).toHaveAttribute("title", "Theme: light");

    await fireEvent.click(button);
    await tick();
    expect(button).toHaveAttribute("title", "Theme: dark");

    await fireEvent.click(button);
    await tick();
    expect(button).toHaveAttribute("title", "Theme: system");
  });
});
