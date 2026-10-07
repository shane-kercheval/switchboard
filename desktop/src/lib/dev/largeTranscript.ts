// Parametric synthetic transcript for transcript-performance work: the
// layout-heavy block shapes the real app produces — markdown prose, Prism
// code fences, tool calls, clip-overflowing long texts, fan-outs — at a size
// where layout cost is measurable. Used by both the dev seeding hook
// (seedTranscript.ts) and the browser containment specs, so the manually
// measured numbers and the CI assertions describe the same DOM. Shape twin of
// `tests/browser/fixtures.ts` (kept separate so src never imports tests) —
// semantic conventions (send_id grouping, timestamp ordering) must change in
// both; the type system only catches shape drift.

import type { AgentId } from "$lib/types";
import type { TurnItem, Turn } from "$lib/state/types";

// Deterministic timestamps: a fixed base stepped per exchange, so generated
// history sorts stably and sits before any real turns it is prepended to.
const BASE_TIME_MS = Date.parse("2026-01-01T00:00:00Z");

function at(step: number, offsetSeconds = 0): string {
  return new Date(BASE_TIME_MS + step * 60_000 + offsetSeconds * 1_000).toISOString();
}

function lines(count: number, make: (i: number) => string): string {
  return Array.from({ length: count }, (_, i) => make(i)).join("\n");
}

function prose(i: number): string {
  return [
    `Here is what I found while looking into request ${i}. The relevant code paths split across two modules, and the behavior you observed comes from how they interact under concurrent updates.`,
    `- The first module owns the state transition and validates inputs.\n- The second module persists the result and emits the change event.\n- The race appears when both run inside one reactive flush.`,
    `In short: the fix belongs in the second module, where the event is emitted before the persisted write is durable. I would move the emit after the write and add a regression test for the reordering.`,
  ].join("\n\n");
}

function codeFence(i: number): string {
  const body = lines(25, (n) => `  const value${n} = compute(${n}, input.field${n % 4});`);
  return `Here is the implementation for case ${i}:\n\n\`\`\`ts\nexport function example${i}(input: Input): Output {\n${body}\n  return finalize([${lines(1, () => "value0, value1, value2")}]);\n}\n\`\`\`\n\nThis covers the edge cases discussed above.`;
}

function textItem(text: string, kind: "text" | "thinking" = "text"): TurnItem {
  return { item_kind: "text", kind, text };
}

function toolItem(step: number, n: number): TurnItem {
  return {
    item_kind: "tool",
    facet: { facet_kind: "other" },
    tool_use_id: `seed-tool-${step}-${n}`,
    kind: "builtin",
    name: n % 2 === 0 ? "Read" : "Bash",
    input: { path: `/repo/src/module-${step}.ts`, pattern: "export function", limit: 50 },
    output: lines(8, (i) => `match ${i}: src/module-${step}.ts:${i * 14 + 3}`),
    is_error: false,
    started_at: at(step, 10 + n),
    completed_at: at(step, 12 + n),
  };
}

/** The five recurring response shapes, cycled so every screenful mixes them. */
function agentItems(step: number): TurnItem[] {
  switch (step % 5) {
    case 0:
      return [textItem(prose(step))];
    case 1:
      // Tall enough to overflow the 14rem compact clip.
      return [textItem(lines(40, (i) => `Line ${i + 1} of long response ${step}.`))];
    case 2:
      return [textItem(codeFence(step))];
    case 3:
      return [
        toolItem(step, 0),
        toolItem(step, 1),
        textItem(`Both probes for step ${step} came back clean; proceeding with the change.`),
      ];
    default:
      return [
        textItem(
          `Weighing the two approaches for step ${step}: the first is simpler but couples the modules; the second needs one more seam but keeps them independent.`,
          "thinking",
        ),
        textItem(`Going with the second approach for step ${step} — the seam pays for itself.`),
      ];
  }
}

export type LargeTranscriptOptions = {
  agentIds: AgentId[];
  /// Total user→agent exchanges across the whole transcript (default 300).
  /// Non-fan-out exchanges round-robin across agents; with ≥2 agents, every
  /// 50th exchange becomes a fan-out to all of them.
  exchanges?: number;
};

/** Per-agent turn lists, ready to assign (or prepend) into `transcripts`. */
export function buildLargeTranscript(opts: LargeTranscriptOptions): Record<AgentId, Turn[]> {
  const { agentIds, exchanges = 300 } = opts;
  const out: Record<AgentId, Turn[]> = Object.fromEntries(agentIds.map((id) => [id, []]));

  for (let step = 0; step < exchanges; step++) {
    const fanout = agentIds.length >= 2 && step > 0 && step % 50 === 0;
    if (fanout) {
      const sendId = `seed-send-${step}`;
      for (const agentId of agentIds) {
        out[agentId]!.push(
          {
            role: "user",
            turn_id: `seed-user-${step}-${agentId}`,
            agent_id: agentId,
            started_at: at(step),
            text: `Fan-out question ${step}: compare your approaches to the migration.`,
            send_id: sendId,
          },
          {
            role: "agent",
            turn_id: `seed-agent-${step}-${agentId}`,
            agent_id: agentId,
            started_at: at(step, 5),
            ended_at: at(step, 30),
            status: "complete",
            send_id: sendId,
            items: agentItems(step + agentIds.indexOf(agentId)),
            model: "claude-fable-5",
          },
        );
      }
      continue;
    }
    const agentId = agentIds[step % agentIds.length]!;
    out[agentId]!.push(
      {
        role: "user",
        turn_id: `seed-user-${step}`,
        agent_id: agentId,
        started_at: at(step),
        text: `Question ${step}: please look into the issue described above.`,
      },
      {
        role: "agent",
        turn_id: `seed-agent-${step}`,
        agent_id: agentId,
        started_at: at(step, 5),
        ended_at: at(step, 30),
        status: "complete",
        items: agentItems(step),
        model: "claude-fable-5",
      },
    );
  }
  return out;
}
