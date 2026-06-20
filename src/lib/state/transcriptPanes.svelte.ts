// Transcript pane layout: an ordered list of panes that assign zero-or-one pane
// to each roster agent, plus per-pane visibility and pane widths.
//
// **Membership is optional and exclusive.** Every pane is a named set of agents;
// a roster agent may belong to one pane or to no pane, but never to two panes.
// The default layout still starts with everyone in Pane 1 for continuity. From
// there, removing an agent from a pane makes it unassigned, and closing a pane
// unassigns its members. Moving an agent to a pane *moves* it (never copies).
//
// **Membership decides *where* an agent appears; `hidden` decides *whether*.**
// The eye/solo toggles edit a pane's `hidden` set without touching membership.
//
// **Widths are fractions, not pixels.** Each pane's share of the row is stored
// as a fraction summing to 1, so a layout saved on a wide monitor restores
// proportionally on a narrow one — the re-clamp-on-restore policy falls out of
// the representation. The cannot-fit floor (N panes × min width > row) is
// handled by CSS `min-width` + row overflow clipping; geometry never alters
// membership.
//
// **Device-local, persisted.** Pane arrangement is a personal, per-device
// preference (like compose drafts — see composeStore's module comment), so it
// lives in localStorage under a versioned envelope, never in the git-tracked
// `.switchboard/` state. Per-pane hidden sets persist with the layout
// deliberately: hiding is curation, like membership, and the sidebar's
// "N hidden · Show all" reset keeps a restored hide discoverable.

import type { AgentId, ProjectId } from "$lib/types";

export type PaneId = string;

export type TranscriptPane = {
  id: PaneId;
  name: string;
  /// Member agent ids, in assignment order. Render order should come from the
  /// roster (filter the roster by membership), not this array, so pane columns
  /// match the sidebar like fan-out columns do.
  members: AgentId[];
  /// Members currently eye-hidden within this pane.
  hidden: AgentId[];
};

export type PaneLayout = {
  panes: TranscriptPane[];
  /// One fraction per pane, summing to 1 — each pane's share of the row width.
  fractions: number[];
  /// Panes collapsed into the minimized strip. Display-only: membership and
  /// width fractions are preserved.
  minimized: PaneId[];
  /// Pane currently taking the transcript area. Display-only and mutually
  /// exclusive with ordinary multi-pane rendering.
  maximized: PaneId | null;
};

const STORAGE_KEY = "switchboard-transcript-panes";
const STORAGE_VERSION = 1;

/// Minimum pane width in px. Mirrors the GitView detail-panel clamp; the
/// layout component applies it both to gutter drags and as a CSS floor.
export const MIN_PANE_WIDTH_PX = 360;

function newPaneId(): PaneId {
  return crypto.randomUUID();
}

function defaultLayout(rosterIds: AgentId[]): PaneLayout {
  return {
    panes: [{ id: newPaneId(), name: "Pane 1", members: [...rosterIds], hidden: [] }],
    fractions: [1],
    minimized: [],
    maximized: null,
  };
}

function equalFractions(count: number): number[] {
  return Array.from({ length: count }, () => 1 / count);
}

function normalizeFractions(fractions: number[], paneCount: number): number[] {
  if (fractions.length !== paneCount) return equalFractions(paneCount);
  const sum = fractions.reduce((acc, f) => acc + f, 0);
  if (!Number.isFinite(sum) || sum <= 0 || fractions.some((f) => !Number.isFinite(f) || f <= 0)) {
    return equalFractions(paneCount);
  }
  return fractions.map((f) => f / sum);
}

function normalizeViewState(
  minimized: unknown,
  maximized: unknown,
  panes: TranscriptPane[],
): Pick<PaneLayout, "minimized" | "maximized"> {
  const ids = panes.map((pane) => pane.id);
  const valid = new Set(ids);
  const out: PaneId[] = [];
  if (Array.isArray(minimized)) {
    for (const id of minimized) {
      if (typeof id === "string" && valid.has(id) && !out.includes(id)) out.push(id);
    }
  }
  const max = typeof maximized === "string" && valid.has(maximized) ? maximized : null;
  if (max === null && out.length >= panes.length && out.length > 0) out.pop();
  return { minimized: out, maximized: max };
}

/// Reconcile a (possibly stale or absent) stored layout against the live roster,
/// returning a layout that satisfies the optional-exclusive membership invariant:
/// - stale agent ids (removed agents) are pruned from members and hidden;
/// - roster agents missing from every pane remain unassigned;
/// - an agent somehow present in two panes keeps its first (leftmost) slot;
/// - at least one pane always exists; fractions are normalized to the pane
///   count. Emptied panes stay open (the user named them; closing is theirs).
///
/// Pure — exported for tests and for read-time use; mutations run it before
/// applying so they always operate on a membership-valid layout.
export function reconcileLayout(layout: PaneLayout | undefined, rosterIds: AgentId[]): PaneLayout {
  if (layout === undefined || layout.panes.length === 0) return defaultLayout(rosterIds);
  // Pure-function locals, never reactive state — plain Sets are correct here.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const roster = new Set(rosterIds);
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const seen = new Set<AgentId>();
  const panes = layout.panes.map((pane) => {
    const members = pane.members.filter((id) => {
      if (!roster.has(id) || seen.has(id)) return false;
      seen.add(id);
      return true;
    });
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const memberSet = new Set(members);
    const hidden = pane.hidden.filter((id) => memberSet.has(id));
    return { ...pane, members, hidden };
  });
  return {
    panes,
    fractions: normalizeFractions(layout.fractions, panes.length),
    ...normalizeViewState(layout.minimized, layout.maximized, panes),
  };
}

// ── Persistence ──────────────────────────────────────────────────────────────

function parseIdList(value: unknown): string[] | null {
  return Array.isArray(value) && value.every((x) => typeof x === "string")
    ? (value as string[])
    : null;
}

function parsePane(value: unknown): TranscriptPane | null {
  if (value === null || typeof value !== "object") return null;
  const v = value as { id?: unknown; name?: unknown; members?: unknown; hidden?: unknown };
  if (typeof v.id !== "string" || typeof v.name !== "string") return null;
  const members = parseIdList(v.members);
  const hidden = parseIdList(v.hidden);
  if (members === null || hidden === null) return null;
  return { id: v.id, name: v.name, members, hidden };
}

/// Parse one project's persisted layout, degrading anything malformed to
/// "no saved layout" (the default single pane) — layout is ergonomic, not
/// load-bearing.
function parseLayout(value: unknown): PaneLayout | null {
  if (value === null || typeof value !== "object") return null;
  const v = value as {
    panes?: unknown;
    fractions?: unknown;
    minimized?: unknown;
    maximized?: unknown;
  };
  if (!Array.isArray(v.panes)) return null;
  const panes: TranscriptPane[] = [];
  for (const p of v.panes) {
    const pane = parsePane(p);
    if (pane === null) return null;
    panes.push(pane);
  }
  if (panes.length === 0) return null;
  const fractions =
    Array.isArray(v.fractions) && v.fractions.every((f) => typeof f === "number")
      ? (v.fractions as number[])
      : [];
  return {
    panes,
    fractions: normalizeFractions(fractions, panes.length),
    ...normalizeViewState(v.minimized, v.maximized, panes),
  };
}

function readStored(): Record<ProjectId, PaneLayout> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object") return {};
    const envelope = parsed as { version?: unknown; projects?: unknown };
    if (envelope.version !== STORAGE_VERSION) return {};
    if (envelope.projects === null || typeof envelope.projects !== "object") return {};
    const out: Record<ProjectId, PaneLayout> = {};
    for (const [id, value] of Object.entries(envelope.projects as Record<string, unknown>)) {
      const layout = parseLayout(value);
      if (layout !== null) out[id] = layout;
    }
    return out;
  } catch {
    return {};
  }
}

// Reactive (unlike composeStore's): pane membership and visibility drive what
// the transcript area renders, so components re-derive from this store.
const store = $state<Record<ProjectId, PaneLayout>>(readStored());

function persist(): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ version: STORAGE_VERSION, projects: store }),
    );
  } catch {
    // Quota or serialization failure — layout degrades to in-memory-only.
  }
}

// ── Reads ────────────────────────────────────────────────────────────────────

/// The project's layout, reconciled against the live roster. Reading
/// `store[projectId]` registers the reactive dependency; the reconcile is pure,
/// so a stale stored layout is never *displayed* invalid even before a
/// mutation rewrites it.
export function layoutFor(projectId: ProjectId, rosterIds: AgentId[]): PaneLayout {
  return reconcileLayout(store[projectId], rosterIds);
}

/// Total eye-hidden agents across the project's panes (the sidebar badge).
export function hiddenCount(projectId: ProjectId, rosterIds: AgentId[]): number {
  return layoutFor(projectId, rosterIds).panes.reduce((acc, p) => acc + p.hidden.length, 0);
}

/// Whether an agent is currently eye-hidden within its pane (the compose
/// chip's targeted-but-hidden cue).
export function isAgentHidden(
  projectId: ProjectId,
  rosterIds: AgentId[],
  agentId: AgentId,
): boolean {
  return layoutFor(projectId, rosterIds).panes.some((p) => p.hidden.includes(agentId));
}

/// The pane hosting an agent, or null when the agent isn't in the roster.
export function paneOfAgent(
  projectId: ProjectId,
  rosterIds: AgentId[],
  agentId: AgentId,
): TranscriptPane | null {
  return layoutFor(projectId, rosterIds).panes.find((p) => p.members.includes(agentId)) ?? null;
}

export function unassignedAgentIds(projectId: ProjectId, rosterIds: AgentId[]): AgentId[] {
  const assigned = layoutFor(projectId, rosterIds).panes.flatMap((p) => p.members);
  return rosterIds.filter((id) => !assigned.includes(id));
}

function sameMembers(a: AgentId[], b: AgentId[]): boolean {
  if (a.length !== b.length) return false;
  const set = new Set(a);
  return b.every((id) => set.has(id));
}

/// The pane to move to when cycling panes by position (⌘⌥[ / ⌘⌥]). `direction`
/// is -1 (previous/left) or +1 (next/right); cycling wraps. Only non-empty panes
/// participate — an empty pane is never a send target. The "current" pane is the
/// maximized one if any, else the pane whose membership matches the live
/// selection (set by the last targeting gesture, including a prior cycle), so no
/// separate focus cursor is stored — selection stays the single source of truth.
/// A custom per-agent selection that matches no pane enters from the leftmost
/// (next) or rightmost (previous) end. Returns null when fewer than two panes can
/// be cycled.
export function paneToCycleTo(
  projectId: ProjectId,
  rosterIds: AgentId[],
  selectedIds: AgentId[],
  direction: 1 | -1,
): TranscriptPane | null {
  const layout = layoutFor(projectId, rosterIds);
  const candidates = layout.panes.filter((pane) => pane.members.length > 0);
  if (candidates.length < 2) return null;
  const currentId =
    layout.maximized ??
    candidates.find((pane) => sameMembers(pane.members, selectedIds))?.id ??
    null;
  const currentIdx =
    currentId === null ? -1 : candidates.findIndex((pane) => pane.id === currentId);
  const startIdx = currentIdx === -1 ? (direction === 1 ? -1 : 0) : currentIdx;
  const nextIdx = (startIdx + direction + candidates.length) % candidates.length;
  return candidates[nextIdx]!;
}

// ── Mutations ────────────────────────────────────────────────────────────────

/// Reconcile-then-mutate: every mutation starts from a membership-valid layout
/// so a stale persisted state can't corrupt an operation.
function update(
  projectId: ProjectId,
  rosterIds: AgentId[],
  mutate: (layout: PaneLayout) => PaneLayout,
): void {
  store[projectId] = mutate(reconcileLayout(store[projectId], rosterIds));
  persist();
}

export function toggleAgentHidden(
  projectId: ProjectId,
  rosterIds: AgentId[],
  agentId: AgentId,
): void {
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    panes: layout.panes.map((pane) => {
      if (!pane.members.includes(agentId)) return pane;
      const hidden = pane.hidden.includes(agentId)
        ? pane.hidden.filter((id) => id !== agentId)
        : [...pane.hidden, agentId];
      return { ...pane, hidden };
    }),
  }));
}

/// Solo an agent within its own pane: hide every other member, show the agent.
/// Re-soloing the already-soloed agent restores the pane (clears its hidden
/// set). Deliberately pane-local — a mixer-style global solo would empty every
/// unrelated pane, which is more disruptive than helpful in a tiled layout.
export function soloAgent(projectId: ProjectId, rosterIds: AgentId[], agentId: AgentId): void {
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    panes: layout.panes.map((pane) => {
      if (!pane.members.includes(agentId)) return pane;
      const others = pane.members.filter((id) => id !== agentId);
      const alreadySolo =
        !pane.hidden.includes(agentId) && others.every((id) => pane.hidden.includes(id));
      return { ...pane, hidden: alreadySolo ? [] : others };
    }),
  }));
}

/// Clear every pane's hidden set (the sidebar's roster-wide
/// "N hidden · Show all" reset).
export function showAllAgents(projectId: ProjectId, rosterIds: AgentId[]): void {
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    panes: layout.panes.map((pane) => ({ ...pane, hidden: [] })),
  }));
}

/// Clear ONE pane's hidden set — the in-pane "all agents in this pane are
/// hidden" hint. Scoped to match its label: revealing this pane must not undo
/// hides the user deliberately set in other panes.
export function showAllInPane(projectId: ProjectId, rosterIds: AgentId[], paneId: PaneId): void {
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    panes: layout.panes.map((pane) => (pane.id === paneId ? { ...pane, hidden: [] } : pane)),
  }));
}

/// Move an agent to an existing pane. **Move, never copy** — the agent leaves
/// its previous pane's members and hidden sets, so it renders in at most one
/// pane. No-op if the target doesn't exist or already hosts the agent.
export function moveAgentToPane(
  projectId: ProjectId,
  rosterIds: AgentId[],
  agentId: AgentId,
  paneId: PaneId,
): void {
  update(projectId, rosterIds, (layout) => {
    const target = layout.panes.find((p) => p.id === paneId);
    if (target === undefined || target.members.includes(agentId)) return layout;
    return {
      ...layout,
      panes: layout.panes.map((pane) => {
        if (pane.id === paneId) return { ...pane, members: [...pane.members, agentId] };
        return {
          ...pane,
          members: pane.members.filter((id) => id !== agentId),
          hidden: pane.hidden.filter((id) => id !== agentId),
        };
      }),
    };
  });
}

export function unassignAgentFromPane(
  projectId: ProjectId,
  rosterIds: AgentId[],
  agentId: AgentId,
): void {
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    panes: layout.panes.map((pane) => ({
      ...pane,
      members: pane.members.filter((id) => id !== agentId),
      hidden: pane.hidden.filter((id) => id !== agentId),
    })),
  }));
}

/// Unique default pane name: "Pane N" counting up from the pane count,
/// skipping collisions with existing (possibly renamed) panes.
function nextPaneName(panes: TranscriptPane[]): string {
  const names = new Set(panes.map((p) => p.name));
  let n = panes.length + 1;
  while (names.has(`Pane ${n}`)) n += 1;
  return `Pane ${n}`;
}

function expandedPaneCount(layout: PaneLayout): number {
  if (layout.maximized !== null) return 1;
  return layout.panes.filter((pane) => !layout.minimized.includes(pane.id)).length;
}

function newPaneStartsMinimized(layout: PaneLayout): boolean {
  return (
    layout.maximized === null &&
    rowGeometry.width > 0 &&
    (expandedPaneCount(layout) + 1) * MIN_PANE_WIDTH_PX > rowGeometry.width
  );
}

export function createEmptyPane(projectId: ProjectId, rosterIds: AgentId[]): PaneId {
  const paneId = newPaneId();
  update(projectId, rosterIds, (layout) => {
    const panes = [
      ...layout.panes,
      { id: paneId, name: nextPaneName(layout.panes), members: [], hidden: [] },
    ];
    const minimized = newPaneStartsMinimized(layout)
      ? [...layout.minimized, paneId]
      : layout.minimized;
    return {
      panes,
      fractions: equalFractions(panes.length),
      minimized,
      maximized: layout.maximized,
    };
  });
  return paneId;
}

/// Create a new rightmost pane and move the agent into it. Returns the new
/// pane's id. The new pane takes an equal share of the row (fractions
/// renormalize). If the live row cannot fit another expanded pane at the
/// minimum width, the new pane starts minimized so the action remains available
/// without creating a cramped transcript layout.
export function moveAgentToNewPane(
  projectId: ProjectId,
  rosterIds: AgentId[],
  agentId: AgentId,
): PaneId {
  const paneId = newPaneId();
  update(projectId, rosterIds, (layout) => {
    const panes = layout.panes.map((pane) => ({
      ...pane,
      members: pane.members.filter((id) => id !== agentId),
      hidden: pane.hidden.filter((id) => id !== agentId),
    }));
    panes.push({ id: paneId, name: nextPaneName(layout.panes), members: [agentId], hidden: [] });
    const startsMinimized = newPaneStartsMinimized(layout);
    return {
      panes,
      fractions: equalFractions(panes.length),
      minimized: startsMinimized ? [...layout.minimized, paneId] : layout.minimized,
      maximized: layout.maximized,
    };
  });
  return paneId;
}

/// Close a pane, leaving its members unassigned. Unavailable (no-op) with a
/// single pane: there must always be at least one place to add agents back.
/// The closed pane's width share is absorbed by its left neighbor, or its right
/// neighbor when the closed pane is leftmost.
export function closePane(projectId: ProjectId, rosterIds: AgentId[], paneId: PaneId): void {
  update(projectId, rosterIds, (layout) => {
    if (layout.panes.length <= 1) return layout;
    const index = layout.panes.findIndex((p) => p.id === paneId);
    if (index === -1) return layout;
    const neighborIndex = index === 0 ? 1 : index - 1;
    const panes = layout.panes.filter((_, i) => i !== index);
    const fractions = layout.fractions
      .map((f, i) => (i === neighborIndex ? f + (layout.fractions[index] ?? 0) : f))
      .filter((_, i) => i !== index);
    const view = normalizeViewState(
      layout.minimized.filter((id) => id !== paneId),
      layout.maximized === paneId ? null : layout.maximized,
      panes,
    );
    return {
      panes,
      fractions: normalizeFractions(fractions, panes.length),
      ...view,
    };
  });
}

/// Collapse every pane back into a single unified pane holding the whole roster
/// — the "return to unified view" / exit-split gesture. Re-merges any unassigned
/// agents so the unified view shows everyone; per-agent eye-hidden state carries
/// over so a hidden agent stays hidden. The recipient selection is the caller's
/// concern (gestures own it), not touched here.
export function returnToUnifiedView(projectId: ProjectId, rosterIds: AgentId[]): void {
  update(projectId, rosterIds, (layout) => {
    // Membership is exclusive, so an agent appears in at most one pane's hidden
    // set — no dedup needed. Filter to the live roster for safety.
    const hidden = layout.panes.flatMap((p) => p.hidden).filter((id) => rosterIds.includes(id));
    return {
      panes: [{ id: newPaneId(), name: "Pane 1", members: [...rosterIds], hidden }],
      fractions: [1],
      minimized: [],
      maximized: null,
    };
  });
}

export function minimizePane(projectId: ProjectId, rosterIds: AgentId[], paneId: PaneId): void {
  update(projectId, rosterIds, (layout) => {
    if (layout.maximized !== null) return layout;
    const paneExists = layout.panes.some((pane) => pane.id === paneId);
    if (!paneExists || layout.minimized.includes(paneId)) return layout;
    const expandedCount = layout.panes.filter((pane) => !layout.minimized.includes(pane.id)).length;
    if (expandedCount <= 1) return layout;
    return { ...layout, minimized: [...layout.minimized, paneId] };
  });
}

export function restorePane(projectId: ProjectId, rosterIds: AgentId[], paneId: PaneId): void {
  // Known tradeoff: restore currently removes the tab without enforcing the
  // row-width fit policy. On narrow windows this can clip rightmost panes until
  // the user minimizes another pane. Keep membership untouched; revisit with an
  // explicit displacement policy if this becomes a common workflow.
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    minimized: layout.minimized.filter((id) => id !== paneId),
    maximized: layout.maximized === paneId ? null : layout.maximized,
  }));
}

export function maximizePane(projectId: ProjectId, rosterIds: AgentId[], paneId: PaneId): void {
  update(projectId, rosterIds, (layout) =>
    layout.panes.some((pane) => pane.id === paneId) ? { ...layout, maximized: paneId } : layout,
  );
}

/// Make a pane visible, preserving the user's view mode: while another pane is
/// maximized the target *replaces* it (focus stays focus); otherwise a
/// minimized target is restored into the row. A revealed pane always leaves
/// the minimized set, so it doesn't vanish back to a tab when maximization is
/// later restored. Already-visible panes are untouched.
///
/// This is the reveal half of pane-targeting gestures (minimized header tab,
/// Cmd+Alt+N, `@panename`): targeting a pane the user cannot see would stream
/// the reply into an invisible pane.
export function revealPane(projectId: ProjectId, rosterIds: AgentId[], paneId: PaneId): void {
  update(projectId, rosterIds, (layout) => {
    if (!layout.panes.some((pane) => pane.id === paneId)) return layout;
    if (layout.maximized !== null && layout.maximized !== paneId) {
      return {
        ...layout,
        maximized: paneId,
        minimized: layout.minimized.filter((id) => id !== paneId),
      };
    }
    if (!layout.minimized.includes(paneId)) return layout;
    return { ...layout, minimized: layout.minimized.filter((id) => id !== paneId) };
  });
}

export function restoreMaximizedPane(projectId: ProjectId, rosterIds: AgentId[]): void {
  // Same fit-policy tradeoff as restorePane: restoring all panes can reveal
  // more expanded panes than a narrow row can display.
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    ...normalizeViewState(layout.minimized, null, layout.panes),
  }));
}

export function renamePane(
  projectId: ProjectId,
  rosterIds: AgentId[],
  paneId: PaneId,
  name: string,
): void {
  const trimmed = name.trim();
  if (trimmed.length === 0) return;
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    panes: layout.panes.map((pane) => (pane.id === paneId ? { ...pane, name: trimmed } : pane)),
  }));
}

/// Replace the row's width fractions (the gutter-drag commit). The caller
/// computes clamping in px against the live row width; this just normalizes
/// and stores.
export function setFractions(
  projectId: ProjectId,
  rosterIds: AgentId[],
  fractions: number[],
): void {
  update(projectId, rosterIds, (layout) => ({
    ...layout,
    fractions: normalizeFractions(fractions, layout.panes.length),
  }));
}

// ── Row geometry ─────────────────────────────────────────────────────────────

// The live pane row's measured width, published by the layout component. Global
// (not per-project) because exactly one project's pane row is mounted at a
// time. Drives only whether a newly-created pane starts minimized — geometry
// never alters membership.
const rowGeometry = $state({ width: 0 });

export function setPaneRowWidth(px: number): void {
  rowGeometry.width = px;
}

/// Test-only API surface. Production hydrates once at module load; tests use
/// `reset` to isolate between cases and `reloadFromStorage` to exercise the
/// restart path.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(store)) delete store[key];
    rowGeometry.width = 0;
    if (typeof localStorage !== "undefined") localStorage.removeItem(STORAGE_KEY);
  },
  reloadFromStorage(): void {
    const next = readStored();
    for (const key of Object.keys(store)) delete store[key];
    Object.assign(store, next);
  },
};
