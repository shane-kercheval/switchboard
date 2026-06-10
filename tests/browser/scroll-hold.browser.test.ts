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
import { setProjectCompact, toggleKey } from "$lib/state/transcriptPreview.svelte";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";

// Behavior 3 — the chat-app scroll contract, all measured in real WebKit (the
// Tauri webview has NO native CSS scroll-anchoring, so the component re-anchors
// itself; jsdom has no scroll geometry at all). A bare `scroll` (scrollbar or
// keyboard — no wheel/touch) with unchanged height unpins and the view holds
// when new content arrives; a content-change-induced scroll (a collapse clamping
// scrollTop) must NOT unpin; returning to the bottom re-pins.

function transcript(): HTMLElement {
  return page.getByTestId("unified-transcript").element() as HTMLElement;
}
function distanceFromBottom(): number {
  const c = transcript();
  return c.scrollHeight - c.scrollTop - c.clientHeight;
}
function scrollTo(top: number): void {
  const c = transcript();
  c.scrollTop = top;
  c.dispatchEvent(new Event("scroll")); // bare scroll: scrollbar/keyboard shape
}

beforeEach(() => {
  resetState();
});

test("a bare scroll up unpins and the view holds when new content arrives", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({ id: "agent-1", agentId: ALICE.id, items: [textItem(longText(20))] }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => transcript().scrollHeight > transcript().clientHeight + 50).toBe(true);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32); // starts pinned

  // Scrollbar/keyboard scroll to the top: this MUST unpin (escape auto-follow).
  scrollTo(0);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);

  // New content arrives at the bottom — the view must hold near the top, not get
  // yanked down to follow it.
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({ id: "agent-1", agentId: ALICE.id, items: [textItem(longText(20))] }),
    agentTurn({
      id: "agent-2",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:09Z",
      items: [textItem(longText(20))],
    }),
  ]);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
});

test("a content-change (collapse) clamp does not unpin", async () => {
  // Companion coverage for the content-change scroll contract. NOTE: in WebKit
  // this case is also protected by the ResizeObserver re-anchor (it corrects
  // scrollTop before the clamp's `scroll` event fires), so it does NOT go red on
  // its own when the `onScroll` content-change gate is removed — the gate's
  // discriminating red guard lives in streaming-pin.browser.test.ts (the live cap
  // dropping on completion, where `scrollSignal` and the clamp race). This test
  // still pins the user-visible contract: a user who scrolled up stays put when a
  // message below collapses.
  setProjectCompact(PROJECT_ID, false); // expanded, so the turns are tall
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({ id: "agent-1", agentId: ALICE.id, items: [textItem(longText(40))] }),
    agentTurn({
      id: "agent-2",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:05Z",
      items: [textItem(longText(40))],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => transcript().scrollHeight > transcript().clientHeight + 400).toBe(true);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32); // pinned

  // Scroll up to a clear gap from the bottom (unpinned).
  const c = transcript();
  scrollTo(c.scrollHeight - c.clientHeight - 150);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(120);

  // Collapse the bottom response — content shrinks sharply, the browser clamps
  // scrollTop. The view must HOLD its gap (stay unpinned), not snap to the bottom.
  toggleKey(PROJECT_ID, "agent:agent-2", false);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(50);
});

test("scrolling back to the bottom re-pins and follows new content", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({ id: "agent-1", agentId: ALICE.id, items: [textItem(longText(20))] }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => transcript().scrollHeight > transcript().clientHeight + 50).toBe(true);

  // Scroll up (unpin), then back to the bottom (re-pin). The return scroll is
  // same-height, so `onScroll` reads it as a user scroll and recomputes `pinned`
  // true — the poll confirms no later re-anchor un-pinned the view in between.
  scrollTo(0);
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);
  scrollTo(transcript().scrollHeight);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // New content now follows, because we're pinned again.
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({ id: "agent-1", agentId: ALICE.id, items: [textItem(longText(20))] }),
    agentTurn({
      id: "agent-2",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:09Z",
      items: [textItem(longText(20))],
    }),
  ]);
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
});
