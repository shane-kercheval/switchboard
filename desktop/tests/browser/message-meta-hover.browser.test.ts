import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string) =>
    command === "list_message_pins" ||
    command === "set_message_pin" ||
    command === "remove_message_pins"
      ? []
      : null,
  ),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, resetState, seedTurns } from "./harness";
import { ALICE, PROJECT_ID, longText, userTurn } from "./fixtures";

beforeEach(() => {
  resetState();
});

function opacity(testid: string): string {
  return getComputedStyle(page.getByTestId(testid).element() as HTMLElement).opacity;
}

test("clicking Pin does not leave hover-only message metadata visible", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(), sendId: "send-1" }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  await expect.poll(() => page.getByTestId("message-pin").elements().length).toBe(1);
  await expect.poll(() => page.getByTestId("turn-preview-toggle").elements().length).toBe(1);
  await expect.poll(() => opacity("message-meta-details")).toBe("0");

  await page.getByTestId("turn").hover();
  await expect.poll(() => opacity("message-meta-details")).toBe("1");
  await expect.poll(() => opacity("turn-preview-toggle")).toBe("1");

  await page.getByTestId("message-pin").click();
  await page.getByTestId("unified-transcript").hover({ position: { x: 4, y: 590 } });

  await expect.poll(() => opacity("message-meta-details")).toBe("0");
  await expect.poll(() => opacity("turn-preview-toggle")).toBe("0");
});
