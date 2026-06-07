import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import CommandPalette from "./CommandPalette.svelte";
import type { Command } from "$lib/state/commandPalette.svelte";

function command(over: Partial<Command> & Pick<Command, "id">): Command {
  return { title: over.id, group: "Navigation", run: vi.fn(), ...over };
}

function setup(commands: Command[]) {
  render(CommandPalette, { props: { open: true, commands } });
}

const COMMANDS: Command[] = [
  command({ id: "toggle-view", title: "Switch to Git view", shortcut: ["mod", "shift", "G"] }),
  command({ id: "add-project", title: "Add project", shortcut: ["mod", "N"] }),
  command({ id: "next-ready", title: "Switch to next ready project", group: "Project" }),
  command({ id: "open-editor", title: "Open project in editor", group: "Project", disabled: true }),
];

describe("CommandPalette", () => {
  it("renders every command grouped, with shortcut text", () => {
    setup(COMMANDS);
    expect(screen.getByTestId("command-option-toggle-view")).toHaveTextContent(
      "Switch to Git view",
    );
    // Group headers render.
    expect(screen.getByText("Navigation")).toBeInTheDocument();
    expect(screen.getByText("Project")).toBeInTheDocument();
    // Shortcut chord is shown alongside the title (OS-aware glyphs/words).
    expect(screen.getByTestId("command-option-add-project").textContent).toMatch(/N/);
  });

  it("filters commands by substring across title and group", async () => {
    setup(COMMANDS);
    await fireEvent.input(screen.getByTestId("command-palette-search"), {
      target: { value: "ready" },
    });
    expect(screen.getByTestId("command-option-next-ready")).toBeInTheDocument();
    expect(screen.queryByTestId("command-option-add-project")).toBeNull();
  });

  it("shows an empty state when nothing matches", async () => {
    setup(COMMANDS);
    await fireEvent.input(screen.getByTestId("command-palette-search"), {
      target: { value: "zzz" },
    });
    expect(screen.getByTestId("command-palette-empty")).toBeInTheDocument();
  });

  it("runs the highlighted command on Enter, skipping disabled rows", async () => {
    setup(COMMANDS);
    const search = screen.getByTestId("command-palette-search");
    // First enabled row is highlighted by default.
    expect(screen.getByTestId("command-option-toggle-view")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    // Down three times: toggle-view -> add-project -> next-ready -> (skips
    // disabled open-editor, wraps) back to toggle-view.
    await fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(screen.getByTestId("command-option-add-project")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(screen.getByTestId("command-option-next-ready")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(screen.getByTestId("command-option-toggle-view")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await fireEvent.keyDown(search, { key: "Enter" });
    expect(COMMANDS[0]!.run).toHaveBeenCalledTimes(1);
  });

  it("runs a command on click and closes the palette", async () => {
    const cmds = [command({ id: "go", title: "Go" })];
    setup(cmds);
    await fireEvent.click(screen.getByTestId("command-option-go"));
    expect(cmds[0]!.run).toHaveBeenCalledTimes(1);
    // Closing unmounts the Dialog body.
    await waitFor(() => expect(screen.queryByTestId("command-palette-search")).toBeNull());
  });

  it("ignores clicks on a disabled command", async () => {
    setup(COMMANDS);
    await fireEvent.click(screen.getByTestId("command-option-open-editor"));
    expect(COMMANDS[3]!.run).not.toHaveBeenCalled();
    // Still open.
    expect(screen.getByTestId("command-palette-search")).toBeInTheDocument();
  });

  it("closes on Escape", async () => {
    setup(COMMANDS);
    await fireEvent.keyDown(screen.getByTestId("command-palette-search"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("command-palette-search")).toBeNull());
  });

  it("autofocuses the search field on open", async () => {
    setup(COMMANDS);
    await waitFor(() => expect(screen.getByTestId("command-palette-search")).toHaveFocus());
  });
});
