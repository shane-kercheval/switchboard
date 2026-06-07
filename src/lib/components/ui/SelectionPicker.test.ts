import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/svelte";
import SelectionPicker from "./SelectionPicker.svelte";

const compactOptions = [
  { label: "Opus", value: "opus" },
  { label: "Sonnet", value: "sonnet" },
  { label: "Haiku", value: "haiku" },
];

const longOptions = Array.from({ length: 5 }, (_, i) => ({
  label: `Option ${i + 1}`,
  value: `option-${i + 1}`,
}));

describe("SelectionPicker", () => {
  it("uses the segmented control for compact option sets", () => {
    render(SelectionPicker, {
      props: { options: compactOptions, value: "opus", testid: "picker", ariaLabel: "Model" },
    });
    expect(screen.getByTestId("picker")).toHaveAttribute("role", "radiogroup");
    expect(screen.getByTestId("picker-option-sonnet")).toBeInTheDocument();
  });

  it("uses a native select for long model option sets", () => {
    render(SelectionPicker, {
      props: { options: longOptions, value: "option-1", testid: "picker", ariaLabel: "Model" },
    });
    const select = screen.getByTestId("picker") as HTMLSelectElement;
    expect(select.tagName).toBe("SELECT");
    expect(select.value).toBe("option-1");
  });

  it("can force segmented presentation for short-label effort options", () => {
    render(SelectionPicker, {
      props: {
        options: longOptions,
        value: "option-1",
        testid: "picker",
        ariaLabel: "Reasoning effort",
        presentation: "segmented",
      },
    });
    expect(screen.getByTestId("picker")).toHaveAttribute("role", "radiogroup");
    expect(screen.getByTestId("picker-option-option-5")).toBeInTheDocument();
  });
});
