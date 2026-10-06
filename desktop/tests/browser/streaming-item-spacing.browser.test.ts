import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, toolItem } from "./fixtures";

// A streaming turn renders its items inside the live-scroll cap, one level
// deeper than a settled turn (whose items are direct children of the
// `space-y-1.5` body). The cap must carry that spacing itself, or tool rows sit
// flush while streaming and only gain their gap once the turn settles — the
// reported "squished then padded" jump. Margins aren't applied under jsdom, so
// this is measured in WebKit.

beforeEach(() => {
  resetState();
});

function gapBetweenTools(): number {
  const tools = page.getByTestId("turn-tool").elements() as HTMLElement[];
  if (tools.length < 2) return -1;
  const first = tools[0]!.getBoundingClientRect();
  const second = tools[1]!.getBoundingClientRect();
  return second.top - first.bottom;
}

test("streaming tool rows carry the same inter-item gap as a settled turn", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({
      id: "agent-streaming",
      agentId: ALICE.id,
      status: "streaming",
      items: [
        toolItem({ id: "t-1", command: "echo one" }),
        toolItem({ id: "t-2", command: "echo two" }),
      ],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  await expect.element(page.getByTestId("turn-live-scroll")).toBeInTheDocument();
  await expect.poll(() => page.getByTestId("turn-tool").elements().length).toBe(2);
  // space-y-1.5 = 0.375rem = 6px; assert a real, non-trivial gap rather than the
  // flush 0 the bug produced.
  await expect.poll(gapBetweenTools).toBeGreaterThan(3);
});

test("a settled turn has the same gap — the streaming cap now matches it", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({
      id: "agent-done",
      agentId: ALICE.id,
      status: "complete",
      endedAt: "2026-05-16T00:00:05Z",
      items: [
        toolItem({ id: "t-1", command: "echo one" }),
        toolItem({ id: "t-2", command: "echo two" }),
      ],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  await expect.poll(() => page.getByTestId("turn-tool").elements().length).toBe(2);
  expect(page.getByTestId("turn-live-scroll").elements().length).toBe(0);
  await expect.poll(gapBetweenTools).toBeGreaterThan(3);
});
