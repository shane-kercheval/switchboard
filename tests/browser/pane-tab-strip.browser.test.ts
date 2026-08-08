import { expect, test } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import PaneTabStripHost from "./PaneTabStripHost.svelte";

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
