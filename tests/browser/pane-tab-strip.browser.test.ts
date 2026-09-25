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
