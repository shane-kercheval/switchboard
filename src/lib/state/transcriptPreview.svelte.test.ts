import { afterEach, describe, expect, it } from "vitest";

const {
  stateFor,
  isCompact,
  toggleKey,
  setManyOverrides,
  setProjectCompact,
  hasOverrides,
  normalizeProjectCompact,
  _testing,
} = await import("./transcriptPreview.svelte");

const A = "project-a";
const B = "project-b";

afterEach(() => {
  _testing.reset();
});

describe("transcriptPreview", () => {
  it("defaults an unknown project to compact with no overrides", () => {
    expect(stateFor(A)).toEqual({ enabled: true, overrides: {} });
    expect(hasOverrides(A)).toBe(false);
  });

  it("scopes compact mode to its project", () => {
    setProjectCompact(A, false);
    expect(stateFor(A).enabled).toBe(false);
    expect(stateFor(B).enabled).toBe(true); // untouched project keeps the default
  });

  it("isCompact returns the default when no override exists", () => {
    expect(isCompact(A, "agent:1", true)).toBe(true);
    expect(isCompact(A, "agent:1", false)).toBe(false);
  });

  it("isCompact lets an override win over the default", () => {
    toggleKey(A, "agent:1", true); // default compact -> override expanded
    expect(isCompact(A, "agent:1", true)).toBe(false);
    expect(isCompact(A, "agent:1", false)).toBe(false);
  });

  it("toggleKey flips the effective state from the default and back", () => {
    toggleKey(A, "agent:1", true);
    expect(isCompact(A, "agent:1", true)).toBe(false);
    toggleKey(A, "agent:1", true);
    expect(isCompact(A, "agent:1", true)).toBe(true);
  });

  it("scopes overrides to their project and key", () => {
    toggleKey(A, "agent:1", true);
    expect(hasOverrides(A)).toBe(true);
    expect(hasOverrides(B)).toBe(false);
    expect(isCompact(B, "agent:1", true)).toBe(true);
    expect(isCompact(A, "agent:2", true)).toBe(true);
  });

  it("setManyOverrides forces several keys to one value", () => {
    setManyOverrides(A, ["fanout:s:1", "fanout:s:2"], false);
    expect(isCompact(A, "fanout:s:1", true)).toBe(false);
    expect(isCompact(A, "fanout:s:2", true)).toBe(false);
  });

  it("setProjectCompact clears that project's overrides", () => {
    toggleKey(A, "agent:1", true);
    setProjectCompact(A, true);
    expect(stateFor(A).enabled).toBe(true);
    expect(hasOverrides(A)).toBe(false);
  });

  describe("normalizeProjectCompact", () => {
    it("enables compact and clears overrides when overrides are present", () => {
      // Project starts at the compact-on default; an override makes normalize a
      // "reset" that keeps compact on and clears the override.
      toggleKey(A, "agent:1", false);
      expect(stateFor(A).enabled).toBe(true);
      expect(hasOverrides(A)).toBe(true);
      normalizeProjectCompact(A);
      expect(stateFor(A).enabled).toBe(true);
      expect(hasOverrides(A)).toBe(false);
    });

    it("resets to clean compact from compact-off when overrides exist", () => {
      // The home state is compact, so reset lands there regardless of prior mode:
      // a user who expanded everything then collapsed one unit gets clean compact.
      setProjectCompact(A, false);
      toggleKey(A, "agent:1", false); // manually collapse one unit while expanded
      expect(stateFor(A).enabled).toBe(false);
      expect(hasOverrides(A)).toBe(true);
      normalizeProjectCompact(A);
      expect(stateFor(A).enabled).toBe(true);
      expect(hasOverrides(A)).toBe(false);
    });

    it("acts as a reset even when already compact with overrides", () => {
      setProjectCompact(A, true);
      toggleKey(A, "agent:1", true); // expand one unit while compact
      expect(hasOverrides(A)).toBe(true);
      normalizeProjectCompact(A);
      expect(stateFor(A).enabled).toBe(true);
      expect(hasOverrides(A)).toBe(false);
    });

    it("inverts compact mode when there are no overrides", () => {
      // Starts compact (default on): first normalize turns it off, next back on.
      normalizeProjectCompact(A);
      expect(stateFor(A).enabled).toBe(false);
      normalizeProjectCompact(A);
      expect(stateFor(A).enabled).toBe(true);
    });

    it("affects only the targeted project", () => {
      normalizeProjectCompact(A);
      expect(stateFor(A).enabled).toBe(false); // inverted off from the default
      expect(stateFor(B).enabled).toBe(true); // untouched, still default-on
    });
  });

  it("_testing.reset clears all preview state", () => {
    setProjectCompact(A, false);
    toggleKey(B, "agent:1", true);
    _testing.reset();
    expect(stateFor(A)).toEqual({ enabled: true, overrides: {} });
    expect(stateFor(B)).toEqual({ enabled: true, overrides: {} });
  });
});
