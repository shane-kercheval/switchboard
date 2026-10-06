import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/svelte";
import SelectionPicker from "./SelectionPicker.svelte";

const compactOptions = [
  { label: "Opus", value: "opus" },
  { label: "Sonnet", value: "sonnet" },
  { label: "Haiku", value: "haiku" },
];

const manyOptions = Array.from({ length: 8 }, (_, i) => ({
  label: `Option ${i + 1}`,
  value: `option-${i + 1}`,
}));

describe("SelectionPicker", () => {
  it("defaults to the segmented control, even for large option sets", () => {
    render(SelectionPicker, {
      props: { options: compactOptions, value: "opus", testid: "picker", ariaLabel: "Model" },
    });
    expect(screen.getByTestId("picker")).toHaveAttribute("role", "radiogroup");
    expect(screen.getByTestId("picker-option-sonnet")).toBeInTheDocument();
  });

  it("keeps a segmented control (single row) for many short-label options", () => {
    render(SelectionPicker, {
      props: {
        options: manyOptions,
        value: "option-1",
        testid: "picker",
        ariaLabel: "Reasoning effort",
      },
    });
    // Every option renders as a radio segment — no wrap to a dropdown.
    expect(screen.getByTestId("picker")).toHaveAttribute("role", "radiogroup");
    expect(screen.getByTestId("picker-option-option-8")).toBeInTheDocument();
  });

  it("uses a native select only when the dropdown presentation is requested", () => {
    render(SelectionPicker, {
      props: {
        options: manyOptions,
        value: "option-1",
        testid: "picker",
        ariaLabel: "Model",
        presentation: "dropdown",
      },
    });
    const select = screen.getByTestId("picker") as HTMLSelectElement;
    expect(select.tagName).toBe("SELECT");
    expect(select.value).toBe("option-1");
  });
});
