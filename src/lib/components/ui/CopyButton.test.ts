import { describe, expect, it, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, fireEvent, waitFor } from "@testing-library/svelte";
import CopyButton from "$lib/components/ui/CopyButton.svelte";

const copyTextMock = vi.fn<(t: string) => Promise<void>>();
vi.mock("$lib/native", () => ({
  copyText: (t: string) => copyTextMock(t),
}));

beforeEach(() => {
  copyTextMock.mockReset();
  copyTextMock.mockResolvedValue(undefined);
});

describe("CopyButton", () => {
  it("copies its text and confirms after the write resolves", async () => {
    const { getByTestId } = render(CopyButton, { text: "hello world", testid: "t" });
    const button = getByTestId("t");

    await fireEvent.click(button);

    expect(copyTextMock).toHaveBeenCalledWith("hello world");
    await waitFor(() => expect(button).toHaveAttribute("data-copied", "true"));
  });

  it("does not confirm when the clipboard write fails", async () => {
    copyTextMock.mockRejectedValueOnce(new Error("nope"));
    const { getByTestId } = render(CopyButton, { text: "x", testid: "t" });
    const button = getByTestId("t");

    await fireEvent.click(button);
    await Promise.resolve();
    await Promise.resolve();

    expect(button).not.toHaveAttribute("data-copied");
  });
});
