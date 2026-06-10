import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

// Canonical IPC mock block — see ./harness header. Browser mode hoists `vi.mock`
// only within the spec file, so it lives here, not in the helper. These cases
// drive no streaming, so they omit the listener-capture map.
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";

// Behavior 1: a message/response that genuinely overflows the clip gets a
// collapse toggle; one that fits gets none. This is the assertion jsdom CANNOT
// make — there `max-height` is parsed but never applied, so
// `scrollHeight === clientHeight` (both 0) and the overflow is invisible. The
// first test is also the canonical shape later specs copy: mount via the
// harness, seed state, poll measured geometry.

beforeEach(() => {
  resetState();
});

test("a long user message overflows the clip and gets a collapse toggle (compact default)", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [userTurn({ id: "user-1", agentId: ALICE.id, text: longText() })]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  // Poll: ResizeObserver-driven measurement settles asynchronously.
  await expect
    .poll(() => {
      const el = page.getByTestId("preview-clip").element() as HTMLElement;
      return el.scrollHeight - el.clientHeight;
    })
    .toBeGreaterThan(1);

  // Real applied CSS: the clip actually hides the overflow (not just a class).
  const clip = page.getByTestId("preview-clip").element() as HTMLElement;
  expect(getComputedStyle(clip).overflowY).toBe("hidden");

  // …and the overflow drives the per-message collapse toggle into existence.
  await expect.element(page.getByTestId("turn-preview-toggle")).toBeInTheDocument();
});

test("a short user message fits the clip and gets no toggle (no false positives)", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [userTurn({ id: "user-1", agentId: ALICE.id, text: "short and sweet" })]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  // The clip still mounts while compact, but the content fits — so its measured
  // overflow stays at zero and no toggle is offered.
  await expect.element(page.getByTestId("preview-clip")).toBeInTheDocument();
  await expect
    .poll(() => {
      const el = page.getByTestId("preview-clip").element() as HTMLElement;
      return el.scrollHeight - el.clientHeight;
    })
    .toBeLessThanOrEqual(1);
  expect(page.getByTestId("turn-preview-toggle").elements()).toHaveLength(0);
});

test("a non-last-block agent response that overflows gets a toggle", async () => {
  // The agent's latest response renders as the full last-block view (no clip);
  // an earlier response renders as the height-clipped preview. Seed two so the
  // first is the clipped one whose overflow must drive a toggle.
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({
      id: "agent-early",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:01Z",
      items: [textItem(longText())],
    }),
    agentTurn({
      id: "agent-latest",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:09Z",
      items: [textItem("the latest, short reply")],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  // Exactly one clip (the earlier, non-last-block response); it overflows.
  await expect.element(page.getByTestId("preview-clip")).toBeInTheDocument();
  await expect
    .poll(() => {
      const el = page.getByTestId("preview-clip").element() as HTMLElement;
      return el.scrollHeight - el.clientHeight;
    })
    .toBeGreaterThan(1);
  await expect.element(page.getByTestId("turn-preview-toggle")).toBeInTheDocument();
});
