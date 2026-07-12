import { afterEach, describe, expect, it } from "vitest";
import {
  _testing,
  consumeJump,
  jumpRequest,
  jumpToRow,
  requestJump,
  resolveJumpPane,
} from "./transcriptJump.svelte";
import {
  _testing as panesTesting,
  layoutFor,
  maximizePane,
  minimizePane,
  moveAgentToNewPane,
  toggleAgentHidden,
} from "./transcriptPanes.svelte";

const PROJECT = "00000000-0000-7000-8000-0000000000ff";
const A = "00000000-0000-7000-8000-000000000aaa";
const B = "00000000-0000-7000-8000-000000000bbb";
const ROSTER = [A, B];

afterEach(() => {
  _testing.reset();
  panesTesting.reset();
});

describe("jump request store", () => {
  it("request bumps the sequence and carries the address; consume clears it", () => {
    requestJump(PROJECT, "pane-1", "u:send-1");
    expect(jumpRequest.seq).toBe(1);
    expect(jumpRequest).toMatchObject({
      projectId: PROJECT,
      paneId: "pane-1",
      rowKey: "u:send-1",
    });

    consumeJump(1);
    expect(jumpRequest.rowKey).toBeNull();
    expect(jumpRequest.paneId).toBeNull();
  });

  it("consuming a stale sequence does not clear a newer request", () => {
    requestJump(PROJECT, "pane-1", "u:send-1");
    requestJump(PROJECT, "pane-2", "u:send-2");
    consumeJump(1);
    expect(jumpRequest.rowKey).toBe("u:send-2");
  });
});

describe("resolveJumpPane", () => {
  it("resolves an agent to its own pane and a user row to the leftmost recipient pane", () => {
    moveAgentToNewPane(PROJECT, ROSTER, B); // pane 1: [A], pane 2: [B]
    const layout = layoutFor(PROJECT, ROSTER);
    const [paneA, paneB] = layout.panes;

    expect(resolveJumpPane(PROJECT, ROSTER, [B])).toBe(paneB!.id);
    // User row fanned out to both: leftmost containing a recipient wins.
    expect(resolveJumpPane(PROJECT, ROSTER, [A, B])).toBe(paneA!.id);
    expect(resolveJumpPane(PROJECT, ROSTER, [B, A])).toBe(paneA!.id);
  });

  it("skips eye-hidden members and returns null when no visible pane hosts the agent", () => {
    toggleAgentHidden(PROJECT, ROSTER, A);
    // A is hidden in its pane → its rows render nowhere.
    expect(resolveJumpPane(PROJECT, ROSTER, [A])).toBeNull();
    // A user row to both recipients still lands via the visible one.
    expect(resolveJumpPane(PROJECT, ROSTER, [A, B])).not.toBeNull();
  });
});

describe("jumpToRow", () => {
  it("restores a minimized target pane and addresses the request to it", () => {
    const paneB = moveAgentToNewPane(PROJECT, ROSTER, B);
    minimizePane(PROJECT, ROSTER, paneB);
    expect(layoutFor(PROJECT, ROSTER).minimized).toContain(paneB);

    expect(jumpToRow(PROJECT, ROSTER, [B], "a:turn-9")).toBe(true);
    expect(layoutFor(PROJECT, ROSTER).minimized).not.toContain(paneB);
    expect(jumpRequest).toMatchObject({ paneId: paneB, rowKey: "a:turn-9" });
  });

  it("replaces the maximized pane when the target is maximized-over", () => {
    const paneB = moveAgentToNewPane(PROJECT, ROSTER, B);
    const paneA = layoutFor(PROJECT, ROSTER).panes[0]!.id;
    maximizePane(PROJECT, ROSTER, paneA);

    expect(jumpToRow(PROJECT, ROSTER, [B], "a:turn-9")).toBe(true);
    // revealPane semantics: focus stays focus — the target replaces the
    // maximized pane rather than exploding back to a split.
    expect(layoutFor(PROJECT, ROSTER).maximized).toBe(paneB);
  });

  it("returns false without touching layout or the store when no pane hosts the agent", () => {
    const paneB = moveAgentToNewPane(PROJECT, ROSTER, B);
    // Close B's pane by moving B out… simplest unassignment: hide A and ask for A.
    toggleAgentHidden(PROJECT, ROSTER, A);
    const before = layoutFor(PROJECT, ROSTER);

    expect(jumpToRow(PROJECT, ROSTER, [A], "a:turn-1")).toBe(false);
    expect(jumpRequest.rowKey).toBeNull();
    expect(layoutFor(PROJECT, ROSTER)).toEqual(before);
    void paneB;
  });
});
