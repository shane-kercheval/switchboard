/// Per-project compose drafts + recipient selection, persisted to localStorage
/// so a half-written message and the chosen agent chips survive both project
/// switches (the ComposeBar is remounted per project via `{#key}`) and app
/// restarts.
///
/// This is pre-durable UI ergonomics — the same category as the theme
/// preference, *not* conversation history. A draft is earlier than even a
/// queued send, which system-design §3 already classes as live-UI-only. It
/// therefore lives in machine-local localStorage rather than the git-tracked
/// `.switchboard/` project state: a half-typed message must not sync to a
/// teammate. localStorage is also origin-scoped, so `make dev DEV_PORT=…`
/// instances get isolated drafts for free.
///
/// Writes are synchronous (no debounce): drafts are tiny, and a deferred write
/// would otherwise race a send-clear (resurrecting just-sent text) or a project
/// switch (writing one project's draft into another's slot).

import type { AgentId, ProjectId } from "$lib/types";

const STORAGE_KEY = "switchboard-compose";

/// A project's compose snapshot. `selectedIds === undefined` means "no saved
/// selection — fall through to the default recipient"; an explicit `[]` means
/// "the user deliberately deselected everyone" and is honored on restore. Keep
/// this distinction deliberate — collapsing them loses deselect-all.
export type ComposeSnapshot = { draft: string; selectedIds?: AgentId[] };

function readStored(): Record<ProjectId, ComposeSnapshot> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object") return {};
    const out: Record<ProjectId, ComposeSnapshot> = {};
    for (const [id, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (value === null || typeof value !== "object") continue;
      const v = value as { draft?: unknown; selectedIds?: unknown };
      const draft = typeof v.draft === "string" ? v.draft : "";
      const selectedIds = Array.isArray(v.selectedIds)
        ? v.selectedIds.filter((x): x is AgentId => typeof x === "string")
        : undefined;
      out[id] = selectedIds === undefined ? { draft } : { draft, selectedIds };
    }
    return out;
  } catch {
    return {};
  }
}

// Hydrated once at module load. Not reactive state — no component re-derives
// from it; the ComposeBar reads its snapshot once at construction and writes
// through on change.
const store: Record<ProjectId, ComposeSnapshot> = readStored();

function persist(): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
  } catch {
    // Quota or serialization failure — drafts are ergonomic, not load-bearing,
    // so a failed persist degrades to in-memory-only rather than throwing.
  }
}

/// Current snapshot for a project; `{ draft: "" }` when nothing is saved.
export function getCompose(projectId: ProjectId): ComposeSnapshot {
  return store[projectId] ?? { draft: "" };
}

export function setDraft(projectId: ProjectId, draft: string): void {
  store[projectId] = { ...(store[projectId] ?? { draft: "" }), draft };
  persist();
}

export function setSelection(projectId: ProjectId, selectedIds: AgentId[]): void {
  store[projectId] = { ...(store[projectId] ?? { draft: "" }), selectedIds };
  persist();
}

/// Test-only API surface. Production hydrates once at module load; tests use
/// `reset` to isolate between cases and `reloadFromStorage` to exercise the
/// restart path (write localStorage, drop the in-memory copy, re-read).
export const _testing = {
  reset(): void {
    for (const key of Object.keys(store)) delete store[key];
    if (typeof localStorage !== "undefined") localStorage.removeItem(STORAGE_KEY);
  },
  reloadFromStorage(): void {
    const next = readStored();
    for (const key of Object.keys(store)) delete store[key];
    Object.assign(store, next);
  },
};
