import { expect, test } from "vitest";
import { page, userEvent } from "vitest/browser";
import { render } from "vitest-browser-svelte";
import AgentEnvironmentHost from "./AgentEnvironmentHost.svelte";
import type { SessionInventory } from "$lib/types";

const DEFAULT_WIDTH = 280;
const MIN_WIDTH = 200;

const BUSY: SessionInventory = {
  mcp_servers: [
    { name: "a", status: "connected" },
    { name: "b", status: "connected" },
    { name: "c", status: "connected" },
    { name: "d", status: "connected" },
    { name: "e", status: "connected" },
    { name: "f", status: "needs-auth" },
    { name: "g", status: "needs-auth" },
  ],
  agents: ["a", "b", "c", "d", "e", "f"],
  plugins: [{ name: "p" }],
  skills: Array.from({ length: 30 }, (_, i) => ({ name: `s${i}` })),
  memory_paths: ["/m"],
};

function overflow(testid: string): number {
  const el = page.getByTestId(testid).element() as HTMLElement;
  return el.scrollWidth - el.clientWidth;
}

/// How many lines of its own text the row's **content box** occupies. Padding
/// and border are subtracted rather than absorbed into the ratio: measuring the
/// padded box left only ~1% of headroom under a `< 1.5` threshold, so restyling
/// the row's spacing would have failed a test whose name is about wrapping.
/// One line reads 1.0 here and a wrapped row reads 2.0, so the threshold sits
/// midway between the two states it distinguishes.
function contentLineCount(testid: string): number {
  const el = page.getByTestId(testid).element() as HTMLElement;
  const style = getComputedStyle(el);
  const vertical =
    Number.parseFloat(style.paddingTop) +
    Number.parseFloat(style.paddingBottom) +
    Number.parseFloat(style.borderTopWidth) +
    Number.parseFloat(style.borderBottomWidth);
  const contentHeight = el.getBoundingClientRect().height - vertical;
  return contentHeight / Number.parseFloat(style.lineHeight);
}

test("the card trigger stays to one line and shows only actionable status", async () => {
  render(AgentEnvironmentHost, { props: { width: DEFAULT_WIDTH, inventory: BUSY } });

  await expect
    .element(page.getByTestId("agent-env-trigger-summary"))
    .toHaveTextContent("2 need auth");
  expect(contentLineCount("agent-env-toggle")).toBeLessThan(1.5);
  expect(overflow("agent-env-toggle")).toBeLessThanOrEqual(1);
});

test("both kinds of MCP trouble remain available without wrapping the card", async () => {
  render(AgentEnvironmentHost, {
    props: {
      width: DEFAULT_WIDTH,
      inventory: {
        ...BUSY,
        mcp_servers: [...BUSY.mcp_servers!, { name: "h", status: "disconnected" }],
      },
    },
  });

  const row = page.getByTestId("agent-env-toggle");
  await expect
    .element(row)
    .toHaveAccessibleName("Environment details, 2 need auth · 1 need attention");
  expect(contentLineCount("agent-env-toggle")).toBeLessThan(1.5);
  expect(overflow("agent-env-toggle")).toBeLessThanOrEqual(1);
});

test("the full inventory moves to the bounded detail popover", async () => {
  render(AgentEnvironmentHost, { props: { width: DEFAULT_WIDTH, inventory: BUSY } });

  await page.getByTestId("agent-env-toggle").click();
  await expect.element(page.getByTestId("agent-env-detail")).toBeVisible();
  await expect
    .element(page.getByTestId("agent-env-inventory-summary"))
    .toHaveTextContent("MCP 7 · 2 need auth · Agents 6 · Plugins 1 · Skills 30 · Memory 1");

  const detail = page.getByTestId("agent-env-detail").element() as HTMLElement;
  expect(detail.scrollHeight).toBeGreaterThanOrEqual(detail.clientHeight);
  expect(detail.getBoundingClientRect().left).toBeGreaterThanOrEqual(0);
});

test("keyboard opening announces a dialog and Escape restores the trigger", async () => {
  render(AgentEnvironmentHost, { props: { width: DEFAULT_WIDTH, inventory: BUSY } });

  const trigger = page.getByTestId("agent-env-toggle");
  (trigger.element() as HTMLElement).focus();

  for (const key of ["{Enter}", "{Space}"]) {
    await userEvent.keyboard(key);
    const dialog = page.getByRole("dialog", { name: "Environment details" });
    await expect.element(dialog).toBeVisible();

    await userEvent.keyboard("{Escape}");
    await expect.element(dialog).not.toBeInTheDocument();
    expect(document.activeElement).toBe(trigger.element());
  }
});

test("opening details does not focus or open the connected-status tooltip", async () => {
  render(AgentEnvironmentHost, { props: { width: DEFAULT_WIDTH, inventory: BUSY } });

  await page.getByTestId("agent-env-toggle").click();
  await expect.element(page.getByTestId("agent-env-detail")).toBeVisible();

  const connected = page.getByTestId("agent-env-mcp-dot").first();
  const dot = connected.element() as HTMLElement;
  expect(dot.classList.contains("bg-accent")).toBe(true);
  expect(dot.tabIndex).toBe(-1);
  expect(document.activeElement).not.toBe(dot);
  expect(document.querySelector('[data-testid="tooltip-content"]')).toBeNull();

  await connected.hover();
  await expect.element(page.getByTestId("tooltip-content")).toHaveTextContent("connected");
});

test("the trigger remains a single contained row at the minimum sidebar width", async () => {
  render(AgentEnvironmentHost, { props: { width: MIN_WIDTH, inventory: BUSY } });

  await expect.element(page.getByTestId("agent-env-toggle")).toBeInTheDocument();
  expect(contentLineCount("agent-env-toggle")).toBeLessThan(1.5);
  expect(overflow("agent-env-toggle")).toBeLessThanOrEqual(1);
});
