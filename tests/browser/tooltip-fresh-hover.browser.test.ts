import { tick } from "svelte";
import { expect, test, vi } from "vitest";
import { page, userEvent } from "vitest/browser";
import { render } from "vitest-browser-svelte";
import TooltipFreshHoverHost from "./TooltipFreshHoverHost.svelte";

test("an activated state-control tooltip waits for a fresh hover", async () => {
  render(TooltipFreshHoverHost);
  const trigger = page.getByTestId("tooltip-fresh-trigger");

  await trigger.hover();
  await expect.element(page.getByTestId("tooltip-content")).toBeVisible();

  await trigger.click();
  await expect.element(page.getByTestId("tooltip-content")).not.toBeInTheDocument();

  vi.useFakeTimers();
  try {
    await trigger.hover({ position: { x: 2, y: 2 } });
    await vi.advanceTimersByTimeAsync(1000);
    await tick();
    expect(document.querySelector('[data-testid="tooltip-content"]')).toBeNull();

    await page.getByTestId("tooltip-parking").hover();
    await trigger.hover();
    await vi.advanceTimersByTimeAsync(499);
    await tick();
    expect(document.querySelector('[data-testid="tooltip-content"]')).toBeNull();

    await vi.advanceTimersByTimeAsync(1);
    await tick();
    expect(document.querySelector('[data-testid="tooltip-content"]')).not.toBeNull();
  } finally {
    vi.useRealTimers();
  }
});

test("real keyboard focus opens a state-control tooltip", async () => {
  render(TooltipFreshHoverHost);
  const trigger = page.getByTestId("tooltip-fresh-trigger");

  await page.getByTestId("tooltip-before").click();
  await userEvent.tab();
  expect(document.activeElement).toBe(trigger.element());
  expect(trigger.element().matches(":focus-visible")).toBe(true);
  await expect.element(page.getByTestId("tooltip-content")).toBeVisible();
});
