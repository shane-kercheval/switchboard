import { beforeEach, expect, test, vi } from "vitest";
import { page, userEvent } from "vitest/browser";

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

import { render } from "vitest-browser-svelte";
import ComposeWithDialogHost from "./ComposeWithDialogHost.svelte";
import { ALICE, BOB, PROJECT_ID } from "./fixtures";
import { resetState } from "./harness";
import { _testing as composeTesting } from "$lib/state/composeStore";
import { setRecipients, selectionFor } from "$lib/state/recipientSelection.svelte";

/// Escape is shared: a dismissible layer closes on it, and the composer clears
/// its recipient chips on it. Which one acts depends on where focus is **at the
/// instant the window listener runs**, and that is an engine behavior — WebKit
/// restores the layer's pre-open focus inside the same keydown dispatch, so the
/// composer sees itself focused while the dialog it just closed is still in the
/// DOM. jsdom defers the restore instead and never shows the overlap, which is
/// why this spec has to run in a real browser.

beforeEach(() => {
  resetState();
  composeTesting.reset();
});

/// Open the host's dialog through the same path the navigator uses: auto-focus
/// prevented, its own field focused a tick later.
async function openDialogWithComposerFocused(): Promise<HTMLTextAreaElement> {
  const textarea = page.getByTestId("compose-textarea").element() as HTMLTextAreaElement;
  textarea.focus();
  // Dispatched rather than clicked: ⌘F is a keyboard shortcut, so the composer
  // keeps focus and stays the element the dialog restores to.
  page
    .getByTestId("host-open-dialog")
    .element()
    .dispatchEvent(new MouseEvent("click", { bubbles: true }));
  await expect.poll(() => document.querySelectorAll('[role="dialog"]').length).toBeGreaterThan(0);
  await expect
    .poll(() => (document.activeElement as HTMLElement | null)?.dataset?.testid)
    .toBe("host-dialog-field");
  return textarea;
}

test("closing a dialog with Escape leaves the recipient chips alone", async () => {
  // Reported symptom: ⌘F to find a message, Escape to close it, and the agent
  // chips come back unselected. The dialog's layer closes on the document
  // listener, restores focus to the composer, and this listener — running later
  // on the window — then read that focus as "the user pressed Escape in the
  // composer" and cleared the send targets.
  render(ComposeWithDialogHost, { projectId: PROJECT_ID, agents: [ALICE, BOB] });
  setRecipients(PROJECT_ID, [ALICE.id]);
  await expect.poll(() => selectionFor(PROJECT_ID)).toEqual([ALICE.id]);

  await openDialogWithComposerFocused();
  await userEvent.keyboard("{Escape}");

  await expect.poll(() => document.querySelectorAll('[role="dialog"]').length).toBe(0);
  expect(selectionFor(PROJECT_ID)).toEqual([ALICE.id]);
  // Focus coming back to the composer is the behavior that made this reachable;
  // asserting it keeps the test honest if a future dialog stops restoring focus.
  expect((document.activeElement as HTMLElement | null)?.dataset?.testid).toBe("compose-textarea");
});

test("Escape in the composer still clears the recipient chips", async () => {
  // The other half: with nothing else claiming the keystroke, Escape keeps its
  // compose-surface meaning. Without this, the guard above could be satisfied by
  // never clearing at all.
  render(ComposeWithDialogHost, { projectId: PROJECT_ID, agents: [ALICE, BOB] });
  setRecipients(PROJECT_ID, [ALICE.id]);
  await expect.poll(() => selectionFor(PROJECT_ID)).toEqual([ALICE.id]);

  const textarea = page.getByTestId("compose-textarea").element() as HTMLTextAreaElement;
  textarea.focus();
  await userEvent.keyboard("{Escape}");

  await expect.poll(() => selectionFor(PROJECT_ID)).toEqual([]);
});
