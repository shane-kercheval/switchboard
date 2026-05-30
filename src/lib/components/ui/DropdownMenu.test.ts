import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import Harness from "./_DropdownMenuHarness.svelte";

describe("DropdownMenu", () => {
  it("is closed until the trigger is activated", () => {
    render(Harness, { props: { onSelect: vi.fn() } });
    expect(screen.queryByTestId("dd-item-a")).not.toBeInTheDocument();
  });

  it("opens on trigger click, fires the item's onSelect, and closes", async () => {
    const onSelect = vi.fn();
    render(Harness, { props: { onSelect } });

    await fireEvent.click(screen.getByTestId("dd-trigger"));
    await waitFor(() => expect(screen.getByTestId("dd-item-a")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("dd-item-a"));
    expect(onSelect).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(screen.queryByTestId("dd-item-a")).not.toBeInTheDocument());
  });

  // Verifies our dismissal wiring delegates to bits-ui; we don't test arrow-key
  // item navigation (that's the library's internals, not our wrapper).
  it("closes on Escape", async () => {
    render(Harness, { props: { onSelect: vi.fn() } });

    await fireEvent.click(screen.getByTestId("dd-trigger"));
    await waitFor(() => expect(screen.getByTestId("dd-item-a")).toBeInTheDocument());

    await fireEvent.keyDown(screen.getByTestId("dd-item-a"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("dd-item-a")).not.toBeInTheDocument());
  });
});
