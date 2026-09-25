import { expect, test } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import PaneTabStripHost from "./PaneTabStripHost.svelte";

function chip(paneId: string): HTMLButtonElement {
  return document.querySelector(
    `[data-testid="app-pane-tab"][data-pane-id="${paneId}"]`,
  ) as HTMLButtonElement;
}

function paneOrder(): string[] {
  return [...document.querySelectorAll<HTMLElement>('[data-testid="app-pane-tab"]')].map(
    (element) => element.dataset.paneId!,
  );
}

function pointerEvent(type: string, x: number, y: number): PointerEvent {
  return new PointerEvent(type, {
    bubbles: true,
    button: 0,
    pointerId: 1,
    clientX: x,
    clientY: y,
  });
}

test("overflowing pane chips remain reachable without displacing fixed header controls", async () => {
  render(PaneTabStripHost);
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();

  const strip = page.getByTestId("app-pane-tab-strip").element() as HTMLElement;
  const header = page.getByTestId("pane-strip-header").element() as HTMLElement;
  const paneControl = page.getByTestId("fixed-pane-control").element() as HTMLElement;
  const viewControl = page.getByTestId("fixed-view-control").element() as HTMLElement;

  await expect.poll(() => strip.scrollWidth > strip.clientWidth).toBe(true);
  const headerRect = header.getBoundingClientRect();
  for (const control of [paneControl, viewControl]) {
    const rect = control.getBoundingClientRect();
    expect(rect.left).toBeGreaterThanOrEqual(headerRect.left);
    expect(rect.right).toBeLessThanOrEqual(headerRect.right);
  }

  const chips = page.getByTestId("app-pane-tab");
  const lastChip = chips.nth(9).element() as HTMLButtonElement;
  expect(lastChip.getBoundingClientRect().right).toBeGreaterThan(
    strip.getBoundingClientRect().right,
  );

  lastChip.focus();
  await expect.poll(() => document.activeElement === lastChip).toBe(true);
  await expect
    .poll(() => {
      const stripRect = strip.getBoundingClientRect();
      const chipRect = lastChip.getBoundingClientRect();
      return chipRect.left >= stripRect.left - 1 && chipRect.right <= stripRect.right + 1;
    })
    .toBe(true);
});

test("dragging a minimized chip reorders the strip without opening or selecting a pane", async () => {
  render(PaneTabStripHost);
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const source = chip("pane-2");
  const target = chip("pane-1");
  const sourceRect = source.getBoundingClientRect();
  const targetRect = target.getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  const x = targetRect.left + 4;

  source.dispatchEvent(pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y));
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  expect(paneOrder().slice(0, 2)).toEqual(["pane-1", "pane-2"]);

  window.dispatchEvent(pointerEvent("pointerup", x, y));
  source.click();
  await expect.poll(() => paneOrder().slice(0, 2)).toEqual(["pane-2", "pane-1"]);
  expect(chip("pane-2").dataset.paneState).toBe("minimized");
  await expect.element(page.getByTestId("pane-select-count")).toHaveTextContent("0");
  await expect.element(page.getByTestId("pane-open-count")).toHaveTextContent("0");
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test.each([2, 3])("dragging the last of %i panes before the leftmost pane", async (count) => {
  render(PaneTabStripHost, { count });
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const strip = page.getByTestId("app-pane-tab-strip").element() as HTMLElement;
  const source = chip(`pane-${count}`);
  const sourceRect = source.getBoundingClientRect();
  const stripRect = strip.getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  const x = stripRect.left - 12;

  source.dispatchEvent(pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y));
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  const indicator = page.getByTestId("pane-drop-indicator").element() as HTMLElement;
  expect(indicator.getBoundingClientRect().left).toBeGreaterThanOrEqual(stripRect.left);
  await expect.element(page.getByTestId("pane-drag-preview")).toHaveTextContent(`Pane ${count}`);
  const preview = page.getByTestId("pane-drag-preview").element() as HTMLElement;
  const previewLeft = preview.getBoundingClientRect().left;
  window.dispatchEvent(pointerEvent("pointermove", x - 18, y));
  await expect.poll(() => preview.getBoundingClientRect().left).toBeLessThan(previewLeft);
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  window.dispatchEvent(pointerEvent("pointerup", x - 18, y));
  await expect.poll(() => paneOrder()[0]).toBe(`pane-${count}`);
  await expect.element(page.getByTestId("pane-drag-preview")).not.toBeInTheDocument();
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test("pane tooltip puts chip actions in a muted line below the divider", async () => {
  render(PaneTabStripHost, { count: 2 });
  await page.getByTestId("app-pane-tab").first().hover();
  const tooltip = page.getByTestId("tooltip-content");
  await expect.element(tooltip).toBeVisible();
  await expect.element(tooltip).toHaveTextContent("Pane 1 — visible.");
  const action = page.getByTestId("pane-chip-tooltip-action");
  await expect.element(action).toHaveTextContent("Click to select. Drag to rearrange.");
  await expect.element(action).toHaveClass("border-t");
});

test("Escape cancels a pane drag without opening the chip", async () => {
  render(PaneTabStripHost);
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const source = chip("pane-2");
  const sourceRect = source.getBoundingClientRect();
  const targetRect = chip("pane-1").getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  const x = targetRect.left + 4;

  source.dispatchEvent(pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y));
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  window.dispatchEvent(pointerEvent("pointerup", x, y));
  source.click();
  expect(paneOrder().slice(0, 2)).toEqual(["pane-1", "pane-2"]);
  await expect.element(page.getByTestId("pane-open-count")).toHaveTextContent("0");
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test("a chip press below the drag threshold keeps its click action", async () => {
  render(PaneTabStripHost);
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const source = chip("pane-2");
  const rect = source.getBoundingClientRect();
  const x = rect.left + rect.width / 2;
  const y = rect.top + rect.height / 2;
  source.dispatchEvent(pointerEvent("pointerdown", x, y));
  window.dispatchEvent(pointerEvent("pointermove", x + 2, y));
  window.dispatchEvent(pointerEvent("pointerup", x + 2, y));
  source.click();
  await expect.element(page.getByTestId("pane-open-count")).toHaveTextContent("1");
  chip("pane-1").click();
  await expect.element(page.getByTestId("pane-select-count")).toHaveTextContent("1");
});
