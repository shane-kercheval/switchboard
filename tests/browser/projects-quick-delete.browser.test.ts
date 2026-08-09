import { beforeEach, expect, test, vi } from "vitest";
import { page, userEvent } from "vitest/browser";
import { render } from "vitest-browser-svelte";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));
vi.mock("$lib/windowDrag", () => ({ windowDragRegion: () => undefined }));

import ProjectsSidebar from "$lib/components/ProjectsSidebar.svelte";
import {
  backgroundCompletedProjectIds,
  projects,
  _testing as workspaceTesting,
} from "$lib/state/workspace.svelte";
import { _testing as workflowsTesting } from "$lib/state/workflows.svelte";
import { layout, _testing as layoutTesting } from "$lib/layout.svelte";

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ad";

beforeEach(() => {
  workspaceTesting.reset();
  workflowsTesting.reset();
  layoutTesting.reset();
  projects.list = [
    {
      id: PROJECT_ID,
      name: "archived-project",
      directory: "/work/archived-project",
      available: true,
      archived: true,
      created_at: "2026-05-16T00:00:00Z",
      last_activity: "2026-05-16T00:00:00Z",
    },
  ];
});

function renderSidebar(): void {
  render(ProjectsSidebar, {
    onAddProject: () => {},
    onOpenSettings: () => {},
    onProjectSelect: () => {},
    onToggleSidebar: () => {},
  });
}

test("archive delete and confirm occupy the same click target", async () => {
  renderSidebar();

  await page.getByTestId("project-view-archived").click();
  await page.getByTestId("project-row").hover();
  const deleteButton = page.getByTestId("project-quick-delete");
  await expect.element(deleteButton).toBeVisible();
  const deleteRect = (deleteButton.element() as HTMLElement).getBoundingClientRect();

  await deleteButton.click();
  const confirmButton = page.getByTestId("project-quick-delete-confirm");
  await expect.element(confirmButton).toBeVisible();
  const confirmRect = (confirmButton.element() as HTMLElement).getBoundingClientRect();

  expect(confirmRect.x).toBeCloseTo(deleteRect.x, 1);
  expect(confirmRect.y).toBeCloseTo(deleteRect.y, 1);
  expect(confirmRect.width).toBeCloseTo(deleteRect.width, 1);
  expect(confirmRect.height).toBeCloseTo(deleteRect.height, 1);
});

test("archive row actions support keyboard confirmation and safe focus restoration", async () => {
  renderSidebar();
  await page.getByTestId("project-view-archived").click();

  const select = page.getByTestId("project-select");
  (select.element() as HTMLElement).focus();
  await expect.poll(() => document.activeElement === select.element()).toBe(true);

  await userEvent.tab();
  await expect
    .poll(() => document.activeElement === page.getByTestId("project-actions-trigger").element())
    .toBe(true);
  await userEvent.tab();
  const deleteButton = page.getByTestId("project-quick-delete");
  await expect.poll(() => document.activeElement === deleteButton.element()).toBe(true);

  await userEvent.keyboard("{Enter}");
  const cancelButton = page.getByTestId("project-quick-delete-cancel");
  await expect.poll(() => document.activeElement === cancelButton.element()).toBe(true);
  await userEvent.keyboard("{Enter}");
  await expect
    .poll(() => document.activeElement === page.getByTestId("project-quick-delete").element())
    .toBe(true);

  await userEvent.keyboard("{Enter}");
  await expect
    .poll(
      () => document.activeElement === page.getByTestId("project-quick-delete-cancel").element(),
    )
    .toBe(true);
  await userEvent.tab();
  await expect
    .poll(
      () => document.activeElement === page.getByTestId("project-quick-delete-confirm").element(),
    )
    .toBe(true);
});

test("completed archived rows keep actions and status separated at minimum width", async () => {
  layout.projectsSidebarWidth = 200;
  backgroundCompletedProjectIds[PROJECT_ID] = true;
  renderSidebar();

  await page.getByTestId("project-view-archived").click();
  await page.getByTestId("project-row").hover();
  const actions = page.getByTestId("project-row-actions");
  const completed = page.getByTestId("project-completed");
  await expect.element(page.getByTestId("project-quick-delete")).toBeVisible();
  await expect.element(completed).toBeVisible();

  const actionsRect = (actions.element() as HTMLElement).getBoundingClientRect();
  const completedRect = (completed.element() as HTMLElement).getBoundingClientRect();
  expect(actionsRect.right).toBeLessThanOrEqual(completedRect.left);
});
