import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_prompts" || cmd === "list_workflows") return [];
    if (cmd === "send_message") return "message-id";
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
import { registerAgent, resetState, seedTurns, userScrollTo } from "./harness";
import { _testing as composeTesting } from "$lib/state/composeStore";
import { moveAgentToNewPane, _testing as panesTesting } from "$lib/state/transcriptPanes.svelte";
import { setRecipients, _testing as selectionTesting } from "$lib/state/recipientSelection.svelte";
import type { AgentRecord } from "$lib/types";

const BOB: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: PROJECT_ID,
  name: "bob",
  harness: "codex",
  session_locator: null,
  model: null,
  effort: null,
  model_choices: [],
  effort_choices: [],
  created_at: "2026-05-16T00:00:01Z",
};

function transcriptOf(index: number): HTMLElement {
  return page.getByTestId("unified-transcript").nth(index).element() as HTMLElement;
}

function distanceFromBottom(index: number): number {
  const transcript = transcriptOf(index);
  return transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight;
}

beforeEach(() => {
  resetState();
  composeTesting.reset();
  panesTesting.reset();
  selectionTesting.reset();
});

test("sending to one pane only force-pins that pane", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-a", agentId: ALICE.id, text: longText(30, "Question") }),
    agentTurn({
      id: "agent-a",
      agentId: ALICE.id,
      items: [textItem(longText(40, "Answer"))],
    }),
  ]);
  seedTurns(BOB.id, [
    userTurn({ id: "user-b", agentId: BOB.id, text: longText(30, "Question") }),
    agentTurn({
      id: "agent-b",
      agentId: BOB.id,
      items: [textItem(longText(40, "Answer"))],
    }),
  ]);
  moveAgentToNewPane(PROJECT_ID, [ALICE.id, BOB.id], BOB.id);
  setRecipients(PROJECT_ID, [ALICE.id]);
  mountPanesWithComposer({ projectId: PROJECT_ID, agents: [ALICE, BOB] });

  await expect.poll(() => distanceFromBottom(0)).toBeLessThan(2);
  await expect.poll(() => distanceFromBottom(1)).toBeLessThan(2);
  userScrollTo(transcriptOf(0), 0);
  userScrollTo(transcriptOf(1), 0);
  await expect.poll(() => distanceFromBottom(0)).toBeGreaterThan(100);
  await expect.poll(() => distanceFromBottom(1)).toBeGreaterThan(100);

  await page.getByTestId("compose-textarea").fill("Follow up");
  await page.getByTestId("compose-send").click();

  await expect.poll(() => distanceFromBottom(0)).toBeLessThan(2);
  await expect.poll(() => transcriptOf(1).scrollTop).toBeLessThan(2);
  expect(distanceFromBottom(1)).toBeGreaterThan(100);
});
