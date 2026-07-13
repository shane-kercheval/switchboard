import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

// `vi.hoisted` makes this reference available inside the hoisted `vi.mock`
// factory below — plain `const` declarations run after hoisting and would be
// in the TDZ when the factory is evaluated.
const { reorderAgentsMock } = vi.hoisted(() => ({
  reorderAgentsMock: vi.fn<(projectId: string, orderedIds: string[]) => Promise<void>>(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));
vi.mock("$lib/state/workspace.svelte", () => ({
  removeAgent: vi.fn(async () => undefined),
  renameAgent: vi.fn(async () => undefined),
  reorderAgents: (projectId: string, orderedIds: string[]) =>
    reorderAgentsMock(projectId, orderedIds),
  setAgentModel: vi.fn(async () => undefined),
  setAgentEffort: vi.fn(async () => undefined),
}));

import { render } from "vitest-browser-svelte";
import SidebarHost from "./SidebarHost.svelte";
import { PROJECT_ID, ALICE } from "./fixtures";
import type { AgentRecord } from "$lib/types";

// Two extra agents to give the roster a real layout to measure against.
const BOB: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: PROJECT_ID,
  name: "bob",
  harness: "codex",
  session_locator: null,
  created_at: "2026-05-16T00:00:01Z",
};

const CAROL: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000ccc",
  project_id: PROJECT_ID,
  name: "carol",
  harness: "gemini",
  session_locator: null,
  created_at: "2026-05-16T00:00:02Z",
};

const THREE_AGENTS = [ALICE, BOB, CAROL];

beforeEach(() => {
  reorderAgentsMock.mockReset();
  reorderAgentsMock.mockResolvedValue(undefined);
});

test("only actionable card controls receive hover fills", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });

  const card = page.getByTestId("sidebar-agent").first();
  const cardResting = getComputedStyle(card.element()).backgroundColor;
  await card.hover();
  expect(getComputedStyle(card.element()).backgroundColor).toBe(cardResting);

  const collapse = page.getByTestId("agent-collapse-toggle").first();
  await collapse.hover();
  await expect
    .poll(() => getComputedStyle(collapse.element()).backgroundColor !== cardResting)
    .toBe(true);

  await card.hover();
  const visibility = page.getByTestId("agent-visibility-toggle").first();
  await visibility.hover();
  await expect
    .poll(() => getComputedStyle(visibility.element()).backgroundColor !== cardResting)
    .toBe(true);
});

// ---------------------------------------------------------------------------
// CSS visibility — what jsdom physically cannot exercise
// ---------------------------------------------------------------------------

// The grip is `invisible` (visibility:hidden, slot width preserved) by
// default; `group-focus-within:visible` reveals it when any element inside the
// card's `group` div receives focus. This tests real WebKit CSS cascade —
// jsdom has no layout engine, so hidden elements still receive events there
// and this class swap goes unverified.
test("drag grip is hidden by default and visible when focus enters its card", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });

  // All three grips start hidden (Tailwind `hidden` = display:none).
  for (let i = 0; i < 3; i++) {
    await expect.element(page.getByTestId("agent-drag-grip").nth(i)).not.toBeVisible();
  }

  // Programmatic focus on the collapse-toggle button (the first button inside
  // the card) triggers `:focus-within` on the `group` ancestor, which the
  // `group-focus-within:block` modifier responds to.
  const collapseBtn = page
    .getByTestId("sidebar-agent")
    .nth(0)
    .getByRole("button")
    .first()
    .element() as HTMLButtonElement;
  collapseBtn.focus();

  await expect.element(page.getByTestId("agent-drag-grip").nth(0)).toBeVisible();
  expect(
    (page.getByTestId("agent-drag-grip").nth(0).element() as HTMLElement).getBoundingClientRect().x,
  ).toBeGreaterThan(
    (page.getByTestId("agent-harness-icon").nth(0).element() as HTMLElement).getBoundingClientRect()
      .x,
  );
  // Other cards' grips are unaffected.
  await expect.element(page.getByTestId("agent-drag-grip").nth(1)).not.toBeVisible();
});

// ---------------------------------------------------------------------------
// Drag gesture — midpoint math against real card geometry
// ---------------------------------------------------------------------------

/// Reveal card 0's grip via focus-within and return it ready to drag. The
/// drag listens on `window` for the gesture's lifetime, so events dispatched
/// on the grip reach it by bubbling — the same path real pointer events take.
async function armedGrip(): Promise<{ grip: HTMLElement; gripRect: DOMRect }> {
  const collapseBtn = page
    .getByTestId("sidebar-agent")
    .nth(0)
    .getByRole("button")
    .first()
    .element() as HTMLButtonElement;
  collapseBtn.focus();
  await expect.element(page.getByTestId("agent-drag-grip").nth(0)).toBeVisible();
  const grip = page.getByTestId("agent-drag-grip").nth(0).element() as HTMLElement;
  return { grip, gripRect: grip.getBoundingClientRect() };
}

function cardMidY(index: number): number {
  const rect = (
    page.getByTestId("sidebar-agent").nth(index).element() as HTMLElement
  ).getBoundingClientRect();
  return rect.top + rect.height / 2;
}

function pointer(grip: HTMLElement, type: string, x: number, y: number): void {
  grip.dispatchEvent(
    new PointerEvent(type, { pointerId: 1, button: 0, clientX: x, clientY: y, bubbles: true }),
  );
}

// jsdom reports zero-height rects for every card, so `dropIndexForPointer`
// always resolves to "after all others" regardless of actual pointer position.
// A drop-to-end assertion would therefore pass under zero geometry too; the
// discriminating case is a MID-LIST drop — past BOB's midpoint but short of
// CAROL's — which only real card heights can produce: [BOB, ALICE, CAROL].
test("drag to a mid-list position commits the order only real geometry can produce", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });
  const { grip, gripRect } = await armedGrip();

  // Between the midpoints of cards 1 and 2 (BOB, CAROL — ALICE is lifted).
  const targetY = (cardMidY(1) + cardMidY(2)) / 2;

  pointer(grip, "pointerdown", gripRect.left, gripRect.top);
  pointer(grip, "pointermove", gripRect.left, targetY);
  pointer(grip, "pointerup", gripRect.left, targetY);

  await expect.poll(() => reorderAgentsMock.mock.calls.length).toBeGreaterThan(0);
  expect(reorderAgentsMock).toHaveBeenCalledWith(PROJECT_ID, [BOB.id, ALICE.id, CAROL.id]);
});

test("Escape cancels an in-flight drag without committing", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });
  const { grip, gripRect } = await armedGrip();
  const targetY = (cardMidY(1) + cardMidY(2)) / 2;

  pointer(grip, "pointerdown", gripRect.left, gripRect.top);
  pointer(grip, "pointermove", gripRect.left, targetY);
  window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  pointer(grip, "pointerup", gripRect.left, targetY);

  // The cards return to roster order and nothing is committed.
  await expect
    .poll(() =>
      Array.from(document.querySelectorAll("[data-agent-id]")).map((el) =>
        el.getAttribute("data-agent-id"),
      ),
    )
    .toEqual(THREE_AGENTS.map((a) => a.id));
  expect(reorderAgentsMock).not.toHaveBeenCalled();
});
