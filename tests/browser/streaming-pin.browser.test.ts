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
import { registerAgent, seedTurns, resetState, distanceFromBottom, userScrollTo } from "./harness";
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

  // One small wheel tick inside the first column's cap: input first (the cap
  // can consume it — it is scrolled to its bottom), then the movement.
  const first = caps.elements()[0] as HTMLElement;
  userScrollTo(first, first.scrollTop - 5);

  // Both streams grow. The scrolled column must hold its place (its gap widens
  // by the growth); the sibling must stay pinned to its bottom.
  seedTurns(ALICE.id, column(ALICE.id, "alice-streaming", 90));
  seedTurns(BOB.id, column(BOB.id, "bob-streaming", 90));

  await expect.poll(() => capGap(caps.elements()[0] as HTMLElement)).toBeGreaterThan(50);
  await expect.poll(() => capGap(caps.elements()[1] as HTMLElement)).toBeLessThan(32);
});

test("a small wheel up over a standalone stream's own text releases the view", async () => {
  // The reported bug. A standalone stream has no inner scrollbox — its live
  // region is overflow-visible, so a wheel over the streamed text moves the
  // OUTER transcript. But the region still carries [data-live-region], and the
  // cap-origin rule downgraded such wheels to intent-only: the pin released,
  // yet the movement was never attributed, so the gap and anchor stayed frozen
  // at the bottom and every chunk restored the view there — auto-scroll that
  // would not die until the user fought far enough to put the pointer over
  // settled content.
  await registerAgent(ALICE);
  const stream = (lines: number) => [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      status: "streaming" as const,
      items: [textItem(longText(lines))],
    }),
  ];
  seedTurns(ALICE.id, stream(60));

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  const c = () => page.getByTestId("unified-transcript").element() as HTMLElement;
  await expect.poll(() => c().scrollHeight > c().clientHeight + 200).toBe(true);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // One gentle wheel ORIGINATING INSIDE the live region (the pointer sits over
  // the streamed text), then the outer movement WebKit produces for it — the
  // region cannot consume a wheel, it has no scrollbar.
  const live = page.getByTestId("turn-live-scroll").element() as HTMLElement;
  live.dispatchEvent(new WheelEvent("wheel", { deltaY: -80, bubbles: true }));
  c().scrollTop -= 80;
  c().dispatchEvent(new Event("scroll"));
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(50);
  const held = c().scrollTop;

  // The stream keeps growing: the view must hold, the gap widening by the
  // growth — not snap back to the bottom.
  seedTurns(ALICE.id, stream(80));
  seedTurns(ALICE.id, stream(100));
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(150);
  expect(Math.abs(c().scrollTop - held)).toBeLessThan(8);
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

test("wheeling back to the bottom mid-stream re-pins, even when a chunk lands first", async () => {
  // The reported bug. A wheel's `scroll` event is delivered asynchronously (at
  // the next rendering update), while a streamed chunk re-anchors in the frame
  // it arrives — so the correction routinely ran BEFORE the event for the
  // user's own scroll, adopted the corrected position as the tracker's
  // baseline, and left the gesture unattributed. Wheeling down to the bottom
  // never re-pinned; dragging the scrollbar (which keeps re-asserting the
  // position) did. Here the chunk deliberately lands first, with no `scroll`
  // event in between.
  await registerAgent(ALICE);
  const stream = (lines: number) => [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      status: "streaming" as const,
      items: [textItem(longText(lines))],
    }),
  ];
  seedTurns(ALICE.id, stream(40));

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  const c = () => page.getByTestId("unified-transcript").element() as HTMLElement;
  await expect.poll(() => c().scrollHeight > c().clientHeight + 200).toBe(true);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Scroll up to read history: unpinned, and the view holds as the stream grows.
  userScrollTo(c(), 0);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(200);
  seedTurns(ALICE.id, stream(60));
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(200);

  // Wheel back down to the bottom, then let a chunk land before the browser
  // delivers the gesture's `scroll` event.
  const target = c().scrollHeight - c().clientHeight;
  c().dispatchEvent(new WheelEvent("wheel", { deltaY: target - c().scrollTop, bubbles: true }));
  c().scrollTop = target;
  seedTurns(ALICE.id, stream(80));

  // Back at the bottom and following again.
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
  seedTurns(ALICE.id, stream(100));
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
});

test("wheeling back to the bottom of a live cap re-pins it to the stream", async () => {
  // The same gesture race one level in: a capped column's follow-write runs
  // when the chunk arrives, before the browser delivers the `scroll` event for
  // the user's own wheel, so the cap must classify from live geometry when a
  // gesture is outstanding. Two columns keep the cap (a standalone stream uses
  // the outer scroll instead).
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
  const grow = (lines: number) => {
    seedTurns(ALICE.id, column(ALICE.id, "alice-streaming", lines));
    seedTurns(BOB.id, column(BOB.id, "bob-streaming", lines));
  };
  grow(60);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  const caps = page.getByTestId("turn-live-scroll");
  await expect.poll(() => caps.elements().length).toBe(2);
  const cap = (): HTMLElement => caps.elements()[0] as HTMLElement;
  const capGap = (): number => cap().scrollHeight - cap().scrollTop - cap().clientHeight;
  await expect.poll(() => cap().scrollHeight - cap().clientHeight).toBeGreaterThan(1);
  await expect.poll(() => capGap()).toBeLessThan(32);

  // Scroll up inside the cap to read: it stops following.
  userScrollTo(cap(), 0);
  grow(90);
  await expect.poll(() => capGap()).toBeGreaterThan(50);

  // Wheel back to the cap's bottom, then let a chunk land before the `scroll`
  // event for that wheel is delivered.
  const target = cap().scrollHeight - cap().clientHeight;
  cap().dispatchEvent(new WheelEvent("wheel", { deltaY: target - cap().scrollTop }));
  cap().scrollTop = target;
  grow(120);

  await expect.poll(() => capGap()).toBeLessThan(32);
  grow(150);
  await expect.poll(() => capGap()).toBeLessThan(32);
});

test("a wheel a live cap consumes gives the transcript intent, not provenance", async () => {
  // Wheel events inside a cap bubble to the transcript. When the cap consumes
  // the movement, the transcript gets the DIRECTION — scrolling up in a column
  // stops the transcript chasing the bottom, which is what a reader focused on
  // that column wants — but it may not move, so no movement evidence arms.
  // (A wheel the cap CANNOT consume — wrong direction for its edge, or a
  // standalone overflow-visible region — chains to the transcript and gets
  // full provenance instead; see the standalone-stream spec above.)
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
  // A tall settled turn above the fan-out, so the OUTER transcript scrolls and
  // "holding a place in history" is meaningful.
  const grow = (lines: number) => {
    seedTurns(ALICE.id, [
      userTurn({ id: "top-a", agentId: ALICE.id, text: longText(40), at: "2026-05-16T00:00:00Z" }),
      ...column(ALICE.id, "alice-streaming", lines),
    ]);
    seedTurns(BOB.id, column(BOB.id, "bob-streaming", lines));
  };
  grow(60);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  const caps = page.getByTestId("turn-live-scroll");
  await expect.poll(() => caps.elements().length).toBe(2);
  const c = () => page.getByTestId("unified-transcript").element() as HTMLElement;
  await expect.poll(() => c().scrollHeight > c().clientHeight + 100).toBe(true);

  // Hold a place in history, then wheel inside a cap.
  c().dispatchEvent(new WheelEvent("wheel", { deltaY: -c().scrollTop, bubbles: true }));
  c().scrollTop = 0;
  c().dispatchEvent(new Event("scroll"));
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
  const held = c().scrollTop;

  // An upward wheel over a column pinned to its own bottom: the cap has room
  // above and consumes it.
  const cap = caps.elements()[0] as HTMLElement;
  cap.dispatchEvent(new WheelEvent("wheel", { deltaY: -120, bubbles: true }));
  grow(90);
  grow(120);

  expect(Math.abs(c().scrollTop - held)).toBeLessThan(8);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
});

test("an unpinned live cap re-pins after many chunks", async () => {
  // An unpinned cap samples nothing and writes nothing, so without the
  // unconditional pre-sample its idea of the content height freezes while the
  // stream grows. The reader's next scroll then arrives carrying every chunk's
  // growth at once, which used to read as an engine adjustment however small
  // the movement — and the cap never followed again.
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
  const grow = (lines: number) => {
    seedTurns(ALICE.id, column(ALICE.id, "alice-streaming", lines));
    seedTurns(BOB.id, column(BOB.id, "bob-streaming", lines));
  };
  grow(60);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  const caps = page.getByTestId("turn-live-scroll");
  await expect.poll(() => caps.elements().length).toBe(2);
  const cap = (): HTMLElement => caps.elements()[0] as HTMLElement;
  const capGap = (): number => cap().scrollHeight - cap().scrollTop - cap().clientHeight;
  await expect.poll(() => capGap()).toBeLessThan(32);

  // Read back in the cap, then let several chunks land.
  userScrollTo(cap(), 0);
  grow(90);
  grow(120);
  grow(150);
  await expect.poll(() => capGap()).toBeGreaterThan(50);

  // Scrolling back to the cap's bottom must re-pin it to the stream.
  userScrollTo(cap(), cap().scrollHeight - cap().clientHeight);
  await expect.poll(() => capGap()).toBeLessThan(32);
  grow(180);
  await expect.poll(() => capGap()).toBeLessThan(32);
});

test("a cap-origin wheel does not drag a reader who is holding a place", async () => {
  // The shipped guarantee of crediting cap direction to the transcript is a
  // NEGATIVE one: the transcript never moves for it. (At the bottom the
  // unpin has no visible effect — the hold target is a zero gap, so holding
  // and following are the same picture. The effect appears where there is a
  // position to hold, which is what this asserts.)
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
  const grow = (lines: number) => {
    seedTurns(ALICE.id, [
      userTurn({ id: "top-a", agentId: ALICE.id, text: longText(40), at: "2026-05-16T00:00:00Z" }),
      ...column(ALICE.id, "alice-streaming", lines),
    ]);
    seedTurns(BOB.id, column(BOB.id, "bob-streaming", lines));
  };
  grow(60);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  const caps = page.getByTestId("turn-live-scroll");
  await expect.poll(() => caps.elements().length).toBe(2);
  const c = () => page.getByTestId("unified-transcript").element() as HTMLElement;
  await expect.poll(() => c().scrollHeight > c().clientHeight + 100).toBe(true);

  // The reader is holding a place in history.
  userScrollTo(c(), 0);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
  const held = c().scrollTop;

  // Scrolling inside a column, in both directions, must not move them.
  const cap = caps.elements()[0] as HTMLElement;
  for (const deltaY of [-60, -40, 50, 70]) {
    cap.dispatchEvent(new WheelEvent("wheel", { deltaY, bubbles: true }));
    cap.scrollTop = Math.max(0, cap.scrollTop + deltaY);
    cap.dispatchEvent(new Event("scroll"));
    grow(90);
  }

  expect(Math.abs(c().scrollTop - held)).toBeLessThan(8);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
});

test("a live cap keeps following when its content is replaced mid-flush", async () => {
  // The cap samples geometry unconditionally when a chunk lands, where the
  // outer transcript gates the same sample — because a mid-flush remount
  // clamps `scrollTop` to 0 and, by sample time, the shrink that caused it can
  // be gone, which reads as a scroll to the top. This asserts the cap is
  // actually safe there rather than assumed to be: its content collapses to
  // nothing and returns within one flush, and it must still be following.
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const column = (agentId: string, turnId: string, lines: number) => [
    userTurn({ id: `user-${turnId}`, agentId, text: "compare", sendId: "send-fanout" }),
    agentTurn({
      id: turnId,
      agentId,
      status: "streaming" as const,
      sendId: "send-fanout",
      items: lines === 0 ? [] : [textItem(longText(lines))],
    }),
  ];
  const grow = (lines: number) => {
    seedTurns(ALICE.id, column(ALICE.id, "alice-streaming", lines));
    seedTurns(BOB.id, column(BOB.id, "bob-streaming", lines));
  };
  grow(60);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  const caps = page.getByTestId("turn-live-scroll");
  await expect.poll(() => caps.elements().length).toBe(2);
  const cap = (): HTMLElement => caps.elements()[0] as HTMLElement;
  const capGap = (): number => cap().scrollHeight - cap().scrollTop - cap().clientHeight;
  await expect.poll(() => cap().scrollHeight - cap().clientHeight).toBeGreaterThan(1);
  await expect.poll(() => capGap()).toBeLessThan(32);

  // Content vanishes and returns within one flush.
  grow(0);
  grow(90);

  await expect.poll(() => caps.elements().length).toBe(2);
  await expect.poll(() => capGap()).toBeLessThan(32);
  grow(120);
  await expect.poll(() => capGap()).toBeLessThan(32);
});
