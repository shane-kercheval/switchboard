import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, resetState, seedTurns } from "./harness";
import { ALICE, BOB, PROJECT_ID, agentTurn, textItem, userTurn } from "./fixtures";

beforeEach(() => {
  resetState();
});

// The whole footer (divider + actions) fades together via the responses' hover
// group, so opacity lives on the footer container — measure it directly.
function footerOpacity(): string {
  const footer = page.getByTestId("fanout-actions-footer").element() as HTMLElement;
  return getComputedStyle(footer).opacity;
}

test("fan-out aggregate footer reveals from responses and hides after leaving them", async () => {
  await registerAgent(ALICE);
  await registerAgent(BOB);
  seedTurns(ALICE.id, [
    userTurn({
      id: "user-alice",
      agentId: ALICE.id,
      sendId: "fan-1",
      text: "fan out",
    }),
    agentTurn({
      id: "agent-alice",
      agentId: ALICE.id,
      sendId: "fan-1",
      endedAt: "2026-05-16T00:00:02Z",
      items: [textItem("alice reply")],
    }),
  ]);
  seedTurns(BOB.id, [
    userTurn({
      id: "user-bob",
      agentId: BOB.id,
      sendId: "fan-1",
      text: "fan out",
    }),
    agentTurn({
      id: "agent-bob",
      agentId: BOB.id,
      sendId: "fan-1",
      endedAt: "2026-05-16T00:00:03Z",
      items: [textItem("bob reply")],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE, BOB] });

  await expect.poll(() => page.getByTestId("fanout-actions-footer").elements().length).toBe(1);
  await expect.poll(footerOpacity).toBe("0");

  await page.getByTestId("fanout-column").first().hover();
  await expect.poll(footerOpacity).toBe("1");

  await page.getByText("fan out").hover();
  await expect.poll(footerOpacity).toBe("0");
});
