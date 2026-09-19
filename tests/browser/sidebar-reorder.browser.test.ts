import { beforeEach, expect, test, vi } from "vitest";
import { page, userEvent } from "vitest/browser";

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
  setAgentSelection: vi.fn(async () => undefined),
}));

import { render } from "vitest-browser-svelte";
import SidebarHost from "./SidebarHost.svelte";
import { PROJECT_ID, ALICE } from "./fixtures";
import { setRecipients, _testing as selectionState } from "$lib/state/recipientSelection.svelte";
import { layout } from "$lib/layout.svelte";
import type { AgentRecord } from "$lib/types";

// Two extra agents to give the roster a real layout to measure against.
const BOB: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: PROJECT_ID,
  name: "bob",
  harness: "codex",
  session_locator: null,
  model: null,
  effort: null,
  model_choices: [],
  effort_choices: [],
  created_at: "2026-05-16T00:00:01Z",
};

const CAROL: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000ccc",
  project_id: PROJECT_ID,
  name: "carol",
  harness: "antigravity",
  session_locator: null,
  model: null,
  effort: null,
  model_choices: [],
  effort_choices: [],
  created_at: "2026-05-16T00:00:02Z",
};

const THREE_AGENTS = [ALICE, BOB, CAROL];

const LONG_ALICE: AgentRecord = { ...ALICE, name: "shared-agent-a" };
const LONG_BOB: AgentRecord = { ...BOB, name: "shared-agent-b" };

beforeEach(() => {
  reorderAgentsMock.mockReset();
  reorderAgentsMock.mockResolvedValue(undefined);
  selectionState.reset();
  layout.agentsSidebarWidth = 280;
});

test("selected recipients keep a thin accent outline at rest and on hover", async () => {
  setRecipients(PROJECT_ID, [ALICE.id]);
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });

  const selected = page.getByTestId("sidebar-agent").nth(0);
  const unselected = page.getByTestId("sidebar-agent").nth(1);
  const swatch = document.createElement("div");
  swatch.style.color = "var(--accent)";
  document.body.append(swatch);
  const accent = getComputedStyle(swatch).color;
  swatch.remove();

  expect(selected.element()).toHaveAttribute("data-recipient-selected", "true");
  expect(unselected.element()).toHaveAttribute("data-recipient-selected", "false");
  expect(getComputedStyle(selected.element()).boxShadow).toContain(accent);
  expect(getComputedStyle(unselected.element()).boxShadow).not.toContain(accent);

  await selected.hover();
  await expect.poll(() => getComputedStyle(selected.element()).boxShadow).toContain(accent);
});

test("the clickable card surface gains an outline and icon controls retain distinct hovers", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });

  const card = page.getByTestId("sidebar-agent").first();
  const cardResting = getComputedStyle(card.element()).backgroundColor;
  const restingShadow = getComputedStyle(card.element()).boxShadow;
  await card.hover();
  expect(getComputedStyle(card.element()).backgroundColor).toBe(cardResting);
  await expect.poll(() => getComputedStyle(card.element()).boxShadow !== restingShadow).toBe(true);

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

// Hover controls give their width back when hidden: the card's header reserves
// no gutter for them, so the name owns the full column until they are revealed.
// This pins both halves — nothing reserved at rest, and revealing the cluster
// stays inside the card rather than pushing the identity icon out of it.
test("hover controls reserve no width at rest and stay inside the card when revealed", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });

  for (let i = 0; i < 3; i++) {
    await expect.element(page.getByTestId("agent-drag-grip").nth(i)).not.toBeVisible();
  }

  const card = page.getByTestId("sidebar-agent").nth(0);
  const name = page.getByTestId("agent-name").nth(0).element() as HTMLElement;
  const harness = page.getByTestId("agent-harness-icon").nth(0).element() as HTMLElement;
  const eye = page.getByTestId("agent-visibility-toggle").nth(0).element() as HTMLElement;
  const actions = page.getByTestId("agent-actions-trigger").nth(0).element() as HTMLElement;

  // At rest the three hidden controls are literally zero-width, so the gap the
  // name leaves is slack it did not need — not a gutter held for them.
  for (const control of [eye, actions]) {
    expect(control.getBoundingClientRect().width).toBe(0);
  }
  // The name's column, not the text span: a short name sizes its span to
  // content, so the column is where the reclaimed width actually shows up.
  const nameColumn = name.parentElement;
  if (nameColumn === null) throw new Error("expected the name to sit in a column");
  const restingColumnWidth = nameColumn.getBoundingClientRect().width;
  const restingHarnessX = harness.getBoundingClientRect().x;

  await card.hover();
  await expect.element(page.getByTestId("agent-drag-grip").nth(0)).toBeVisible();
  await expect.poll(() => eye.getBoundingClientRect().width).toBeGreaterThan(20);

  // Revealing them costs the name width — the trade this design makes, and the
  // reason the name carries a tooltip — but the cluster stays ordered and
  // wholly inside the card.
  const gripRect = (
    page.getByTestId("agent-drag-grip").nth(0).element() as HTMLElement
  ).getBoundingClientRect();
  expect(gripRect.x).toBeGreaterThanOrEqual(harness.getBoundingClientRect().right);
  expect(card.element().getBoundingClientRect().right - gripRect.right).toBeGreaterThanOrEqual(8);
  expect(harness.getBoundingClientRect().x).toBeLessThan(restingHarnessX);
  expect(nameColumn.getBoundingClientRect().width).toBeLessThan(restingColumnWidth);
  // Other cards' grips are unaffected.
  await expect.element(page.getByTestId("agent-drag-grip").nth(1)).not.toBeVisible();
});

// The gutter used to cost the name 59px at the default width while it was
// clipped by 39px. Reclaiming it is the whole point, so pin the outcome: a
// realistic long name reads in full at rest.
test("a long agent name reads in full at the default width once nothing is reserved", async () => {
  await page.viewport(1600, 900);
  layout.agentsSidebarWidth = 280;
  const longName = "claude-fable-claude-fable";
  render(SidebarHost, {
    projectId: PROJECT_ID,
    agents: [{ ...ALICE, name: longName }, BOB],
  });

  const name = page.getByTestId("agent-name").first();
  await expect.element(name).toHaveAttribute("data-truncated", "false");
  const el = name.element() as HTMLElement;
  expect(el.scrollWidth - el.clientWidth).toBe(0);
  expect(el.textContent?.trim()).toBe(longName);
});

// The reserved action gutter is what narrows this column, so these record the
// price it charges: at which widths a realistic name clips, and that the full
// value is recoverable at every one of them. `data-truncated` is the component's
// own measurement, so the assertion tracks what actually drives the tooltip
// rather than re-deriving it here.
test.each([
  { width: 280, truncated: "false" },
  { width: 240, truncated: "true" },
  { width: 200, truncated: "true" },
])("the full agent name remains available at a $width px sidebar", async ({ width, truncated }) => {
  await page.viewport(1600, 900);
  layout.agentsSidebarWidth = width;
  render(SidebarHost, { projectId: PROJECT_ID, agents: [LONG_ALICE, LONG_BOB, CAROL] });

  const card = page.getByTestId("sidebar-agent").first();
  const name = page.getByTestId("agent-name").first();
  await expect.element(name).toHaveAttribute("data-truncated", truncated);
  const nameElement = name.element() as HTMLElement;
  expect(nameElement.scrollWidth - nameElement.clientWidth > 1).toBe(truncated === "true");
  // Keyboard users never reach the hover tooltip (it is not focusable), so the
  // card's own accessible name has to carry the full value.
  await expect.element(card).toHaveAccessibleName(/shared-agent-a/);
});

test("a clipped agent name reveals its full value on hover; a fitting one stays quiet", async () => {
  await page.viewport(1600, 900);
  layout.agentsSidebarWidth = 200;
  render(SidebarHost, { projectId: PROJECT_ID, agents: [LONG_ALICE, LONG_BOB, CAROL] });

  const clipped = page.getByTestId("agent-name").first();
  await expect.element(clipped).toHaveAttribute("data-truncated", "true");
  await clipped.hover();
  await expect.element(page.getByTestId("tooltip-content")).toHaveTextContent("shared-agent-a");

  // CAROL's name fits even at the floor, so hovering it must not raise a
  // tooltip that only repeats text already on screen.
  const fitting = page.getByTestId("agent-name").nth(2);
  await expect.element(fitting).toHaveAttribute("data-truncated", "false");
  await fitting.hover();
  await expect.poll(() => document.querySelector('[data-testid="tooltip-content"]')).toBeNull();
});

test("pointer focus does not pin a card's hover controls after the pointer leaves", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });

  const firstCard = page.getByTestId("sidebar-agent").nth(0);
  await firstCard.click({ position: { x: 4, y: 4 } });
  expect(document.activeElement).toBe(firstCard.element());

  await page.getByTestId("sidebar-agent").nth(1).hover();

  await expect.element(page.getByTestId("agent-visibility-toggle").nth(0)).not.toBeVisible();
  await expect.element(page.getByTestId("agent-actions-trigger").nth(0)).not.toBeVisible();
  await expect.element(page.getByTestId("agent-drag-grip").nth(0)).not.toBeVisible();
  await expect.element(page.getByTestId("agent-actions-trigger").nth(1)).toBeVisible();
});

test("keyboard focus reveals the card controls and keeps them visible within the card", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });

  const firstCard = page.getByTestId("sidebar-agent").nth(0);
  for (let i = 0; i < 4 && document.activeElement !== firstCard.element(); i += 1) {
    await userEvent.tab();
  }
  expect(document.activeElement).toBe(firstCard.element());
  expect(firstCard.element().matches(":focus-visible")).toBe(true);
  await expect.element(page.getByTestId("agent-actions-trigger").nth(0)).toBeVisible();

  const collapse = page.getByTestId("agent-collapse-toggle").nth(0).element() as HTMLElement;
  collapse.focus();
  expect(document.activeElement).toBe(collapse);
  await userEvent.tab();
  expect(document.activeElement).toBe(page.getByTestId("agent-visibility-toggle").nth(0).element());
  await expect.element(page.getByTestId("agent-actions-trigger").nth(0)).toBeVisible();
});

test("an open actions trigger keeps the entire card action cluster stable", async () => {
  render(SidebarHost, { projectId: PROJECT_ID, agents: THREE_AGENTS });

  const card = page.getByTestId("sidebar-agent").nth(0);
  await card.hover();
  const trigger = page.getByTestId("agent-actions-trigger").nth(0).element();
  // The menu primitive owns this state; setting it directly isolates the card's
  // real CSS latch without coupling this layout test to the portaled menu lifecycle.
  trigger.setAttribute("data-state", "open");
  await page.getByTestId("sidebar-agent").nth(1).hover();

  await expect.element(page.getByTestId("agent-visibility-toggle").nth(0)).toBeVisible();
  await expect.element(page.getByTestId("agent-actions-trigger").nth(0)).toBeVisible();
  await expect.element(page.getByTestId("agent-drag-grip").nth(0)).toBeVisible();

  trigger.setAttribute("data-state", "closed");
  await expect.element(page.getByTestId("agent-visibility-toggle").nth(0)).not.toBeVisible();
  await expect.element(page.getByTestId("agent-drag-grip").nth(0)).not.toBeVisible();
});

// ---------------------------------------------------------------------------
// Drag gesture — midpoint math against real card geometry
// ---------------------------------------------------------------------------

/// Reveal card 0's grip via hover and return it ready to drag. The
/// drag listens on `window` for the gesture's lifetime, so events dispatched
/// on the grip reach it by bubbling — the same path real pointer events take.
async function armedGrip(): Promise<{ grip: HTMLElement; gripRect: DOMRect }> {
  await page.getByTestId("sidebar-agent").nth(0).hover();
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
