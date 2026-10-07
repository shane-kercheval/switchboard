import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";
import { render } from "vitest-browser-svelte";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));
vi.mock("$lib/windowDrag", () => ({ windowDragRegion: () => undefined }));

import ProjectsSidebar from "$lib/components/ProjectsSidebar.svelte";
import { projects, _testing as workspaceTesting } from "$lib/state/workspace.svelte";
import { _testing as workflowsTesting } from "$lib/state/workflows.svelte";
import { _testing as layoutTesting } from "$lib/layout.svelte";

beforeEach(() => {
  workspaceTesting.reset();
  workflowsTesting.reset();
  layoutTesting.reset();
  projects.list = [
    {
      id: "00000000-0000-7000-8000-0000000000b1",
      name: "alpha",
      directory: "/work/alpha",
      directory_available: true,
      archived: false,
      created_at: "2026-05-16T00:00:00Z",
      last_activity: "2026-05-16T00:00:00Z",
    },
  ];
});

test("a project's directory tooltip sits clear of the row's menu button", async () => {
  render(ProjectsSidebar, {
    onAddProject: () => {},
    onOpenSettings: () => {},
    onProjectSelect: () => {},
    onToggleSidebar: () => {},
    onLocateFolder: () => {},
  });

  await page.getByTestId("project-select").hover();
  const tooltip = page.getByTestId("tooltip-content");
  await expect.element(tooltip).toBeVisible();
  const menu = page.getByTestId("project-actions-trigger");
  await expect.element(menu).toBeVisible();

  // Poll: the menu button slides in as the row reveals its actions.
  await expect
    .poll(() => {
      const tip = (tooltip.element() as HTMLElement).getBoundingClientRect();
      const row = (
        page.getByTestId("project-row").element() as HTMLElement
      ).getBoundingClientRect();
      const button = (menu.element() as HTMLElement).getBoundingClientRect();
      return tip.left >= row.right && tip.left >= button.right;
    })
    .toBe(true);
});
