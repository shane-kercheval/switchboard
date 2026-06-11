import { afterEach, describe, expect, it } from "vitest";

const { selectionFor, setRecipients, targetRecipients, setTargetingLocked, _testing } =
  await import("./recipientSelection.svelte");

const P = "project-1";

afterEach(() => {
  _testing.reset();
});

describe("recipientSelection targeting lock", () => {
  it("targetRecipients writes when unlocked and reports success", () => {
    expect(targetRecipients(P, ["a", "b"])).toBe(true);
    expect(selectionFor(P)).toEqual(["a", "b"]);
  });

  it("targetRecipients is refused while locked", () => {
    setRecipients(P, ["a"]);
    setTargetingLocked(P, true);
    expect(targetRecipients(P, ["b"])).toBe(false);
    expect(selectionFor(P)).toEqual(["a"]);
  });

  it("raw setRecipients bypasses the lock (internal reconciliation)", () => {
    setRecipients(P, ["a", "gone"]);
    setTargetingLocked(P, true);
    setRecipients(P, ["a"]); // e.g. pruning a removed agent mid-render
    expect(selectionFor(P)).toEqual(["a"]);
  });

  it("unlocking restores targeting; the lock is per-project", () => {
    setTargetingLocked(P, true);
    expect(targetRecipients("other", ["x"])).toBe(true);
    setTargetingLocked(P, false);
    expect(targetRecipients(P, ["b"])).toBe(true);
    expect(selectionFor(P)).toEqual(["b"]);
  });
});
