import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

const { listings } = vi.hoisted(() => ({
  listings: Array.from({ length: 30 }, (_, index) => ({
    repo: {
      root: `/repos/repo-${index}`,
      name: `repo-${index}`,
      default_branch: "main",
      available: true,
      is_bare: false,
      last_commit_at: null,
      local_branches: [],
      remote_branches: [],
      detached_worktrees: [],
    },
    linked_projects: {},
  })),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => (cmd === "list_tracked_repos" ? listings : null)),
  convertFileSrc: (path: string) => path,
}));

vi.mock("@tauri-apps/api/path", () => ({
  homeDir: async () => "/repos",
}));

vi.mock("$lib/native", () => ({
  pickDirectory: vi.fn(async () => null),
  copyText: vi.fn(async () => undefined),
}));

import { render } from "vitest-browser-svelte";
import GitViewToggleHost from "./GitViewToggleHost.svelte";
import { _testing, refreshAll, requestRepoReveal } from "$lib/state/gitView.svelte";

beforeEach(async () => {
  _testing.reset();
  await refreshAll();
});

test("Git view remount restores repository expansion and exact scroll position", async () => {
  render(GitViewToggleHost);

  await expect.element(page.getByText("repo-29", { exact: true })).toBeInTheDocument();
  await page.getByRole("button", { name: "Collapse repo" }).nth(0).click();
  const scroller = page.getByTestId("git-repo-list").element() as HTMLElement;
  await expect.poll(() => scroller.scrollHeight - scroller.clientHeight).toBeGreaterThan(400);

  const target = 311;
  scroller.scrollTop = target;
  scroller.dispatchEvent(new Event("scroll"));
  await expect.poll(() => scroller.scrollTop).toBe(target);

  await page.getByTestId("toggle-git-view").click();
  await expect.element(page.getByTestId("git-view")).not.toBeInTheDocument();
  await page.getByTestId("toggle-git-view").click();
  await expect.element(page.getByText("repo-29", { exact: true })).toBeInTheDocument();

  const restored = page.getByTestId("git-repo-list").element() as HTMLElement;
  await expect.poll(() => restored.scrollTop).toBe(target);
  expect(page.getByRole("button", { name: "Expand repo" }).elements()).toHaveLength(1);
  expect(page.getByRole("button", { name: "Collapse repo" }).elements()).toHaveLength(29);
});

test("revealing an off-screen repository makes it visible without expanding the others", async () => {
  render(GitViewToggleHost);

  await expect.element(page.getByText("repo-29", { exact: true })).toBeInTheDocument();
  await page.getByTestId("git-repos-toggle-all").click();
  const scroller = page.getByTestId("git-repo-list").element() as HTMLElement;
  const target = document.querySelector('[data-repo-root="/repos/repo-29"]') as HTMLElement;
  scroller.scrollTop = 0;

  requestRepoReveal("/repos/repo-29");

  await expect
    .poll(() => {
      const scrollerRect = scroller.getBoundingClientRect();
      const targetRect = target.getBoundingClientRect();
      return targetRect.top >= scrollerRect.top && targetRect.bottom <= scrollerRect.bottom;
    })
    .toBe(true);
  expect(page.getByRole("button", { name: "Expand repo" }).elements()).toHaveLength(29);
  expect(page.getByRole("button", { name: "Collapse repo" }).elements()).toHaveLength(1);
});
