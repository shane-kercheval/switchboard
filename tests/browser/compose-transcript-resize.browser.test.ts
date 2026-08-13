import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_prompts" || cmd === "list_workflows") return [];
    return null;
  }),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => vi.fn()) }),
}));

import { mountPanesWithComposer } from "./composeMount";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";
import { distanceFromBottom, registerAgent, resetState, seedTurns } from "./harness";
import { _testing as composeTesting } from "$lib/state/composeStore";
import { _testing as panesTesting } from "$lib/state/transcriptPanes.svelte";
import { _testing as selectionTesting } from "$lib/state/recipientSelection.svelte";

beforeEach(() => {
  resetState();
  composeTesting.reset();
  panesTesting.reset();
  selectionTesting.reset();
});

test("growing the composer keeps a pinned transcript at the bottom", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(30, "Question") }),
    agentTurn({
      id: "agent-1",
      agentId: ALICE.id,
      items: [textItem(longText(40, "Answer"))],
    }),
  ]);
  mountPanesWithComposer({ projectId: PROJECT_ID, agents: [ALICE] });

  await expect.poll(distanceFromBottom).toBeLessThan(2);
  const transcript = page.getByTestId("unified-transcript").element() as HTMLElement;
  const initialHeight = transcript.clientHeight;

  await page.getByTestId("compose-textarea").fill("one\ntwo\nthree\nfour\nfive\nsix");

  await expect.poll(() => transcript.clientHeight).toBeLessThan(initialHeight - 20);
  await expect.poll(distanceFromBottom).toBeLessThan(2);

  const lastBlock = page.getByTestId("transcript-block").last().element();
  expect(lastBlock.getBoundingClientRect().bottom).toBeLessThanOrEqual(
    transcript.getBoundingClientRect().bottom + 1,
  );
});

test("growing the composer does not pull an unpinned transcript to the bottom", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(30, "Question") }),
    agentTurn({
      id: "agent-1",
      agentId: ALICE.id,
      items: [textItem(longText(40, "Answer"))],
    }),
  ]);
  mountPanesWithComposer({ projectId: PROJECT_ID, agents: [ALICE] });

  await expect.poll(distanceFromBottom).toBeLessThan(2);
  const transcript = page.getByTestId("unified-transcript").element() as HTMLElement;
  transcript.scrollTop = 0;
  transcript.dispatchEvent(new Event("scroll"));
  await expect.poll(distanceFromBottom).toBeGreaterThan(100);

  await page.getByTestId("compose-textarea").fill("one\ntwo\nthree\nfour\nfive\nsix");

  await expect.poll(() => transcript.scrollTop).toBeLessThan(2);
  expect(distanceFromBottom()).toBeGreaterThan(100);
});
