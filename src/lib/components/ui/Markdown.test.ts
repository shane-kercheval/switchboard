import { describe, expect, it, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, fireEvent, waitFor } from "@testing-library/svelte";
import Markdown from "$lib/components/ui/Markdown.svelte";

const copyTextMock = vi.fn<(t: string) => Promise<void>>();
vi.mock("$lib/native", () => ({
  copyText: (t: string) => copyTextMock(t),
}));

const openExternalUrlMock = vi.fn<(u: string) => Promise<void>>();
vi.mock("$lib/api", () => ({
  openExternalUrl: (u: string) => openExternalUrlMock(u),
}));

beforeEach(() => {
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
