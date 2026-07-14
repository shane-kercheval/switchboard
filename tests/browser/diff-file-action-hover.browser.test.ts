import { expect, test, vi } from "vitest";
import { page } from "vitest/browser";

// The changed-files action icons (AsyncIconButton) use a stronger gray than the
// row hover by default. On a
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

test("a selected row's action icon hovers white; an unselected row's is stronger than its row hover", async () => {
  mountDiffPanel({ target: TARGET });

  const rows = page.getByTestId("changed-file");
  const difftools = page.getByTestId("changed-file-difftool");
  await expect.element(rows.nth(0)).toBeInTheDocument();

  // Reveal + hover the selected row's action icon — it must resolve to white.
  await rows.nth(0).hover();
  await difftools.nth(0).hover();
  await expect.poll(() => getComputedStyle(difftools.nth(0).element()).backgroundColor).toBe(WHITE);

  // The unselected row's icon hovers a stronger gray than the row itself.
  await rows.nth(1).hover();
  await difftools.nth(1).hover();
  const unselectedColors = await vi.waitUntil(() => {
    const row = rows.nth(1).element().parentElement as HTMLElement;
    const rowBg = getComputedStyle(row).backgroundColor;
    const iconBg = getComputedStyle(difftools.nth(1).element()).backgroundColor;
    return iconBg !== TRANSPARENT && iconBg !== rowBg ? { rowBg, iconBg } : null;
  });
  expect(unselectedColors.iconBg).not.toBe(WHITE);
  expect(unselectedColors.iconBg).not.toBe(TRANSPARENT);
  expect(unselectedColors.iconBg).not.toBe(unselectedColors.rowBg);
});
