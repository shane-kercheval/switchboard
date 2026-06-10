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
import { ALICE, PROJECT_ID, longText, userTurn } from "./fixtures";

// Behavior 2 — BUG GUARD: a long user message, once expanded, KEEPS its toggle
// and can be re-collapsed. A user message's toggle is driven purely by measured
// overflow (`clipOverflow[key]`); the clip + its ResizeObserver mount only while
// compact, so expanding unmounts the measurer. The fix is that `clipOverflow`
// entries are deliberately NOT deleted on the observer's `destroy` — the retained
// `true` keeps the re-collapse toggle alive.
//
// Observed red: with `delete clipOverflow[key]` added to `measureClip`'s
// `destroy` (reverting the fix), this test fails at "toggle still present after
// expand" — the toggle vanishes and the message can't be re-collapsed. Confirmed
// failing against that revert, then reverted back.

beforeEach(() => {
  resetState();
});

test("expanding a long user message keeps the toggle and allows re-collapse", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [userTurn({ id: "user-1", agentId: ALICE.id, text: longText() })]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  // Collapsed to start (compact default): the clip overflows and a toggle shows.
  await expect
    .poll(() => {
      const el = page.getByTestId("preview-clip").element() as HTMLElement;
      return el.scrollHeight - el.clientHeight;
    })
    .toBeGreaterThan(1);
  const toggle = page.getByTestId("turn-preview-toggle");
  await expect.element(toggle).toBeInTheDocument();
  await expect.element(toggle).toHaveAttribute("aria-label", "Expand");

  // Expand: the clip (and its measurer) unmount, but the toggle must remain so
  // the message can be collapsed again.
  await toggle.click();
  await expect.poll(() => page.getByTestId("preview-clip").elements().length).toBe(0);
  await expect.element(toggle).toBeInTheDocument();
  await expect.element(toggle).toHaveAttribute("aria-label", "Collapse");

  // Re-collapse: the clip comes back.
  await toggle.click();
  await expect.element(page.getByTestId("preview-clip")).toBeInTheDocument();
  await expect.element(toggle).toHaveAttribute("aria-label", "Expand");
});
