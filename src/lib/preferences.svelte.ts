/// Backend-owned personal preferences (`config.yaml`), loaded once at startup and
/// written back on change. Distinct from `theme.svelte.ts` (frontend-only,
/// localStorage): these are consumed by the backend's Git-view open-actions, so
/// the backend is the source of truth and this store is a cached mirror.
///
/// Loading is lazy + idempotent (`load()` is safe to call repeatedly; it fetches
/// once). Saving is optimistic: the in-memory value updates immediately and the
/// backend write happens behind it, so the UI stays responsive — a write failure
/// is logged, not surfaced as a blocking error (the running session still
/// reflects the user's choice).

import * as api from "$lib/api";
import type { Preferences } from "$lib/types";

const DEFAULTS: Preferences = {
  editor_command: null,
  terminal_app: "Terminal",
};

export const preferences = $state<Preferences>({ ...DEFAULTS });

/// The last save failure, or null. Surfaced inline in Settings so a rare
/// `config.yaml` write failure isn't silent — the setting still works this
/// session (the in-memory value stands) but the user is told it may not survive
/// restart, and can report it. Cleared on the next successful save.
export const saveStatus = $state<{ error: string | null }>({ error: null });

let loaded = false;
/// Set once the user changes a preference this session. Guards against a slow
/// startup `loadPreferences` resolving *after* an edit and clobbering it (the
/// fetched value would otherwise snap the input back to the on-disk value).
let dirtied = false;

/// Fetch preferences from the backend once. Subsequent calls are no-ops. Safe to
/// call from multiple mount points; the first wins and the rest see the result.
export async function loadPreferences(): Promise<void> {
  if (loaded) return;
  loaded = true;
  try {
    const fetched = await api.getPreferences();
    // If the user already edited a field while this was in flight, their intent
    // wins — don't overwrite it with the just-loaded on-disk value.
    if (dirtied) return;
    preferences.editor_command = fetched.editor_command;
    preferences.terminal_app = fetched.terminal_app;
  } catch (err) {
    // Backend unreachable / no config location — keep defaults. Allow a retry.
    loaded = false;
    console.warn("[switchboard] loadPreferences failed", err);
  }
}

/// Apply a partial update, persisting the merged result. Updates memory first
/// (optimistic, so the session reflects the user's intent immediately), then
/// writes. On write failure the in-memory value stands but `saveStatus.error` is
/// set so Settings can surface it — the backend deliberately reports a failed
/// explicit save rather than hiding it.
export async function updatePreferences(patch: Partial<Preferences>): Promise<void> {
  dirtied = true;
  const next: Preferences = { ...$state.snapshot(preferences), ...patch };
  preferences.editor_command = next.editor_command;
  preferences.terminal_app = next.terminal_app;
  try {
    await api.setPreferences(next);
    saveStatus.error = null;
  } catch (err) {
    saveStatus.error = err instanceof Error ? err.message : String(err);
  }
}

/// Test-only reset.
export const _testing = {
  reset(): void {
    preferences.editor_command = DEFAULTS.editor_command;
    preferences.terminal_app = DEFAULTS.terminal_app;
    saveStatus.error = null;
    loaded = false;
    dirtied = false;
  },
};
