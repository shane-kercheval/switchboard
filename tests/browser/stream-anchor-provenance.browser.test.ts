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
  transcriptContainer as transcript,
  distanceFromBottom,
} from "./harness";
import { setProjectCompact, toggleKey } from "$lib/state/transcriptPreview.svelte";
import { ALICE, BOB, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";

// Provenance for in-anchor height changes while a stream is live. A fan-out
// block holds settled columns ALONGSIDE a streaming sibling, so "the anchor
// block contains a live region" is membership, not provenance — a user toggle
// in such a block must keep the strict "clicked control stays put" (gap-hold)
// contract, while genuine stream append gets anchor-restore (reading position
// holds). The discriminators under test: the transcript revision (chunks bump
// it, toggles never do), click intent (a click inside the transcript wins over
// a chunk landing in the same pass), and the captured had-live-region flag
// (completion transitions where the live wrapper unmounts before the pass).

function scrollTo(top: number): void {
  const c = transcript();
  c.scrollTop = top;
  c.dispatchEvent(new Event("scroll"));
}

/** Document-space top of the fan-out block, for placing the viewport inside it
 * (which makes it the scroll anchor deterministically). */
function fanoutBlockTop(): number {
  const c = transcript();
  const block = page.getByTestId("fanout-group").element();
  return block.getBoundingClientRect().top - c.getBoundingClientRect().top + c.scrollTop;
}

function fanoutToggleTop(): number {
  return page
    .getByTestId("fanout-group")
    .getByTestId("turn-preview-toggle")
    .element()
    .getBoundingClientRect().top;
}

beforeEach(() => {
  resetState();
});

test("a state-driven column toggle under a streaming sibling holds the reading gap", async () => {
  // Revision provenance alone (no click involved — the toggle is driven at the
  // state level, like scroll-hold's collapse test, so the clicked-control hold
  // never arms): the resize pass sees a live region in the anchor block but NO
  // content change since the last pass, so it must take the strict gap-hold
  // path. A membership-only waiver would restore the block top instead and the
  // shrink would clamp the view to the bottom. The click-driven variant below
  // asserts exact toggle stability; this one asserts the held gap, which is
  // the enforceable contract for a non-click mutation.
  setProjectCompact(PROJECT_ID, false);
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTurns(ALICE.id, [
    // A tall settled block above keeps the transcript scrollable after the
    // collapse shrinks the fan-out.
    userTurn({ id: "top-user", agentId: ALICE.id, text: longText(80), at: "2026-05-16T00:00:00Z" }),
    userTurn({
      id: "user-alice",
      agentId: ALICE.id,
      text: "compare",
      at: "2026-05-16T00:00:01Z",
      sendId: "send-fanout",
    }),
    agentTurn({
      id: "alice-settled",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      endedAt: "2026-05-16T00:00:03Z",
      sendId: "send-fanout",
      // Long body + short final answer: expanded is tall, collapsed renders
      // only the last answer text, and the difference keeps the toggle alive.
      // Sized so the collapse stays well inside the scroll slack below the
      // viewport — shrinking past the clamp would legitimately fall back to
      // the "don't slam into the bottom" contract and move the toggle.
      items: [textItem(longText(25)), textItem("Short answer.")],
    }),
  ]);
  seedTurns(BOB.id, [
    userTurn({
      id: "user-bob",
      agentId: BOB.id,
      text: "compare",
      at: "2026-05-16T00:00:01Z",
      sendId: "send-fanout",
    }),
    agentTurn({
      id: "bob-streaming",
      agentId: BOB.id,
      at: "2026-05-16T00:00:02Z",
      status: "streaming",
      sendId: "send-fanout",
      items: [textItem(longText(3))],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  await expect.element(page.getByTestId("fanout-group")).toBeInTheDocument();
  await expect
    .element(page.getByTestId("fanout-group").getByTestId("turn-preview-toggle"))
    .toBeInTheDocument();

  // Put the viewport top INSIDE the fan-out block so it is the anchor.
  scrollTo(fanoutBlockTop() + 40);
  const heightBefore = transcript().scrollHeight;
  const gapBefore = distanceFromBottom();
  toggleKey(PROJECT_ID, `fanout:send-fanout:${ALICE.id}`, false);

  // The collapse really happened (the document shrank sharply)...
  await expect.poll(() => transcript().scrollHeight).toBeLessThan(heightBefore - 200);
  // ...and the strict gap-hold contract held the user's distance from the
  // bottom. (Pixel-exact toggle anchoring is unattainable in a grid once the
  // streaming sibling overtakes the collapsed column — the block bottom moves
  // relative to the toggle regardless of scroll position — so the held gap IS
  // the contract here.) A membership-only waiver would block-top-restore
  // instead: the shrink clamps scrollTop to the new bottom and the gap
  // collapses to ~0.
  await expect.poll(() => Math.abs(distanceFromBottom() - gapBefore)).toBeLessThan(40);
});

test("a clicked prompt expand wins over a stream chunk landing in the same pass", async () => {
  // The adversarial collision: the toggle's resize is processed in the same
  // re-anchor pass as a genuine streamed chunk, so the revision HAS changed and
  // the anchor block DOES host live regions — only click intent distinguishes
  // it. The clicked control must stay put. The columns are capped (both still
  // streaming), so the chunk itself cannot move the block's geometry — the only
  // height change is the expansion above the toggle.
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const column = (agentId: string, turnId: string, lines: number) => [
    userTurn({
      id: `user-${agentId}`,
      agentId,
      text: longText(30, "Prompt"),
      at: "2026-05-16T00:00:01Z",
      sendId: "send-fanout",
    }),
    agentTurn({
      id: turnId,
      agentId,
      at: "2026-05-16T00:00:02Z",
      status: "streaming" as const,
      sendId: "send-fanout",
      items: [textItem(longText(lines))],
    }),
  ];
  seedTurns(ALICE.id, [
    userTurn({ id: "top-a", agentId: ALICE.id, text: longText(40), at: "2026-05-16T00:00:00Z" }),
    ...column(ALICE.id, "alice-streaming", 50),
  ]);
  seedTurns(BOB.id, column(BOB.id, "bob-streaming", 50));

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  // Both columns capped and overflowing (fanoutLiveCap is on while all stream).
  const caps = page.getByTestId("turn-live-scroll");
  await expect.poll(() => caps.elements().length).toBe(2);
  for (const cap of caps.elements()) {
    await expect.poll(() => cap.scrollHeight - cap.clientHeight).toBeGreaterThan(1);
  }
  const promptToggle = page.getByTestId("fanout-group").getByTestId("turn-preview-toggle");
  await expect.element(promptToggle).toBeInTheDocument();

  scrollTo(fanoutBlockTop() + 40);
  const heightBefore = transcript().scrollHeight;
  const toggleBefore = fanoutToggleTop();

  // Real click (arms intent through the capture-phase listener), then a chunk
  // seeded synchronously — both land in one flush, the ambiguous pass.
  (promptToggle.element() as HTMLElement).click();
  seedTurns(ALICE.id, [
    userTurn({ id: "top-a", agentId: ALICE.id, text: longText(40), at: "2026-05-16T00:00:00Z" }),
    ...column(ALICE.id, "alice-streaming", 70),
  ]);

  // The prompt really expanded (the document grew)...
  await expect.poll(() => transcript().scrollHeight).toBeGreaterThan(heightBefore + 200);
  // ...and the clicked toggle held its screen position: the click wins over
  // the same-pass chunk, so the pass gap-holds instead of restoring the block
  // top (which would throw the toggle down by the expansion).
  await expect.poll(() => Math.abs(fanoutToggleTop() - toggleBefore)).toBeLessThan(8);
});

test("a clicked column toggle stays put exactly, even when the grid crosses over", async () => {
  // Same mixed fan-out as the state-driven test, but the collapse comes from a
  // real CLICK — arming the clicked-control hold. Grid geometry makes bottom-gap
  // preservation inexact here (the streaming sibling's column overtakes the
  // collapsed one, moving the block bottom relative to the toggle), but the
  // control hold corrects by the clicked element's own drift, so the toggle
  // holds to the pixel regardless.
  setProjectCompact(PROJECT_ID, false);
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTurns(ALICE.id, [
    userTurn({ id: "top-user", agentId: ALICE.id, text: longText(80), at: "2026-05-16T00:00:00Z" }),
    userTurn({
      id: "user-alice",
      agentId: ALICE.id,
      text: "compare",
      at: "2026-05-16T00:00:01Z",
      sendId: "send-fanout",
    }),
    agentTurn({
      id: "alice-settled",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      endedAt: "2026-05-16T00:00:03Z",
      sendId: "send-fanout",
      items: [textItem(longText(25)), textItem("Short answer.")],
    }),
  ]);
  seedTurns(BOB.id, [
    userTurn({
      id: "user-bob",
      agentId: BOB.id,
      text: "compare",
      at: "2026-05-16T00:00:01Z",
      sendId: "send-fanout",
    }),
    agentTurn({
      id: "bob-streaming",
      agentId: BOB.id,
      at: "2026-05-16T00:00:02Z",
      status: "streaming",
      sendId: "send-fanout",
      items: [textItem(longText(3))],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  const toggle = page.getByTestId("fanout-group").getByTestId("turn-preview-toggle");
  await expect.element(toggle).toBeInTheDocument();

  scrollTo(fanoutBlockTop() + 40);
  const heightBefore = transcript().scrollHeight;
  const toggleBefore = fanoutToggleTop();

  (toggle.element() as HTMLElement).click();

  await expect.poll(() => transcript().scrollHeight).toBeLessThan(heightBefore - 200);
  await expect.poll(() => Math.abs(fanoutToggleTop() - toggleBefore)).toBeLessThan(8);
});

test("a non-layout click does not surrender the reading position to the stream", async () => {
  // Only controls marked `data-layout-toggle` arm the clicked-control hold.
  // Clicking anything else (message text here; a copy button behaves the same)
  // must leave the stream-append attribution untouched — a leaked hold would
  // divert the next chunks' correction to gap-hold and drag a reader parked
  // inside the streaming message.
  await registerAgent(ALICE);
  const turns = (lines: number) => [
    userTurn({ id: "user-1", agentId: ALICE.id, text: "go", at: "2026-05-16T00:00:00Z" }),
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:01Z",
      status: "streaming",
      items: [textItem(longText(lines))],
    }),
  ];
  seedTurns(ALICE.id, turns(60));

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => transcript().scrollHeight > transcript().clientHeight + 400).toBe(true);

  // Read inside the streaming message (anchor = the streaming block).
  scrollTo(transcript().scrollHeight - transcript().clientHeight - 200);
  const scrollTopBefore = transcript().scrollTop;

  // Click the streamed text itself — no layout-toggle ancestor.
  const paragraph = transcript().querySelector('[data-testid="turn-live-scroll"] p');
  (paragraph as HTMLElement).click();

  // Two growth rounds: the reading position must hold both times.
  seedTurns(ALICE.id, turns(75));
  await expect.poll(() => Math.abs(transcript().scrollTop - scrollTopBefore)).toBeLessThan(8);
  seedTurns(ALICE.id, turns(90));
  await expect.poll(() => Math.abs(transcript().scrollTop - scrollTopBefore)).toBeLessThan(8);
});

test("a settled toggle click does not tax a later chunk", async () => {
  // The hold must expire cleanly: click a real layout toggle (the clipped user
  // message above), let the resize settle, then stream. The leftover held pass
  // sees zero drift on the toggle (nothing above it changed) and the chunks
  // after that take the ordinary stream-append path — the reading position
  // must hold throughout. Under the old global two-pass counter, the leftover
  // pass gap-held the chunk and dragged the reader.
  await registerAgent(ALICE);
  const turns = (lines: number) => [
    userTurn({
      id: "user-long",
      agentId: ALICE.id,
      text: longText(40),
      at: "2026-05-16T00:00:00Z",
    }),
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:01Z",
      status: "streaming",
      items: [textItem(longText(lines))],
    }),
  ];
  seedTurns(ALICE.id, turns(60));

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
  // Compact default clips the long user message and gives it the only toggle.
  const toggle = page.getByTestId("turn-preview-toggle");
  await expect.element(toggle).toBeInTheDocument();
  await expect.poll(() => transcript().scrollHeight > transcript().clientHeight + 400).toBe(true);

  // Read inside the streaming message, below the clipped prompt.
  scrollTo(transcript().scrollHeight - transcript().clientHeight - 250);
  const toggleBefore = (toggle.element() as HTMLElement).getBoundingClientRect().top;
  const heightBefore = transcript().scrollHeight;

  // Expand the prompt (real click, offscreen above — programmatic click needs
  // no visibility). The control hold keeps the toggle put, which equally keeps
  // the reading position below it still.
  (toggle.element() as HTMLElement).click();
  await expect.poll(() => transcript().scrollHeight).toBeGreaterThan(heightBefore + 200);
  await expect
    .poll(() =>
      Math.abs((toggle.element() as HTMLElement).getBoundingClientRect().top - toggleBefore),
    )
    .toBeLessThan(8);

  // Now stream: the settled position must survive both the pass that consumes
  // the leftover hold (zero drift) and the ordinary passes after it.
  const settled = transcript().scrollTop;
  seedTurns(ALICE.id, turns(75));
  await expect.poll(() => Math.abs(transcript().scrollTop - settled)).toBeLessThan(8);
  seedTurns(ALICE.id, turns(90));
  await expect.poll(() => Math.abs(transcript().scrollTop - settled)).toBeLessThan(8);
});

test("stream completion holds the reading position for a reader inside the block", async () => {
  // The live→settled transition: completing a column removes its live wrapper
  // BEFORE the re-anchor pass runs, and dropping the caps ungates the sibling
  // too — the block below the reader balloons. The captured had-live-region
  // flag must let the pass restore the block top (reading position holds)
  // instead of gap-holding, which would shove the view down by the growth.
  setProjectCompact(PROJECT_ID, false);
  await registerAgent(ALICE);
  await registerAgent(BOB);
  const aliceTurns = (status: "streaming" | "complete") => [
    userTurn({ id: "top-user", agentId: ALICE.id, text: longText(80), at: "2026-05-16T00:00:00Z" }),
    userTurn({
      id: "user-alice",
      agentId: ALICE.id,
      text: longText(30, "Prompt"),
      at: "2026-05-16T00:00:01Z",
      sendId: "send-fanout",
    }),
    agentTurn({
      id: "alice-turn",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      status,
      ...(status === "complete" ? { endedAt: "2026-05-16T00:00:05Z" } : {}),
      sendId: "send-fanout",
      items: [textItem(longText(50))],
    }),
  ];
  seedTurns(ALICE.id, aliceTurns("streaming"));
  seedTurns(BOB.id, [
    userTurn({
      id: "user-bob",
      agentId: BOB.id,
      text: longText(30, "Prompt"),
      at: "2026-05-16T00:00:01Z",
      sendId: "send-fanout",
    }),
    agentTurn({
      id: "bob-streaming",
      agentId: BOB.id,
      at: "2026-05-16T00:00:02Z",
      status: "streaming",
      sendId: "send-fanout",
      items: [textItem(longText(50))],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });
  await expect.poll(() => page.getByTestId("turn-live-scroll").elements().length).toBe(2);

  // Read inside the block: viewport top 100px in (the prompt area). The unpin
  // scroll also captures the anchor — and with it the had-live-region flag.
  scrollTo(fanoutBlockTop() + 100);
  const scrollTopBefore = transcript().scrollTop;

  // ALICE completes in place; her live wrapper unmounts and the cap gating
  // flips (a settled column ungates the fan-out's live caps).
  seedTurns(ALICE.id, aliceTurns("complete"));

  await expect.poll(() => page.getByTestId("turn-live-scroll").elements().length).toBe(1);
  // The block grew below the reader (caps dropped)...
  await expect
    .poll(() => transcript().scrollHeight - transcript().clientHeight)
    .toBeGreaterThan(scrollTopBefore + 100);
  // ...and the reading position held still.
  await expect.poll(() => Math.abs(transcript().scrollTop - scrollTopBefore)).toBeLessThan(8);
});
