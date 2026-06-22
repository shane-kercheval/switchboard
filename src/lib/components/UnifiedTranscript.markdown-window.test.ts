import { afterEach, beforeEach, expect, it, vi, type Mock } from "vitest";
import { render } from "@testing-library/svelte";
import { tick } from "svelte";
import type { AgentRecord, NormalizedEvent } from "$lib/types";

// Wrap (not replace) renderMarkdown so output stays real but calls are counted.
vi.mock("$lib/markdown", async (importOriginal) => {
  const actual = await importOriginal<typeof import("$lib/markdown")>();
  return { ...actual, renderMarkdown: vi.fn((t: string) => actual.renderMarkdown(t)) };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

// jsdom has no IntersectionObserver; the reveal sentinel constructs one.
class StubIO {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
  takeRecords(): IntersectionObserverEntry[] {
    return [];
  }
}
globalThis.IntersectionObserver = StubIO as unknown as typeof IntersectionObserver;

import { renderMarkdown } from "$lib/markdown";
import UnifiedTranscript from "./UnifiedTranscript.svelte";
import type { Turn } from "$lib/state/index.svelte";

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";
const ALICE: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: PROJECT_ID,
  name: "alice",
  harness: "claude_code",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-05-16T00:00:00Z",
};

async function loadState() {
  return await import("$lib/state/index.svelte");
}

beforeEach(() => {
  (renderMarkdown as Mock).mockClear();
});

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
});

function agentTurns(n: number): Turn[] {
  const turns: Turn[] = [];
  for (let k = 0; k < n; k++) {
    const mm = String(Math.floor(k / 60)).padStart(2, "0");
    const ss = String(k % 60).padStart(2, "0");
    turns.push({
      role: "agent",
      turn_id: `aturn-${k}`,
      agent_id: ALICE.id,
      started_at: `2026-05-16T00:${mm}:${ss}Z`,
      status: "complete",
      items: [{ item_kind: "text", kind: "text", text: `message body ${k}` }],
    });
  }
  return turns;
}

// Regression guard for the effect-ordering bug where the FIRST render mounted the
// whole transcript (markdown-parsing every block) before the windowing effect
// bounded it down — invisible to a settled-DOM assertion, but it made windowing
// give zero markdown savings. With a synchronous (derived) window bound, only the
// visible tail is ever parsed.
it("parses markdown only for the windowed tail on first paint, not the whole transcript", async () => {
  const state = await loadState();
  await state.registerAgent(ALICE);
  // 200 standalone blocks (1 markdown segment each), window is 20.
  state.transcripts[ALICE.id] = agentTurns(200);

  (renderMarkdown as Mock).mockClear();
  render(UnifiedTranscript, {
    props: { projectId: PROJECT_ID, agents: [ALICE], loadStatus: "complete" },
  });
  await tick();

  const calls = (renderMarkdown as Mock).mock.calls.length;
  // Bounded by the ~20-block window — far below the 200 total. The pre-fix bug
  // parsed all 200 here.
  expect(calls).toBeLessThan(50);
});

export type { NormalizedEvent };
