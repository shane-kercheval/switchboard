import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem, userTurn } from "./fixtures";

// Behavior 7: expanding a mid-list message keeps its footer anchored on screen —
// the "the place I clicked stays put" contract. Expanding grows content ABOVE the
// footer; the component holds its gap-from-bottom so the footer (and the toggle
// in it) does not move. jsdom can't check this — there is no layout, so the
// toggle has no screen position to hold.

function transcript(): HTMLElement {
  return page.getByTestId("unified-transcript").element() as HTMLElement;
}

beforeEach(() => {
  resetState();
});

test("expanding a mid-list message keeps its toggle anchored on screen", async () => {
  await registerAgent(ALICE);
  // A long (clipped → toggled) user message at the top, with a tall response
  // below it so the message is mid-list and the transcript scrolls. The latest
  // agent response renders as the full latest-response view (no toggle of its own),
  // so the user message owns the only toggle.
  seedTurns(ALICE.id, [
    userTurn({ id: "user-1", agentId: ALICE.id, text: longText(30) }),
    agentTurn({
      id: "agent-1",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:02Z",
      items: [textItem(longText(40))],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });

  // Scroll to the top so the user message and its footer are on screen, then
  // record the toggle's viewport position.
  await expect.poll(() => transcript().scrollHeight > transcript().clientHeight + 100).toBe(true);
  transcript().scrollTop = 0;
  transcript().dispatchEvent(new Event("scroll"));

  const toggle = page.getByTestId("turn-preview-toggle");
  await expect.element(toggle).toBeInTheDocument();
  const before = (toggle.element() as HTMLElement).getBoundingClientRect().top;

  // Expand: the body grows above the footer. The toggle must stay put.
  await toggle.click();
  await expect.poll(() => page.getByTestId("preview-clip").elements().length).toBe(0);

  await expect
    .poll(() => {
      const after = (toggle.element() as HTMLElement).getBoundingClientRect().top;
      return Math.abs(after - before);
    })
    .toBeLessThan(8);
});
