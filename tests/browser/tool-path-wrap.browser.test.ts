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

function pathMetrics(
  testid: string,
  index = 0,
): {
  height: number;
  lineHeight: number;
  scrollWidth: number;
  clientWidth: number;
} {
  const element = page.getByTestId(testid).elements()[index] as HTMLElement | undefined;
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
        toolItem({
          id: "rename",
          name: "apply_patch",
          facet: {
            facet_kind: "edit",
            files: [
              {
                // A rename doubles the path text (source → destination) —
                // the worst wrapping case for this row.
                path: longPath,
                change: "modified",
                edits: [{ old: "x\n", new: "y\n" }],
                truncated: false,
                moved_to: longPath.replace("file-with-a-long-name", "renamed-to-a-long-name"),
              },
            ],
          },
        }),
      ],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE], width: 280 });

  const pathRows = page.getByTestId("tool-path-row");
  const copyButtons = page.getByTestId("tool-path-copy");
  await expect.poll(() => pathRows.elements().length).toBe(3);
  await expect.poll(() => copyButtons.elements().length).toBe(3);

  const firstCopyContainer = copyButtons.nth(0).element().parentElement as HTMLElement;
  expect(getComputedStyle(firstCopyContainer).opacity).toBe("0");
  await pathRows.nth(0).hover();
  await expect.poll(() => getComputedStyle(firstCopyContainer).opacity).toBe("1");

  const copyRect = copyButtons.nth(0).element().getBoundingClientRect();
  const statusRect = page.getByTestId("tool-done").nth(0).element().getBoundingClientRect();
  expect(
    Math.abs(copyRect.x + copyRect.width / 2 - (statusRect.x + statusRect.width / 2)),
  ).toBeLessThan(1);

  // The second edit path is the rename row — source → destination doubles the
  // text, the worst wrapping case.
  for (const [testid, index] of [
    ["tool-read-path", 0],
    ["tool-edit-path", 0],
    ["tool-edit-path", 1],
  ] as const) {
    await expect.poll(() => pathMetrics(testid, index).height).toBeGreaterThan(20);
    await expect
      .poll(() => {
        const metrics = pathMetrics(testid, index);
        return metrics.height > metrics.lineHeight * 1.5;
      })
      .toBe(true);
    await expect
      .poll(() => {
        const metrics = pathMetrics(testid, index);
        return metrics.scrollWidth <= metrics.clientWidth + 1;
      })
      .toBe(true);
  }

  // A rename breaks **at the arrow**, not mid-path: the two endpoints are flex
  // items, so at this width they land on different lines with each path whole,
  // rather than the single run of text splitting wherever it runs out of room.
  // Geometry is the only way to assert a break point — the text content is
  // identical either way, which is why this lives in the WebKit suite.
  const source = page.getByTestId("tool-edit-path-source").element();
  const destination = page.getByTestId("tool-edit-path-destination").element();
  await expect
    .poll(() => destination.getBoundingClientRect().top - source.getBoundingClientRect().top)
    .toBeGreaterThan(0);
  // The arrow stays with the source — it must never orphan onto line two.
  expect(source.textContent?.trim().endsWith("→")).toBe(true);
});
