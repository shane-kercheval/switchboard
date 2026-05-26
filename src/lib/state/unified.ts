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
  | {
      kind: "user";
      at: string;
      rank: 0;
      key: string;
      send_id?: string;
      agent_ids: AgentId[];
      text: string;
    }
  | { kind: "agent"; at: string; rank: 1; key: string; send_id?: string; turn: AgentTurn }
  | {
      kind: "outcome";
      at: string;
      rank: 2;
      key: string;
      send_id?: string;
      turn_id: TurnId;
      agent_id: AgentId;
      status: OutcomeStatus;
      reason?: string | null;
    };

/// A render unit for the transcript: either a standalone row, or a fan-out
/// group (a single user message sent to >1 recipient) whose responses render
/// as per-recipient columns instead of stacked in the timeline. The fan-out's
/// agent/outcome rows are pulled out of the flat stream into `columns`, so they
/// render inside the group, not again chronologically.
export type RenderBlock =
  | { kind: "row"; row: UnifiedRow }
  | {
      kind: "fanout";
      key: string;
      send_id: string;
      user: Extract<UnifiedRow, { kind: "user" }>;
      columns: { agent_id: AgentId; rows: Exclude<UnifiedRow, { kind: "user" }>[] }[];
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

  // Live user turns of one fan-out share a `send_id` (one per recipient), so
  // collapse them into a single user row whose `agent_ids` is the recipient set
  // (first-seen order) — the user's message renders once. A user turn without a
  // `send_id` (shouldn't happen for live sends, but stay defensive) keys by its
  // own turn_id so it still renders.
  //
  // Two passes so the returned row objects are never mutated after creation:
  // accumulate group metadata first (a plain scratch map), then build each user
  // row exactly once.
  type UserGroup = { send_id?: string; agent_ids: AgentId[]; text: string; at: string };
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const groups = new Map<string, UserGroup>();
  const groupOrder: string[] = [];
  for (const turn of turns) {
    if (turn.role === "user") {
      const groupKey = turn.send_id ?? turn.turn_id;
      const g = groups.get(groupKey);
      if (g === undefined) {
        groups.set(groupKey, {
          send_id: turn.send_id,
          agent_ids: [turn.agent_id],
          text: turn.text,
          at: turn.started_at,
        });
        groupOrder.push(groupKey);
      } else {
        if (!g.agent_ids.includes(turn.agent_id)) g.agent_ids.push(turn.agent_id);
        if (turn.started_at < g.at) g.at = turn.started_at;
      }
    } else {
      rows.push({
        kind: "agent",
        at: turn.started_at,
        rank: KIND_RANK.agent,
        key: `a:${turn.turn_id}`,
        send_id: turn.send_id,
        turn,
      });
    }
  }
  for (const groupKey of groupOrder) {
    const g = groups.get(groupKey);
    if (g === undefined) continue;
    rows.push({
      kind: "user",
      at: g.at,
      rank: KIND_RANK.user,
      key: `u:${groupKey}`,
      send_id: g.send_id,
      agent_ids: g.agent_ids,
      text: g.text,
    });
  }

  for (const item of overlay) {
    if (item.kind === "user_message") {
      rows.push({
        kind: "user",
        at: item.at,
        rank: KIND_RANK.user,
        key: `u:${item.send_id}`,
        send_id: item.send_id,
        agent_ids: item.agent_ids,
        text: item.text,
      });
    } else if (item.kind === "outcome") {
      rows.push({
        kind: "outcome",
        at: item.at,
        rank: KIND_RANK.outcome,
        key: `o:${item.turn_id}`,
        send_id: item.send_id,
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

/// Group a flat row list into render blocks, collapsing each fan-out (a user
/// message to >1 recipient) into one block whose responses render as
/// per-recipient columns. A single-recipient send stays a sequence of
/// standalone rows (the user row, then its agent row) exactly as before.
///
/// The fan-out's agent + outcome rows (matched by `send_id`) are pulled out of
/// the flat stream into the group's columns — ordered by the recipient set
/// (`user.agent_ids`), a **stable** order so streaming arrival never reshuffles
/// columns. The group is anchored at the user message's position in the
/// timeline; rows belonging to other sends flow normally around it.
type NonUserRow = Exclude<UnifiedRow, { kind: "user" }>;

/// The recipient an agent/outcome row belongs to (agent rows carry it under
/// `.turn`; outcome rows carry it directly).
function rowAgentId(row: NonUserRow): AgentId {
  return row.kind === "agent" ? row.turn.agent_id : row.agent_id;
}

export function groupRenderBlocks(rows: UnifiedRow[]): RenderBlock[] {
  // Pass 1: identify fan-out sends (a user row to >1 recipient) and bucket
  // every agent/outcome row of those sends by recipient.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const fanoutUsers = new Map<string, Extract<UnifiedRow, { kind: "user" }>>();
  for (const row of rows) {
    if (row.kind === "user" && row.send_id !== undefined && row.agent_ids.length > 1) {
      fanoutUsers.set(row.send_id, row);
    }
  }
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const colsBySend = new Map<string, Map<AgentId, NonUserRow[]>>();
  for (const row of rows) {
    if (row.kind === "user" || row.send_id === undefined || !fanoutUsers.has(row.send_id)) continue;
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const perAgent = colsBySend.get(row.send_id) ?? new Map<AgentId, NonUserRow[]>();
    const agentId = rowAgentId(row);
    const col = perAgent.get(agentId) ?? [];
    col.push(row);
    perAgent.set(agentId, col);
    colsBySend.set(row.send_id, perAgent);
  }

  // Only group a fan-out that actually has correlated content. A multi-recipient
  // user message whose responses can't be correlated to it (e.g. historical
  // turns with no recoverable send_id, or a just-dispatched send before any
  // response) renders as a plain user message — its responses flow in the
  // stream — rather than a group of empty "queued" columns.
  for (const sendId of [...fanoutUsers.keys()]) {
    if (!colsBySend.has(sendId)) fanoutUsers.delete(sendId);
  }

  // Pass 2: emit blocks in order. A fan-out user row becomes a group anchored
  // at its position (columns in recipient order — stable); the fan-out's
  // agent/outcome rows are skipped (they live in the group). Everything else is
  // a standalone row.
  const blocks: RenderBlock[] = [];
  for (const row of rows) {
    if (row.kind === "user" && row.send_id !== undefined && fanoutUsers.has(row.send_id)) {
      const perAgent = colsBySend.get(row.send_id) ?? new Map<AgentId, NonUserRow[]>();
      blocks.push({
        kind: "fanout",
        key: `f:${row.send_id}`,
        send_id: row.send_id,
        user: row,
        columns: row.agent_ids.map((agent_id) => ({
          agent_id,
          rows: perAgent.get(agent_id) ?? [],
        })),
      });
    } else if (row.kind !== "user" && row.send_id !== undefined && fanoutUsers.has(row.send_id)) {
      // Routed into its fan-out's column above — don't emit standalone.
      continue;
    } else {
      blocks.push({ kind: "row", row });
    }
  }
  return blocks;
}
