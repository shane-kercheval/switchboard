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

/// The percentage column is reserved in `ch`, which is exact only for tabular
/// digits. jsdom reports no geometry, so whether the reservation actually lines
/// the rows up can only be settled by measuring rendered boxes.
const future = (seconds: number): number => Math.floor(Date.now() / 1000) + seconds;

beforeEach(() => {
  _testing.reset();
});

/// Left edge of each meter's right-hand group, which is where a percentage wider
/// than its reservation shows up: the group grows leftward and drags the detail
/// text with it.
///
/// Scoped to the **most recently mounted** section, because a second `render` in
/// one test leaves the first still in the document and a whole-document query
/// would measure both.
function detailLeftEdges(): number[] {
  const sections = document.querySelectorAll<HTMLElement>("[data-testid='harness-usage']");
  const latest = sections[sections.length - 1];
  if (latest === undefined) return [];
  return Array.from(latest.querySelectorAll<HTMLElement>("[data-testid='harness-usage-window']"))
    .map((meter) => meter.querySelector<HTMLElement>("span.ml-auto"))
    .map((group) => {
      if (group === null) throw new Error("expected a right-hand group in every meter");
      return Math.round(group.getBoundingClientRect().left);
    });
}

/// Seed a Claude reading the way the event path does: the payload plus the windows
/// lifted out of it. A reading recorded without them renders nothing, so every seed
/// goes through here rather than reaching into the store directly.
function seedClaude(payload: unknown, model?: string): void {
  const observedAt = new Date().toISOString();
  observeUsage("claude_code", {
    payload,
    observed_at: observedAt,
    windows: claudeStoredWindows(payload, { observedAt, model }),
  });
}

test("rows with different digit counts keep their detail text on one left edge", async () => {
  seedClaude(
    {
      status: "allowed",
      unifiedWindows: {
        five_hour: { utilization: 0.28, resetsAt: future(3 * 3600) },
        seven_day: { utilization: 0.7, resetsAt: future(5 * 86400) },
        seven_day_overage_included: { utilization: 1, resetsAt: future(5 * 86400) },
      },
    },
    "claude-fable-5-1",
  );
  render(SidebarHost, { projectId: PROJECT_ID, agents: [ALICE] });

  await expect.poll(() => detailLeftEdges().length).toBe(3);
  const edges = detailLeftEdges();
  // Every countdown starts at the same x, including the row reading 100%. The
  // reservation is per digit with the percent sign outside it, so a third digit
  // fills reserved space rather than overflowing and shifting the row.
  expect(new Set(edges).size).toBe(1);
});

test("a section that never reaches three digits is not indented for one", async () => {
  seedClaude({
    status: "allowed",
    unifiedWindows: { five_hour: { utilization: 0.28, resetsAt: future(3 * 3600) } },
  });
  render(SidebarHost, { projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => detailLeftEdges().length).toBe(1);
  const narrow = detailLeftEdges()[0]!;

  _testing.reset();
  seedClaude({
    status: "allowed",
    unifiedWindows: {
      five_hour: { utilization: 0.28, resetsAt: future(3 * 3600) },
      seven_day: { utilization: 1, resetsAt: future(5 * 86400) },
    },
  });
  render(SidebarHost, { projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => detailLeftEdges().length).toBe(2);

  // The two-digit-only section reserves less room, so its detail text sits
  // further right than the section carrying a three-digit reading.
  const wide = detailLeftEdges().at(-1)!;
  expect(narrow).toBeGreaterThan(wide);
});

test("the harness name sits further from its meters than the meters do from each other", async () => {
  // The gap is a bottom margin on the name that collapses against the list's own
  // top margins, so it is 8px rather than the sum of the two. Switching the list
  // to a flex gap would stop the collapse and silently change this, which is why
  // it is measured rather than assumed from the classes.
  seedClaude({
    status: "allowed",
    unifiedWindows: {
      five_hour: { utilization: 0.28, resetsAt: future(3 * 3600) },
      seven_day: { utilization: 0.7, resetsAt: future(5 * 86400) },
    },
  });
  render(SidebarHost, { projectId: PROJECT_ID, agents: [ALICE] });
  await expect.poll(() => detailLeftEdges().length).toBe(2);

  const card = document.querySelector("[data-testid='harness-usage-claude_code']");
  if (card === null) throw new Error("expected the Claude row");
  const name = card.querySelector("div.flex.items-center");
  if (name === null) throw new Error("expected the harness name row");
  const meters = Array.from(card.querySelectorAll("[data-testid='harness-usage-window']"));
  const [first, second] = meters.map((m) => m.getBoundingClientRect());
  if (first === undefined || second === undefined) throw new Error("expected two meters");

  const underName = first.top - name.getBoundingClientRect().bottom;
  const betweenMeters = second.top - first.bottom;
  expect(underName).toBeGreaterThan(betweenMeters);
});
