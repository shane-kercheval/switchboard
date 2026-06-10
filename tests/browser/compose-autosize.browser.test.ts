import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));
// The compose bar subscribes to OS drag-drop on mount.
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => vi.fn()) }),
}));

import { mountComposeBar, mountPromptComposer } from "./composeMount";
import { resetState } from "./harness";
import { ALICE, PROJECT_ID } from "./fixtures";
import { _testing as composeTesting } from "$lib/state/composeStore";
import type { Prompt } from "$lib/types";

// Autosize is real geometry: the shared textarea writes an explicit height from
// a measured `scrollHeight`, capped by the INSTANCE's own max-height class.
// jsdom only ever sees mocked numbers; this spec proves the caps hold in real
// layout for both consumers — the compose bar and the prompt composer, whose
// caps differ (which is exactly why the cap is cached per instance).

const PROMPT: Prompt = {
  provider: "local",
  name: "review",
  title: "Code Review",
  description: "Review code",
  arguments: [{ name: "focus", description: "What to focus on", required: true }],
  tags: [],
};

const LINES_PAST_ANY_CAP = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join("\n");

beforeEach(() => {
  resetState();
  // localStorage is real here and the page is long-lived — without a reset, a
  // draft persisted by one test would be restored into the next test's mount.
  composeTesting.reset();
});

function heightOf(el: HTMLTextAreaElement): number {
  return el.getBoundingClientRect().height;
}

function capOf(el: HTMLTextAreaElement): number {
  return Number.parseFloat(getComputedStyle(el).maxHeight);
}

test("compose textarea grows with content, caps with inner scroll, shrinks on delete", async () => {
  mountComposeBar({ projectId: PROJECT_ID, agents: [ALICE] });
  const locator = page.getByTestId("compose-textarea");
  await expect.element(locator).toBeInTheDocument();
  const el = (): HTMLTextAreaElement => locator.element() as HTMLTextAreaElement;

  let initial = 0;
  await expect
    .poll(() => {
      initial = heightOf(el());
      return initial;
    })
    .toBeGreaterThan(0);

  await locator.fill("one\ntwo\nthree\nfour\nfive\nsix");
  await expect.poll(() => heightOf(el())).toBeGreaterThan(initial);
  const grown = heightOf(el());

  await locator.fill(LINES_PAST_ANY_CAP);
  await expect.poll(() => Math.abs(heightOf(el()) - capOf(el()))).toBeLessThanOrEqual(1);
  expect(el().scrollHeight).toBeGreaterThan(el().clientHeight);
  expect(el().style.overflowY).toBe("auto");

  await locator.fill("short");
  await expect.poll(() => heightOf(el())).toBeLessThan(grown);
  expect(el().style.overflowY).toBe("hidden");
});

test("prompt-composer argument textarea caps at its own (smaller) max-height", async () => {
  mountPromptComposer({ prompt: PROMPT, args: { focus: "" } });
  const locator = page.getByTestId("prompt-arg-focus");
  await expect.element(locator).toBeInTheDocument();
  const el = (): HTMLTextAreaElement => locator.element() as HTMLTextAreaElement;

  // A real, finite cap is in effect (`max-h-40` — not the compose bar's).
  await expect.poll(() => Number.isFinite(capOf(el()))).toBe(true);

  await locator.fill(LINES_PAST_ANY_CAP);
  await expect.poll(() => Math.abs(heightOf(el()) - capOf(el()))).toBeLessThanOrEqual(1);
  expect(el().scrollHeight).toBeGreaterThan(el().clientHeight);
  expect(el().style.overflowY).toBe("auto");
});
