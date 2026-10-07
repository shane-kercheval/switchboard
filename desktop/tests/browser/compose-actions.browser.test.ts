import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_prompts" || cmd === "list_workflows") return [];
    return null;
  }),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => vi.fn()) }),
}));

import { mountPanesWithComposer, mountPromptComposer } from "./composeMount";
import { ALICE, BOB, PROJECT_ID } from "./fixtures";
import { resetState } from "./harness";
import { _testing as composeTesting } from "$lib/state/composeStore";
import type { Prompt } from "$lib/types";

beforeEach(() => {
  resetState();
  composeTesting.reset();
});

function rect(testid: string): DOMRect {
  return page.getByTestId(testid).element().getBoundingClientRect();
}

test("the compact action rail stays contained and adjacent tooltips never block its controls", async () => {
  mountPanesWithComposer({ projectId: PROJECT_ID, agents: [ALICE, BOB], width: 340 });

  const box = rect("compose-box");
  const rail = rect("compose-action-rail");
  expect(rail.right).toBeLessThanOrEqual(box.right);
  expect(rail.top).toBeGreaterThanOrEqual(box.top);
  expect(rail.bottom).toBeLessThanOrEqual(box.bottom);

  for (const testid of [
    "compose-forward-button",
    "compose-prompt-button",
    "compose-workflow-button",
  ]) {
    const control = rect(testid);
    expect(Math.abs(control.width - 28)).toBeLessThanOrEqual(1);
    expect(Math.abs(control.height - 28)).toBeLessThanOrEqual(1);
  }

  const workflow = page.getByTestId("compose-workflow-button");
  await workflow.hover();
  await expect.element(page.getByTestId("tooltip-content")).toHaveTextContent("Run a workflow");

  const prompt = page.getByTestId("compose-prompt-button");
  await prompt.hover();
  await expect.element(page.getByTestId("tooltip-content")).toHaveTextContent("Insert a prompt");
  await prompt.click();
  await expect.element(page.getByTestId("prompt-menu")).toBeVisible();
});

test("overflowing prompt fields keep their action column aligned and clickable", async () => {
  const arguments_ = Array.from({ length: 12 }, (_, index) => ({
    name: `field_${index + 1}`,
    description: `Field ${index + 1}`,
    required: false,
  }));
  const prompt: Prompt = {
    provider: "local",
    name: "many-fields",
    title: "Many fields",
    description: "Exercises the capped prompt scroller.",
    arguments: arguments_,
    tags: [],
  };
  const args = Object.fromEntries(arguments_.map((argument) => [argument.name, ""]));
  mountPromptComposer({ prompt, args, agents: [ALICE, BOB], width: 340 });

  const scroll = page.getByTestId("prompt-fields-scroll").element();
  await expect.poll(() => scroll.scrollHeight > scroll.clientHeight).toBe(true);

  const remove = rect("prompt-remove");
  const forward = rect("prompt-arg-forward-field_1");
  const send = rect("prompt-send-probe");
  expect(Math.abs(remove.right - forward.right)).toBeLessThanOrEqual(1);
  expect(Math.abs(forward.right - send.right)).toBeLessThanOrEqual(1);
  expect(Math.abs(forward.width - 28)).toBeLessThanOrEqual(1);
  expect(Math.abs(forward.height - 28)).toBeLessThanOrEqual(1);

  await page.getByTestId("prompt-arg-forward-field_1").click();
  await expect.element(page.getByTestId("forward-picker-menu")).toBeVisible();
});
