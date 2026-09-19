import { tick } from "svelte";
import { expect, test, vi } from "vitest";
import { page } from "vitest/browser";

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
import { transcripts } from "$lib/state/index.svelte";
import { layout } from "$lib/layout.svelte";

/// The default and minimum agent-sidebar widths.
const DEFAULT_WIDTH = 280;
const MIN_WIDTH = 200;

/// A Claude agent whose last turn reported occupancy, which is what makes the
/// context row render at all. The token figures are the widest realistic shape:
/// six characters of used, a megabyte window, a two-digit percentage.
function seedContextBar(): void {
  transcripts[ALICE.id] = [
    {
      role: "agent",
      turn_id: "t1",
      agent_id: ALICE.id,
      started_at: "2026-05-16T00:00:00Z",
      ended_at: "2026-05-16T00:00:01Z",
      status: "complete",
      items: [],
      usage: {
        input_tokens: 10,
        output_tokens: 5,
        context_input_tokens: 121_100,
        context_tokens_after_turn: 121_100,
        context_window: 1_000_000,
      },
    },
  ];
}

/// The meter's label is the only element in this row that can shrink — every
/// button and the token detail beside it are `shrink-0` — so its clipping is
/// the row's whole fit story.
function labelClipping(): number {
  const bar = page.getByTestId("agent-context-bar").element() as HTMLElement;
  const label = bar.querySelector("span.truncate") as HTMLElement;
  return label.scrollWidth - label.clientWidth;
}

/// The harness caps the sidebar to a fraction of the viewport, so a narrow
/// default window silently measures the minimum width instead of the one asked
/// for. Widen the viewport first or this test quietly stops testing anything.
async function renderAt(width: number): Promise<void> {
  await page.viewport(1600, 900);
  layout.agentsSidebarWidth = width;
  seedContextBar();
  render(SidebarHost, { projectId: PROJECT_ID, agents: [ALICE] });
  await expect.element(page.getByTestId("agent-context-bar")).toBeInTheDocument();
}

test("the context meter's label fits beside both of the row's buttons", async () => {
  // Measured, not estimated. Adding the breakdown chevron beside the existing
  // compact button took ~26px from this label, and "Context used" — which fit
  // exactly with one button — began clipping by 11px. Shortening the label is
  // what bought the room back; this is the assertion that notices if a third
  // control, a longer label, or a wider token format spends it again.
  await renderAt(DEFAULT_WIDTH);

  const bar = page.getByTestId("agent-context-bar").element() as HTMLElement;
  expect(bar.querySelectorAll("button")).toHaveLength(2);
  expect(labelClipping()).toBe(0);
});

test("the context row records its residual clipping at the minimum width", async () => {
  // **Deliberately not zero.** At 200px the label was already clipped before the
  // chevron existed, and the only fix that would clear it is dropping the
  // percentage — which the bar does not replace, since the bar is approximate
  // and clamps above 100%. This pins the residual as a number so a regression
  // moves it rather than hiding inside an already-failing state.
  await renderAt(MIN_WIDTH);

  expect(labelClipping()).toBeGreaterThan(0);
  expect(labelClipping()).toBeLessThan(30);
});

test("restoring focus to context breakdown does not open its tooltip", async () => {
  await renderAt(DEFAULT_WIDTH);

  // Establish pointer modality before simulating focus restoration. WebKit
  // treats bare programmatic focus as keyboard-visible until it has observed
  // pointer input, which is not the dialog-close path this guards.
  const collapse = page.getByTestId("agent-collapse-toggle");
  await collapse.click();
  await collapse.click();
  const breakdown = page.getByTestId("agent-context-breakdown-button").element() as HTMLElement;
  breakdown.focus();
  await tick();

  expect(document.activeElement).toBe(breakdown);
  expect(breakdown.matches(":focus-visible")).toBe(false);
  expect(document.querySelector('[data-testid="tooltip-content"]')).toBeNull();
});
