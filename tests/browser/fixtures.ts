import type { AgentRecord } from "$lib/types";
import type { Turn } from "$lib/state/index.svelte";

// Shared seed data for the M2 browser specs. Mirrors the agent/turn shapes the
// jsdom suite (`UnifiedTranscript.test.ts`) uses, so the two suites stay legible
// against each other. Builders keep specs focused on the layout fact under test.
// Shape twin of `$lib/dev/largeTranscript.ts` (which builds `Turn`s directly so
// src never imports tests) — semantic conventions (send_id grouping, timestamp
// ordering) must change in both; the type system only catches shape drift.

export const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";

export const ALICE: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: PROJECT_ID,
  name: "alice",
  harness: "claude_code",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-05-16T00:00:00Z",
};

/** Second agent for fan-out specs (same project as ALICE). */
export const BOB: AgentRecord = {
  ...ALICE,
  id: "00000000-0000-7000-8000-000000000bbb",
  name: "bob",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000002" },
};

type AgentTurn = Extract<Turn, { role: "agent" }>;
type Item = AgentTurn["items"][number];

/** A block of text tall enough to overflow the 14rem clip at any sane font size. */
export function longText(lines = 40, prefix = "Line"): string {
  return Array.from({ length: lines }, (_, i) => `${prefix} ${i + 1} of a long message.`).join(
    "\n",
  );
}

export function textItem(text: string): Item {
  return { item_kind: "text", kind: "text", text };
}

export function userTurn(opts: {
  id: string;
  agentId: string;
  text: string;
  at?: string;
  sendId?: string;
}): Turn {
  return {
    role: "user",
    turn_id: opts.id,
    agent_id: opts.agentId,
    started_at: opts.at ?? "2026-05-16T00:00:00Z",
    text: opts.text,
    ...(opts.sendId ? { send_id: opts.sendId } : {}),
  };
}

export function agentTurn(opts: {
  id: string;
  agentId: string;
  items: Item[];
  status?: AgentTurn["status"];
  at?: string;
  endedAt?: string;
  sendId?: string;
  model?: string;
}): AgentTurn {
  return {
    role: "agent",
    turn_id: opts.id,
    agent_id: opts.agentId,
    started_at: opts.at ?? "2026-05-16T00:00:01Z",
    status: opts.status ?? "complete",
    items: opts.items,
    ...(opts.endedAt ? { ended_at: opts.endedAt } : {}),
    ...(opts.sendId ? { send_id: opts.sendId } : {}),
    ...(opts.model ? { model: opts.model } : {}),
  };
}
