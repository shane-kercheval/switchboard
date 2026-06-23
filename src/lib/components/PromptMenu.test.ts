import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import PromptMenu from "./PromptMenu.svelte";
import type { Prompt } from "$lib/types";

const PROMPTS: Prompt[] = [
  {
    provider: "local",
    name: "code-review",
    title: "Code Review",
    description: "Review a diff",
    arguments: [],
    tags: [],
  },
  {
    provider: "tiddly",
    name: "summarize",
    title: "Summarize Text",
    description: "Summarize text",
    arguments: [],
    tags: [],
  },
  // No title → falls back to the slug.
  {
    provider: "tiddly",
    name: "translate",
    title: null,
    description: null,
    arguments: [],
    tags: [],
  },
];

const BUILTIN: Prompt = {
  provider: "builtin",
  name: "code-review",
  title: null,
  description: "Review the current uncommitted changes",
  arguments: [],
  tags: [],
};

function setup(prompts: Prompt[] = PROMPTS, loading = false) {
  const onpick = vi.fn();
  const oncopy = vi.fn();
  const onclose = vi.fn();
  render(PromptMenu, { props: { prompts, loading, onpick, oncopy, onclose } });
  return { onpick, oncopy, onclose };
}

describe("PromptMenu", () => {
  it("lists every cached prompt on open", () => {
    setup();
    expect(screen.getByTestId("prompt-option-local:code-review")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-option-tiddly:summarize")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-option-tiddly:translate")).toBeInTheDocument();
  });

  it("shows the friendly title when present, falling back to the slug", () => {
    setup();
    // Title shown instead of the `code-review` slug.
    expect(screen.getByTestId("prompt-option-local:code-review")).toHaveTextContent("Code Review");
    // No title → the slug is shown.
    expect(screen.getByTestId("prompt-option-tiddly:translate")).toHaveTextContent("translate");
  });

  it("matches the friendly title in search", async () => {
    setup();
    await fireEvent.input(screen.getByTestId("prompt-menu-search"), {
      target: { value: "Code Review" },
    });
    expect(screen.getByTestId("prompt-option-local:code-review")).toBeInTheDocument();
    expect(screen.queryByTestId("prompt-option-tiddly:translate")).toBeNull();
  });

  it("filters by name/provider via the search field", async () => {
    setup();
    await fireEvent.input(screen.getByTestId("prompt-menu-search"), {
      target: { value: "summ" },
    });
    expect(screen.getByTestId("prompt-option-tiddly:summarize")).toBeInTheDocument();
    expect(screen.queryByTestId("prompt-option-local:code-review")).toBeNull();
  });

  it("shows an empty state when nothing matches", async () => {
    setup();
    await fireEvent.input(screen.getByTestId("prompt-menu-search"), {
      target: { value: "zzz" },
    });
    expect(screen.getByTestId("prompt-menu-empty")).toHaveTextContent("No matching prompts");
  });

  it("shows a distinct empty state when there are no prompts at all", () => {
    setup([]);
    expect(screen.getByTestId("prompt-menu-empty")).toHaveTextContent("No prompts available");
  });

  it("shows a loading row (not the empty state) before the cache is read", () => {
    setup([], true);
    expect(screen.getByTestId("prompt-menu-loading")).toBeInTheDocument();
    expect(screen.queryByTestId("prompt-menu-empty")).toBeNull();
  });

  it("picks the highlighted prompt with arrow keys + Enter", async () => {
    const { onpick } = setup();
    const search = screen.getByTestId("prompt-menu-search");
    await fireEvent.keyDown(search, { key: "ArrowDown" }); // highlight 0 -> 1
    await fireEvent.keyDown(search, { key: "Enter" });
    expect(onpick).toHaveBeenCalledTimes(1);
    expect(onpick.mock.calls[0]?.[0]).toMatchObject({ provider: "tiddly", name: "summarize" });
  });

  it("lets the mouse claim the highlight, then hands back to the keyboard", async () => {
    setup();
    const first = screen.getByTestId("prompt-option-local:code-review");
    const third = screen.getByTestId("prompt-option-tiddly:translate");
    expect(first).toHaveAttribute("aria-selected", "true"); // keyboard default

    // Hovering claims the highlight; the previously-highlighted row drops it.
    await fireEvent.mouseMove(third);
    expect(third).toHaveAttribute("aria-selected", "true");
    expect(first).toHaveAttribute("aria-selected", "false");

    // The keyboard resumes from the mouse position (3 items: index 2 wraps to 0).
    await fireEvent.keyDown(screen.getByTestId("prompt-menu-search"), { key: "ArrowDown" });
    expect(first).toHaveAttribute("aria-selected", "true");
    expect(third).toHaveAttribute("aria-selected", "false");
  });

  it("picks on click", async () => {
    const { onpick } = setup();
    await fireEvent.click(screen.getByTestId("prompt-option-tiddly:translate"));
    expect(onpick.mock.calls[0]?.[0]).toMatchObject({ provider: "tiddly", name: "translate" });
  });

  it("closes on Escape", async () => {
    const { onclose } = setup();
    await fireEvent.keyDown(screen.getByTestId("prompt-menu-search"), { key: "Escape" });
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it("autofocuses the search field on open", async () => {
    setup();
    await waitFor(() => expect(screen.getByTestId("prompt-menu-search")).toHaveFocus());
  });

  it("tags a built-in read-only and offers a copy action; a user prompt gets neither", () => {
    setup([BUILTIN, PROMPTS[0]!]);
    // The built-in carries the read-only tag and a copy button.
    expect(screen.getByTestId("prompt-builtin-tag-builtin:code-review")).toBeInTheDocument();
    // The copy action is icon-only — its accessible name is the only text
    // affordance, so it carries the discoverability/a11y contract.
    expect(screen.getByTestId("prompt-copy-builtin:code-review")).toHaveAccessibleName(
      "Copy to my prompts",
    );
    // The user's own local prompt does not.
    expect(screen.queryByTestId("prompt-builtin-tag-local:code-review")).toBeNull();
    expect(screen.queryByTestId("prompt-copy-local:code-review")).toBeNull();
  });

  it("invokes oncopy (not onpick) when the copy action is clicked", async () => {
    const { oncopy, onpick } = setup([BUILTIN]);
    await fireEvent.click(screen.getByTestId("prompt-copy-builtin:code-review"));
    expect(oncopy).toHaveBeenCalledTimes(1);
    expect(oncopy.mock.calls[0]?.[0]).toMatchObject({ provider: "builtin", name: "code-review" });
    expect(onpick).not.toHaveBeenCalled();
  });

  it("still picks the built-in row itself", async () => {
    const { onpick } = setup([BUILTIN]);
    await fireEvent.click(screen.getByTestId("prompt-option-builtin:code-review"));
    expect(onpick.mock.calls[0]?.[0]).toMatchObject({ provider: "builtin", name: "code-review" });
  });
});
