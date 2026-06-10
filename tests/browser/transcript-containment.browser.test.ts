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
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";
import { buildLargeTranscript } from "$lib/dev/largeTranscript";
import type { AgentRecord } from "$lib/types";

// The containment spike's executable answers (plan M3): with
// `content-visibility: auto` on every transcript block, off-screen history
// must contribute placeholder geometry (that's the perf win), while every
// behavior that depends on real measurement — bottom anchoring, the
// measureClip-driven collapse toggles, the streaming inner pin, the
// remembered-size anti-jitter cache — must still hold when blocks
// materialize. These run against the same generator the dev seeding hook
// uses, so CI exercises the DOM the manual protocol measures.
//
// Deliberately NOT covered here: estimate-error jitter while scrolling
// GRADUALLY through never-rendered history. Any displacement bound chosen
// before the engine's materialization timing is characterized would be an
// arbitrary-threshold flake; the M3 protocol's manual scroll-up step covers
// it (both compact modes), and its observed signature is what a future
// automated bound should be calibrated against.

const BOB: AgentRecord = {
  ...ALICE,
  id: "00000000-0000-7000-8000-000000000bbb",
  name: "bob",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000002" },
};

/// The block's DECLARED `contain-intrinsic-size` length (the pre-first-render
/// placeholder), read from computed style so assertions track the per-kind
/// estimates instead of hardcoding them. Note: this is always the declared
/// value — the engine's remembered size is not exposed here.
function declaredPlaceholderPx(el: HTMLElement): number {
  const style = getComputedStyle(el) as CSSStyleDeclaration & {
    containIntrinsicHeight?: string;
    containIntrinsicSize?: string;
  };
  const raw = style.containIntrinsicHeight ?? style.containIntrinsicSize ?? "";
  const match = /([\d.]+)px/.exec(raw);
  if (match === null) throw new Error(`could not parse contain-intrinsic-size from "${raw}"`);
  return Number.parseFloat(match[1]!);
}

function firstBlock(): HTMLElement {
  return page.getByTestId("transcript-block").first().element() as HTMLElement;
}

beforeEach(() => {
  resetState();
});

test("large fixture: blocks are contained and the mount lands at the bottom", async () => {
  await registerAgent(ALICE);
  const seeded = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 120 });
  seedTurns(ALICE.id, seeded[ALICE.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  await expect.element(page.getByTestId("transcript-block").first()).toBeInTheDocument();

  // The containment class is applied and the engine honors it (this suite runs
  // a Safari-18+ WebKit; older engines would report "visible" and the perf win
  // silently wouldn't exist). The proof that off-screen blocks are *genuinely
  // skipped* lives in the remembered-size test below, whose probe block has a
  // placeholder that measurably differs from its real height — the per-kind
  // estimates are tuned to match typical blocks, so generator blocks can't
  // discriminate skipped from rendered.
  expect(getComputedStyle(firstBlock()).contentVisibility).toBe("auto");

  // Mount lands bottom-anchored despite off-screen heights being estimates.
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
});

test("a never-visited block sits at its declared placeholder; once visited it keeps its real height (auto remembered size)", async () => {
  // The anti-jitter mechanism the estimates rely on: `auto` makes the engine
  // reuse a block's real rendered height after first layout instead of the
  // declared estimate. Engine behavior, version-sensitive — if it regressed,
  // first-scroll jitter would return with no other test failing.
  await registerAgent(ALICE);
  // Probe block: a mid-length user message deliberately TALLER than the
  // user-row placeholder (4rem) but under the 14rem compact clip, so skipped
  // (placeholder), rendered (real), and remembered (real) heights are
  // measurably distinct. Timestamped before the generator's 2026-01-01 base
  // so it sorts first.
  const probe = userTurn({
    id: "probe-tall-user",
    agentId: ALICE.id,
    text: longText(8),
    at: "2025-12-31T00:00:00Z",
  });
  const history = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 60 })[ALICE.id]!;
  seedTurns(ALICE.id, [probe, ...history]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Never visited: the off-screen probe contributes its declared placeholder,
  // not its (taller) real height — the skipping is genuinely active.
  const placeholder = declaredPlaceholderPx(firstBlock());
  await expect
    .poll(() => Math.abs(firstBlock().getBoundingClientRect().height - placeholder))
    .toBeLessThanOrEqual(1);

  // Visit it: materializes at its real height.
  transcriptContainer().scrollTop = 0;
  await expect
    .poll(() => firstBlock().getBoundingClientRect().height)
    .toBeGreaterThan(placeholder + 8);
  const visitedHeight = firstBlock().getBoundingClientRect().height;

  // Leave again: skipped once more, but now at the remembered real height —
  // NOT back down to the declared placeholder.
  const c = transcriptContainer();
  c.scrollTop = c.scrollHeight;
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);
  await expect
    .poll(() => Math.abs(firstBlock().getBoundingClientRect().height - visitedHeight))
    .toBeLessThanOrEqual(2);
});

test("a clipped unit scrolled into view materializes and grows its collapse toggle", async () => {
  // Off-screen, a skipped block's measureClip ResizeObserver has nothing real
  // to measure — acceptable (no toggle is visible anyway). The behavior that
  // must hold: scrolling the unit into view materializes it, the observer
  // fires, and the toggle appears.
  await registerAgent(ALICE);
  const seeded = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 120 });
  seedTurns(ALICE.id, seeded[ALICE.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Jump to the top of history (generator step 1 is a 40-line response — a
  // guaranteed clip overflow — and far from the last block).
  transcriptContainer().scrollTop = 0;

  await expect
    .poll(() => {
      const c = transcriptContainer().getBoundingClientRect();
      return page
        .getByTestId("turn-preview-toggle")
        .elements()
        .some((el) => {
          const r = el.getBoundingClientRect();
          return r.top >= c.top && r.bottom <= c.bottom && r.height > 0;
        });
    })
    .toBe(true);
});

test("an unfollowed streaming unit keeps its inner pin and content through skip and return", async () => {
  // The one inner-scroll case (liveScroll exists only while streaming): the
  // user scrolls away, the live-capped unit leaves the viewport and becomes
  // skip-eligible while it keeps growing, then the user scrolls back — the
  // inner region must still be bottom-pinned on the newest content, with
  // nothing missing.
  await registerAgent(ALICE);
  const history = buildLargeTranscript({ agentIds: [ALICE.id], exchanges: 40 })[ALICE.id]!;
  const streaming = (length: number) =>
    agentTurn({
      id: "live-1",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:00Z",
      status: "streaming",
      items: [textItem(longText(length))],
    });
  seedTurns(ALICE.id, [...history, streaming(40)]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.element(page.getByTestId("turn-live-scroll")).toBeInTheDocument();
  await expect.poll(() => distanceFromBottom()).toBeLessThan(32);

  // Scroll away: the live block leaves the viewport.
  transcriptContainer().scrollTop = 0;
  await expect.poll(() => distanceFromBottom()).toBeGreaterThan(100);

  // It keeps streaming while off-screen.
  seedTurns(ALICE.id, [...history, streaming(80)]);

  // Return to the bottom. The scroll-down is INSIDE the poll on purpose: the
  // grown content keeps changing scrollHeight between iterations, so each
  // pass re-targets the latest bottom and the assertion is that we settle
  // there — a single pre-poll assignment could land on a stale height.
  await expect
    .poll(() => {
      const c = transcriptContainer();
      c.scrollTop = c.scrollHeight;
      return distanceFromBottom();
    })
    .toBeLessThan(32);

  // Materialized again: full content present, inner pin on the newest line.
  const cap = (): HTMLElement => page.getByTestId("turn-live-scroll").element() as HTMLElement;
  await expect.poll(() => cap().textContent?.includes("Line 80 of")).toBe(true);
  await expect
    .poll(() => {
      const el = cap();
      return el.scrollHeight - el.scrollTop - el.clientHeight;
    })
    .toBeLessThan(4);
});

test("the generator's fan-outs render as fan-out groups under containment", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const seeded = buildLargeTranscript({ agentIds: [ALICE.id, BOB.id], exchanges: 60 });
  seedTurns(ALICE.id, seeded[ALICE.id]!);
  seedTurns(BOB.id, seeded[BOB.id]!);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });

  await expect.poll(() => page.getByTestId("fanout-group").elements().length).toBeGreaterThan(0);
});
