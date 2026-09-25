import { expect, test } from "vitest";
import { render } from "vitest-browser-svelte";
import { page, userEvent } from "vitest/browser";
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
    buttons: type === "pointerup" ? 0 : 1,
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
  await expect.element(page.getByTestId("pane-drag-cursor-layer")).toBeVisible();
  const cursorLayer = page.getByTestId("pane-drag-cursor-layer").element() as HTMLElement;
  expect(document.elementFromPoint(x, y)).toBe(cursorLayer);
  expect(getComputedStyle(cursorLayer).cursor).toBe("grabbing");
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  const indicator = page.getByTestId("pane-drop-indicator").element() as HTMLElement;
  const indicatorRect = indicator.getBoundingClientRect();
  expect(indicatorRect.left).toBeGreaterThan(stripRect.left);
  expect(indicatorRect.right).toBeLessThan(chip("pane-1").getBoundingClientRect().left);
  await expect.element(page.getByTestId("pane-drag-preview")).toHaveTextContent(`Pane ${count}`);
  const preview = page.getByTestId("pane-drag-preview").element() as HTMLElement;
  await expect
    .poll(() => {
      const rect = preview.getBoundingClientRect();
      return Math.abs(rect.left + rect.width / 2 - x);
    })
    .toBeLessThan(1);
  expect(preview.getBoundingClientRect().top).toBeGreaterThan(y);
  const previewLeft = preview.getBoundingClientRect().left;
  window.dispatchEvent(pointerEvent("pointermove", x - 18, y));
  await expect.poll(() => preview.getBoundingClientRect().left).toBeLessThan(previewLeft);
  await expect
    .poll(() => {
      const rect = preview.getBoundingClientRect();
      return Math.abs(rect.left + rect.width / 2 - (x - 18));
    })
    .toBeLessThan(1);
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  window.dispatchEvent(pointerEvent("pointerup", x - 18, y));
  await expect.poll(() => paneOrder()[0]).toBe(`pane-${count}`);
  await expect.element(page.getByTestId("pane-drag-preview")).not.toBeInTheDocument();
  await expect.element(page.getByTestId("pane-drag-cursor-layer")).not.toBeInTheDocument();
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test("dragging the first pane to the end keeps the marker clear of the last chip", async () => {
  render(PaneTabStripHost, { count: 2 });
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const strip = page.getByTestId("app-pane-tab-strip").element() as HTMLElement;
  const source = chip("pane-1");
  const sourceRect = source.getBoundingClientRect();
  const stripRect = strip.getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  const x = stripRect.right + 12;

  source.dispatchEvent(pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y));
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  const indicatorRect = (
    page.getByTestId("pane-drop-indicator").element() as HTMLElement
  ).getBoundingClientRect();
  expect(indicatorRect.left).toBeGreaterThan(chip("pane-2").getBoundingClientRect().right);
  expect(indicatorRect.right).toBeLessThan(stripRect.right);
  window.dispatchEvent(pointerEvent("pointerup", x, y));
  await expect.poll(() => paneOrder()).toEqual(["pane-2", "pane-1"]);
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test("the insertion marker stays in the gap between chips", async () => {
  render(PaneTabStripHost, { count: 3 });
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const source = chip("pane-3");
  const sourceRect = source.getBoundingClientRect();
  const targetRect = chip("pane-2").getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  const x = targetRect.left + 2;

  source.dispatchEvent(pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y));
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  const indicatorRect = (
    page.getByTestId("pane-drop-indicator").element() as HTMLElement
  ).getBoundingClientRect();
  expect(indicatorRect.left).toBeGreaterThan(chip("pane-1").getBoundingClientRect().right);
  expect(indicatorRect.right).toBeLessThan(targetRect.left);
  window.dispatchEvent(pointerEvent("pointerup", x, y));
  await expect.poll(() => paneOrder()).toEqual(["pane-1", "pane-3", "pane-2"]);
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test("holding a dragged chip at the edge scrolls to off-screen panes", async () => {
  render(PaneTabStripHost);
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const strip = page.getByTestId("app-pane-tab-strip").element() as HTMLElement;
  const sourceRect = chip("pane-1").getBoundingClientRect();
  const stripRect = strip.getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  const x = stripRect.right - 8;

  chip("pane-1").dispatchEvent(
    pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y),
  );
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.poll(() => strip.scrollLeft).toBeGreaterThan(100);
  await expect
    .poll(() => strip.scrollWidth - strip.clientWidth - strip.scrollLeft)
    .toBeLessThan(20);
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  window.dispatchEvent(pointerEvent("pointerup", x, y));
  await expect.poll(() => paneOrder().at(-1)).toBe("pane-1");
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test("the marker disappears where releasing cannot reorder", async () => {
  render(PaneTabStripHost, { count: 3 });
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const sourceRect = chip("pane-3").getBoundingClientRect();
  const targetRect = chip("pane-1").getBoundingClientRect();
  const x = targetRect.left + 2;
  const y = sourceRect.top + sourceRect.height / 2;

  chip("pane-3").dispatchEvent(
    pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y),
  );
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  window.dispatchEvent(pointerEvent("pointermove", x, y + 40));
  await expect.element(page.getByTestId("pane-drop-indicator")).not.toBeInTheDocument();
  window.dispatchEvent(pointerEvent("pointerup", x, y + 40));
  expect(paneOrder()).toEqual(["pane-1", "pane-2", "pane-3"]);
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test("a missed release recovers when the pointer moves without a held button", async () => {
  render(PaneTabStripHost, { count: 2 });
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const sourceRect = chip("pane-2").getBoundingClientRect();
  const targetRect = chip("pane-1").getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  const x = targetRect.left + 2;

  chip("pane-2").dispatchEvent(
    pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y),
  );
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drag-cursor-layer")).toBeVisible();
  window.dispatchEvent(
    new PointerEvent("pointermove", { bubbles: true, pointerId: 1, clientX: x, clientY: y }),
  );
  await expect.element(page.getByTestId("pane-drag-cursor-layer")).not.toBeInTheDocument();
  expect(paneOrder()).toEqual(["pane-1", "pane-2"]);
});

test("an order change during a drag cancels the pending drop and chip click", async () => {
  render(PaneTabStripHost, { count: 3 });
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const source = chip("pane-2");
  const sourceRect = source.getBoundingClientRect();
  const targetRect = chip("pane-1").getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  const x = targetRect.left + 2;

  source.dispatchEvent(pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y));
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drop-indicator")).toBeVisible();
  (page.getByTestId("reorder-pane-externally").element() as HTMLButtonElement).click();
  await expect.poll(() => paneOrder()).toEqual(["pane-2", "pane-3", "pane-1"]);
  window.dispatchEvent(pointerEvent("pointermove", x, y));
  await expect.element(page.getByTestId("pane-drag-cursor-layer")).not.toBeInTheDocument();
  window.dispatchEvent(pointerEvent("pointerup", x, y));
  source.click();
  expect(paneOrder()).toEqual(["pane-2", "pane-3", "pane-1"]);
  await expect.element(page.getByTestId("pane-open-count")).toHaveTextContent("0");
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
});

test("unmounting during a drag removes its window listeners", async () => {
  const host = render(PaneTabStripHost, { count: 2 });
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  const sourceRect = chip("pane-2").getBoundingClientRect();
  const y = sourceRect.top + sourceRect.height / 2;
  chip("pane-2").dispatchEvent(
    pointerEvent("pointerdown", sourceRect.left + sourceRect.width / 2, y),
  );
  window.dispatchEvent(pointerEvent("pointermove", sourceRect.left - 20, y));
  await expect.element(page.getByTestId("pane-drag-cursor-layer")).toBeVisible();
  host.unmount();
  await expect.element(page.getByTestId("pane-drag-cursor-layer")).not.toBeInTheDocument();
  window.dispatchEvent(pointerEvent("pointermove", sourceRect.left - 30, y));
  window.dispatchEvent(pointerEvent("pointerup", sourceRect.left - 30, y));
});

test("a real click selects a pane, while a real drag only reorders it", async () => {
  render(PaneTabStripHost, { count: 2 });
  await expect.element(page.getByTestId("app-pane-tab-strip")).toBeInTheDocument();
  await userEvent.click(chip("pane-1"));
  await expect.element(page.getByTestId("pane-select-count")).toHaveTextContent("1");

  await userEvent.dragAndDrop(chip("pane-2"), chip("pane-1"));
  await expect.poll(() => paneOrder()).toEqual(["pane-2", "pane-1"]);
  await expect.element(page.getByTestId("pane-select-count")).toHaveTextContent("1");
  await expect.element(page.getByTestId("pane-open-count")).toHaveTextContent("0");
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
