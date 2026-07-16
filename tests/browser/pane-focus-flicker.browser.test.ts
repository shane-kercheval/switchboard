import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) =>
    cmd === "search_project_files" || cmd === "existing_attachment_paths" ? [] : null,
  ),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));
// The compose bar subscribes to OS drag-drop on mount.
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => vi.fn()) }),
}));

import { mountPanesWithComposer } from "./composeMount";
import { registerAgent, seedTurns, resetState } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";
import {
  moveAgentToNewPane,
  _testing as panesState,
} from "$lib/state/transcriptPanes.svelte";
import {
  selectionFor,
  setRecipients,
  _testing as selectionState,
} from "$lib/state/recipientSelection.svelte";
import { setProjectCompact } from "$lib/state/transcriptPreview.svelte";
import type { AgentRecord } from "$lib/types";

// The no-flicker guarantee is native mousedown behavior jsdom cannot reproduce:
// a plain click on a pane blurs a focused textarea, which would clear the
// compose focus ring for a frame before the focus request restores it. The
// pane's Cmd+click `mousedown` preventDefault suppresses that blur entirely.
// Proven here by asserting the compose box sees ZERO focusout events across a
// real WebKit Meta-click — a final focus assertion alone would miss a
// blur-then-refocus flicker.

const BOB: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: PROJECT_ID,
  name: "bob",
  harness: "codex",
  session_locator: null,
  created_at: "2026-05-16T00:00:01Z",
};
const ROSTER_IDS = [ALICE.id, BOB.id];

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

test("Cmd+click a pane while the composer is focused re-targets without a focus-ring flicker", async () => {
  await seedTwoAgents();
  moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id); // pane 0: alice, pane 1: bob
  mountPanesWithComposer({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 1000 });

  // Target bob's pane, then put keyboard focus in the composer.
  setRecipients(PROJECT_ID, [BOB.id]);
  const textarea = page.getByTestId("compose-textarea").element() as HTMLTextAreaElement;
  textarea.focus();
  await expect.poll(() => document.activeElement === textarea).toBe(true);

  // Count blurs that leave the compose box (each would drop the focus ring).
  const box = page.getByTestId("compose-box").element();
  let focusouts = 0;
  const onFocusOut = (): void => {
    focusouts += 1;
  };
  box.addEventListener("focusout", onFocusOut);

  // Real WebKit modifier-click on alice's pane body: re-targets to alice.
  await page.getByTestId("transcript-pane").nth(0).click({ modifiers: ["Meta"] });

  await expect.poll(() => selectionFor(PROJECT_ID)).toEqual([ALICE.id]);
  // The gesture never blurred the composer, so the ring never flickered, and
  // focus stayed put (the focus request is a no-op on an already-focused box).
  expect(focusouts).toBe(0);
  expect(document.activeElement).toBe(textarea);

  box.removeEventListener("focusout", onFocusOut);
});
