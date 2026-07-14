import { afterEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

// The changed-files action icons (AsyncIconButton) use the stronger nested-row
// hover. On a
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
const LIGHT_CONTROL_HOVER = "rgb(236, 236, 239)";
const LIGHT_ACTIVE = "rgb(226, 226, 229)";
const DARK_RAISED = "rgb(39, 39, 42)";
const DARK_CONTROL_HOVER = "rgb(43, 43, 48)";
const DARK_ACTIVE = "rgb(48, 48, 54)";
const TRANSPARENT = "rgba(0, 0, 0, 0)"; // no background at all

afterEach(() => document.documentElement.classList.remove("dark"));

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
  await expect
    .poll(() => getComputedStyle(difftools.nth(1).element()).backgroundColor)
    .toBe(LIGHT_ACTIVE);
  const row = rows.nth(1).element().parentElement as HTMLElement;
  const rowBg = getComputedStyle(row).backgroundColor;
  expect(LIGHT_ACTIVE).not.toBe(TRANSPARENT);
  expect(LIGHT_ACTIVE).not.toBe(rowBg);

  // Header icon buttons use the same compact-control role on a resting raised surface.
  const close = page.getByTestId("detail-close");
  await close.hover();
  await expect
    .poll(() => getComputedStyle(close.element()).backgroundColor)
    .toBe(LIGHT_CONTROL_HOVER);
});

test("control and selected-row icon hovers retain their hierarchy in dark mode", async () => {
  document.documentElement.classList.add("dark");
  mountDiffPanel({ target: TARGET });

  const rows = page.getByTestId("changed-file");
  const difftools = page.getByTestId("changed-file-difftool");
  await expect.element(rows.nth(0)).toBeInTheDocument();

  await rows.nth(0).hover();
  await difftools.nth(0).hover();
  await expect
    .poll(() => getComputedStyle(difftools.nth(0).element()).backgroundColor)
    .toBe(DARK_RAISED);

  await rows.nth(1).hover();
  await difftools.nth(1).hover();
  await expect
    .poll(() => getComputedStyle(difftools.nth(1).element()).backgroundColor)
    .toBe(DARK_ACTIVE);

  const close = page.getByTestId("detail-close");
  await close.hover();
  await expect
    .poll(() => getComputedStyle(close.element()).backgroundColor)
    .toBe(DARK_CONTROL_HOVER);
});
