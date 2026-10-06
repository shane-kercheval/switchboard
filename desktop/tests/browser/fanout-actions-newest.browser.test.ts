import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, resetState, seedTurns } from "./harness";
import { ALICE, BOB, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";

// Regression coverage for the fan-out aggregate footer hover behavior, focused
// on the case the original implementation got stuck on: the NEWEST fan-out,
// which re-renders when its turn finishes streaming. Reveal is driven by a
// window-level pointer hit-test (not CSS `:hover`/`pointerleave`), so a hover
// captured before the re-render can't stick on a replaced node.

beforeEach(() => {
  resetState();
});

/** Opacity of the newest fan-out's aggregate footer (the reveal target). */
function newestActionsOpacity(): string {
  const footers = page.getByTestId("fanout-actions-footer").elements() as HTMLElement[];
  const last = footers[footers.length - 1];
  return last ? getComputedStyle(last).opacity : "missing";
}

/** Seed two fan-out sends (fan-1 older, fan-2 newest). The newest can be seeded
 *  mid-stream so a follow-up `seedTwoSends(false)` simulates streaming→final. */
function seedTwoSends(newestStreaming: boolean): void {
  seedTurns(ALICE.id, [
    userTurn({ id: "u-a-1", agentId: ALICE.id, sendId: "fan-1", text: "prompt one" }),
    agentTurn({
      id: "a-a-1",
      agentId: ALICE.id,
      sendId: "fan-1",
      endedAt: "2026-05-16T00:00:02Z",
      items: [textItem(longText(40, "alice-one"))],
    }),
    userTurn({ id: "u-a-2", agentId: ALICE.id, sendId: "fan-2", text: "prompt two" }),
    agentTurn({
      id: "a-a-2",
      agentId: ALICE.id,
      sendId: "fan-2",
      status: newestStreaming ? "streaming" : "complete",
      endedAt: newestStreaming ? undefined : "2026-05-16T00:01:02Z",
      items: [textItem("alice two")],
    }),
  ]);
  seedTurns(BOB.id, [
    userTurn({ id: "u-b-1", agentId: BOB.id, sendId: "fan-1", text: "prompt one" }),
    agentTurn({
      id: "a-b-1",
      agentId: BOB.id,
      sendId: "fan-1",
      endedAt: "2026-05-16T00:00:03Z",
      items: [textItem(longText(40, "bob-one"))],
    }),
    userTurn({ id: "u-b-2", agentId: BOB.id, sendId: "fan-2", text: "prompt two" }),
    agentTurn({
      id: "a-b-2",
      agentId: BOB.id,
      sendId: "fan-2",
      status: newestStreaming ? "streaming" : "complete",
      endedAt: newestStreaming ? undefined : "2026-05-16T00:01:03Z",
      items: [textItem("bob two")],
    }),
  ]);
}

/** Hover the newest fan-out's first column. */
async function hoverNewest(): Promise<void> {
  const colCount = (page.getByTestId("fanout-column").elements() as HTMLElement[]).length;
  await page
    .getByTestId("fanout-column")
    .nth(colCount - 2)
    .hover();
}

async function frame(): Promise<void> {
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r(null))));
}

test("newest fan-out: hover reveals, moving to an older block hides", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTwoSends(false);
  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 900 });

  await expect.poll(() => page.getByTestId("fanout-actions-footer").elements().length).toBe(2);
  await hoverNewest();
  await expect.poll(newestActionsOpacity).toBe("1");

  await page.getByText("prompt one").first().hover();
  await expect.poll(newestActionsOpacity).toBe("0");
});

test("newest fan-out stays dismissible after a streaming→final re-render", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTwoSends(true); // newest mid-stream
  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 900 });

  await expect.poll(() => page.getByTestId("fanout-actions-footer").elements().length).toBe(2);
  await hoverNewest();
  await expect.poll(newestActionsOpacity).toBe("1");

  seedTwoSends(false); // streaming finishes → newest columns re-render
  await frame();

  await page.getByText("prompt one").first().hover();
  await expect.poll(newestActionsOpacity).toBe("0");
});

test("newest fan-out: moving into empty container space hides the actions", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTwoSends(false);
  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 900 });

  await expect.poll(() => page.getByTestId("fanout-actions-footer").elements().length).toBe(2);
  await hoverNewest();
  await expect.poll(newestActionsOpacity).toBe("1");

  // The user prompt sits outside the responses' hover group; hovering it leaves
  // the group and the footer hides.
  await page.getByText("prompt two").first().hover();
  await expect.poll(newestActionsOpacity).toBe("0");
});

test("newest fan-out: the copy button is reachable without the actions hiding", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTwoSends(false);
  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 900 });

  await expect.poll(() => page.getByTestId("fanout-actions-footer").elements().length).toBe(2);
  await hoverNewest();
  await expect.poll(newestActionsOpacity).toBe("1");

  // Moving down onto the copy button (inside the footer) keeps the group active.
  const copies = page.getByTestId("fanout-copy").elements() as HTMLElement[];
  await page.elementLocator(copies[copies.length - 1]!).hover();
  await frame();
  await expect.poll(newestActionsOpacity).toBe("1");
});
