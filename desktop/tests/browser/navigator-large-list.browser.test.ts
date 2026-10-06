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
import { ALICE, PROJECT_ID, agentTurn, textItem } from "./fixtures";
import { jumpRequest, _testing as jumpTesting } from "$lib/state/transcriptJump.svelte";

beforeEach(() => {
  resetState();
  jumpTesting.reset();
});

test("distant results remain reachable and keyboard selection stays visible", async () => {
  await registerAgent(ALICE);
  seedTurns(
    ALICE.id,
    Array.from({ length: 300 }, (_, index) =>
      agentTurn({
        id: `turn-${index}`,
        agentId: ALICE.id,
        at: new Date(Date.UTC(2026, 4, 16, 0, 0, index)).toISOString(),
        items: [textItem(`message ${index}`)],
      }),
    ),
  );
  mountNavigator({ projectId: PROJECT_ID, agents: [ALICE] });

  await page.getByTestId("transcript-navigator-toggle").click();
  await expect.element(page.getByTestId("navigator-count")).toHaveTextContent("300 messages");

  const list = page.getByTestId("navigator-list").element() as HTMLElement;
  expect(list.scrollHeight).toBeGreaterThan(list.clientHeight);
  expect(list.querySelectorAll('[data-testid="navigator-entry"]').length).toBeLessThan(80);

  list.scrollTop = list.scrollHeight;
  list.dispatchEvent(new Event("scroll"));
  await expect
    .poll(() => {
      const result = list.querySelector('[data-row-key="a:turn-0"]');
      if (!(result instanceof HTMLElement)) return false;
      const listRect = list.getBoundingClientRect();
      const resultRect = result.getBoundingClientRect();
      return resultRect.top >= listRect.top - 1 && resultRect.bottom <= listRect.bottom + 1;
    })
    .toBe(true);

  list.scrollTop = 0;
  list.dispatchEvent(new Event("scroll"));
  await expect
    .poll(() => {
      const result = list.querySelector('[data-row-key="a:turn-299"]');
      if (!(result instanceof HTMLElement)) return false;
      const listRect = list.getBoundingClientRect();
      const resultRect = result.getBoundingClientRect();
      return resultRect.top >= listRect.top - 1 && resultRect.bottom <= listRect.bottom + 1;
    })
    .toBe(true);

  const search = page.getByTestId("navigator-search").element() as HTMLInputElement;
  for (let index = 0; index < 120; index += 1) {
    search.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
  }
  await expect
    .poll(() => {
      const selected = list.querySelector('[aria-selected="true"]');
      if (!(selected instanceof HTMLElement)) return false;
      const listRect = list.getBoundingClientRect();
      const selectedRect = selected.getBoundingClientRect();
      return selectedRect.top >= listRect.top - 1 && selectedRect.bottom <= listRect.bottom + 1;
    })
    .toBe(true);

  search.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  await expect.element(page.getByTestId("dialog-content")).not.toBeInTheDocument();
  expect(jumpRequest.rowKey).toBe("a:turn-179");
});
