import { describe, expect, it } from "vitest";
import { render } from "@testing-library/svelte";
import StatusDot from "./StatusDot.svelte";

describe("StatusDot", () => {
  it("maps the status to its token color", () => {
    const { container } = render(StatusDot, { props: { status: "processing" } });
    expect(container.querySelector("span")).toHaveClass("bg-status-processing");
  });

  it("uses the idle token for idle", () => {
    const { container } = render(StatusDot, { props: { status: "idle" } });
    expect(container.querySelector("span")).toHaveClass("bg-status-idle");
  });

  it("uses the green accent token for a successful state", () => {
    const { container } = render(StatusDot, { props: { status: "success" } });
    expect(container.querySelector("span")).toHaveClass("bg-accent");
  });

  it("can retain a labelled hover tooltip without joining the Tab order", () => {
    const { container } = render(StatusDot, {
      props: { status: "success", label: "connected", focusable: false },
    });
    const dot = container.querySelector("span");
    expect(dot).toHaveAttribute("aria-label", "connected");
    expect(dot).not.toHaveAttribute("tabindex");
  });
});
