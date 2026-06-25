import { expect, test, vi } from "vitest";
import { page } from "vitest/browser";

// The changed-files action icons (AsyncIconButton) hover gray by default. On a
// selected (blue) row that's overridden to the white `bg-raised` hover via a
// `group-data-[selected=true]` variant on the row. jsdom can't evaluate the
// cascade (`:hover` + group-data specificity), so this asserts the real computed
// color in WebKit. Two files → row 0 auto-selected (blue), row 1 not.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "changed_files")
      return [
        { path: "a.ts", change: "modified" },
        { path: "b.ts", change: "modified" },
      ];
    if (cmd === "file_diff")
      return {
        path: "a.ts",
        binary: false,
        truncated: false,
        too_large: false,
        too_large_bytes: null,
        hunks: [],
      };
    return null;
  }),
  convertFileSrc: (p: string) => p,
}));

vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountDiffPanel } from "./mount";

const TARGET = {
  kind: "uncommitted" as const,
  repoRoot: "/repo",
  worktreePath: "/wt",
  title: "Uncommitted changes",
  subtitle: "~/wt",
};

const WHITE = "rgb(255, 255, 255)"; // light-mode `--raised`
const TRANSPARENT = "rgba(0, 0, 0, 0)"; // no background at all

test("a selected row's action icon hovers white; an unselected row's hovers a non-white gray", async () => {
  mountDiffPanel({ target: TARGET });

  const rows = page.getByTestId("changed-file");
  const difftools = page.getByTestId("changed-file-difftool");
  await expect.element(rows.nth(0)).toBeInTheDocument();

  // Reveal + hover the selected row's action icon — it must resolve to white.
  await rows.nth(0).hover();
  await difftools.nth(0).hover();
  await expect.poll(() => getComputedStyle(difftools.nth(0).element()).backgroundColor).toBe(WHITE);

  // The unselected row's icon hovers gray — assert an actual fill (not white,
  // and not transparent), so a regression that drops the hover entirely fails.
  await rows.nth(1).hover();
  await difftools.nth(1).hover();
  const unselectedBg = await vi.waitUntil(() => {
    const bg = getComputedStyle(difftools.nth(1).element()).backgroundColor;
    return bg !== TRANSPARENT ? bg : null;
  });
  expect(unselectedBg).not.toBe(WHITE);
  expect(unselectedBg).not.toBe(TRANSPARENT);
});
