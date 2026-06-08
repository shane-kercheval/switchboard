import { afterEach, describe, expect, it } from "vitest";
import {
  _testing,
  clearCommandSource,
  contributedCommands,
  palette,
  setCommandSource,
  togglePalette,
  type Command,
} from "./commandPalette.svelte";

function cmd(id: string): Command {
  return { id, title: id, group: "Test", run: () => {} };
}

afterEach(() => _testing.reset());

describe("command palette state", () => {
  it("toggles the open flag", () => {
    expect(palette.open).toBe(false);
    togglePalette();
    expect(palette.open).toBe(true);
    togglePalette();
    expect(palette.open).toBe(false);
  });

  it("flattens contributed sources in registration order", () => {
    setCommandSource("a", [cmd("a1"), cmd("a2")]);
    setCommandSource("b", [cmd("b1")]);
    expect(contributedCommands().map((c) => c.id)).toEqual(["a1", "a2", "b1"]);
  });

  it("replaces a source in place on re-set without duplicating it", () => {
    setCommandSource("a", [cmd("a1")]);
    setCommandSource("b", [cmd("b1")]);
    setCommandSource("a", [cmd("a1-updated"), cmd("a2-updated")]);
    // 'a' keeps its original slot (before 'b'), but its commands are replaced.
    expect(contributedCommands().map((c) => c.id)).toEqual(["a1-updated", "a2-updated", "b1"]);
  });

  it("drops a source on clear", () => {
    setCommandSource("a", [cmd("a1")]);
    setCommandSource("b", [cmd("b1")]);
    clearCommandSource("a");
    expect(contributedCommands().map((c) => c.id)).toEqual(["b1"]);
  });
});
