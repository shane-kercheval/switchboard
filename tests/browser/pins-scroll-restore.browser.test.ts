import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

const { persistedPins } = vi.hoisted(() => ({
  persistedPins: [
    {
      key: "agent:hydration:00000000-0000-7000-8000-000000000aaa:message-1",
      pinned_at: "2026-08-07T12:01:00Z",
    },
  ],
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string) =>
    command === "list_message_pins" ? persistedPins : null,
  ),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { render } from "vitest-browser-svelte";
import PinsSidebarToggleHost from "./PinsSidebarToggleHost.svelte";
import { registerAgent, resetState, seedTurns } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, textItem } from "./fixtures";
import { _testing as pinsState } from "$lib/state/messagePins.svelte";
import { _testing as layoutState } from "$lib/layout.svelte";

beforeEach(() => {
  resetState();
  pinsState.reset();
  layoutState.reset();
});

test("switching from Pins to Agents and back restores the exact Pins scroll position", async () => {
  await registerAgent(ALICE);
  const paragraphs = Array.from(
    { length: 80 },
    (_, index) => `Pinned paragraph ${index + 1} with enough text to occupy a full line.`,
  ).join("\n\n");
  seedTurns(ALICE.id, [
    {
      ...agentTurn({
        id: "pinned-turn",
        agentId: ALICE.id,
        items: [textItem(paragraphs)],
      }),
      hydration_key: "message-1",
    },
  ]);
  render(PinsSidebarToggleHost, { projectId: PROJECT_ID, agents: [ALICE] });

  await expect.element(page.getByTestId("pinned-message-body")).toBeVisible();
  const scroller = page.getByTestId("pins-scroll").element() as HTMLElement;
  await expect.poll(() => scroller.scrollHeight - scroller.clientHeight).toBeGreaterThan(400);

  const target = 233;
  scroller.scrollTop = target;
  scroller.dispatchEvent(new Event("scroll"));
  await expect.poll(() => scroller.scrollTop).toBe(target);

  await page.getByTestId("show-agents").click();
  await expect.element(page.getByTestId("agents-placeholder")).toBeVisible();
  await page.getByTestId("show-pins").click();
  await expect.element(page.getByTestId("pinned-message-body")).toBeVisible();

  const restored = page.getByTestId("pins-scroll").element() as HTMLElement;
  await expect.poll(() => restored.scrollTop).toBe(target);
});
