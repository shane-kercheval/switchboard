import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import SegmentedSelect from "./SegmentedSelect.svelte";

const OPTIONS = [
  { label: "Opus", value: "opus" },
  { label: "Sonnet", value: "sonnet" },
];

describe("SegmentedSelect", () => {
  it("renders one radio segment per item with the selected value", () => {
    render(SegmentedSelect, {
      props: { options: OPTIONS, value: "opus", testid: "sel", ariaLabel: "Model" },
    });
    const group = screen.getByTestId("sel");
    expect(group).toHaveAttribute("role", "radiogroup");
    expect(group).toHaveAttribute("data-value", "opus");
    expect(screen.getByTestId("sel-option-opus")).toHaveAttribute("aria-checked", "true");
    expect(screen.getByTestId("sel-option-sonnet")).toHaveAttribute("aria-checked", "false");
  });

  it("splits long option sets into two balanced rows", () => {
    render(SegmentedSelect, {
      props: {
        options: Array.from({ length: 8 }, (_, i) => ({
          label: `Option ${i + 1}`,
          value: `option-${i + 1}`,
        })),
        value: "option-1",
        testid: "sel",
        ariaLabel: "Model",
      },
    });
    expect(screen.getByTestId("sel")).toHaveStyle({
      gridTemplateColumns: "repeat(4, minmax(0, 1fr))",
    });
  });

  it("reflects a click change to the selected value", async () => {
    render(SegmentedSelect, {
      props: { options: OPTIONS, value: "opus", testid: "sel", ariaLabel: "Model" },
    });
    await fireEvent.click(screen.getByTestId("sel-option-sonnet"));
    expect(screen.getByTestId("sel")).toHaveAttribute("data-value", "sonnet");
    expect(screen.getByTestId("sel-option-sonnet")).toHaveAttribute("aria-checked", "true");
  });

  it("honors disabled", async () => {
    render(SegmentedSelect, {
      props: { options: OPTIONS, value: "opus", disabled: true, testid: "sel", ariaLabel: "Model" },
    });
    expect(screen.getByTestId("sel")).toHaveAttribute("aria-disabled", "true");
    await fireEvent.click(screen.getByTestId("sel-option-sonnet"));
    expect(screen.getByTestId("sel")).toHaveAttribute("data-value", "opus");
  });
});
