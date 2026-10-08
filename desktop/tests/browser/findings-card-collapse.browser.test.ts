import { beforeEach, expect, test, vi } from "vitest";

// Canonical IPC mock block — see ./harness header. No streaming, so no
// listener-capture map.
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import type { Turn } from "$lib/state/types";
import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, paddingSends, userTurn } from "./fixtures";
import { EXPANDED_RECENT_SENDS } from "$lib/state/unified";

beforeEach(() => {
  resetState();
});

const FINDING_COUNT = 20;

function reviewItem(): Extract<Turn, { role: "agent" }>["items"][number] {
  return {
    item_kind: "tool",
    tool_use_id: "toolu_review",
    kind: "builtin",
    name: "ReportFindings",
    input: {},
    facet: {
      facet_kind: "findings",
      level: "high",
      findings: Array.from({ length: FINDING_COUNT }, (_, i) => ({
        file: `src/module_${i}.py`,
        line: i + 1,
        summary: `Finding number ${i + 1}.`,
        failure_scenario: "Scenario.",
        verdict: "PLAUSIBLE" as const,
      })),
      text: "REVIEW",
    },
    output: `${FINDING_COUNT} findings reported.`,
    is_error: false,
    started_at: "2026-05-16T00:00:01Z",
    completed_at: "2026-05-16T00:00:02Z",
  };
}

function agentTurns(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>('[data-testid="turn"]')).filter(
    (el) => el.getAttribute("data-role") === "agent",
  );
}

// A tall review clips inside the collapsed preview. The card's rows don't open
// while collapsed (the hidden finding details are what make Expand available;
// the jsdom suite covers a short review). This checks the real geometry jsdom
// cannot produce: the clip actually overflows, and expanding the response
// reveals every row on screen.
test("a collapsed review-only response taller than the preview cap offers Expand and reveals every row", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    userTurn({ id: "user-review", agentId: ALICE.id, text: "review it", sendId: "send-review" }),
    agentTurn({
      id: "agent-review",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:01Z",
      endedAt: "2026-05-16T00:00:02Z",
      sendId: "send-review",
      items: [reviewItem()],
    }),
    ...paddingSends(ALICE.id, EXPANDED_RECENT_SENDS),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  const review = agentTurns()[0]!;
  // Older responses open as a clipped preview; the card is in it, unexpandable.
  expect(review.querySelector('[data-testid="findings-card"]')).not.toBeNull();
  expect(review.querySelector('[data-testid="finding-toggle"]')).toBeNull();
  expect(review.querySelector('[data-testid="hidden-items-indicator"]')).toBeNull();

  await expect
    .poll(() => {
      const clip = review.querySelector('[data-testid="preview-clip"]');
      return clip instanceof HTMLElement ? clip.scrollHeight - clip.clientHeight : 0;
    })
    .toBeGreaterThan(1);
  await expect
    .poll(() =>
      review.querySelector('[data-testid="turn-preview-toggle"]')?.getAttribute("aria-label"),
    )
    .toBe("Expand");

  (review.querySelector('[data-testid="turn-preview-toggle"]') as HTMLElement).click();

  await expect
    .poll(() => review.querySelectorAll('[data-testid="finding-toggle"]').length)
    .toBe(FINDING_COUNT);
  // Every row is actually on screen inside the response, not clipped away.
  const rows = Array.from(review.querySelectorAll<HTMLElement>('[data-testid="finding-row"]'));
  const responseBottom = review.getBoundingClientRect().bottom;
  expect(rows).toHaveLength(FINDING_COUNT);
  expect(rows.at(-1)!.getBoundingClientRect().bottom).toBeLessThanOrEqual(responseBottom + 1);
  expect(review.querySelector('[data-testid="preview-clip"]')).toBeNull();
});
