import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import Banner from "./Banner.svelte";

describe("Banner", () => {
  it("renders the message and no dismiss button when onDismiss is omitted", () => {
    render(Banner, { props: { message: "Claude Code not found", testid: "banner-x" } });
    expect(screen.getByTestId("banner-x")).toHaveTextContent("Claude Code not found");
    expect(screen.queryByTestId("banner-x-dismiss")).not.toBeInTheDocument();
  });

  it("renders a dismiss button that fires onDismiss when provided", async () => {
    const onDismiss = vi.fn();
    render(Banner, {
      props: { message: "Couldn't create the Codex agent", testid: "banner-y", onDismiss },
    });
    await fireEvent.click(screen.getByTestId("banner-y-dismiss"));
    expect(onDismiss).toHaveBeenCalledOnce();
  });
});
