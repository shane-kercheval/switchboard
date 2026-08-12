import { afterEach, describe, expect, it } from "vitest";

const { selectionFor, setRecipients, targetRecipients, _testing } =
  await import("./recipientSelection.svelte");
const ops = await import("./composeOperations.svelte");

const P = "project-1";

afterEach(() => {
  _testing.reset();
  ops._testing.reset();
});

describe("recipientSelection: targeting is refused while a send builds its message", () => {
  it("targetRecipients writes when unlocked and reports success", () => {
    expect(targetRecipients(P, ["a", "b"])).toBe(true);
    expect(selectionFor(P)).toEqual(["a", "b"]);
  });

  it("targetRecipients is refused while a send is building its message", () => {
    setRecipients(P, ["a"]);
    const id = ops.beginOperation(P, { kind: "prompt_send" });
    expect(id).not.toBeNull();
    expect(targetRecipients(P, ["b"])).toBe(false);
    expect(selectionFor(P)).toEqual(["a"]);
  });

  it("raw setRecipients is not refused (internal reconciliation)", () => {
    setRecipients(P, ["a", "gone"]);
    ops.beginOperation(P, { kind: "prompt_send" });
    setRecipients(P, ["a"]); // e.g. pruning a removed agent mid-render
    expect(selectionFor(P)).toEqual(["a"]);
  });

  it("allows targeting during an unbounded sign-in wait and once registering", () => {
    // Blocking is scoped to `rendering`: a browser sign-in has no bound, and by
    // the time a branch is registering the send is committed, so a recipient
    // change is handled by preserving the newer composer rather than refused.
    const id = ops.beginOperation(P, { kind: "prompt_send" })!;
    ops.setOperationPhase(P, id, { name: "awaiting_user", provider: "tiddly" });
    expect(targetRecipients(P, ["b"])).toBe(true);

    ops._testing.reset();
    const forkId = ops.beginOperation(P, { kind: "prompt_fork", sourceId: "a" })!;
    ops.setOperationPhase(P, forkId, { name: "registering" });
    expect(targetRecipients(P, ["c"])).toBe(true);
  });

  it("the block is per-project and ends with the operation", () => {
    const id = ops.beginOperation(P, { kind: "prompt_send" })!;
    expect(targetRecipients("other", ["x"])).toBe(true);
    ops.finishOperation(P, id);
    expect(targetRecipients(P, ["b"])).toBe(true);
    expect(selectionFor(P)).toEqual(["b"]);
  });
});
