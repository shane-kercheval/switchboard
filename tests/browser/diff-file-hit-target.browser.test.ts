import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

// DiffPanel loads the changed-files list through `$lib/api`, which calls the real
// `invoke` — so mocking `@tauri-apps/api/core` (returning a file for the
// `changed_files` command) is the whole surface, same as the jsdom DiffPanel
// suite.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "changed_files") return [{ path: "src/code.ts", change: "modified" }];
    return null;
  }),
  convertFileSrc: (p: string) => p,
}));

import { mountDiffPanel } from "./mount";

const TARGET = {
  kind: "uncommitted" as const,
  repoRoot: "/repo",
  worktreePath: "/wt",
  title: "Uncommitted changes",
  subtitle: "~/wt",
};

beforeEach(() => {
  // No shared state to reset; vitest-browser-svelte unmounts between tests.
});

// BUG GUARD: in the Git view's "Changed files" list, the hover background and
// padding sat on the row while the click handler was on an inner button — so the
// top/bottom padding band highlighted on hover but swallowed the click. The fix
// moves the padding onto the button so the click target fills the row.
//
// Observed red: with the padding back on the row (and the button left un-padded),
// the button no longer spans the row's full height and this assertion fails.
// jsdom can't see it (no measured geometry).
test("a changed-file row's click target fills the full row height", async () => {
  mountDiffPanel({ target: TARGET });

  const button = page.getByTestId("changed-file");
  await expect.element(button).toBeInTheDocument();

  await expect
    .poll(() => {
      const el = page.getByTestId("changed-file").element() as HTMLElement;
      const row = el.parentElement as HTMLElement; // the hover-background row <div>
      const b = el.getBoundingClientRect();
      const r = row.getBoundingClientRect();
      // The button covers the row top-to-bottom — no dead padding band above or below.
      return b.top - r.top <= 0.5 && r.bottom - b.bottom <= 0.5;
    })
    .toBe(true);
});
