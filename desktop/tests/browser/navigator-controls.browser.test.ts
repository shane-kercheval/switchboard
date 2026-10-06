import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountNavigator } from "./navigatorMount";
import { resetState } from "./harness";
import { PROJECT_ID } from "./fixtures";
import { _testing as jumpTesting } from "$lib/state/transcriptJump.svelte";

beforeEach(() => {
  resetState();
  jumpTesting.reset();
});

test("opening the navigator removes its tooltip and aligns the toolbar controls", async () => {
  mountNavigator({ projectId: PROJECT_ID, agents: [] });

  const toggle = page.getByTestId("transcript-navigator-toggle");
  await toggle.hover();
  await expect.element(page.getByTestId("tooltip-content")).toBeVisible();

  await toggle.click();
  await expect.element(page.getByTestId("dialog-content")).toBeVisible();
  await expect.element(page.getByTestId("tooltip-content")).not.toBeInTheDocument();

  const roleHeight = page.getByTestId("navigator-role").element().getBoundingClientRect().height;
  const sortHeight = page.getByTestId("navigator-sort").element().getBoundingClientRect().height;
  expect(roleHeight).toBe(sortHeight);

  await page.getByTestId("dialog-close").click();
  await expect.element(page.getByTestId("tooltip-content")).not.toBeInTheDocument();

  await toggle.hover();
  await expect.element(page.getByTestId("tooltip-content")).toBeVisible();
});
