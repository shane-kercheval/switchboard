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
});
