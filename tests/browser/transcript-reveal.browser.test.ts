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
import { ALICE, PROJECT_ID } from "./fixtures";
import { buildLargeTranscript } from "$lib/dev/largeTranscript";
import { INITIAL_WINDOW } from "$lib/state/unified";

// Upward reveal in real WebKit — the authoritative coverage, since
// scroll-position preservation is layout-coupled and jsdom can't measure it. The
// real IntersectionObserver fires the reveal when the sentinel scrolls into view;
// the assertion is that prepending older blocks does NOT move the reading
// position. There is no bespoke scroll correction: the existing ResizeObserver →
// reanchor path holds the anchor exactly (its own height is unchanged on a
// top-prepend) now that no content-visibility estimates sit above it.

const blockCount = (): number => page.getByTestId("transcript-block").elements().length;

// scrollTop + a synchronous scroll event, so onScroll → captureAnchor runs before
// the (async) IntersectionObserver fires — matching the scroll-hold suite's shape.
function scrollToTop(): void {
  const c = transcriptContainer();
  c.scrollTop = 0;
  c.dispatchEvent(new Event("scroll"));
}

beforeEach(() => {
  resetState();
});

test("reaching the top reveals older blocks and holds the reading position", async () => {
  await registerAgent(ALICE);
  // 120 exchanges → 240 blocks, windowed to the last INITIAL_WINDOW.
  seedTurns(ALICE.id, buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 120 })[ALICE.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => blockCount()).toBeLessThanOrEqual(INITIAL_WINDOW);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
  const initialCount = blockCount();

  // Scroll to the top of the window; the first block is now the reading anchor.
  // Capture it BEFORE the async reveal fires.
  scrollToTop();
  const refEl = page.getByTestId("transcript-block").first().element() as HTMLElement;
  const before = refEl.getBoundingClientRect().top;

  // The sentinel intersects → the reveal mounts an older batch.
  await expect.poll(() => blockCount()).toBeGreaterThan(initialCount);

  // Reading position held: the reference block did not jump as older blocks
  // prepended above it (reanchor alone, no manual scrollTop math).
  await expect
    .poll(() => Math.abs(refEl.getBoundingClientRect().top - before))
    .toBeLessThanOrEqual(4);
});

test("repeated reveals walk back to the start and retire the sentinel", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 120 })[ALICE.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Reveal a batch at a time until all 240 blocks are mounted. Bounce to the
  // bottom then back to the top each round so the sentinel cleanly leaves and
  // re-enters the viewport — the IntersectionObserver only re-fires on a
  // transition, which is exactly how a real user re-scrolling up triggers it.
  const c = transcriptContainer();
  const settle = (): Promise<void> =>
    new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())));
  // Generous iteration cap — enough rounds even for a small batch size.
  for (let guard = 0; guard < 40 && blockCount() < 240; guard++) {
    const before = blockCount();
    c.scrollTop = c.scrollHeight;
    c.dispatchEvent(new Event("scroll"));
    await settle();
    scrollToTop();
    await expect.poll(() => blockCount()).toBeGreaterThan(before);
    await settle();
  }

  await expect.poll(() => blockCount()).toBe(240);
  // Nothing left above the window → the sentinel is gone.
  await expect.element(page.getByTestId("reveal-sentinel")).not.toBeInTheDocument();
});
