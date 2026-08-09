import { tick } from "svelte";
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

test("a small scroll up inside a live cap releases its pin; other columns keep following", async () => {
  // Each capped fan-out column pins to its own bottom independently. The same
  // streaming-escape contract as the outer transcript applies inside a cap: one
  // small upward scroll (well inside the 32px re-pin threshold) must release
  // that column's pin immediately — under the old distance rule every streamed
  // token re-pinned the cap and a gentle scroll could never escape. The sibling
  // column must keep following its own stream, untouched.
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const column = (agentId: string, turnId: string, lines: number) => [
    userTurn({ id: `user-${turnId}`, agentId, text: "compare", sendId: "send-fanout" }),
    agentTurn({
      id: turnId,
      agentId,
      status: "streaming" as const,
      sendId: "send-fanout",
      items: [textItem(longText(lines))],
    }),
  ];
  seedTurns(ALICE.id, column(ALICE.id, "alice-streaming", 60));
  seedTurns(BOB.id, column(BOB.id, "bob-streaming", 60));

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });

  const caps = page.getByTestId("turn-live-scroll");
  await expect.poll(() => caps.elements().length).toBe(2);
  const capGap = (el: Element): number => el.scrollHeight - el.scrollTop - el.clientHeight;
  // Both caps overflow and start pinned to their own bottoms.
  for (const cap of caps.elements()) {
    await expect.poll(() => cap.scrollHeight - cap.clientHeight).toBeGreaterThan(1);
    await expect.poll(() => capGap(cap)).toBeLessThan(32);
  }

  // One small wheel tick inside the first column's cap.
  const first = caps.elements()[0] as HTMLElement;
  first.scrollTop = first.scrollTop - 5;
  first.dispatchEvent(new Event("scroll"));

  // Both streams grow. The scrolled column must hold its place (its gap widens
  // by the growth); the sibling must stay pinned to its bottom.
  seedTurns(ALICE.id, column(ALICE.id, "alice-streaming", 90));
  seedTurns(BOB.id, column(BOB.id, "bob-streaming", 90));

  await expect.poll(() => capGap(caps.elements()[0] as HTMLElement)).toBeGreaterThan(50);
  await expect.poll(() => capGap(caps.elements()[1] as HTMLElement)).toBeLessThan(32);
});

test("the stop control stays fixed when elapsed seconds gain a digit", async () => {
  await registerAgent(ALICE);
  vi.useFakeTimers();
  try {
    const now = new Date("2026-05-16T00:00:10Z");
    vi.setSystemTime(now);
    seedTurns(ALICE.id, [
      agentTurn({
        id: "agent-streaming",
        agentId: ALICE.id,
        at: new Date(now.getTime() - 9_000).toISOString(),
        status: "streaming",
        sendId: "send-timer",
        items: [textItem("working")],
      }),
    ]);

    mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
    await tick();

    const timer = page.getByTestId("turn-elapsed");
    const stop = page.getByTestId("turn-live-control");
    expect(timer.element().textContent?.trim()).toBe("9s");
    expect(stop.element().getBoundingClientRect().x).toBeLessThan(
      timer.element().getBoundingClientRect().x,
    );
    const xAtNineSeconds = stop.element().getBoundingClientRect().x;

    await vi.advanceTimersByTimeAsync(1_000);
    await tick();

    expect(timer.element().textContent?.trim()).toBe("10s");
    expect(stop.element().getBoundingClientRect().x).toBeCloseTo(xAtNineSeconds, 1);
  } finally {
    vi.useRealTimers();
  }
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
