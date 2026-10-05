import { describe, expect, it, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, fireEvent, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import Markdown from "$lib/components/ui/Markdown.svelte";
import MarkdownRowHarness from "$lib/components/ui/_MarkdownRowHarness.svelte";
import { renderMarkdown } from "$lib/markdown";

vi.mock("$lib/markdown", async (importOriginal) => {
  const actual = await importOriginal<typeof import("$lib/markdown")>();
  return { ...actual, renderMarkdown: vi.fn(actual.renderMarkdown) };
});
const renderMarkdownMock = vi.mocked(renderMarkdown);

const copyTextMock = vi.fn<(t: string) => Promise<void>>();
vi.mock("$lib/native", () => ({
  copyText: (t: string) => copyTextMock(t),
}));

const openExternalUrlMock = vi.fn<(u: string) => Promise<void>>();
vi.mock("$lib/api", () => ({
  openExternalUrl: (u: string) => openExternalUrlMock(u),
}));

beforeEach(() => {
  renderMarkdownMock.mockClear();
  copyTextMock.mockReset();
  copyTextMock.mockResolvedValue(undefined);
  openExternalUrlMock.mockReset();
  openExternalUrlMock.mockResolvedValue(undefined);
});

describe("Markdown copy button", () => {
  it("copies the exact source (not highlighted markup) including tricky characters", async () => {
    const source = `{"a": "<tag> & 'quote'", "b": 1}`;
    const { container } = render(Markdown, { text: "```json\n" + source + "\n```" });

    const code = container.querySelector("code");
    const button = container.querySelector(".md-code-copy");
    if (!code || !button) throw new Error("expected a code block with a copy button");

    await fireEvent.click(button);

    expect(copyTextMock).toHaveBeenCalledTimes(1);
    // The contract: copy from the rendered <code>'s textContent.
    expect(copyTextMock).toHaveBeenCalledWith(code.textContent);
    const copied = copyTextMock.mock.calls[0]![0];
    // Literal special characters survive — not the escaped/markup form.
    expect(copied).toContain("<tag> & 'quote'");
    expect(copied).not.toContain("&lt;");
    expect(copied).not.toContain("<span");
    // Confirmation (icon swap via data-copied) appears only after the clipboard
    // write resolves.
    await waitFor(() => expect(button).toHaveAttribute("data-copied", "true"));
  });

  it("does not show 'Copied' when the clipboard write fails", async () => {
    copyTextMock.mockRejectedValueOnce(new Error("clipboard unavailable"));
    const { container } = render(Markdown, { text: "```\nplain\n```" });
    const button = container.querySelector(".md-code-copy");
    if (!button) throw new Error("expected a copy button");

    await fireEvent.click(button);
    await Promise.resolve();
    await Promise.resolve();

    expect(copyTextMock).toHaveBeenCalledTimes(1);
    expect(button).not.toHaveAttribute("data-copied");
  });

  it("resets each block's button independently (no shared timer)", async () => {
    vi.useFakeTimers();
    try {
      const { container } = render(Markdown, {
        text: "```\nfirst\n```\n\n```\nsecond\n```",
      });
      const buttons = container.querySelectorAll(".md-code-copy");
      expect(buttons.length).toBe(2);
      const [a, b] = [buttons[0]!, buttons[1]!];

      await fireEvent.click(a);
      await vi.advanceTimersByTimeAsync(0);
      expect(a).toHaveAttribute("data-copied", "true");

      // Click B partway through A's reset window.
      await vi.advanceTimersByTimeAsync(400);
      await fireEvent.click(b);
      await vi.advanceTimersByTimeAsync(0);
      expect(b).toHaveAttribute("data-copied", "true");

      // A's own timer must still fire — it isn't cancelled by B's click.
      await vi.advanceTimersByTimeAsync(700);
      expect(a).not.toHaveAttribute("data-copied");
      expect(b).toHaveAttribute("data-copied", "true");

      await vi.advanceTimersByTimeAsync(400);
      expect(b).not.toHaveAttribute("data-copied");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("Markdown links", () => {
  it("intercepts link clicks, opens externally, and prevents webview navigation", async () => {
    const { container } = render(Markdown, { text: "[site](https://example.com/x?y=1)" });
    const link = container.querySelector("a");
    if (!link) throw new Error("expected a rendered link");

    const notCancelled = await fireEvent.click(link);

    expect(openExternalUrlMock).toHaveBeenCalledWith("https://example.com/x?y=1");
    // fireEvent returns false when the default was prevented (no navigation).
    expect(notCancelled).toBe(false);
  });

  it("does not navigate or throw when the backend rejects the URL", async () => {
    // Non-web schemes (file:, javascript:, …) are stripped at the sanitization
    // layer, so the backend validator is defense-in-depth. Still, if a backend
    // open ever rejects, the click must stay intercepted (no webview navigation)
    // and the rejection must be swallowed rather than thrown.
    openExternalUrlMock.mockRejectedValueOnce(new Error("refusing to open non-web URL"));
    const { container } = render(Markdown, { text: "[link](https://example.com)" });
    const link = container.querySelector("a");
    if (!link) throw new Error("expected a rendered link");

    const notCancelled = await fireEvent.click(link);
    await Promise.resolve();

    expect(openExternalUrlMock).toHaveBeenCalledWith("https://example.com");
    expect(notCancelled).toBe(false);
  });
});

describe("Markdown parse memoization", () => {
  it("does not re-parse when the parent hands it a new row with the same text", async () => {
    // The transcript rebuilds its row objects on every update to the pane, so a
    // user message would otherwise be re-parsed per streamed chunk of some
    // other agent's reply. Equal text must mean no parse.
    const { rerender } = render(MarkdownRowHarness, { props: { row: { text: "**same**" } } });
    expect(renderMarkdownMock).toHaveBeenCalledTimes(1);

    await rerender({ row: { text: "**same**" } });
    await tick();
    expect(renderMarkdownMock).toHaveBeenCalledTimes(1);

    await rerender({ row: { text: "**changed**" } });
    await tick();
    expect(renderMarkdownMock).toHaveBeenCalledTimes(2);
  });
});

describe("Markdown long text", () => {
  it("formats the entire text beyond 50,000 characters", (): void => {
    const text = "**bold**\n\n" + "plain text ".repeat(6_000) + "\n\n**tail**";
    const { container } = render(Markdown, { text });

    expect(renderMarkdownMock).toHaveBeenCalledWith(text);
    expect(Array.from(container.querySelectorAll("strong"), (node) => node.textContent)).toEqual([
      "bold",
      "tail",
    ]);
  });

  it("keeps formatting as text grows past 50,000 characters without re-parsing unchanged text", async (): Promise<void> => {
    const text = "**bold**\n\n" + "prose ".repeat(8_000);
    const { container, rerender } = render(MarkdownRowHarness, { row: { text } });
    expect(container.querySelector("strong")?.textContent).toBe("bold");

    const grown = text + "\n\n" + "more prose ".repeat(1_000) + "\n\n**tail**";
    await rerender({ row: { text: grown } });
    await tick();
    expect(Array.from(container.querySelectorAll("strong"), (node) => node.textContent)).toEqual([
      "bold",
      "tail",
    ]);
    expect(renderMarkdownMock).toHaveBeenCalledTimes(2);

    await rerender({ row: { text: grown } });
    await tick();
    expect(renderMarkdownMock).toHaveBeenCalledTimes(2);
  });

  it.each([49_999, 50_000, 50_001])("formats text of %i characters", (length: number): void => {
    const text = "**bold**" + "a".repeat(length - 8);
    const { container } = render(Markdown, { text });
    expect(renderMarkdownMock).toHaveBeenCalledTimes(1);
    expect(container.querySelector("strong")?.textContent).toBe("bold");
    expect(container.textContent).toBe("bold" + "a".repeat(length - 8) + "\n");
  });

  it("sanitizes HTML in long text", (): void => {
    const text = "a".repeat(60_000) + '\n\n<img src="x" onerror="alert(1)">\n\n**tail**';
    const { container } = render(Markdown, { text });
    expect(container.querySelector("img")).not.toBeNull();
    expect(container.querySelector("[onerror]")).toBeNull();
    expect(container.querySelector("strong")?.textContent).toBe("tail");
  });
});
