import { beforeEach, expect, test, vi } from "vitest";

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
  transcriptContainer as transcript,
  distanceFromBottom,
  userScrollTo,
} from "./harness";
import {
  ALICE,
  BOB,
  PROJECT_ID,
  agentTurn,
  longText,
  paddingSends,
  textItem,
  userTurn,
} from "./fixtures";
import type { Turn } from "$lib/state/index.svelte";
import { EXPANDED_RECENT_SENDS } from "$lib/state/unified";

// Nothing collapses under a reader scrolled up. The rules that open messages
// expanded move without the reader acting — a queued send starts, another
// agent's reply finishes — and a block collapsing mid-read takes the text being
// read with it, then gap-holds the view somewhere else. While unpinned the
// expanded set only grows; back at the bottom the owed collapses happen.

function scrollTo(top: number): void {
  userScrollTo(transcript(), top);
}

function agentTurnEl(turnId: string): HTMLElement {
  const el = document.querySelector(`[data-preview-key="agent:${turnId}"]`);
  if (!(el instanceof HTMLElement)) throw new Error(`no agent turn ${turnId}`);
  return el;
}

/** Scroll so `el`'s top sits `offset` px below the viewport top. */
function scrollInto(el: HTMLElement, offset: number): void {
  const c = transcript();
  const top = el.getBoundingClientRect().top - c.getBoundingClientRect().top + c.scrollTop;
  scrollTo(top - offset);
}

function viewportTop(el: HTMLElement): number {
  return el.getBoundingClientRect().top - transcript().getBoundingClientRect().top;
}

function exchange(n: number, reply: string): Turn[] {
  const at = `2026-05-16T00:00:${String(n * 10).padStart(2, "0")}Z`;
  return [
    userTurn({ id: `user-${n}`, agentId: ALICE.id, text: `prompt ${n}`, at, sendId: `send-${n}` }),
    agentTurn({
      id: `agent-${n}`,
      agentId: ALICE.id,
      at,
      endedAt: at,
      sendId: `send-${n}`,
      items: [textItem(reply)],
    }),
  ];
}

beforeEach(() => {
  resetState();
});

test("a queued send starting does not collapse the exchange being read", async () => {
  await registerAgent(ALICE);
  const history = [
    ...exchange(0, longText(80)),
    ...Array.from({ length: EXPANDED_RECENT_SENDS - 1 }, (_, i) =>
      exchange(i + 1, longText(10)),
    ).flat(),
  ];
  const queuedAt = "2026-05-16T00:01:00Z";
  seedTurns(ALICE.id, [
    ...history,
    userTurn({
      id: "user-q",
      agentId: ALICE.id,
      text: "queued",
      at: queuedAt,
      sendId: "send-q",
      pending: true,
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => transcript().scrollHeight > transcript().clientHeight + 400).toBe(true);

  // Read inside the oldest (tall) reply, well clear of the bottom.
  const oldest = agentTurnEl("agent-0");
  scrollInto(oldest, -200);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(200);
  const before = viewportTop(oldest);

  // The queued send starts and replies — the range moves past the oldest exchange.
  seedTurns(ALICE.id, [
    ...history,
    userTurn({ id: "user-q", agentId: ALICE.id, text: "queued", at: queuedAt, sendId: "send-q" }),
    agentTurn({
      id: "agent-q",
      agentId: ALICE.id,
      at: queuedAt,
      endedAt: queuedAt,
      sendId: "send-q",
      items: [textItem("queued reply")],
    }),
  ]);
  await expect
    .poll(() => document.querySelector('[data-preview-key="agent:agent-q"]'))
    .not.toBeNull();

  // Still expanded, still where the reader left it.
  expect(oldest.querySelector('[data-testid="preview-clip"]')).toBeNull();
  await expect.poll(() => Math.abs(viewportTop(oldest) - before)).toBeLessThan(8);

  // Back at the bottom, the owed collapse happens.
  scrollTo(transcript().scrollHeight);
  await expect.poll(() => oldest.querySelector('[data-testid="preview-clip"]')).not.toBeNull();
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
});

test("an agent's previous reply stays expanded while read when its next reply finishes", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  // Alice's only reply is outside the recent range (Bob's sends fill it), so the
  // latest-reply rule alone keeps it open.
  const alice = exchange(0, longText(80));
  const bob = paddingSends(BOB.id, EXPANDED_RECENT_SENDS);
  seedTurns(ALICE.id, alice);
  seedTurns(BOB.id, bob);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  await expect.poll(() => transcript().scrollHeight > transcript().clientHeight + 400).toBe(true);

  const previous = agentTurnEl("agent-0");
  expect(previous.querySelector('[data-testid="preview-clip"]')).toBeNull();
  scrollInto(previous, -200);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(200);
  const before = viewportTop(previous);

  // Alice's next reply finishes: her previous one is no longer her latest.
  const nextAt = "2026-05-16T02:00:00Z";
  seedTurns(ALICE.id, [
    ...alice,
    userTurn({ id: "user-next", agentId: ALICE.id, text: "next", at: nextAt, sendId: "send-next" }),
    agentTurn({
      id: "agent-next",
      agentId: ALICE.id,
      at: nextAt,
      endedAt: nextAt,
      sendId: "send-next",
      items: [textItem("next reply")],
    }),
  ]);
  await expect
    .poll(() => document.querySelector('[data-preview-key="agent:agent-next"]'))
    .not.toBeNull();

  expect(previous.querySelector('[data-testid="preview-clip"]')).toBeNull();
  await expect.poll(() => Math.abs(viewportTop(previous) - before)).toBeLessThan(8);

  scrollTo(transcript().scrollHeight);
  await expect.poll(() => previous.querySelector('[data-testid="preview-clip"]')).not.toBeNull();
});
