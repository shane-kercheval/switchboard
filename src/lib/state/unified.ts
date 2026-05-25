// Pure render-merge for the unified project transcript.
//
// **Ownership split (decompose model).** After restart, a project's
// conversation has two disjoint frontend sources, by ownership:
//   - Per-agent transcript state (`transcripts[agent_id]`, `$lib/state`) owns
//     **agent-turn content** — both live (streamed this session) and hydrated
//     (regrouped from the backend's `agent_turn` items into the existing
//     per-agent hydrate path), plus **live user turns** (this session's sends).
//   - The per-project overlay (`conversations[project_id]`, the workspace
//     store) owns the journal-sourced **historical user messages** (grouped by
//     `send_id`, fan-out-aware) and **historical outcome markers**
//     (failed/cancelled).
//
// The two sources share no rendered kind, so there is no cross-source dedup:
// agent content lives only per-agent; user/outcome history lives only in the
// overlay. (The per-agent hydrate reducer already dedups hydrated vs. live
// agent turns by `turn_id`.)
//
// This module merges those two sources into one chronological row list. The
// sort replicates the backend's `(timestamp, kind_rank)` tiebreak
// (`commands.rs::conversation_item_sort_key`): a user message and its turn's
// outcome marker carry an identical timestamp in real data (both equal the
// turn's `started_at`), so a timestamp-only sort would render a
// failed/cancelled marker *above* the prompt that caused it. Ranks: user (0) <
// agent (1) < outcome (2).

import type { AgentId, ConversationItem, OutcomeStatus, TurnId } from "$lib/types";
import type { Turn } from "./types";

type AgentTurn = Extract<Turn, { role: "agent" }>;

/// One rendered row in the unified transcript.
///
/// `agent_ids` on the user row is the recipient set — length 1 today
/// (single-recipient sends), N once M4.7 fan-out lands. The array shape is
/// uniform now (live single-recipient turns normalize to a length-1 array) so
/// M4.7 adds only a grouping pass over live sends sharing a `send_id`, never a
/// row-shape change.
export type UnifiedRow =
  | { kind: "user"; at: string; rank: 0; key: string; agent_ids: AgentId[]; text: string }
  | { kind: "agent"; at: string; rank: 1; key: string; turn: AgentTurn }
  | {
      kind: "outcome";
      at: string;
      rank: 2;
      key: string;
      turn_id: TurnId;
      agent_id: AgentId;
      status: OutcomeStatus;
      reason?: string | null;
    };

const KIND_RANK = { user: 0, agent: 1, outcome: 2 } as const;

/// Merge the active project's per-agent turns (live + hydrated agent content
/// and this-session user turns) with its journal overlay (historical user
/// messages + outcome markers) into one chronological row list.
///
/// `turns` is the flattened union of the active project's agents'
/// `transcripts[agent_id]`. `overlay` is `conversations[project_id].items` —
/// expected to contain only `user_message` and `outcome` kinds (the workspace
/// store routes `agent_turn` items into per-agent state); any stray
/// `agent_turn` in the overlay is ignored defensively to avoid double-rendering
/// agent content.
export function buildUnifiedRows(turns: Turn[], overlay: ConversationItem[]): UnifiedRow[] {
  const rows: UnifiedRow[] = [];

  for (const turn of turns) {
    if (turn.role === "user") {
      rows.push({
        kind: "user",
        at: turn.started_at,
        rank: KIND_RANK.user,
        key: `u:${turn.turn_id}`,
        agent_ids: [turn.agent_id],
        text: turn.text,
      });
    } else {
      rows.push({
        kind: "agent",
        at: turn.started_at,
        rank: KIND_RANK.agent,
        key: `a:${turn.turn_id}`,
        turn,
      });
    }
  }

  for (const item of overlay) {
    if (item.kind === "user_message") {
      rows.push({
        kind: "user",
        at: item.at,
        rank: KIND_RANK.user,
        key: `u:${item.send_id}`,
        agent_ids: item.agent_ids,
        text: item.text,
      });
    } else if (item.kind === "outcome") {
      rows.push({
        kind: "outcome",
        at: item.at,
        rank: KIND_RANK.outcome,
        key: `o:${item.turn_id}`,
        turn_id: item.turn_id,
        agent_id: item.agent_id,
        status: item.status,
        reason: item.reason,
      });
    }
    // `agent_turn` overlay items are not expected in the decompose model
    // (routed to per-agent state); ignore to avoid double-rendering.
  }

  // ISO-8601 timestamps sort lexicographically; tie-break by kind rank so a
  // user message precedes its own turn's content/markers at an equal instant.
  // `Array.prototype.sort` is stable (ES2019+), so same-(at,rank) rows keep
  // insertion order.
  rows.sort((a, b) => {
    const t = a.at.localeCompare(b.at);
    return t !== 0 ? t : a.rank - b.rank;
  });
  return rows;
}
