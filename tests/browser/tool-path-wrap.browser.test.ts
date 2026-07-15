import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, resetState, seedTurns } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, toolItem } from "./fixtures";

beforeEach(() => {
  resetState();
});

function pathMetrics(testid: string): {
  height: number;
  lineHeight: number;
  scrollWidth: number;
  clientWidth: number;
} {
  const element = page.getByTestId(testid).elements()[0] as HTMLElement | undefined;
  if (element === undefined) return { height: 0, lineHeight: 0, scrollWidth: 1, clientWidth: 0 };
  const style = getComputedStyle(element);
  return {
    height: element.getBoundingClientRect().height,
    lineHeight: Number.parseFloat(style.lineHeight),
    scrollWidth: element.scrollWidth,
    clientWidth: element.clientWidth,
  };
}

test("filesystem paths below tool headers wrap without horizontal clipping", async () => {
  const longPath = `/repo/${"deeply-nested-directory/".repeat(10)}file-with-a-long-name.ts`;
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({
      id: "path-wrap",
      agentId: ALICE.id,
      endedAt: "2026-05-16T00:00:05Z",
      items: [
        toolItem({
          id: "read",
          name: "Read",
          facet: { facet_kind: "read", path: longPath },
        }),
        toolItem({
          id: "delete",
          name: "apply_patch",
          facet: {
            facet_kind: "edit",
            files: [
              {
                path: longPath,
                change: "deleted",
                edits: [{ old: "deleted\n", new: "" }],
                truncated: false,
              },
            ],
          },
        }),
      ],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE], width: 280 });

  for (const testid of ["tool-read-path", "tool-edit-path"]) {
    await expect.poll(() => pathMetrics(testid).height).toBeGreaterThan(20);
    await expect
      .poll(() => {
        const metrics = pathMetrics(testid);
        return metrics.height > metrics.lineHeight * 1.5;
      })
      .toBe(true);
    await expect
      .poll(() => {
        const metrics = pathMetrics(testid);
        return metrics.scrollWidth <= metrics.clientWidth + 1;
      })
      .toBe(true);
  }
});
