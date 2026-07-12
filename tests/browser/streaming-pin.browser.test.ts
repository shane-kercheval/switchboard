import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState, distanceFromBottom } from "./harness";
import { ALICE, BOB, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";

// Standalone streaming responses use the transcript's outer scroll rather than
// introducing a nested scrollbar. The view stays pinned while the response
// grows and when it completes. jsdom has no scroll geometry, so these contracts
// need real WebKit.

beforeEach(() => {
  resetState();
});

test("a standalone live response uses the transcript's outer scroll", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      status: "streaming",
      items: [textItem(longText(60))],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  const live = page.getByTestId("turn-live-scroll");
  await expect.element(live).toBeInTheDocument();
  expect(getComputedStyle(live.element()).overflowY).toBe("visible");
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
});

test("concurrent fan-out responses retain independent live caps", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const column = (agentId: string, turnId: string) => [
    userTurn({ id: `user-${turnId}`, agentId, text: "compare", sendId: "send-fanout" }),
    agentTurn({
      id: turnId,
      agentId,
      status: "streaming" as const,
      sendId: "send-fanout",
      items: [textItem(longText(60))],
    }),
  ];
  seedTurns(ALICE.id, column(ALICE.id, "alice-streaming"));
  seedTurns(BOB.id, column(BOB.id, "bob-streaming"));

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });

  const caps = page.getByTestId("turn-live-scroll");
  await expect.poll(() => caps.elements().length).toBe(2);
  const transcript = page.getByTestId("unified-transcript").element() as HTMLElement;
  for (const cap of caps.elements()) {
    await expect.poll(() => cap.clientHeight <= transcript.clientHeight * 0.75 + 1).toBe(true);
    expect(cap.scrollHeight - cap.clientHeight).toBeGreaterThan(1);
  }
});

test("the stop control stays fixed when elapsed seconds gain a digit", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      at: new Date(Date.now() - 7_500).toISOString(),
      status: "streaming",
      sendId: "send-timer",
      items: [textItem("working")],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  const timer = page.getByTestId("turn-elapsed");
  const stop = page.getByTestId("turn-live-control");
  const xAtNineSeconds = await vi.waitUntil(
    () => {
      if (timer.element().textContent?.trim() !== "9s") return false;
      return stop.element().getBoundingClientRect().x;
    },
    { timeout: 3_000, interval: 20 },
  );

  await expect
    .poll(() => timer.element().textContent?.trim(), { timeout: 2_500, interval: 20 })
    .toBe("10s");
  expect(stop.element().getBoundingClientRect().x).toBeCloseTo(xAtNineSeconds, 1);
});

test("on stream completion the view stays pinned with the response end in view", async () => {
  await registerAgent(ALICE);
  // A prior tall turn above the streaming one, so the outer transcript scrolls
  // and "pinned at the bottom" is meaningful.
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      status: "streaming",
      items: [textItem(longText(40))],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  // Streaming: live wrapper present and the outer transcript is pinned to the bottom.
  await expect.element(page.getByTestId("turn-live-scroll")).toBeInTheDocument();
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Complete the turn in place (same turn_id): the live wrapper is removed. The
  // view must stay at the bottom rather than stranding the response end below it.
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      endedAt: "2026-05-16T00:00:05Z",
      status: "complete",
      items: [textItem(longText(40))],
    }),
  ]);

  await expect.poll(() => page.getByTestId("turn-live-scroll").elements().length).toBe(0);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
});
