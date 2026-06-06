import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import Select from "./Select.svelte";

const OPTIONS = [
  { label: "Opus", value: "opus" },
  { label: "Sonnet", value: "sonnet" },
];

describe("Select", () => {
  it("renders one option per item with its label and value", () => {
    render(Select, { props: { options: OPTIONS, value: "opus", "data-testid": "sel" } });
    const select = screen.getByTestId("sel") as HTMLSelectElement;
    expect(select.value).toBe("opus");
    const options = Array.from(select.options).map((o) => ({ label: o.label, value: o.value }));
    expect(options).toEqual(OPTIONS);
  });

  it("reflects a change to the selected value", async () => {
    render(Select, { props: { options: OPTIONS, value: "opus", "data-testid": "sel" } });
    const select = screen.getByTestId("sel") as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "sonnet" } });
    expect(select.value).toBe("sonnet");
  });

  it("honors disabled", () => {
    render(Select, {
      props: { options: OPTIONS, value: "opus", disabled: true, "data-testid": "sel" },
    });
    expect((screen.getByTestId("sel") as HTMLSelectElement).disabled).toBe(true);
  });
});
