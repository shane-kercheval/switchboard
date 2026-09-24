import { beforeEach, expect, test, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
// Pins persist through IPC: `pinned` holds the backend's pin list.
let pinned: string[] = [];
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    const list = () => pinned.map((key) => ({ key, pinned_at: "2026-05-16T01:00:00Z" }));
    if (cmd === "list_message_pins") return list();
    if (cmd === "set_message_pin") {
      const key = String(args?.key);
      pinned = args?.pinned === true ? [...pinned, key] : pinned.filter((k) => k !== key);
      return list();
    }
    return null;
  }),
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
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";
import type { Turn } from "$lib/state/index.svelte";
import { _testing as pinState } from "$lib/state/messagePins.svelte";
import { EXPANDED_RECENT_SENDS } from "$lib/state/unified";

// Pinning keeps a message expanded, so a pin click can reshape the transcript
// under the cursor. Pinning an older collapsed message expands it — the clicked
// button must hold its place like any expand control. Unpinning collapses
// nothing under the click; the message returns to the normal rules at the next
// update at the bottom. A pin click that changes nothing must not disturb a
// later streamed chunk.

const OLD = "agent-old";
const OLD_KEY = `agent:hydration:${ALICE.id}:hk-old`;

function exchange(n: number, reply: string, hydrationKey?: string): Turn[] {
  const at = `2026-05-16T00:00:${String(n * 10).padStart(2, "0")}Z`;
  return [
    userTurn({
      id: `user-${n}`,
      agentId: ALICE.id,
      text: longText(12, `Prompt ${n}`),
      at,
      sendId: `send-${n}`,
    }),
    agentTurn({
      id: n === 0 ? OLD : `agent-${n}`,
      agentId: ALICE.id,
      at,
      endedAt: at,
      sendId: `send-${n}`,
      items: [textItem(reply)],
      ...(hydrationKey ? { hydrationKey } : {}),
    }),
  ];
}

/** An older long reply outside the recent range, then the recent sends. */
function transcriptTurns(): Turn[] {
  return [
    ...exchange(0, longText(60), "hk-old"),
    ...Array.from({ length: EXPANDED_RECENT_SENDS }, (_, i) =>
      exchange(i + 1, longText(15), `hk-${i + 1}`),
    ).flat(),
  ];
}

function unit(turnId: string): HTMLElement {
  return document.querySelector(`[data-preview-key="agent:${turnId}"]`) as HTMLElement;
}

function pinButton(turnId: string): HTMLElement | null {
  return unit(turnId)?.querySelector('[data-testid="message-pin"]') ?? null;
}

function viewportTop(el: HTMLElement): number {
  return el.getBoundingClientRect().top - transcript().getBoundingClientRect().top;
}

/** Scroll so the prompt above the old reply is the block at the top of the view,
 * leaving the old reply mid-viewport — not the anchor block. */
function scrollToOldReply(): void {
  const prompt = document.querySelector('[data-preview-key="user:u:send-0"]') as HTMLElement;
  const c = transcript();
  const top = prompt.getBoundingClientRect().top - c.getBoundingClientRect().top + c.scrollTop;
  userScrollTo(c, top + 40);
}

beforeEach(() => {
  resetState();
  pinState.reset();
  pinned = [];
});

test("pinning a collapsed older reply expands it and keeps the pin button in place", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, transcriptTurns());
  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  await expect.poll(() => pinButton(OLD)).not.toBeNull();
  expect(unit(OLD).querySelector('[data-testid="preview-clip"]')).not.toBeNull();
  scrollToOldReply();
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(200);
  const button = pinButton(OLD)!;
  const before = viewportTop(button);
  expect(before).toBeGreaterThan(0);
  expect(before).toBeLessThan(transcript().clientHeight);

  button.click();

  await expect.poll(() => unit(OLD).querySelector('[data-testid="preview-clip"]')).toBeNull();
  await expect.poll(() => Math.abs(viewportTop(pinButton(OLD)!) - before)).toBeLessThan(8);
});

test("unpinning while scrolled up moves nothing; the reply collapses back at the bottom", async () => {
  pinned = [OLD_KEY];
  await registerAgent(ALICE);
  seedTurns(ALICE.id, transcriptTurns());
  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  await expect.poll(() => pinButton(OLD)?.getAttribute("aria-pressed")).toBe("true");
  await expect.poll(() => unit(OLD).querySelector('[data-testid="preview-clip"]')).toBeNull();
  scrollToOldReply();
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(200);
  const before = viewportTop(pinButton(OLD)!);

  pinButton(OLD)!.click();
  await expect.poll(() => pinButton(OLD)?.getAttribute("aria-pressed")).toBe("false");

  // Still expanded, still in place. What's expanded re-derives in the same
  // update that flips the pressed state polled above, so no wait is needed.
  expect(unit(OLD).querySelector('[data-testid="preview-clip"]')).toBeNull();
  expect(Math.abs(viewportTop(pinButton(OLD)!) - before)).toBeLessThan(8);

  userScrollTo(transcript(), transcript().scrollHeight);
  await expect.poll(() => unit(OLD).querySelector('[data-testid="preview-clip"]')).not.toBeNull();
});

test("a pin click that changes nothing does not disturb a later streamed chunk", async () => {
  await registerAgent(ALICE);
  const streaming = (lines: number): Turn[] => [
    userTurn({
      id: "user-s",
      agentId: ALICE.id,
      text: "stream please",
      at: "2026-05-16T00:01:00Z",
      sendId: "send-s",
    }),
    agentTurn({
      id: "agent-s",
      agentId: ALICE.id,
      at: "2026-05-16T00:01:00Z",
      status: "streaming",
      sendId: "send-s",
      items: [textItem(longText(lines))],
    }),
  ];
  const base = transcriptTurns();
  seedTurns(ALICE.id, [...base, ...streaming(20)]);
  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  // A recent reply is already expanded: pinning it resizes nothing.
  const recent = `agent-${EXPANDED_RECENT_SENDS}`;
  await expect.poll(() => pinButton(recent)).not.toBeNull();
  const c = transcript();
  const recentTop =
    unit(recent).getBoundingClientRect().top - c.getBoundingClientRect().top + c.scrollTop;
  userScrollTo(c, recentTop - 40);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
  const before = viewportTop(unit(recent));

  pinButton(recent)!.click();
  await expect.poll(() => pinButton(recent)?.getAttribute("aria-pressed")).toBe("true");
  expect(Math.abs(viewportTop(unit(recent)) - before)).toBeLessThan(8);

  // The stream grows below the reader: the reading position must hold.
  seedTurns(ALICE.id, [...base, ...streaming(40)]);
  await expect.poll(() => Math.abs(viewportTop(unit(recent)) - before)).toBeLessThan(8);
  seedTurns(ALICE.id, [...base, ...streaming(60)]);
  await expect.poll(() => Math.abs(viewportTop(unit(recent)) - before)).toBeLessThan(8);
});
