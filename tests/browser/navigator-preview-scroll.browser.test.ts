import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountNavigator } from "./navigatorMount";
import { registerAgent, resetState, seedTurns } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem } from "./fixtures";
import { _testing as jumpTesting } from "$lib/state/transcriptJump.svelte";

beforeEach(() => {
  resetState();
  jumpTesting.reset();
});

test("selecting another long message resets its preview to the top", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({
      id: "older",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:01Z",
      items: [textItem(`older-message-start\n${longText(80)}`)],
    }),
    agentTurn({
      id: "newer",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      items: [textItem(`newer-message-start\n${longText(80)}`)],
    }),
  ]);
  mountNavigator({ projectId: PROJECT_ID, agents: [ALICE] });
  await page.getByTestId("transcript-navigator-toggle").click();

  const search = page.getByTestId("navigator-search").element() as HTMLInputElement;
  search.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
  await expect
    .element(page.getByTestId("navigator-preview"))
    .toHaveTextContent("older-message-start");

  const preview = page.getByTestId("navigator-preview").element() as HTMLElement;
  preview.scrollTop = preview.scrollHeight;
  preview.dispatchEvent(new Event("scroll"));
  await expect.poll(() => preview.scrollTop).toBeGreaterThan(100);

  search.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }));
  await expect
    .element(page.getByTestId("navigator-preview"))
    .toHaveTextContent("newer-message-start");
  await expect.poll(() => preview.scrollTop).toBe(0);
  await expect.poll(() => preview.hasAttribute("data-fade-top")).toBe(false);
  await expect.poll(() => preview.hasAttribute("data-fade-bottom")).toBe(true);
});
