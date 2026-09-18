import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/svelte";
import Meter from "./Meter.svelte";

describe("Meter", () => {
  it("renders the label, the detail, and the used percentage", () => {
    render(Meter, {
      props: {
        label: "Context after last turn",
        value: 0.6,
        detail: "120k / 200k",
        separateDetail: true,
        alignPercentage: true,
        testid: "m",
      },
    });

    const meter = screen.getByTestId("m");
    expect(meter).toHaveTextContent("Context after last turn");
    expect(meter).toHaveTextContent("120k / 200k · 60%");
  });

  it("omits the detail when the call site has none", () => {
    render(Meter, { props: { label: "5-hour limit", value: 0.25, testid: "m" } });

    const meter = screen.getByTestId("m");
    expect(meter).toHaveTextContent("5-hour limit");
    expect(meter).toHaveTextContent("25%");
  });

  it("fills the track in proportion to the value", () => {
    render(Meter, { props: { label: "Weekly", value: 0.125, testid: "m" } });

    expect(screen.getByTestId("m-fill")).toHaveStyle({ width: "12.5%" });
  });

  it("clamps the fill at both ends while still reporting the value", () => {
    // A source over 100% is saying something true and the text must carry it;
    // the bar physically cannot, so only the fill clamps.
    const { unmount } = render(Meter, { props: { label: "Weekly", value: 1.4, testid: "over" } });
    expect(screen.getByTestId("over-fill")).toHaveStyle({ width: "100.0%" });
    expect(screen.getByTestId("over")).toHaveTextContent("140%");
    unmount();

    render(Meter, { props: { label: "Weekly", value: -0.2, testid: "under" } });
    expect(screen.getByTestId("under-fill")).toHaveStyle({ width: "0.0%" });
  });

  it("switches the fill token on the warning tone", () => {
    const { unmount } = render(Meter, {
      props: { label: "5-hour limit", value: 0.8, testid: "calm" },
    });
    expect(screen.getByTestId("calm-fill")).toHaveClass("bg-fg");
    expect(screen.getByTestId("calm-fill")).not.toHaveClass("bg-warning");
    unmount();

    render(Meter, {
      props: { label: "5-hour limit", value: 0.8, tone: "warning", testid: "loud" },
    });
    expect(screen.getByTestId("loud-fill")).toHaveClass("bg-warning");
    expect(screen.getByTestId("loud-fill")).not.toHaveClass("bg-fg");
  });

  it("renders nothing at all for a non-finite value", () => {
    // Left to the browser, the invalid width declaration is dropped and the
    // fill falls back to auto — a completely full bar reading "NaN%", which
    // says the quota is spent when it is actually unknown.
    render(Meter, { props: { label: "5-hour limit", value: NaN, testid: "m" } });

    expect(screen.queryByTestId("m")).toBeNull();
    expect(screen.queryByTestId("m-fill")).toBeNull();
    expect(screen.queryByText(/NaN/)).toBeNull();
  });

  it("names the track fill off the meter's own test id", () => {
    render(Meter, { props: { label: "Weekly", value: 0.5 } });

    // No test id in, none out — the fill is not separately addressable, which
    // keeps a call site from depending on an id it never declared.
    expect(document.querySelector("[data-testid$='-fill']")).toBeNull();
  });
});
