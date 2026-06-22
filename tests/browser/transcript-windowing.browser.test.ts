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

// Render-windowing in real WebKit (plan: 2026-06-21-transcript-render-windowing
// M1). The jsdom suite proves the cursor logic; this proves the window is a real
// DOM bound — only a tail of blocks mounts — and that the mount still lands at
// the true bottom despite the off-window history being absent.

const blockCount = (): number => page.getByTestId("transcript-block").elements().length;

beforeEach(() => {
  resetState();
});

test("a long transcript mounts only a bounded tail window, pinned at the bottom", async () => {
  await registerAgent(ALICE);
  // 120 exchanges → 240 blocks, far over the 50-block window.
  seedTurns(ALICE.id, buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 120 })[ALICE.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.element(page.getByTestId("transcript-block").first()).toBeInTheDocument();

  // Bounded well below the 240 seeded blocks, and non-empty — a real window.
  await expect.poll(() => blockCount()).toBeLessThanOrEqual(50);
  expect(blockCount()).toBeGreaterThan(0);

  // Mount lands at the true bottom (the window IS the bottom of the stream).
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
});
