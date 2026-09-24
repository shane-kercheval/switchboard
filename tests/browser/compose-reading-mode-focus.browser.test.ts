import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { page, userEvent } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => vi.fn()) }),
}));

import { mountComposeBar } from "./composeMount";
import { resetState } from "./harness";
import { ALICE, PROJECT_ID } from "./fixtures";
import { _testing as composeTesting } from "$lib/state/composeStore";
import {
  clearReadingMode,
  enterReadingMode,
  _testing as readingModeTesting,
} from "$lib/state/readingMode.svelte";

/// Reading mode removes the compose box while its textarea usually holds focus
/// (the user just sent). WebKit fires no focusout for a focused element that is
/// removed, so anything tracking focus by events alone goes stale here — jsdom
/// cannot show it, which is why this runs in a real browser.

beforeEach(() => {
  resetState();
  composeTesting.reset();
  readingModeTesting.reset();
});

afterEach(() => {
  readingModeTesting.reset();
  for (const el of document.querySelectorAll("[data-focus-elsewhere]")) el.remove();
});

function textarea(): HTMLTextAreaElement {
  return page.getByTestId("compose-textarea").element() as HTMLTextAreaElement;
}

function focusBorderLit(): boolean {
  return page.getByTestId("compose-box").element().classList.contains("border-focus");
}

async function hideComposeBoxWhileFocused(): Promise<void> {
  mountComposeBar({ projectId: PROJECT_ID, agents: [ALICE] });
  textarea().focus();
  await expect.poll(focusBorderLit).toBe(true);
  enterReadingMode(PROJECT_ID);
  await expect.poll(() => document.querySelector('[data-testid="compose-box"]')).toBeNull();
}

test("the compose box comes back with the cursor in it", async () => {
  await hideComposeBoxWhileFocused();

  clearReadingMode(PROJECT_ID);

  await expect
    .poll(() => (document.activeElement as HTMLElement | null)?.dataset.testid)
    .toBe("compose-textarea");
  expect(focusBorderLit()).toBe(true);
});

test("typing in another field keeps the cursor there, and the border off", async () => {
  await hideComposeBoxWhileFocused();
  const elsewhere = document.createElement("input");
  elsewhere.dataset.focusElsewhere = "";
  document.body.appendChild(elsewhere);
  elsewhere.focus();

  clearReadingMode(PROJECT_ID);
  await expect.poll(() => document.querySelector('[data-testid="compose-box"]')).not.toBeNull();
  await new Promise(requestAnimationFrame);
  await new Promise(requestAnimationFrame);

  expect(document.activeElement).toBe(elsewhere);
  // The border means "the cursor is in here" — lit without it is the lie that
  // made an unfocused box look ready to type into.
  expect(focusBorderLit()).toBe(false);
});

test("a highlight survives the box coming back, and typing still reaches the box", async () => {
  await hideComposeBoxWhileFocused();
  const reply = document.createElement("p");
  reply.dataset.focusElsewhere = "";
  reply.textContent = "text the user is about to copy";
  document.body.appendChild(reply);
  (document.activeElement as HTMLElement | null)?.blur();
  const range = document.createRange();
  range.selectNodeContents(reply);
  document.getSelection()!.removeAllRanges();
  document.getSelection()!.addRange(range);

  clearReadingMode(PROJECT_ID);
  await expect.poll(() => document.querySelector('[data-testid="compose-box"]')).not.toBeNull();
  await new Promise(requestAnimationFrame);
  await new Promise(requestAnimationFrame);

  // Focusing a field would have cleared the highlight.
  expect(document.getSelection()!.toString()).toBe("text the user is about to copy");
  expect(document.activeElement).not.toBe(textarea());

  // Copying is a shortcut, so the highlight stays and the box keeps waiting.
  await userEvent.keyboard("{Meta>}c{/Meta}");
  expect(document.getSelection()!.toString()).toBe("text the user is about to copy");
  expect(document.activeElement).not.toBe(textarea());

  // The first typed character lands in the box.
  await userEvent.keyboard("h");
  await expect.poll(() => document.activeElement).toBe(textarea());
  expect(textarea().value).toBe("h");
});
