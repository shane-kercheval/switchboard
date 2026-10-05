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
import DialogThenComposeFocusHost from "./DialogThenComposeFocusHost.svelte";
import { ALICE, PROJECT_ID } from "./fixtures";
import { resetState } from "./harness";
import { _testing as composeTesting } from "$lib/state/composeStore";

/// Adding an agent that fills an empty pane closes the add-agent dialog and puts
/// the cursor in the composer. Closing a dialog hands focus back to whatever
/// opened it, synchronously as it unmounts, so the order matters — and jsdom
/// doesn't reproduce the hand-back, which is why this runs in a real browser.

beforeEach(() => {
  resetState();
  composeTesting.reset();
});

async function submitDialog(waitForClose: boolean): Promise<string | undefined> {
  render(DialogThenComposeFocusHost, { projectId: PROJECT_ID, agents: [ALICE], waitForClose });
  await userEvent.click(page.getByTestId("host-open-dialog"));
  await expect.poll(() => document.querySelectorAll('[role="dialog"]').length).toBeGreaterThan(0);
  await userEvent.click(page.getByTestId("host-submit"));
  await expect.poll(() => document.querySelectorAll('[role="dialog"]').length).toBe(0);
  await new Promise(requestAnimationFrame);
  await new Promise(requestAnimationFrame);
  return (document.activeElement as HTMLElement | null)?.dataset?.testid;
}

test("asking for compose focus after the dialog has closed puts the cursor in the composer", async () => {
  expect(await submitDialog(true)).toBe("compose-textarea");
});

test("asking in the same update that closes the dialog loses focus to the dialog's hand-back", async () => {
  // Pins why `handleAddAgent` waits a tick: without it the request is spent
  // before the dialog restores focus to the "+" that opened it.
  expect(await submitDialog(false)).not.toBe("compose-textarea");
});
