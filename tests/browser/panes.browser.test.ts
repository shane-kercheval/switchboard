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
  MIN_PANE_WIDTH_PX,
  moveAgentToNewPane,
  _testing as panesState,
} from "$lib/state/transcriptPanes.svelte";
import {
  selectionFor,
  setRecipients,
  _testing as selectionState,
} from "$lib/state/recipientSelection.svelte";
import { setProjectCompact } from "$lib/state/transcriptPreview.svelte";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";
import type { AgentRecord } from "$lib/types";

// Multi-pane layout behaviors that only exist with real geometry: gutter-drag
// resizing, the min-width clamp, per-pane scroll independence (two
// UnifiedTranscript anchors in one row), and Cmd+click targeting in real
// WebKit (no native side effect may swallow or reinterpret the modifier).

const BOB: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: PROJECT_ID,
  name: "bob",
  harness: "codex",
  session_locator: null,
  created_at: "2026-05-16T00:00:01Z",
};
const ROSTER_IDS = [ALICE.id, BOB.id];

function paneEl(i: number): HTMLElement {
  return page.getByTestId("transcript-pane").nth(i).element() as HTMLElement;
}
function paneWidth(i: number): number {
  return paneEl(i).getBoundingClientRect().width;
}
function gutterEl(): HTMLElement {
  return page.getByTestId("pane-gutter-1").element() as HTMLElement;
}

function dragGutterTo(clientX: number): void {
  const gutter = gutterEl();
  const rect = gutter.getBoundingClientRect();
  gutter.dispatchEvent(
    new PointerEvent("pointerdown", { clientX: rect.left, clientY: 300, bubbles: true }),
  );
  window.dispatchEvent(new PointerEvent("pointermove", { clientX, clientY: 300 }));
  window.dispatchEvent(new PointerEvent("pointerup", { clientX, clientY: 300 }));
}

async function seedTwoAgents(): Promise<void> {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({ id: "agent-1", agentId: ALICE.id, items: [textItem(longText(20))] }),
  ]);
  seedTurns(BOB.id, [
    userTurn({ id: "user-2", agentId: BOB.id, text: longText(20) }),
    agentTurn({ id: "agent-2", agentId: BOB.id, items: [textItem(longText(20))] }),
  ]);
}

beforeEach(() => {
  resetState();
  panesState.reset();
  selectionState.reset();
  setProjectCompact(PROJECT_ID, false);
});

test("dragging the gutter resizes both panes; total width is conserved", async () => {
  await seedTwoAgents();
  moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
  mountPanes({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 1000 });

  // Equal split to start (within a few px for the gutter itself).
  await expect.poll(() => Math.abs(paneWidth(0) - paneWidth(1))).toBeLessThan(12);
  const before0 = paneWidth(0);

  const rowLeft = paneEl(0).getBoundingClientRect().left;
  dragGutterTo(rowLeft + 620);

  await expect.poll(() => paneWidth(0)).toBeGreaterThan(before0 + 80);
  await expect
    .poll(() => Math.abs(paneWidth(0) + paneWidth(1) - (1000 - gutterEl().offsetWidth)))
    .toBeLessThan(8);
});

test("the min-width clamp holds: a gutter dragged past the floor stops there", async () => {
  await seedTwoAgents();
  moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
  mountPanes({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 1000 });
  await expect.poll(() => paneWidth(0)).toBeGreaterThan(100);

  const rowLeft = paneEl(0).getBoundingClientRect().left;
  dragGutterTo(rowLeft + 50); // far past the 360px floor

  await expect.poll(() => paneWidth(0)).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX - 2);
});

test("two panes scroll independently: one held off-bottom, the other stays pinned", async () => {
  await seedTwoAgents();
  moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
  mountPanes({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 1000 });

  const transcriptOf = (i: number): HTMLElement =>
    paneEl(i).querySelector('[data-testid="unified-transcript"]') as HTMLElement;
  const distanceFromBottom = (c: HTMLElement): number =>
    c.scrollHeight - c.scrollTop - c.clientHeight;

  await expect
    .poll(() => transcriptOf(0).scrollHeight > transcriptOf(0).clientHeight + 50)
    .toBe(true);
  await expect.poll(() => distanceFromBottom(transcriptOf(0))).toBeLessThan(32);
  await expect.poll(() => distanceFromBottom(transcriptOf(1))).toBeLessThan(32);

  // Scroll pane A to the top (bare scroll: unpins A only).
  transcriptOf(0).scrollTop = 0;
  transcriptOf(0).dispatchEvent(new Event("scroll"));
  await expect.poll(() => distanceFromBottom(transcriptOf(0))).toBeGreaterThan(100);

  // New content lands in BOTH panes: A holds its place, B stays pinned.
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(20) }),
    agentTurn({ id: "agent-1", agentId: ALICE.id, items: [textItem(longText(20))] }),
    agentTurn({
      id: "agent-3",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:09Z",
      items: [textItem(longText(20))],
    }),
  ]);
  seedTurns(BOB.id, [
    userTurn({ id: "user-2", agentId: BOB.id, text: longText(20) }),
    agentTurn({ id: "agent-2", agentId: BOB.id, items: [textItem(longText(20))] }),
    agentTurn({
      id: "agent-4",
      agentId: BOB.id,
      at: "2026-05-16T00:00:09Z",
      items: [textItem(longText(20))],
    }),
  ]);

  await expect.poll(() => distanceFromBottom(transcriptOf(0))).toBeGreaterThan(100);
  await expect.poll(() => distanceFromBottom(transcriptOf(1))).toBeLessThan(32);
});

test("Cmd+click targets the pane in real WebKit; plain click never re-targets", async () => {
  await seedTwoAgents();
  moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
  setRecipients(PROJECT_ID, [BOB.id]);
  mountPanes({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 1000 });

  // A real modifier-click on the pane body (not the header).
  await page
    .getByTestId("transcript-pane")
    .nth(0)
    .click({ modifiers: ["Meta"] });
  await expect.poll(() => selectionFor(PROJECT_ID)).toEqual([ALICE.id]);

  // Plain click elsewhere in pane B: reading never re-aims the draft.
  await page.getByTestId("transcript-pane").nth(1).click();
  await expect.poll(() => selectionFor(PROJECT_ID)).toEqual([ALICE.id]);
});
