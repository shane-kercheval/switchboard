import { describe, expect, it, vi } from "vitest";
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
    const onchange = vi.fn();
    render(Select, {
      props: { options: OPTIONS, value: "opus", onchange, "data-testid": "sel" },
    });
    const select = screen.getByTestId("sel") as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "sonnet" } });
    expect(select.value).toBe("sonnet");
    expect(onchange).toHaveBeenCalledOnce();
  });

  it("honors disabled", () => {
    render(Select, {
      props: { options: OPTIONS, value: "opus", disabled: true, "data-testid": "sel" },
    });
    expect((screen.getByTestId("sel") as HTMLSelectElement).disabled).toBe(true);
  });

  it("supports a placeholder and disabled options", () => {
    render(Select, {
      props: {
        options: [
          { label: "Opus", value: "opus" },
          { label: "Sonnet", value: "sonnet", disabled: true },
        ],
        value: "",
        placeholder: "Select a value",
        "data-testid": "sel",
      },
    });

    const select = screen.getByTestId("sel") as HTMLSelectElement;
    expect(select.options[0]).toHaveTextContent("Select a value");
    expect(select.options[0]?.disabled).toBe(true);
    expect(select.options[2]?.disabled).toBe(true);
  });
});
