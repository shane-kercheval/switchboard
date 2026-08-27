import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

const { listing } = vi.hoisted(() => ({
  listing: {
    repo: {
      root: "/repos/app",
      name: "app",
      default_branch: "main",
      available: true,
      is_bare: false,
      last_commit_at: null,
      local_branches: [],
      remote_branches: [],
      detached_worktrees: [],
    },
    linked_projects: {},
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => (cmd === "list_tracked_repos" ? [listing] : null)),
  convertFileSrc: (path: string) => path,
}));

vi.mock("@tauri-apps/api/path", () => ({
  homeDir: async () => "/repos",
}));

vi.mock("$lib/native", () => ({
  pickDirectory: vi.fn(async () => null),
  copyText: vi.fn(async () => undefined),
}));

import { _testing, refreshAll } from "$lib/state/gitView.svelte";
import { mountGitView } from "./gitMount";

beforeEach(async () => {
  _testing.reset();
  await refreshAll();
});

test("repo actions hide after collapsing and moving the pointer away", async () => {
  mountGitView();

  const repo = page.getByTestId("git-repo");
  const collapse = page.getByRole("button", { name: "Collapse repo" });
  const collapseElement = collapse.element();
  const actions = page.getByTestId("repo-refresh").element().parentElement as HTMLElement;
  await expect.element(repo).toBeInTheDocument();

  await repo.hover();
  await expect.poll(() => getComputedStyle(actions).opacity).toBe("1");
  await collapse.click();
  await page.getByTestId("git-repo-list").hover();

  expect(document.activeElement).toBe(collapseElement);
  await expect.poll(() => getComputedStyle(actions).opacity).toBe("0");
});

test("repo actions remain visible when keyboard focus enters the action cluster", async () => {
  mountGitView();

  const refresh = page.getByTestId("repo-refresh");
  const actions = refresh.element().parentElement as HTMLElement;
  await expect.element(page.getByTestId("git-repo")).toBeInTheDocument();

  refresh.element().focus();

  expect(document.activeElement).toBe(refresh.element());
  await expect.poll(() => getComputedStyle(actions).opacity).toBe("1");
});
