import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "changed_files")
      return [
        {
          path: "src/app.ts",
          change: "modified",
          additions: 2,
          deletions: 1,
        },
      ];
    if (cmd === "file_diff")
      return {
        path: "src/app.ts",
        binary: false,
        truncated: false,
        too_large: false,
        too_large_bytes: null,
        hunks: [],
      };
    return null;
  }),
  convertFileSrc: (path: string) => path,
}));

vi.mock("@tauri-apps/api/path", () => ({
  homeDir: async () => "/repos",
}));

vi.mock("$lib/native", () => ({
  pickDirectory: vi.fn(async () => null),
  copyText: vi.fn(async () => undefined),
}));

import {
  DIFF_FILE_LIST_MAX_WIDTH,
  DIFF_FILE_LIST_MIN_WIDTH,
  GIT_REPO_MAX_WIDTH,
  GIT_REPO_MIN_WIDTH,
  _testing as layoutTesting,
  layout,
} from "$lib/layout.svelte";
import { _testing as gitTesting, diffTarget } from "$lib/state/gitView.svelte";
import { mountGitView } from "./gitMount";

beforeEach(async () => {
  layoutTesting.reset();
  gitTesting.reset();
  await page.viewport(1400, 800);
  layout.gitRepoWidth = GIT_REPO_MAX_WIDTH;
  layout.diffFileListWidth = DIFF_FILE_LIST_MAX_WIDTH;
  diffTarget.current = {
    kind: "uncommitted",
    repoRoot: "/repos/app",
    worktreePath: "/repos/app",
    title: "Uncommitted changes",
    subtitle: "~/app",
  };
});

function paneWidth(testid: string): number {
  const element = page.getByTestId(testid).element() as HTMLElement;
  return element.getBoundingClientRect().width;
}

test("window resizing changes only the diff at maximum legal rail widths", async () => {
  mountGitView();

  await expect.element(page.getByTestId("changed-files-list")).toBeInTheDocument();
  await expect.poll(() => paneWidth("git-repo-list")).toBeCloseTo(GIT_REPO_MAX_WIDTH, 0);
  await expect.poll(() => paneWidth("changed-files-list")).toBeCloseTo(DIFF_FILE_LIST_MAX_WIDTH, 0);

  const initialDiffWidth = paneWidth("diff-scroll");
  await page.viewport(1100, 800);

  await expect.poll(() => paneWidth("git-repo-list")).toBeCloseTo(GIT_REPO_MAX_WIDTH, 0);
  await expect.poll(() => paneWidth("changed-files-list")).toBeCloseTo(DIFF_FILE_LIST_MAX_WIDTH, 0);
  await expect.poll(() => initialDiffWidth - paneWidth("diff-scroll")).toBeCloseTo(300, 0);

  await page.viewport(1400, 800);
  await expect.poll(() => paneWidth("diff-scroll")).toBeCloseTo(initialDiffWidth, 0);
});

test("narrow-window drags cannot cross either rail's declared minimum", async () => {
  layout.gitRepoWidth = GIT_REPO_MIN_WIDTH;
  layout.diffFileListWidth = DIFF_FILE_LIST_MIN_WIDTH;
  await page.viewport(550, 800);
  mountGitView();

  await expect.element(page.getByTestId("changed-files-list")).toBeInTheDocument();

  for (const [handleId, paneId, minimum] of [
    ["git-repo-resizer", "git-repo-list", GIT_REPO_MIN_WIDTH],
    ["changed-files-resizer", "changed-files-list", DIFF_FILE_LIST_MIN_WIDTH],
  ] as const) {
    const handle = page.getByTestId(handleId).element() as HTMLElement;
    const from = handle.getBoundingClientRect().left;
    handle.dispatchEvent(
      new PointerEvent("pointerdown", { clientX: from, clientY: 200, bubbles: true }),
    );
    window.dispatchEvent(new PointerEvent("pointermove", { clientX: from - 40, clientY: 200 }));
    await expect.poll(() => paneWidth(paneId)).toBeCloseTo(minimum, 0);
    window.dispatchEvent(new PointerEvent("pointerup", { clientX: from - 40, clientY: 200 }));
    await expect.poll(() => paneWidth(paneId)).toBeCloseTo(minimum, 0);
  }
});
