import { beforeEach, expect, test, vi } from "vitest";

// Captured listener map so specs can drive REAL events through the state
// module's synchronous event path (see harness header for the hoist rationale).
const { listeners } = vi.hoisted(() => ({
  listeners: new Map<string, (e: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(name, cb);
    return vi.fn();
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { page } from "vitest/browser";
import { mountTranscript } from "./mount";
import {
  fireTo,
  registerAgent,
  seedTurns,
  resetState,
  transcriptContainer,
  distanceFromBottom,
} from "./harness";
import type { NormalizedEvent } from "$lib/types";
import { ALICE, BOB, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";
import { buildLargeTranscript } from "$lib/dev/largeTranscript";

// The read-while-streaming contract: a user who scrolled up to read an earlier
// response must not be moved by content arriving below — neither a streaming
// response growing nor a new completed turn appearing. "Not moved" is measured
// as the SCREEN POSITION of the block being read, not scrollTop: under
// containment, off-screen blocks above legitimately change height as they
// skip/materialize, which moves scrollTop while the view stays still — and the
// anchor-based re-anchor compensates with scrollTop adjustments by design.
// (The pre-fix bug: gap-from-bottom maintenance shifted the view by exactly
// any below-viewport growth; reproduced at gap≈300 with a one-time ~200px
// upward jump.)

function scrollTo(top: number): void {
  const c = transcriptContainer();
  c.scrollTop = top;
  c.dispatchEvent(new Event("scroll")); // bare scroll: scrollbar/keyboard shape
}

/// The block the user is reading: first block intersecting the viewport top.
function readBlock(): HTMLElement {
  const c = transcriptContainer();
  const kids = Array.from(
    (c.querySelector('[data-testid="transcript-block"]')?.parentElement as HTMLElement).children,
  ) as HTMLElement[];
  const top = c.getBoundingClientRect().top;
  const found = kids.find((k) => k.getBoundingClientRect().bottom > top);
  if (!found) throw new Error("no block at viewport top");
  return found;
}

function streamingTurn(length: number) {
  return agentTurn({
    id: "live-1",
    agentId: ALICE.id,
    at: "2026-05-16T00:00:00Z",
    status: "streaming",
    items: [textItem(longText(length))],
  });
}

beforeEach(() => {
  resetState();
  listeners.clear();
});

// Listener map uses `unknown` payloads (the hoist guard forbids importing
// types into the factory); narrow at the call boundary.
function fire(channel: string, event: NormalizedEvent): void {
  fireTo(listeners as Parameters<typeof fireTo>[0], channel, event);
}

test("a streaming response growing just below never moves an unpinned reader", async () => {
  await registerAgent(ALICE);
  const history = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 30 })[ALICE.id]!;
  // Start the stream SMALL (below the live cap) so its block genuinely grows
  // with each chunk while the reader sits a few hundred px above — the
  // position where the pre-fix jump reproduced.
  seedTurns(ALICE.id, [...history, streamingTurn(3)]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  const c = transcriptContainer();
  scrollTo(c.scrollHeight - c.clientHeight - 300);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(250);
  const anchor = readBlock();
  const reading = Math.round(anchor.getBoundingClientRect().top);

  for (const length of [10, 20, 30, 40]) {
    seedTurns(ALICE.id, [...history, streamingTurn(length)]);
    await expect
      .poll(() => Math.abs(Math.round(anchor.getBoundingClientRect().top) - reading))
      .toBeLessThanOrEqual(1);
  }
  // Still unpinned — reading was never interrupted by a re-pin.
  expect(distanceFromBottom()).toBeGreaterThan(100);
});

test("a streaming response growing far below never moves a mid-history reader", async () => {
  await registerAgent(ALICE);
  const history = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 30 })[ALICE.id]!;
  seedTurns(ALICE.id, [...history, streamingTurn(3)]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  const c = transcriptContainer();
  scrollTo(Math.floor((c.scrollHeight - c.clientHeight) / 2));
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(500);
  const anchor = readBlock();
  const reading = Math.round(anchor.getBoundingClientRect().top);

  for (const length of [10, 20, 30, 40]) {
    seedTurns(ALICE.id, [...history, streamingTurn(length)]);
    await expect
      .poll(() => Math.abs(Math.round(anchor.getBoundingClientRect().top) - reading))
      .toBeLessThanOrEqual(1);
  }
});

test("fan-out columns streaming at different rates never move an unpinned reader, through completion", async () => {
  // Also pins the fan-out half of the live containment exemption (the whole
  // block stays uncontained while ANY column streams) and the
  // completion-time class flip: when both columns finish, the block becomes
  // contained for the first time — losing any remembered size — and that
  // height perturbation must be absorbed by the anchor, not passed to the
  // reader.
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const history = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 20 })[ALICE.id]!;
  const column = (agentId: string, length: number, status: "streaming" | "complete") => [
    userTurn({
      id: `fan-user-${agentId}`,
      agentId,
      text: "compare your approaches",
      at: "2026-05-16T00:00:00Z",
      sendId: "fan-1",
    }),
    agentTurn({
      id: `fan-agent-${agentId}`,
      agentId,
      at: "2026-05-16T00:00:01Z",
      status,
      ...(status === "complete" ? { endedAt: "2026-05-16T00:00:30Z" } : {}),
      items: [textItem(longText(length))],
      sendId: "fan-1",
    }),
  ];
  seedTurns(ALICE.id, [...history, ...column(ALICE.id, 3, "streaming")]);
  seedTurns(BOB.id, column(BOB.id, 3, "streaming"));

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  await expect.poll(() => page.getByTestId("fanout-group").elements().length).toBe(1);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // The live fan-out block is containment-exempt (real geometry while any
  // column streams).
  const fanoutBlock = (): HTMLElement =>
    page.getByTestId("fanout-group").element().closest('[data-testid="transcript-block"]')!;
  expect(getComputedStyle(fanoutBlock()).contentVisibility).not.toBe("auto");

  const c = transcriptContainer();
  scrollTo(c.scrollHeight - c.clientHeight - 300);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(250);
  const anchor = readBlock();
  const reading = Math.round(anchor.getBoundingClientRect().top);

  // Columns grow at different rates while the reader sits above.
  for (const [a, b] of [
    [10, 6],
    [20, 15],
    [30, 28],
  ] as const) {
    seedTurns(ALICE.id, [...history, ...column(ALICE.id, a, "streaming")]);
    seedTurns(BOB.id, column(BOB.id, b, "streaming"));
    await expect
      .poll(() => Math.abs(Math.round(anchor.getBoundingClientRect().top) - reading))
      .toBeLessThanOrEqual(1);
  }

  // Both columns complete: the block gains containment for the first time.
  seedTurns(ALICE.id, [...history, ...column(ALICE.id, 30, "complete")]);
  seedTurns(BOB.id, column(BOB.id, 28, "complete"));
  await expect.poll(() => getComputedStyle(fanoutBlock()).contentVisibility).toBe("auto");
  await expect
    .poll(() => Math.abs(Math.round(anchor.getBoundingClientRect().top) - reading))
    .toBeLessThanOrEqual(1);
});

test("a new completed turn appearing below never moves an unpinned reader", async () => {
  await registerAgent(ALICE);
  const base = [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({ id: "agent-1", agentId: ALICE.id, items: [textItem(longText(30))] }),
  ];
  seedTurns(ALICE.id, base);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect
    .poll(() => transcriptContainer().scrollHeight)
    .toBeGreaterThan(transcriptContainer().clientHeight + 100);

  scrollTo(0);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
  const anchor = readBlock();
  const reading = Math.round(anchor.getBoundingClientRect().top);

  // A whole new exchange lands below.
  seedTurns(ALICE.id, [
    ...base,
    userTurn({ id: "user-2", agentId: ALICE.id, text: "next", at: "2026-05-16T00:00:08Z" }),
    agentTurn({
      id: "agent-2",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:09Z",
      items: [textItem(longText(30))],
    }),
  ]);

  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
  await expect
    .poll(() => Math.abs(Math.round(anchor.getBoundingClientRect().top) - reading))
    .toBeLessThanOrEqual(1);
});

test("real streamed events keep an unpinned reader still", async () => {
  // Integration of the whole live path: wire listener -> reducers -> revision
  // bump -> re-anchor, in real WebKit. The other tests in this spec seed state
  // directly; this one streams the way production does (real per-chunk events).
  await registerAgent(ALICE);
  const history = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 30 })[ALICE.id]!;
  seedTurns(ALICE.id, history);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  const channel = `agent:${ALICE.id}`;
  fire(channel, {
    type: "turn_start",
    turn_id: "live-rt",
    message_id: "00000000-0000-7000-8000-0000000000f1",
    started_at: "2026-05-16T00:00:00Z",
  });
  fire(channel, { type: "content_chunk", turn_id: "live-rt", kind: "text", text: longText(4) });
  await expect.poll(() => page.getByTestId("turn-live-scroll").elements().length).toBe(1);

  const c = transcriptContainer();
  scrollTo(c.scrollHeight - c.clientHeight - 300);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(250);
  const anchor = readBlock();
  const reading = Math.round(anchor.getBoundingClientRect().top);

  // Many chunks growing the live block below the reader — the anchor must
  // absorb the applied growth on every one.
  for (let round = 0; round < 5; round++) {
    for (let i = 0; i < 8; i++) {
      fire(channel, {
        type: "content_chunk",
        turn_id: "live-rt",
        kind: "text",
        text: `\nstreamed line ${round}-${i} of the live response`,
      });
    }
    await expect
      .poll(() => Math.abs(Math.round(anchor.getBoundingClientRect().top) - reading))
      .toBeLessThanOrEqual(1);
  }

  // Completion — still still.
  fire(channel, {
    type: "turn_end",
    turn_id: "live-rt",
    outcome: { status: "completed" },
    ended_at: "2026-05-16T00:00:30Z",
  });
  await expect.poll(() => page.getByTestId("turn-live-scroll").elements().length).toBe(0);
  await expect
    .poll(() => Math.abs(Math.round(anchor.getBoundingClientRect().top) - reading))
    .toBeLessThanOrEqual(1);
});
