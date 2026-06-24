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
// agent (1) < system_marker (2) < outcome (3).

import type {
  AgentId,
  Attachment,
  ConversationItem,
  OutcomeStatus,
  SystemMarker,
  TurnId,
} from "$lib/types";
import type { AgentCopyMode } from "$lib/agentCopyMode";
import type { Turn } from "./types";

type AgentTurn = Extract<Turn, { role: "agent" }>;

function trimBlankOuterLines(text: string): string {
  const lines = text.split(/\r?\n/);
  let start = 0;
  while (start < lines.length && lines[start]!.trim().length === 0) start += 1;

  let end = lines.length;
  while (end > start && lines[end - 1]!.trim().length === 0) end -= 1;

  return lines.slice(start, end).join("\n");
}

/// The canonical "answer prose" of an agent turn: its `text`-kind chunks joined,
/// with tool calls AND reasoning (`kind: "thinking"`) excluded.
///
/// Use this for ANY "what is the response" extraction — copy today; forwarding
/// one agent's reply into another's prompt, export, and search later. The trap
/// it guards: `item_kind: "text"` spans both answer text and reasoning (the
/// inner `kind` discriminates), so an ad-hoc `filter(item_kind === "text")` that
/// forgets the inner `kind === "text"` check silently leaks the model's private
/// reasoning into the response. Route every such consumer through here so that
/// rule lives in exactly one place. Empty outer lines are removed from each
/// answer block, while indentation and trailing spaces on meaningful lines are
/// preserved for Markdown fidelity. See docs/research/harness-behavior.md §3.2.
export function answerTextOf(turn: AgentTurn): string {
  return turn.items
    .filter((i) => i.item_kind === "text")
    .filter((i) => i.kind === "text")
    .map((i) => trimBlankOuterLines(i.text))
    .filter((text) => text.length > 0)
    .join("\n\n");
}

/// The final answer-prose block of an agent turn: scan backward for the last
/// non-empty answer text item, skipping tools and model reasoning.
export function lastAnswerTextOf(turn: AgentTurn): string {
  for (let i = turn.items.length - 1; i >= 0; i--) {
    const item = turn.items[i]!;
    if (item.item_kind === "text" && item.kind === "text") {
      const text = trimBlankOuterLines(item.text);
      if (text.length > 0) return text;
    }
  }
  return "";
}

export function copyTextOf(turn: AgentTurn, mode: AgentCopyMode): string {
  switch (mode) {
    case "last_answer_block":
      return lastAnswerTextOf(turn);
    case "full_answer":
      return answerTextOf(turn);
  }
}

/// One rendered row in the unified transcript.
///
/// `agent_ids` on the user row is the recipient set — length 1 today
/// (single-recipient sends), N once multi-recipient fan-out lands. The array
/// shape is uniform now (live single-recipient turns normalize to a length-1
/// array) so fan-out adds only a grouping pass over live sends sharing a
/// `send_id`, never a row-shape change.
export type UnifiedRow =
  | {
      kind: "user";
      at: string;
      rank: 0;
      key: string;
      send_id?: string;
      agent_ids: AgentId[];
      text: string;
      attachments: Attachment[];
      // True for this-session sends (built from per-agent transcripts), false
      // for journal-sourced history (the overlay). A live fan-out groups even
      // before any recipient responds — so all-busy sends show their per-recipient
      // queued columns and cancel-send affordance immediately; a historical
      // fan-out only groups once its responses can be correlated.
      live: boolean;
    }
  | { kind: "agent"; at: string; rank: 1; key: string; send_id?: string; turn: AgentTurn }
  | {
      // An agent-scoped inter-turn marker (compaction). Never correlated to a
      // send — `send_id` is always absent, which keeps it out of fan-out
      // grouping and anchors it to its own timestamp.
      kind: "system_marker";
      at: string;
      rank: 2;
      key: string;
      send_id?: undefined;
      agent_id: AgentId;
      marker: SystemMarker;
    }
  | {
      kind: "outcome";
      at: string;
      rank: 3;
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

/// Render-windowing tunables (block counts are a loose proxy for per-item render
/// cost). Shared with the component that consumes them and the browser tests that
/// assert the bound, so the window size has one source of truth.
export const INITIAL_WINDOW = 20;
export const REVEAL_BATCH = 20;

const KIND_RANK = { user: 0, agent: 1, system_marker: 2, outcome: 3 } as const;

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
///
/// `knownAgentIds`, when provided, is the live roster: rows are filtered to it
/// so a **removed agent** leaves no orphan in the transcript. This matters
/// because the journal overlay's `user_message.agent_ids` carries the *original*
/// recipient set (and survives on the backend across reload), so a removed
/// agent would otherwise render a phantom fan-out column — "unknown" (gone from
/// the roster) stuck "queued" (its per-agent content was deleted). Pruning a
/// fan-out recipient set down to one agent collapses it back to a plain
/// single-recipient send; a user message left with no surviving recipients is
/// dropped entirely. Omit `knownAgentIds` to disable filtering.
export function buildUnifiedRows(
  turns: Turn[],
  overlay: ConversationItem[],
  knownAgentIds?: ReadonlySet<AgentId>,
): UnifiedRow[] {
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
  type UserGroup = {
    send_id?: string;
    agent_ids: AgentId[];
    text: string;
    attachments: Attachment[];
    at: string;
  };
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
          // Identical across a fan-out's recipients (the compose bar shares one
          // list), so the first turn's attachments stand for the group.
          attachments: turn.attachments ?? [],
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
      attachments: g.attachments,
      live: true,
    });
  }

  for (const item of overlay) {
    if (item.kind === "user_message") {
      // Skip an overlay user message whose send already has a **live** user row
      // (same `send_id`): the live turn(s) and the journal overlay both describe
      // the same send, and a background refresh can surface both at once. Without
      // this, a single-recipient send renders twice (the fan-out path is collapsed
      // later by `groupRenderBlocks`, but a single-recipient row would double). The
      // live row wins — it carries the in-session realtime state. An imported
      // prompt (`send_id` null) has no live group, so it always renders.
      if (item.send_id != null && groups.has(item.send_id)) continue;
      rows.push({
        kind: "user",
        at: item.at,
        rank: KIND_RANK.user,
        // Key off `id` (always present); an imported prompt's `send_id` is null,
        // which the anchor/grouping passes treat as "no send" via `=== undefined`.
        key: `u:${item.id}`,
        send_id: item.send_id ?? undefined,
        agent_ids: item.agent_ids,
        text: item.text,
        attachments: item.attachments ?? [],
        live: false,
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
    } else if (item.kind === "system_marker") {
      rows.push({
        kind: "system_marker",
        at: item.at,
        rank: KIND_RANK.system_marker,
        // Key on the parse-stable (agent, timestamp), NOT `item.id`: the marker's
        // `turn_id` is regenerated on every parse, so an `id`-keyed row is
        // destroyed and recreated on each project refresh — resetting the
        // `<details>` recap a user expanded back to collapsed. `at` derives from
        // the summary record's own timestamp and is invariant across re-parses.
        key: `s:${item.agent_id}:${item.at}`,
        agent_id: item.agent_id,
        marker: item.marker,
      });
    }
    // `agent_turn` overlay items are not expected in the decompose model
    // (routed to per-agent state); ignore to avoid double-rendering.
  }

  // Drop rows that belong to agents no longer in the roster (a removed agent),
  // pruning fan-out recipient sets and discarding messages left with no
  // surviving recipient. Live `turns` are already roster-scoped (the caller only
  // flattens roster agents), so this matters for the journal overlay, whose
  // `agent_ids` retains the original recipient set.
  const visibleRows: UnifiedRow[] = [];
  for (const row of rows) {
    if (knownAgentIds === undefined) {
      visibleRows.push(row);
    } else if (row.kind === "user") {
      const agent_ids = row.agent_ids.filter((id) => knownAgentIds.has(id));
      if (agent_ids.length > 0) visibleRows.push({ ...row, agent_ids });
    } else if (row.kind === "outcome" || row.kind === "system_marker") {
      if (knownAgentIds.has(row.agent_id)) visibleRows.push(row);
    } else if (knownAgentIds.has(row.turn.agent_id)) {
      visibleRows.push(row);
    }
  }

  // Order by **send**, not raw timestamp: each row is anchored to the time of
  // its send's user message (`sendAnchor`), so a send's response renders
  // directly under its prompt even when it ran much later. This matters for a
  // *queued* single-recipient send — live, all three user messages are stamped
  // near submit time (t1<t2<t3) while a queued send's response only starts after
  // the earlier turn finishes, so its own timestamp is *later than the next
  // prompt*. A raw-timestamp sort would float that response below the later
  // prompt (detached from its own); anchoring keeps it adjacent. Rows whose
  // send has no user message (pre-journal history with no recoverable send_id)
  // fall back to their own `at`. Within one anchor: kind rank (user < agent <
  // system_marker < outcome), then own `at`; `Array.prototype.sort` is stable so
  // ties hold insertion order.
  const sendAnchor = new Map<string, string>();
  for (const row of visibleRows) {
    if (row.kind === "user" && row.send_id !== undefined) {
      const prior = sendAnchor.get(row.send_id);
      if (prior === undefined || row.at < prior) sendAnchor.set(row.send_id, row.at);
    }
  }
  const anchorOf = (row: UnifiedRow): string =>
    (row.send_id !== undefined ? sendAnchor.get(row.send_id) : undefined) ?? row.at;
  visibleRows.sort((a, b) => {
    const t = anchorOf(a).localeCompare(anchorOf(b));
    if (t !== 0) return t;
    if (a.rank !== b.rank) return a.rank - b.rank;
    return a.at.localeCompare(b.at);
  });
  return visibleRows;
}

/// Group a flat row list into render blocks, collapsing each fan-out (a user
/// message to >1 recipient) into one block whose responses render as
/// per-recipient columns. A single-recipient send stays a sequence of
/// standalone rows (the user row, then its agent row) exactly as before.
///
/// The fan-out's agent + outcome rows (matched by `send_id`) are pulled out of
/// the flat stream into the group's columns. Columns are ordered by
/// `agentOrder` — the project's canonical roster order, the same list that
/// drives the sidebar and the compose-bar chips — so a fan-out's columns match
/// that order both live and after restart (the recipient set's own order
/// differs between the two: live is dispatch order, restored is journal order),
/// and will follow user-defined roster reordering once that lands. A recipient
/// absent from `agentOrder` sorts to the end (stable). The group is anchored at
/// the user message's position in the timeline; rows belonging to other sends
/// flow normally around it.
type NonUserRow = Exclude<UnifiedRow, { kind: "user" }>;

/// The recipient an agent/outcome row belongs to (agent rows carry it under
/// `.turn`; outcome rows carry it directly).
function rowAgentId(row: NonUserRow): AgentId {
  return row.kind === "agent" ? row.turn.agent_id : row.agent_id;
}

export function groupRenderBlocks(rows: UnifiedRow[], agentOrder: AgentId[] = []): RenderBlock[] {
  // Canonical column ordering: an agent's index in the roster. Recipients not
  // in the roster (shouldn't happen, but stay defensive) sort to the end.
  const orderIndex = new Map<AgentId, number>();
  agentOrder.forEach((id, i) => orderIndex.set(id, i));
  const rankOf = (id: AgentId): number => orderIndex.get(id) ?? Number.MAX_SAFE_INTEGER;
  // Pass 1: identify fan-out sends (a user row to >1 recipient) and bucket
  // every agent/outcome row of those sends by recipient.
  const fanoutUsers = new Map<string, Extract<UnifiedRow, { kind: "user" }>>();
  for (const row of rows) {
    if (row.kind === "user" && row.send_id !== undefined && row.agent_ids.length > 1) {
      fanoutUsers.set(row.send_id, row);
    }
  }
  const colsBySend = new Map<string, Map<AgentId, NonUserRow[]>>();
  for (const row of rows) {
    if (row.kind === "user" || row.send_id === undefined || !fanoutUsers.has(row.send_id)) continue;
    const perAgent = colsBySend.get(row.send_id) ?? new Map<AgentId, NonUserRow[]>();
    const agentId = rowAgentId(row);
    const col = perAgent.get(agentId) ?? [];
    col.push(row);
    perAgent.set(agentId, col);
    colsBySend.set(row.send_id, perAgent);
  }

  // A *live* fan-out always groups, even before any recipient responds, so an
  // all-busy send shows its per-recipient queued columns and cancel-send
  // affordance right away. A *historical* (journal-sourced) fan-out only groups
  // once its responses can be correlated: an uncorrelated multi-recipient
  // history row (no recoverable send_id match) renders as a plain user message
  // — its responses flow in the stream — rather than a group of empty columns.
  for (const [sendId, user] of [...fanoutUsers.entries()]) {
    if (!user.live && !colsBySend.has(sendId)) fanoutUsers.delete(sendId);
  }

  // Pass 2: emit blocks in order. A fan-out user row becomes a group anchored
  // at its position (columns in recipient order — stable); the fan-out's
  // agent/outcome rows are skipped (they live in the group). Everything else is
  // a standalone row.
  const blocks: RenderBlock[] = [];
  // A fan-out send can surface as more than one user row sharing its `send_id`
  // (the live row plus the journal-overlay row — they carry distinct row keys,
  // `u:<send_id>` vs `u:<journal_id>`, so nothing upstream collapses them).
  // Emitting a block per occurrence would mint duplicate `f:<send_id>` block
  // keys, which breaks the keyed `{#each}` in the transcript: Svelte leaves
  // orphaned, no-longer-updated DOM copies behind (a stuck hover footer on the
  // in-flight send is the visible symptom). The user's message renders once per
  // send (system-design §3), so anchor the group at the first occurrence and
  // skip any later user row for the same send.
  const emittedFanouts = new Set<string>();
  for (const row of rows) {
    if (row.kind === "user" && row.send_id !== undefined && fanoutUsers.has(row.send_id)) {
      if (emittedFanouts.has(row.send_id)) continue;
      emittedFanouts.add(row.send_id);
      const perAgent = colsBySend.get(row.send_id) ?? new Map<AgentId, NonUserRow[]>();
      blocks.push({
        kind: "fanout",
        key: `f:${row.send_id}`,
        send_id: row.send_id,
        user: row,
        columns: [...row.agent_ids]
          .sort((a, b) => rankOf(a) - rankOf(b))
          .map((agent_id) => ({
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
