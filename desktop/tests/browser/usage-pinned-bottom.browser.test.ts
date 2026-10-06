import { beforeEach, expect, test, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));
vi.mock("$lib/state/workspace.svelte", () => ({
  removeAgent: vi.fn(async () => undefined),
  renameAgent: vi.fn(async () => undefined),
  reorderAgents: vi.fn(async () => undefined),
  setAgentSelection: vi.fn(async () => undefined),
}));

import { render } from "vitest-browser-svelte";
import SidebarHost from "./SidebarHost.svelte";
import { PROJECT_ID, ALICE } from "./fixtures";
import { claudeStoredWindows } from "$lib/usageWindows";
import { observeUsage, _testing } from "$lib/state/harnessUsage.svelte";
import type { AgentRecord } from "$lib/types";

/// Where the Usage limits section sits relative to the roster is a flex-layout
/// fact: the roster section takes the free height and scrolls, and the usage
/// strip keeps its content height at the foot. jsdom reports no geometry at all,
/// so neither the pinning nor the roster's overflow can be measured there —
/// only DOM order could be, and DOM order is not the claim.
const future = (seconds: number): number => Math.floor(Date.now() / 1000) + seconds;

/// More agents than the 600px host can show, so the roster genuinely overflows
/// and the pinning has something to be true against.
const MANY_AGENTS: AgentRecord[] = Array.from({ length: 14 }, (_, i) => ({
  ...ALICE,
  id: `00000000-0000-7000-8000-0000000${String(i).padStart(5, "0")}`,
  name: `agent-${i}`,
  session_locator: { uuid: `00000000-0000-7000-8000-1000000${String(i).padStart(5, "0")}` },
}));

beforeEach(() => {
  _testing.reset();
});

function seedClaudeReading(): void {
  const observedAt = new Date().toISOString();
  const payload = {
    status: "allowed",
    unifiedWindows: {
      five_hour: { utilization: 0.28, resetsAt: future(3 * 3600) },
      seven_day: { utilization: 0.7, resetsAt: future(5 * 86400) },
    },
  };
  observeUsage("claude_code", {
    payload,
    observed_at: observedAt,
    windows: claudeStoredWindows(payload, { observedAt }),
  });
}

/// The roster's scroll container, found by climbing from an agent card rather
/// than by class name, so a styling change to the section shell doesn't silently
/// turn this into a test of nothing.
function rosterScroller(panel: HTMLElement): HTMLElement {
  const card = panel.querySelector<HTMLElement>("[data-testid='sidebar-agent']");
  if (card === null) throw new Error("expected the roster to render agent cards");
  let node: HTMLElement | null = card.parentElement;
  while (node !== null && node !== panel) {
    if (getComputedStyle(node).overflowY === "auto") return node;
    node = node.parentElement;
  }
  throw new Error("expected an ancestor scroll container around the agent cards");
}

function latest(testid: string): HTMLElement {
  const found = document.querySelectorAll<HTMLElement>(`[data-testid='${testid}']`);
  const last = found[found.length - 1];
  if (last === undefined) throw new Error(`expected a [data-testid='${testid}'] element`);
  return last;
}

test("usage limits sit at the panel's foot while the overflowing roster scrolls above them", async () => {
  seedClaudeReading();
  render(SidebarHost, { projectId: PROJECT_ID, agents: MANY_AGENTS });

  const panel = latest("sidebar");
  const usage = latest("harness-usage");
  await expect.poll(() => usage.getBoundingClientRect().height).toBeGreaterThan(0);

  const scroller = rosterScroller(panel);
  expect(scroller.scrollHeight).toBeGreaterThan(scroller.clientHeight);

  const panelBox = panel.getBoundingClientRect();
  const usageBox = usage.getBoundingClientRect();
  expect(Math.abs(usageBox.bottom - panelBox.bottom)).toBeLessThanOrEqual(1);
  expect(usageBox.top).toBeGreaterThanOrEqual(scroller.getBoundingClientRect().bottom - 1);

  scroller.scrollTop = scroller.scrollHeight;
  await expect.poll(() => scroller.scrollTop).toBeGreaterThan(0);

  const afterScroll = usage.getBoundingClientRect();
  expect(Math.round(afterScroll.top)).toBe(Math.round(usageBox.top));
  expect(Math.abs(afterScroll.bottom - panelBox.bottom)).toBeLessThanOrEqual(1);
});

test("a roster shorter than the panel still leaves the usage strip at the foot", async () => {
  seedClaudeReading();
  render(SidebarHost, { projectId: PROJECT_ID, agents: [ALICE] });

  const panel = latest("sidebar");
  const usage = latest("harness-usage");
  await expect.poll(() => usage.getBoundingClientRect().height).toBeGreaterThan(0);

  const panelBox = panel.getBoundingClientRect();
  const usageBox = usage.getBoundingClientRect();
  expect(Math.abs(usageBox.bottom - panelBox.bottom)).toBeLessThanOrEqual(1);
  // The gap the roster leaves is empty roster, not a usage strip floated up to
  // meet the last card: that is the difference between "pinned" and "after".
  const lastCard = panel.querySelectorAll<HTMLElement>("[data-testid='sidebar-agent']");
  const cardBottom = lastCard[lastCard.length - 1]?.getBoundingClientRect().bottom;
  if (cardBottom === undefined) throw new Error("expected one agent card");
  expect(usageBox.top - cardBottom).toBeGreaterThan(50);
});
