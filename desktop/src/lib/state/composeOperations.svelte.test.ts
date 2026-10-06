import { afterEach, describe, expect, it } from "vitest";

const ops = await import("./composeOperations.svelte");

const P = "project-1";

afterEach(() => {
  ops._testing.reset();
});

describe("composeOperations: abandonment is confined to the sign-in wait", () => {
  // The confinement is what makes "abandoned while registering" unrepresentable
  // rather than merely absent from today's UI: both prompt continuations
  // re-check ownership immediately after the sign-in await, so an abandoned one
  // exits before it can register anything. Without it, a future caller could
  // release a registering fork's slot and strand a committed branch with no
  // first message.
  it("refuses while the message is still being built", () => {
    const id = ops.beginOperation(P, { kind: "prompt_send" })!;
    expect(ops.abandonAwaitingUserOperation(P, id)).toBe(false);
    expect(ops.ownsOperation(P, id)).toBe(true);
  });

  it("refuses while a branch is being registered", () => {
    const id = ops.beginOperation(P, { kind: "prompt_fork", sourceId: "a" })!;
    ops.setOperationPhase(P, id, { name: "registering" });
    expect(ops.abandonAwaitingUserOperation(P, id)).toBe(false);
    expect(ops.ownsOperation(P, id)).toBe(true);
  });

  it("succeeds while parked on a sign-in, and releases the slot", () => {
    const id = ops.beginOperation(P, { kind: "prompt_send" })!;
    ops.setOperationPhase(P, id, { name: "awaiting_user", provider: "tiddly" });
    expect(ops.abandonAwaitingUserOperation(P, id, { message: "stopped", tone: "notice" })).toBe(
      true,
    );
    expect(ops.ownsOperation(P, id)).toBe(false);
    expect(ops.takeOutcome(P)?.message).toBe("stopped");
  });

  it("refuses for an id that no longer owns the slot", () => {
    const first = ops.beginOperation(P, { kind: "prompt_send" })!;
    ops.setOperationPhase(P, first, { name: "awaiting_user", provider: "tiddly" });
    ops.abandonAwaitingUserOperation(P, first);
    const second = ops.beginOperation(P, { kind: "prompt_send" })!;
    ops.setOperationPhase(P, second, { name: "awaiting_user", provider: "tiddly" });
    // The stale id must not release the operation that replaced it.
    expect(ops.abandonAwaitingUserOperation(P, first)).toBe(false);
    expect(ops.ownsOperation(P, second)).toBe(true);
  });
});
