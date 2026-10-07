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
import { ALICE, PROJECT_ID } from "./fixtures";
import { buildLargeTranscript } from "$lib/dev/largeTranscript";
import { INITIAL_WINDOW } from "$lib/state/unified";

// Render-windowing in real WebKit. The jsdom suite proves the cursor logic; this
// proves the window is a real DOM bound — only a tail of blocks mounts — and that
// the mount still lands at the true bottom despite the off-window history being
// absent.

const blockCount = (): number => page.getByTestId("transcript-block").elements().length;

beforeEach(() => {
  resetState();
});

test("a long transcript mounts only a bounded tail window, pinned at the bottom", async () => {
  await registerAgent(ALICE);
  // 120 exchanges → 240 blocks, far over the INITIAL_WINDOW-block window.
  seedTurns(ALICE.id, buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 120 })[ALICE.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.element(page.getByTestId("transcript-block").first()).toBeInTheDocument();

  // Bounded to the real window, not a loose proxy: the seeded exchange blocks
  // overflow the 600px host, so the reveal sentinel stays above the viewport and
  // no auto-reveal fires — the settled mount is exactly INITIAL_WINDOW. (Asserting
  // the true bound is what would have caught the first-render parse-storm bug,
  // which a generous `<= 50` left invisible.)
  await expect.poll(() => blockCount()).toBeLessThanOrEqual(INITIAL_WINDOW);
  expect(blockCount()).toBeGreaterThan(0);

  // Mount lands at the true bottom (the window IS the bottom of the stream).
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
});
