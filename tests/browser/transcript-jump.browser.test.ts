import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountPanes } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";
import {
  layoutFor,
  minimizePane,
  moveAgentToNewPane,
  _testing as panesState,
} from "$lib/state/transcriptPanes.svelte";
import { jumpToRow, _testing as jumpState } from "$lib/state/transcriptJump.svelte";
import { INITIAL_WINDOW } from "$lib/state/unified";
import { ALICE, BOB, PROJECT_ID, agentTurn, textItem, userTurn } from "./fixtures";
import type { Turn } from "$lib/state/index.svelte";

// The navigator jump is geometry: the target block must land with its top at
// the scroll container's top, the render window must have GROWN (mounting the
// off-window target) rather than re-pinned to the tail, and the anchoring
// machinery must defend the jumped-to position instead of snapping back to the
// bottom. jsdom has no layout, so all of that is asserted here in WebKit.

const OVER = INITIAL_WINDOW * 3;

function turnsFor(agent: typeof ALICE, n: number): Turn[] {
  const turns: Turn[] = [];
  for (let k = 0; k < n; k++) {
    const mm = String(Math.floor(k / 60)).padStart(2, "0");
    const ss = String(k % 60).padStart(2, "0");
    turns.push(
      agentTurn({
        id: `turn-${k}`,
        agentId: agent.id,
        at: `2026-05-16T00:${mm}:${ss}Z`,
        items: [textItem(`message ${k}\n\nsome body content for height`)],
      }),
    );
  }
  return turns;
}

function blockEl(key: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-block-key="${CSS.escape(key)}"]`);
}

function scrollContainer(): HTMLElement {
  return page.getByTestId("unified-transcript").element() as HTMLElement;
}

beforeEach(() => {
  resetState();
  panesState.reset();
  jumpState.reset();
});

test("jumping to an off-window block grows the window and lands it at the container top", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, turnsFor(ALICE, OVER));
  mountPanes({ projectId: PROJECT_ID, agents: [ALICE] });

  // Tail-windowed: the oldest turn is not mounted.
  await expect
    .poll(() => document.querySelectorAll('[data-testid="transcript-block"]').length)
    .toBe(INITIAL_WINDOW);
  expect(blockEl("a:turn-0")).toBeNull();

  const ok = jumpToRow(PROJECT_ID, [ALICE.id], [ALICE.id], "a:turn-0");
  expect(ok).toBe(true);

  // The window grew to include target..tail (not re-pinned to tail)…
  await expect
    .poll(() => document.querySelectorAll('[data-testid="transcript-block"]').length)
    .toBe(OVER);
  // …and the target's top sits at the scroll container's top (± a px).
  await expect
    .poll(() => {
      const el = blockEl("a:turn-0");
      if (el === null) return Number.NaN;
      return Math.abs(
        el.getBoundingClientRect().top - scrollContainer().getBoundingClientRect().top,
      );
    })
    .toBeLessThan(2);

  // The anchoring machinery defends the position (no snap back to bottom).
  const c = scrollContainer();
  expect(c.scrollHeight - c.scrollTop - c.clientHeight).toBeGreaterThan(100);
});

test("jumping into a minimized pane restores it; the fresh mount picks up the pending request", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const roster = [ALICE.id, BOB.id];
  seedTurns(ALICE.id, turnsFor(ALICE, 3));
  seedTurns(BOB.id, [
    agentTurn({
      id: "bob-1",
      agentId: BOB.id,
      at: "2026-05-16T01:00:00Z",
      items: [textItem("bob says hi")],
    }),
  ]);
  const paneB = moveAgentToNewPane(PROJECT_ID, roster, BOB.id);
  minimizePane(PROJECT_ID, roster, paneB);
  mountPanes({ projectId: PROJECT_ID, agents: [ALICE, BOB] });

  // Minimized: bob's transcript is unmounted.
  await expect.poll(() => document.body.textContent?.includes("bob says hi")).toBe(false);

  const ok = jumpToRow(PROJECT_ID, roster, [BOB.id], "a:bob-1");
  expect(ok).toBe(true);

  // The pane was restored and the freshly-mounted transcript executed the
  // pending request (consumption prevents any later replay).
  expect(layoutFor(PROJECT_ID, roster).minimized).not.toContain(paneB);
  await expect.poll(() => blockEl("a:bob-1") !== null).toBe(true);
});

// Contract (deliberate, per the M9 as-built decision): a fan-out is one render
// block keyed by send_id, with no per-response DOM anchor. Jumping to one
// agent's fan-out response therefore lands the whole SEND block at the top —
// the prompt with the response columns beneath it (send + response are the
// retrieval unit). This pins that so a future per-response-anchor change is a
// conscious choice, not an accidental regression.
test("jumping to a fan-out response lands the containing send block at the top", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const roster = [ALICE.id, BOB.id];

  // Filler around the fan-out (above so it isn't already at the top, below so
  // it can actually be scrolled to the top rather than clamping at the end).
  // The fan-out is one send to both agents (shared send_id), a response each.
  const laterFiller = Array.from({ length: 12 }, (_, k) =>
    agentTurn({
      id: `after-${k}`,
      agentId: ALICE.id,
      at: `2026-05-16T03:${String(k).padStart(2, "0")}:00Z`,
      items: [textItem(`after ${k}\n\nfiller body content`)],
    }),
  );
  seedTurns(ALICE.id, [
    ...turnsFor(ALICE, 12),
    userTurn({
      id: "u-a",
      agentId: ALICE.id,
      text: "review this",
      at: "2026-05-16T02:00:00Z",
      sendId: "s1",
    }),
    agentTurn({
      id: "fa-alice",
      agentId: ALICE.id,
      at: "2026-05-16T02:00:01Z",
      sendId: "s1",
      items: [textItem("alice reviews it")],
    }),
    ...laterFiller,
  ]);
  seedTurns(BOB.id, [
    userTurn({
      id: "u-b",
      agentId: BOB.id,
      text: "review this",
      at: "2026-05-16T02:00:00Z",
      sendId: "s1",
    }),
    agentTurn({
      id: "fa-bob",
      agentId: BOB.id,
      at: "2026-05-16T02:00:02Z",
      sendId: "s1",
      items: [textItem("bob reviews it")],
    }),
  ]);
  mountPanes({ projectId: PROJECT_ID, agents: [ALICE, BOB] });

  // The fan-out block exists keyed by send_id; the individual response row has
  // NO standalone block anchor (it lives inside the fan-out columns).
  await expect.poll(() => blockEl("f:s1") !== null).toBe(true);
  expect(blockEl("a:fa-bob")).toBeNull();

  // Jump to Bob's response row → the send block lands at the container top.
  const ok = jumpToRow(PROJECT_ID, roster, [BOB.id], "a:fa-bob");
  expect(ok).toBe(true);
  await expect
    .poll(() => {
      const el = blockEl("f:s1");
      if (el === null) return Number.NaN;
      return Math.abs(
        el.getBoundingClientRect().top - scrollContainer().getBoundingClientRect().top,
      );
    })
    .toBeLessThan(2);
});
