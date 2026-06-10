import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";

// Behavior 6: the streaming live cap is sized to the transcript AREA (the
// `[container-type:size]` ancestor via `75cqh`), not the viewport — so it never
// exceeds ~3/4 of the available height and scales with it.
// Behavior 4: a tall streaming response is capped while live and, on completion,
// the view STAYS PINNED at the bottom so the finished response's end remains in
// view (the reported "jerk away from the bottom" bug). jsdom can't see either:
// `cqh`/`max-height` aren't applied and there's no scroll geometry.

function distanceFromBottom(): number {
  const c = page.getByTestId("unified-transcript").element() as HTMLElement;
  return c.scrollHeight - c.scrollTop - c.clientHeight;
}

beforeEach(() => {
  resetState();
});

test("the live cap is bounded to ~3/4 of the transcript area and clips overflow", async () => {
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

  const cap = page.getByTestId("turn-live-scroll");
  await expect.element(cap).toBeInTheDocument();

  // Capped to the transcript area, not the viewport: the live region is no taller
  // than ~75% of the transcript container (allow a px of rounding), and its
  // content overflows that cap (so it genuinely scrolls inside).
  await expect
    .poll(() => {
      const capEl = page.getByTestId("turn-live-scroll").element() as HTMLElement;
      const transcript = page.getByTestId("unified-transcript").element() as HTMLElement;
      return capEl.clientHeight <= transcript.clientHeight * 0.75 + 1;
    })
    .toBe(true);
  const capEl = cap.element() as HTMLElement;
  expect(capEl.scrollHeight - capEl.clientHeight).toBeGreaterThan(1);

  // …and it is pinned to the tail: `liveScroll` scrolls the cap to the bottom so
  // the user sees the newest streamed tokens, not the start of the response.
  await expect
    .poll(() => {
      const el = page.getByTestId("turn-live-scroll").element() as HTMLElement;
      return el.scrollHeight - el.scrollTop - el.clientHeight;
    })
    .toBeLessThan(4);
});

// BUG GUARD for the content-change scroll gate (`scrollHeight === lastScrollHeight`
// in `onScroll`). Observed red: with that gate removed (so `onScroll` recomputes
// `pinned` on every scroll, including the browser's clamp as the live cap drops),
// this test fails — the view unpins on completion and the finished response's end
// is stranded below the fold (the reported "jerk away from the bottom" bug).
// Confirmed failing against that revert, then reverted back.
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

  // Streaming: live cap present and the outer transcript is pinned to the bottom.
  await expect.element(page.getByTestId("turn-live-scroll")).toBeInTheDocument();
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Complete the turn in place (same turn_id): the live cap is removed and the
  // full response renders, growing the content. The view must follow to stay at
  // the bottom — not unpin and strand the end below the fold.
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
