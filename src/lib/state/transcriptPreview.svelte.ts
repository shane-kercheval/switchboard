// Compact-transcript state: per-project "compact mode" plus per-unit overrides.
//
// **Project-scoped, session-only.** Each project carries its own compact mode
// and its own set of manual per-unit overrides, keyed by `ProjectId`, so one
// project can be compact while another stays expanded. Switching projects does
// not touch this state — it is intentionally remembered for the app session and
// reset only on restart (it lives in memory, never persisted).
//
// **Compact is on by default.** A project the user has never toggled starts
// compact, so long transcripts open scannable (older units collapsed, the
// latest response expanded). The header control inverts from there.
//
// **Default vs. override.** A visible transcript unit's *default* compactness is
// computed by the renderer (compact mode on/off, plus the latest-completed-
// response exception) and passed in as `defaultCompact`. This module only owns
// the user's deviations from that default: an entry in `overrides` is an
// explicit per-unit choice that wins over the default. Touching a unit creates a
// sticky override that persists until a project-level clear (header normalize,
// `setProjectCompact`, or `clearProjectOverrides`) — so `hasOverrides` means
// "the user has manually opened or closed some units," which is exactly the
// signal the header's reset action keys on.
//
// **Virtualization-safe keys.** Override keys are stable, data-derived ids
// supplied by the caller (never DOM position or list index), so they survive a
// future virtualizer unmounting off-screen units.

import type { ProjectId } from "$lib/types";

export type TranscriptPreviewProjectState = {
  enabled: boolean;
  overrides: Record<string, boolean>;
};

// Dynamic keyed `$state` record: adding/deleting a project key is reactive (the
// same pattern `gitView`'s `fetchStates` relies on), so consumers reading
// `stateFor(id)` re-run when that project's state first appears or changes.
const store = $state<Record<ProjectId, TranscriptPreviewProjectState>>({});

/// The project's compact state, or a fresh read-only default when it has none
/// yet. Reading `store[projectId]` here registers the reactive dependency, so a
/// later mutation that creates the entry re-runs the caller. Treat the result as
/// read-only — mutations go through the helpers below.
export function stateFor(projectId: ProjectId): TranscriptPreviewProjectState {
  return store[projectId] ?? { enabled: true, overrides: {} };
}

/// The mutable, proxied state for a project, creating it on first write.
function ensure(projectId: ProjectId): TranscriptPreviewProjectState {
  if (store[projectId] === undefined) {
    store[projectId] = { enabled: true, overrides: {} };
  }
  return store[projectId]!;
}

/// Whether a visible unit should render compact: an explicit override wins,
/// otherwise the caller-computed default.
export function isCompact(projectId: ProjectId, key: string, defaultCompact: boolean): boolean {
  return stateFor(projectId).overrides[key] ?? defaultCompact;
}

/// Flip one unit's effective compactness, recording the result as an explicit
/// override. Toggling from the default state still writes an override (the user
/// has now deviated), so the header gains its reset affordance.
export function toggleKey(projectId: ProjectId, key: string, defaultCompact: boolean): void {
  const state = ensure(projectId);
  state.overrides[key] = !(state.overrides[key] ?? defaultCompact);
}

/// Force a set of units to the same compactness (the fan-out group control).
export function setManyOverrides(projectId: ProjectId, keys: string[], compact: boolean): void {
  const state = ensure(projectId);
  for (const key of keys) state.overrides[key] = compact;
}

/// Set a project's compact mode and drop its overrides — a clean mode switch
/// with no lingering per-unit deviations.
export function setProjectCompact(projectId: ProjectId, enabled: boolean): void {
  const state = ensure(projectId);
  state.enabled = enabled;
  state.overrides = {};
}

export function hasOverrides(projectId: ProjectId): boolean {
  return Object.keys(stateFor(projectId).overrides).length > 0;
}

/// Drop a project's per-unit overrides, leaving its compact mode untouched.
export function clearProjectOverrides(projectId: ProjectId): void {
  const state = store[projectId];
  if (state !== undefined) state.overrides = {};
}

/// The header toggle's action. With manual overrides present it acts as a
/// reset — enable compact mode and clear the deviations, giving a reliable way
/// back to clean compact after manually opening/closing several units. With no
/// overrides it simply inverts compact mode. Either way it ends with no
/// overrides.
export function normalizeProjectCompact(projectId: ProjectId): void {
  const state = ensure(projectId);
  state.enabled = hasOverrides(projectId) ? true : !state.enabled;
  state.overrides = {};
}

/// Test-only reset.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(store)) delete store[key];
  },
};
