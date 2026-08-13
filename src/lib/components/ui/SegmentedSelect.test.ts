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

  it("keeps every option on a single row (one column each)", () => {
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
      gridTemplateColumns: "repeat(8, minmax(0, 1fr))",
    });
  });

  it("shrinks segment text past five options so they still fit one row", () => {
    const five = Array.from({ length: 5 }, (_, i) => ({
      label: `Option ${i + 1}`,
      value: `o${i + 1}`,
    }));
    const { rerender } = render(SegmentedSelect, {
      props: { options: five, value: "o1", testid: "sel", ariaLabel: "Model" },
    });
    // Five or fewer keeps the default segment typography.
    expect(screen.getByTestId("sel-option-o1")).not.toHaveClass("text-[11px]");

    rerender({
      options: [...five, { label: "Option 6", value: "o6" }],
      value: "o1",
      testid: "sel",
      ariaLabel: "Model",
    });
    // Six or more steps down to the compact size.
    expect(screen.getByTestId("sel-option-o1")).toHaveClass("text-[11px]");
  });

  it("reflects a click change to the selected value", async () => {
    render(SegmentedSelect, {
      props: { options: OPTIONS, value: "opus", testid: "sel", ariaLabel: "Model" },
    });
    await fireEvent.click(screen.getByTestId("sel-option-sonnet"));
    expect(screen.getByTestId("sel")).toHaveAttribute("data-value", "sonnet");
    expect(screen.getByTestId("sel-option-sonnet")).toHaveAttribute("aria-checked", "true");
  });

  it("shows hover on only the current option and clears it on exit", async () => {
    render(SegmentedSelect, {
      props: { options: OPTIONS, value: "", testid: "sel", ariaLabel: "Model" },
    });
    const group = screen.getByTestId("sel");
    const opus = screen.getByTestId("sel-option-opus");
    const sonnet = screen.getByTestId("sel-option-sonnet");

    await fireEvent.pointerEnter(opus);
    expect(opus).toHaveClass("bg-control-hover");
    expect(sonnet).not.toHaveClass("bg-control-hover");

    await fireEvent.pointerEnter(sonnet);
    expect(opus).not.toHaveClass("bg-control-hover");
    expect(sonnet).toHaveClass("bg-control-hover");

    await fireEvent.pointerLeave(group);
    expect(opus).not.toHaveClass("bg-control-hover");
    expect(sonnet).not.toHaveClass("bg-control-hover");
  });

  it("does not add native tooltips to visible option labels", () => {
    render(SegmentedSelect, {
      props: { options: OPTIONS, value: "opus", testid: "sel", ariaLabel: "Model" },
    });
    expect(screen.getByTestId("sel-option-opus")).not.toHaveAttribute("title");
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
