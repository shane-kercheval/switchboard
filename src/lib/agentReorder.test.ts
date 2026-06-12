import { describe, expect, it } from "vitest";
import { dropIndexForPointer, movedOrder } from "./agentReorder";

describe("movedOrder", () => {
  it("moves an item toward the front", () => {
    expect(movedOrder(["a", "b", "c"], 2, 0)).toEqual(["c", "a", "b"]);
  });

  it("moves an item toward the back", () => {
    expect(movedOrder(["a", "b", "c"], 0, 2)).toEqual(["b", "c", "a"]);
  });

  it("swaps adjacent items in both directions", () => {
    expect(movedOrder(["a", "b", "c"], 1, 0)).toEqual(["b", "a", "c"]);
    expect(movedOrder(["a", "b", "c"], 1, 2)).toEqual(["a", "c", "b"]);
  });

  it("returns the input order for identity and out-of-range moves", () => {
    expect(movedOrder(["a", "b"], 1, 1)).toEqual(["a", "b"]);
    expect(movedOrder(["a", "b"], 0, -1)).toEqual(["a", "b"]);
    expect(movedOrder(["a", "b"], 1, 2)).toEqual(["a", "b"]);
    expect(movedOrder(["a", "b"], -1, 0)).toEqual(["a", "b"]);
  });

  it("returns a fresh array, never the input", () => {
    const input = ["a", "b"];
    expect(movedOrder(input, 0, 0)).not.toBe(input);
  });
});

describe("dropIndexForPointer", () => {
  // Midpoints of the OTHER (non-dragged) cards, in display order.
  const midpoints = [50, 150, 250];

  it("targets the top when the pointer is above every midpoint", () => {
    expect(dropIndexForPointer(midpoints, 10)).toBe(0);
  });

  it("counts each crossed midpoint", () => {
    expect(dropIndexForPointer(midpoints, 100)).toBe(1);
    expect(dropIndexForPointer(midpoints, 200)).toBe(2);
  });

  it("targets the end when the pointer is below every midpoint", () => {
    expect(dropIndexForPointer(midpoints, 999)).toBe(3);
  });

  it("a pointer exactly on a midpoint does not cross it", () => {
    expect(dropIndexForPointer(midpoints, 50)).toBe(0);
  });

  it("handles an empty list (single-card roster)", () => {
    expect(dropIndexForPointer([], 100)).toBe(0);
  });
});
