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
import { AGENTS_SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MIN_WIDTH, layout } from "$lib/layout.svelte";
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

// The card's status line is drawn over the hidden model chips, so a card must
// keep its height in every state, at the widest and narrowest sidebar; and the
// running indicators must stop under reduced motion while plain spinners keep
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

function cards(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>('[data-testid="sidebar-agent"]')];
}

beforeEach(() => {
  resetState();
  heldForwards.reset();
  panesState.reset();
});

afterEach(async () => {
  await commands.setReducedMotion(false);
});

describe("card height", () => {
  for (const width of [AGENTS_SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MIN_WIDTH])
    for (const configured of [true, false])
      test(`is the same in every state at ${width}px, ${configured ? "with model chips" : "with the default text"}`, async () => {
        layout.agentsSidebarWidth = width;
        const agents = roster(configured);
        await seedStates(agents);
        render(SidebarHost, { projectId: PROJECT_ID, agents });

        await expect
          .poll(() => cards().map((card) => card.dataset.activity))
          .toEqual(["working", "waiting", "failed", "idle"]);
        const heights = cards().map((card) => card.getBoundingClientRect().height);
        expect(new Set(heights).size, `heights ${heights.join(", ")}`).toBe(1);

        expect(
          cards()[1]!.querySelector('[data-testid="agent-status-wait-more"]'),
        ).toHaveTextContent("+1");
        expect(cards()[0]!.querySelector('[data-testid="agent-status-elapsed"]')).not.toBeNull();
        // The status line stays inside its card, and nothing in it overflows.
        for (const card of cards().slice(0, 3)) {
          const status = card.querySelector<HTMLElement>('[data-testid="agent-status"]')!;
          expect(status.getBoundingClientRect().right).toBeLessThanOrEqual(
            card.getBoundingClientRect().right,
          );
          expect(status.scrollWidth).toBeLessThanOrEqual(status.clientWidth);
        }
        // The chips under the status line are out of reach, and the idle card's are not.
        const selection = (card: HTMLElement): HTMLElement | null =>
          card.querySelector('[data-testid="agent-selection"]');
        if (configured) {
          expect(selection(cards()[0]!)).toHaveAttribute("inert");
          expect(selection(cards()[3]!)).not.toHaveAttribute("inert");
        }
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

describe("reduced motion", () => {
  const icon = createRawSnippet(() => ({
    render: () => '<span style="display:block;width:32px;height:32px;border-radius:9999px"></span>',
  }));

  async function mountIndicators(): Promise<{
    ring: HTMLElement;
    still: HTMLElement;
    plain: HTMLElement;
  }> {
    render(ActivityRing, { active: true, children: icon });
    render(Spinner, { class: "h-3 w-3 still", stillWhenReducedMotion: true });
    render(Spinner, { class: "h-3 w-3 plain" });
    await expect.poll(() => document.querySelector('[data-testid="activity-ring"]')).not.toBeNull();
    return {
      ring: document.querySelector<HTMLElement>('[data-testid="activity-ring"]')!,
      still: document.querySelector<HTMLElement>(".still")!,
      plain: document.querySelector<HTMLElement>(".plain")!,
    };
  }

  test("stops the ring and the state spinner, drawing a full circle, and leaves plain spinners turning", async () => {
    await commands.setReducedMotion(true);
    const { ring, still, plain } = await mountIndicators();
    for (const el of [ring, still]) {
      const style = getComputedStyle(el);
      expect(style.animationName).toBe("none");
      expect(style.borderTopColor).toBe(style.borderRightColor);
    }
    expect(getComputedStyle(plain).animationName).not.toBe("none");
  });

  test("turns the ring and the state spinner when motion is allowed", async () => {
    const { ring, still } = await mountIndicators();
    for (const el of [ring, still]) {
      const style = getComputedStyle(el);
      expect(style.animationName).not.toBe("none");
      expect(style.borderTopColor).not.toBe(style.borderRightColor);
    }
    expect(getComputedStyle(ring).animationDuration).toBe("2s");
  });
});
