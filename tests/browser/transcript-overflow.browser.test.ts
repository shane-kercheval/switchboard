import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";
import type { AgentRecord } from "$lib/types";

// Canonical IPC mock block — see ./harness header. Browser mode hoists `vi.mock`
// only within the spec file, so it lives here, not in the helper. This case
// drives no streaming, so it omits the listener-capture map.
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";

// Canonical browser-test example: proves the CSS → measurement → toggle path
// end-to-end in real WebKit. This is the assertion jsdom CANNOT make — there
// `max-height` is parsed but never applied, so `scrollHeight === clientHeight`
// (both 0) and the overflow that drives the toggle is invisible. Later browser
// specs copy this shape (mount via harness, seed state, poll measured geometry).

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";

const AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: PROJECT_ID,
  name: "alice",
  harness: "claude_code",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-05-16T00:00:00Z",
};

// Far taller than the 14rem clip, so it overflows regardless of root font size.
const LONG_TEXT = Array.from(
  { length: 40 },
  (_, i) => `Line ${i + 1} of a very long user message.`,
).join("\n");

// Reset in `beforeEach` (not `afterEach`) — the canonical clean-slate guarantee.
// See ./harness header.
beforeEach(() => {
  resetState();
});

test("a long user message overflows the clip and gets a collapse toggle (compact default)", async () => {
  await registerAgent(AGENT);
  seedTurns(AGENT.id, [
    {
      role: "user",
      turn_id: "user-1",
      agent_id: AGENT.id,
      started_at: "2026-05-16T00:00:00Z",
      text: LONG_TEXT,
    },
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [AGENT] });

  // Poll: ResizeObserver-driven measurement settles asynchronously.
  await expect
    .poll(() => {
      const el = page.getByTestId("preview-clip").element() as HTMLElement;
      return el.scrollHeight - el.clientHeight;
    })
    .toBeGreaterThan(1);

  // Real applied CSS: the clip actually hides the overflow (not just a class).
  const clip = page.getByTestId("preview-clip").element() as HTMLElement;
  expect(getComputedStyle(clip).overflowY).toBe("hidden");

  // …and the overflow drives the per-message collapse toggle into existence.
  await expect.element(page.getByTestId("turn-preview-toggle")).toBeVisible();
});
