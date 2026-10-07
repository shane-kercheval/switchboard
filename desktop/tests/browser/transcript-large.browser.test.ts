import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import {
  registerAgent,
  seedTurns,
  resetState,
  transcriptContainer,
  distanceFromBottom,
} from "./harness";
import { ALICE, BOB, PROJECT_ID, agentTurn, longText, textItem } from "./fixtures";
import { buildLargeTranscript } from "$lib/dev/largeTranscript";

// Large-transcript rendering behaviors in real WebKit, against the same
// generator the dev seeding hook uses: the measureClip-driven collapse toggle
// appearing when a clipped unit scrolls into view, standalone streaming content
// surviving a scroll away and back, and fan-outs grouping into columns. (These
// once also asserted `content-visibility` containment, removed when windowing
// took over bounding the mounted set — see UnifiedTranscript.)

beforeEach(() => {
  resetState();
});

test("a clipped unit scrolled into view shows its collapse toggle", async () => {
  await registerAgent(ALICE);
  // Small enough to fit within the render window (≈2 blocks/exchange), so the
  // whole history is mounted and the top block is reachable by scrolling, not by
  // the upward-reveal path — this isolates the clip-on-scroll behavior.
  const seeded = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 10 });
  seedTurns(ALICE.id, seeded[ALICE.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Jump to the top of history (generator step 1 is a 40-line response — a
  // guaranteed clip overflow).
  transcriptContainer().scrollTop = 0;

  await expect
    .poll(() => {
      const c = transcriptContainer().getBoundingClientRect();
      return page
        .getByTestId("turn-preview-toggle")
        .elements()
        .some((el) => {
          const r = el.getBoundingClientRect();
          return r.top >= c.top && r.bottom <= c.bottom && r.height > 0;
        });
    })
    .toBe(true);
});

test("an unfollowed standalone stream keeps its content through scroll away and return", async () => {
  // The user scrolls away while the live unit keeps growing, then returns to
  // the transcript bottom. The complete streamed content must still be present.
  await registerAgent(ALICE);
  const history = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 40 })[ALICE.id]!;
  const streaming = (length: number) =>
    agentTurn({
      id: "live-1",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:00Z",
      status: "streaming",
      items: [textItem(longText(length))],
    });
  seedTurns(ALICE.id, [...history, streaming(40)]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.element(page.getByTestId("turn-live-scroll")).toBeInTheDocument();
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Scroll away: the live block leaves the viewport.
  transcriptContainer().scrollTop = 0;
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);

  // It keeps streaming while off-screen.
  seedTurns(ALICE.id, [...history, streaming(80)]);

  // Return to the bottom. The scroll-down is INSIDE the poll on purpose: the
  // grown content keeps changing scrollHeight between iterations, so each pass
  // re-targets the latest bottom and the assertion is that we settle there — a
  // single pre-poll assignment could land on a stale height.
  await expect
    .poll(() => {
      const c = transcriptContainer();
      c.scrollTop = c.scrollHeight;
      return distanceFromBottom();
    })
    .toBeLessThan(32);

  const live = (): HTMLElement => page.getByTestId("turn-live-scroll").element() as HTMLElement;
  await expect.poll(() => live().textContent?.includes("Line 80 of")).toBe(true);
  expect(getComputedStyle(live()).overflowY).toBe("visible");
});

test("the generator's fan-outs render as fan-out groups", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  // The generator emits a fan-out at step 50; keep it near the end so it lands
  // inside the render window (only the tail mounts).
  const seeded = buildLargeTranscript({ agentIds: [ALICE.id, BOB.id], exchanges: 52 });
  seedTurns(ALICE.id, seeded[ALICE.id]!);
  seedTurns(BOB.id, seeded[BOB.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });

  await expect.poll(() => page.getByTestId("fanout-group").elements().length).toBeGreaterThan(0);
});
