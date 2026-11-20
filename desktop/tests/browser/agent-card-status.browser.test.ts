import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { commands, page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { render } from "vitest-browser-svelte";
import SidebarHost from "./SidebarHost.svelte";
import { mountPanes } from "./mount";
import { registerAgent, resetState, seedTurns, state } from "./harness";
import { PROJECT_ID, agentTurn, textItem } from "./fixtures";
import {
  AGENTS_SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MIN_WIDTH,
  layout,
  _testing as layoutState,
} from "$lib/layout.svelte";
import { addHeldForward, _testing as heldForwards } from "$lib/state/heldForwards.svelte";
import { moveAgentToNewPane, _testing as panesState } from "$lib/state/transcriptPanes.svelte";
import ActivityRing from "$lib/components/ui/ActivityRing.svelte";
import Spinner from "$lib/components/ui/Spinner.svelte";
import { createRawSnippet } from "svelte";
import type { AgentRecord } from "$lib/types";

declare module "vitest/browser" {
  interface BrowserCommands {
    setReducedMotion: (reduce: boolean) => Promise<void>;
  }
}

// A card's status row sits below its model chips and adds exactly its own
// height, at the default and narrowest sidebar; and the rings must turn at
// their own speeds and stop under reduced motion while plain spinners keep
// turning. Both need real layout and real media queries.

function agent(n: number, name: string, configured: boolean): AgentRecord {
  return {
    id: `00000000-0000-7000-8000-00000000000${n}`,
    project_id: PROJECT_ID,
    name,
    harness: "claude_code",
    session_locator: null,
    model: configured ? "opus" : null,
    effort: configured ? "high" : null,
    model_choices: configured ? ["opus", "sonnet"] : [],
    effort_choices: configured ? ["high", "medium"] : [],
    created_at: `2026-05-16T00:00:0${n}Z`,
  };
}

/// Working, waiting, failed, idle, in that order. Names long enough to need
/// truncating at 200px.
function roster(configured: boolean): AgentRecord[] {
  return [
    agent(1, "implementer-for-the-parser-rewrite", configured),
    agent(2, "reviewer-for-the-parser-rewrite", configured),
    agent(3, "tester", configured),
    agent(4, "docs-writer", configured),
  ];
}

async function seedStates(agents: AgentRecord[]): Promise<void> {
  const [working, waiting, failed] = agents;
  for (const a of agents) await registerAgent(a);
  const startedAt = new Date(Date.now() - 74_000).toISOString();
  seedTurns(working!.id, [
    agentTurn({
      id: "a1",
      agentId: working!.id,
      at: startedAt,
      status: "streaming",
      items: [textItem("Reading the parser.")],
    }),
  ]);
  state.runtimes[working!.id] = {
    ...state.runtimes[working!.id]!,
    run_status: "processing",
    in_flight_turn_id: "a1",
    turn_started_at: startedAt,
  };
  addHeldForward(PROJECT_ID, {
    forwardId: "f1",
    sendId: "s9",
    body: "",
    // A running source and one in a project this window hasn't opened, so the
    // waiting line names one and counts the other.
    sources: [
      { id: working!.id, name: working!.name },
      {
        id: "00000000-0000-7000-8000-0000000000e1",
        name: "planner",
        projectId: "00000000-0000-7000-8000-0000000000ee",
        projectName: "planning",
      },
    ],
    recipients: [waiting!.id],
  });
  state.runtimes[failed!.id] = {
    ...state.runtimes[failed!.id]!,
    last_error: { message: "The CLI exited with status 1.", kind: "harness_error" },
  };
}

/// Gives every agent a finished turn that reported context use, which is what
/// makes a card draw its context bar, ahead of whatever turns it already has.
function seedContext(agents: AgentRecord[]): void {
  for (const a of agents) {
    const usage = agentTurn({ id: `ctx-${a.id}`, agentId: a.id, items: [] });
    usage.ended_at = "2026-05-16T00:00:01Z";
    usage.usage = {
      input_tokens: 10,
      output_tokens: 5,
      context_input_tokens: 121_100,
      context_tokens_after_turn: 121_100,
      context_window: 1_000_000,
    };
    seedTurns(a.id, [usage, ...(state.transcripts[a.id] ?? [])]);
  }
}

function cards(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>('[data-testid="sidebar-agent"]')];
}

beforeEach(() => {
  layoutState.reset();
  resetState();
  heldForwards.reset();
  panesState.reset();
});

afterEach(async () => {
  await commands.setReducedMotion(false);
});

describe("card layout", () => {
  for (const width of [AGENTS_SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MIN_WIDTH])
    for (const configured of [true, false])
      test(`adds the status row below visible chips at ${width}px, ${configured ? "with model chips" : "with the default text"}`, async () => {
        layout.agentsSidebarWidth = width;
        const agents = roster(configured);
        await seedStates(agents);
        // The narrowest width carries a context bar too, the row the status
        // must clear.
        const withContext = width === SIDEBAR_MIN_WIDTH;
        if (withContext) seedContext(agents);
        render(SidebarHost, { projectId: PROJECT_ID, agents });

        await expect
          .poll(() => cards().map((card) => card.dataset.activity))
          .toEqual(["working", "waiting", "failed", "idle"]);
        expect(
          cards()[1]!.querySelector('[data-testid="agent-status-wait-more"]'),
        ).toHaveTextContent("+1");
        expect(cards()[0]!.querySelector('[data-testid="agent-status-elapsed"]')).not.toBeNull();

        const idle = cards()[3]!;
        expect(idle.querySelector('[data-testid="agent-status"]')).toBeNull();
        const idleHeight = idle.getBoundingClientRect().height;
        for (const card of cards()) {
          const selection = card.querySelector<HTMLElement>('[data-testid="agent-selection"]')!;
          expect(getComputedStyle(selection).visibility).toBe("visible");
          expect(selection).not.toHaveAttribute("inert");
          expect(selection.getBoundingClientRect().height).toBeGreaterThan(0);
        }
        for (const card of cards().slice(0, 3)) {
          const selection = card.querySelector<HTMLElement>('[data-testid="agent-selection"]')!;
          const status = card.querySelector<HTMLElement>('[data-testid="agent-status"]')!;
          const row = status.getBoundingClientRect();
          // Below the chips and any context bar, inside the card, with nothing
          // in it overflowing.
          expect(row.top).toBeGreaterThanOrEqual(selection.getBoundingClientRect().bottom);
          const bar = card.querySelector<HTMLElement>('[data-testid="agent-context-bar"]');
          expect(bar !== null, "context bar").toBe(withContext);
          if (bar !== null)
            expect(row.top).toBeGreaterThanOrEqual(bar.getBoundingClientRect().bottom);
          expect(row.right).toBeLessThanOrEqual(card.getBoundingClientRect().right);
          expect(status.scrollWidth).toBeLessThanOrEqual(status.clientWidth);
          // The card grows by the row and its top margin, and by nothing else.
          const grown = card.getBoundingClientRect().height - idleHeight;
          const margin = parseFloat(getComputedStyle(status).marginTop);
          expect(
            Math.abs(grown - (row.height + margin)),
            `${card.dataset.activity}`,
          ).toBeLessThanOrEqual(1);
        }

        if (!withContext) return;
        // Expanding adds the details below the row and leaves the row where it was.
        const failed = cards()[2]!;
        const failedId = agents[2]!.id;
        state.runtimes[failedId] = {
          ...state.runtimes[failedId]!,
          meta: {
            model: "claude-opus",
            harness_version: "2.1.274",
            inventory: { skills: [{ name: "debug" }] },
          },
        };
        const status = (): HTMLElement =>
          failed.querySelector<HTMLElement>('[data-testid="agent-status"]')!;
        const collapsedTop = status().getBoundingClientRect().top;
        const collapsedHeight = failed.getBoundingClientRect().height;
        expect(failed).toHaveAttribute("data-collapsed", "true");
        status().click();
        await expect.poll(() => failed.querySelector('[data-testid="agent-meta"]')).not.toBeNull();
        expect(failed.getBoundingClientRect().height).toBeGreaterThan(collapsedHeight);
        expect(status().getBoundingClientRect().top).toBe(collapsedTop);
      });
});

test("a pane member chip keeps its width when its icon becomes a spinner", async () => {
  const [alice, bob] = roster(false);
  for (const a of [alice!, bob!]) await registerAgent(a);
  moveAgentToNewPane(PROJECT_ID, [alice!.id, bob!.id], bob!.id);
  mountPanes({ projectId: PROJECT_ID, agents: [alice!, bob!] });
  const chip = (): HTMLElement =>
    page
      .getByTestId("transcript-pane")
      .nth(1)
      .getByTestId("pane-member-chip")
      .element() as HTMLElement;
  await expect.poll(() => chip().getBoundingClientRect().width).toBeGreaterThan(0);
  const idle = chip().getBoundingClientRect();

  state.runtimes[bob!.id] = { ...state.runtimes[bob!.id]!, run_status: "processing" };
  await expect
    .poll(() => chip().querySelector('[data-testid="pane-member-working"]'))
    .not.toBeNull();
  const working = chip().getBoundingClientRect();
  expect(working.width).toBe(idle.width);
  expect(working.height).toBe(idle.height);
});

test("a pane member chip keeps its width when its icon becomes a waiting clock", async () => {
  const [alice, bob] = roster(false);
  for (const a of [alice!, bob!]) await registerAgent(a);
  moveAgentToNewPane(PROJECT_ID, [alice!.id, bob!.id], bob!.id);
  mountPanes({ projectId: PROJECT_ID, agents: [alice!, bob!] });
  const chip = (): HTMLElement =>
    page
      .getByTestId("transcript-pane")
      .nth(1)
      .getByTestId("pane-member-chip")
      .element() as HTMLElement;
  await expect.poll(() => chip().getBoundingClientRect().width).toBeGreaterThan(0);
  const idle = chip().getBoundingClientRect();

  state.runtimes[alice!.id] = { ...state.runtimes[alice!.id]!, run_status: "processing" };
  addHeldForward(PROJECT_ID, {
    forwardId: "f1",
    sendId: "s9",
    body: "",
    sources: [{ id: alice!.id, name: alice!.name }],
    recipients: [bob!.id],
  });
  await expect
    .poll(() => chip().querySelector('[data-testid="pane-member-waiting"]'))
    .not.toBeNull();
  const clock = chip()
    .querySelector('[data-testid="pane-member-waiting"]')!
    .getBoundingClientRect();
  expect(clock.width).toBe(12);
  expect(clock.height).toBe(12);
  const waiting = chip().getBoundingClientRect();
  expect(waiting.width).toBe(idle.width);
  expect(waiting.height).toBe(idle.height);
});

describe("ring motion", () => {
  const icon = createRawSnippet(() => ({
    render: () =>
      '<span class="icon" style="display:block;width:32px;height:32px;border-radius:9999px"></span>',
  }));

  async function mountIndicators(): Promise<{
    ring: HTMLElement;
    waiting: HTMLElement;
    still: HTMLElement;
    plain: HTMLElement;
  }> {
    render(ActivityRing, { state: "working", children: icon });
    render(ActivityRing, { state: "waiting", children: icon });
    render(Spinner, { class: "h-3 w-3 still", stillWhenReducedMotion: true });
    render(Spinner, { class: "h-3 w-3 plain" });
    await expect.poll(() => document.querySelector('[data-testid="waiting-ring"]')).not.toBeNull();
    return {
      ring: document.querySelector<HTMLElement>('[data-testid="activity-ring"]')!,
      waiting: document.querySelector<HTMLElement>('[data-testid="waiting-ring"]')!,
      still: document.querySelector<HTMLElement>(".still")!,
      plain: document.querySelector<HTMLElement>(".plain")!,
    };
  }

  /// The icon a ring is drawn around: the first child of the ring's wrapper.
  function iconOf(ring: HTMLElement): DOMRect {
    return ring.parentElement!.querySelector<HTMLElement>(".icon")!.getBoundingClientRect();
  }

  test("centers both rings on the icon with 3px of clearance, measured while still", async () => {
    // Reduced motion stops the rotation, which would otherwise widen each
    // ring's bounding box by an angle that differs between the two.
    await commands.setReducedMotion(true);
    const { ring, waiting } = await mountIndicators();
    const svg = waiting.querySelector("svg")!.getBoundingClientRect();
    expect(svg.width).toBe(waiting.getBoundingClientRect().width);
    expect(svg.height).toBe(waiting.getBoundingClientRect().height);
    for (const el of [ring, waiting]) {
      const box = el.getBoundingClientRect();
      const inner = iconOf(el);
      expect(box.left + box.width / 2).toBeCloseTo(inner.left + inner.width / 2, 1);
      expect(box.top + box.height / 2).toBeCloseTo(inner.top + inner.height / 2, 1);
      expect(inner.left - box.left).toBeCloseTo(3, 1);
      expect(box.right - inner.right).toBeCloseTo(3, 1);
      expect(inner.top - box.top).toBeCloseTo(3, 1);
      expect(box.bottom - inner.bottom).toBeCloseTo(3, 1);
    }
  });

  test("sets the waiting ring's dash settings for 8 even dashes, in muted gray", async () => {
    // Checks the dash settings as computed, so a style that made the ring solid
    // fails; it does not inspect drawn pixels.
    await commands.setReducedMotion(true);
    const { waiting } = await mountIndicators();
    const circle = waiting.querySelector("circle")!;
    // A path length of 16 split 1-on, 1-off is exactly 8 dashes and 8 gaps.
    expect(circle.getAttribute("pathLength")).toBe("16");
    expect(getComputedStyle(circle).strokeDasharray.replace(/px/g, "")).toMatch(/^1,?\s*1$/);
    const probe = document.createElement("span");
    probe.className = "text-muted";
    document.body.append(probe);
    expect(getComputedStyle(circle).stroke).toBe(getComputedStyle(probe).color);
    probe.remove();
  });

  test("turns the working ring every 2s and the waiting ring every 6s when motion is allowed", async () => {
    const { ring, waiting, still } = await mountIndicators();
    for (const el of [ring, still]) {
      const style = getComputedStyle(el);
      expect(style.animationName).not.toBe("none");
      expect(style.borderTopColor).not.toBe(style.borderRightColor);
    }
    expect(getComputedStyle(ring).animationDuration).toBe("2s");
    expect(getComputedStyle(waiting).animationName).not.toBe("none");
    expect(getComputedStyle(waiting).animationDuration).toBe("6s");
  });

  test("stops both rings and the state spinner under reduced motion, and leaves plain spinners turning", async () => {
    await commands.setReducedMotion(true);
    const { ring, waiting, still, plain } = await mountIndicators();
    for (const el of [ring, still]) {
      const style = getComputedStyle(el);
      expect(style.animationName).toBe("none");
      expect(style.borderTopColor).toBe(style.borderRightColor);
    }
    expect(getComputedStyle(waiting).animationName).toBe("none");
    expect(getComputedStyle(plain).animationName).not.toBe("none");
  });
});
