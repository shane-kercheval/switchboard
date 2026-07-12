// The transcript navigator's message index: a flat, chronological list of
// every message derived from the unified row model (never the DOM — the
// transcript is render-windowed, so off-window messages aren't mounted), plus
// the search/filter rules the navigator popover applies to it.
//
// Entries address rows by key (grouping-independent — see
// `state/transcriptJump.svelte.ts`), carry the agents that anchor pane
// resolution, and precompute their searchable text once so typing in the
// filter doesn't re-normalize every message per keystroke.

import type { UnifiedRow } from "$lib/state/unified";
import type { Turn } from "$lib/state/types";
import type { AgentId } from "$lib/types";
import { toolDetail, toolVerb } from "$lib/toolRow";
import { previewLine } from "$lib/markdown";

type AgentTurn = Extract<Turn, { role: "agent" }>;

export type NavigatorRole = "user" | "agent";
export type NavigatorRoleFilter = "all" | NavigatorRole;

export type NavigatorEntry = {
  /// UnifiedRow key — the jump target.
  rowKey: string;
  role: NavigatorRole;
  /// Pane-resolution anchors: the recipients for a user send, the author for
  /// an agent turn.
  agentIds: AgentId[];
  /// "You", or the agent's name.
  attribution: string;
  /// ISO timestamp (row order is already chronological; this is display-only).
  at: string;
  /// One cleaned line for the list row.
  preview: string;
  /// The message's full prose — the hover-preview source. User text, or an
  /// agent turn's text items concatenated. Tool calls and thinking are
  /// excluded from search by design (they'd drown prose hits).
  prose: string;
  /// Whitespace-collapsed, lowercased `prose`, precomputed for the matcher.
  searchText: string;
};

function collapseWhitespace(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

function agentProse(turn: AgentTurn): string {
  return turn.items
    .filter((item) => item.item_kind === "text" && item.kind === "text")
    .map((item) => (item.item_kind === "text" ? item.text : ""))
    .join("\n");
}

/// Preview for a turn with no answer prose yet (or ever): the first tool
/// call's normalized verb + detail (the same vocabulary the tool rows render),
/// else the first thinking line, else empty (the UI shows a placeholder).
function agentFallbackPreview(turn: AgentTurn): string {
  for (const item of turn.items) {
    if (item.item_kind === "tool") {
      const detail = toolDetail(item.facet, item.input);
      const verb = toolVerb(item.facet, item.name);
      return detail === undefined ? verb : `${verb} · ${detail}`;
    }
  }
  for (const item of turn.items) {
    if (item.item_kind === "text" && item.kind === "thinking") {
      const line = previewLine(item.text);
      if (line !== "") return `Thinking · ${line}`;
    }
  }
  return "";
}

function entry(
  rowKey: string,
  role: NavigatorRole,
  agentIds: AgentId[],
  attribution: string,
  at: string,
  preview: string,
  prose: string,
): NavigatorEntry {
  return {
    rowKey,
    role,
    agentIds,
    attribution,
    at,
    preview,
    prose,
    searchText: collapseWhitespace(prose).toLowerCase(),
  };
}

/// Flatten the unified rows into navigator entries, in transcript order: one
/// entry per user send (the row model already collapses a fan-out's per-agent
/// user copies) and one attributed entry per agent turn. Outcome markers and
/// system markers aren't messages and don't index.
export function buildNavigatorEntries(
  rows: UnifiedRow[],
  agentNames: ReadonlyMap<AgentId, string>,
): NavigatorEntry[] {
  const entries: NavigatorEntry[] = [];
  for (const row of rows) {
    if (row.kind === "user") {
      const attachmentOnly = row.text.trim() === "" && row.attachments.length > 0;
      const preview = attachmentOnly ? row.attachments[0]!.label : previewLine(row.text);
      entries.push(entry(row.key, "user", row.agent_ids, "You", row.at, preview, row.text));
    } else if (row.kind === "agent") {
      const prose = agentProse(row.turn);
      const preview = prose.trim() === "" ? agentFallbackPreview(row.turn) : previewLine(prose);
      const name = agentNames.get(row.turn.agent_id) ?? row.turn.agent_id;
      entries.push(entry(row.key, "agent", [row.turn.agent_id], name, row.at, preview, prose));
    }
  }
  return entries;
}

/// Case-insensitive, whitespace-collapsed substring over the message prose —
/// the projects-sidebar search precedent. No fuzzy matching: this is
/// find-in-conversation, not a search engine.
export function filterEntries(
  entries: NavigatorEntry[],
  query: string,
  role: NavigatorRoleFilter,
): NavigatorEntry[] {
  const q = collapseWhitespace(query).toLowerCase();
  return entries.filter(
    (e) => (role === "all" || e.role === role) && (q === "" || e.searchText.includes(q)),
  );
}
