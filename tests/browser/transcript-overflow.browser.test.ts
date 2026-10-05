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
import {
  ALICE,
  PROJECT_ID,
  agentTurn,
  longText,
  paddingSends,
  textItem,
  userTurn,
} from "./fixtures";
import { EXPANDED_RECENT_SENDS } from "$lib/state/unified";

// Behavior 1: a message/response that genuinely overflows the clip gets a
// collapse toggle; one that fits gets none. This is the assertion jsdom CANNOT
// make — there `max-height` is parsed but never applied, so
// `scrollHeight === clientHeight` (both 0) and the overflow is invisible. The
// first test is also the canonical shape later specs copy: mount via the
// harness, seed state, poll measured geometry.

beforeEach(() => {
  resetState();
});

// The message under test is followed by padding sends so it falls outside the
// recent-sends range, which opens expanded; older prompts are the clipped ones.

test("a long user message overflows the clip and gets a collapse toggle (compact default)", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText() }),
    ...paddingSends(ALICE.id, EXPANDED_RECENT_SENDS),
  ]);

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
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: "short and sweet" }),
    ...paddingSends(ALICE.id, EXPANDED_RECENT_SENDS),
  ]);

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

test("a non-latest agent response that overflows gets a toggle", async () => {
  // The agent's latest response renders as the full latest-response view (no clip);
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

  // Exactly one clip (the earlier, non-latest response); it overflows.
  await expect.element(page.getByTestId("preview-clip")).toBeInTheDocument();
  await expect
    .poll(() => {
      const el = page.getByTestId("preview-clip").element() as HTMLElement;
      return el.scrollHeight - el.clientHeight;
    })
    .toBeGreaterThan(1);
  await expect.element(page.getByTestId("turn-preview-toggle")).toBeInTheDocument();
});

// Behavior 2: the bottom fade is a claim that text is hidden, so it may only
// appear when text is actually hidden. The gradient's first stop is at 7rem and
// the cap is at 14rem, which leaves a band where a message is fully visible and
// was being faded anyway — with no toggle, since nothing overflowed to expand.

/// Computed mask on the clip, normalized across the two property names WebKit
/// reports. "none" is the fade being off.
function clipMask(): string {
  const el = page.getByTestId("preview-clip").element() as HTMLElement;
  const style = getComputedStyle(el);
  const mask = style.maskImage;
  if (mask !== undefined && mask !== "") return mask;
  return style.webkitMaskImage;
}

test("a message past the fade's first stop but inside the cap is not faded", async () => {
  await registerAgent(ALICE);
  // Tall enough to reach into the gradient (7rem ≈ 112px), short enough to fit
  // the 14rem cap. The height assertion below pins it to that band, so a future
  // line-height change fails here rather than silently testing a short message.
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(6) }),
    ...paddingSends(ALICE.id, EXPANDED_RECENT_SENDS),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.element(page.getByTestId("preview-clip")).toBeInTheDocument();

  await expect
    .poll(() => {
      const el = page.getByTestId("preview-clip").element() as HTMLElement;
      return el.scrollHeight - el.clientHeight;
    })
    .toBeLessThanOrEqual(1);

  const height = (page.getByTestId("preview-clip").element() as HTMLElement).clientHeight;
  expect(height).toBeGreaterThan(112);
  expect(height).toBeLessThanOrEqual(224);

  expect(page.getByTestId("turn-preview-toggle").elements()).toHaveLength(0);
  expect(clipMask()).toBe("none");
});

test("a message that overflows the cap keeps its fade", async () => {
  // The other half: the fade still marks hidden text, so switching it off for
  // the fitting case cannot have switched it off everywhere.
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText() }),
    ...paddingSends(ALICE.id, EXPANDED_RECENT_SENDS),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.element(page.getByTestId("turn-preview-toggle")).toBeInTheDocument();
  await expect.poll(() => clipMask()).not.toBe("none");
});

// Behavior 3: a prompt in the recent-sends range opens expanded, but a long one
// must still be collapsible — its toggle comes from measuring the unclipped body
// against the cap, which jsdom cannot do.

test("a long prompt in the recent range opens expanded with a collapse toggle", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [userTurn({ id: "user-1", agentId: ALICE.id, text: longText() })]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  const toggle = page.getByTestId("turn-preview-toggle");
  await expect.element(toggle).toHaveAttribute("aria-label", "Collapse");
  expect(page.getByTestId("preview-clip").elements()).toHaveLength(0);

  await toggle.click();
  await expect.element(page.getByTestId("preview-clip")).toBeInTheDocument();
  await expect.element(toggle).toHaveAttribute("aria-label", "Expand");
});

test("a short prompt in the recent range opens expanded with no toggle", async () => {
  await registerAgent(ALICE);
  // The short prompt sits past the fade's first stop but inside the cap:
  // collapsing would hide nothing. The long prompt beside it is the positive
  // control — its toggle appearing proves the unclipped measurement has run.
  seedTurns(ALICE.id, [
    userTurn({ id: "user-long", agentId: ALICE.id, text: longText(), sendId: "send-long" }),
    userTurn({
      id: "user-short",
      agentId: ALICE.id,
      text: longText(6),
      at: "2026-05-16T00:00:05Z",
      sendId: "send-short",
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  const [longTurn, shortTurn] = userTurns();
  await expect
    .poll(() => longTurn!.querySelector('[data-testid="turn-preview-toggle"]'))
    .not.toBeNull();
  expect(page.getByTestId("preview-clip").elements()).toHaveLength(0);
  expect(shortTurn!.querySelector('[data-testid="turn-preview-toggle"]')).toBeNull();
});

// Behavior 4: a reply in the recent range opens expanded, and a long one must be
// collapsible even when it is a single block — collapsing clips it to the cap.

test("a long single-block reply in the recent range offers Collapse and clips when collapsed", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-long", agentId: ALICE.id, text: "long please", sendId: "send-long" }),
    agentTurn({
      id: "agent-long",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:01Z",
      endedAt: "2026-05-16T00:00:02Z",
      sendId: "send-long",
      items: [textItem(longText())],
    }),
    userTurn({
      id: "user-short",
      agentId: ALICE.id,
      text: "short please",
      at: "2026-05-16T00:00:05Z",
      sendId: "send-short",
    }),
    agentTurn({
      id: "agent-short",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:06Z",
      endedAt: "2026-05-16T00:00:07Z",
      sendId: "send-short",
      items: [textItem("the short reply")],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  const [longReply, shortReply] = agentTurns();
  // The toggle must exist BEFORE any click — measured on the expanded reply.
  await expect
    .poll(() =>
      longReply!.querySelector('[data-testid="turn-preview-toggle"]')?.getAttribute("aria-label"),
    )
    .toBe("Collapse");
  expect(shortReply!.querySelector('[data-testid="turn-preview-toggle"]')).toBeNull();

  (longReply!.querySelector('[data-testid="turn-preview-toggle"]') as HTMLElement).click();
  await expect
    .poll(() => {
      const clip = longReply!.querySelector('[data-testid="preview-clip"]');
      return clip instanceof HTMLElement ? clip.scrollHeight - clip.clientHeight : 0;
    })
    .toBeGreaterThan(1);
  expect(
    longReply!.querySelector('[data-testid="turn-preview-toggle"]')?.getAttribute("aria-label"),
  ).toBe("Expand");
});

function userTurns(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>('[data-testid="turn"][data-role="user"]')];
}

function agentTurns(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>('[data-testid="turn"][data-role="agent"]')];
}
