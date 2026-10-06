/// Backend-owned personal preferences (`config.yaml`), loaded once at startup and
/// written back on change. Distinct from `theme.svelte.ts` (frontend-only,
/// localStorage, device-local presentation): these are real settings — some
/// consumed by the backend (Git-view open-actions, dispatch), some read only
/// here (`diff_style`, `auto_reading_mode`) — so the backend is the source of
/// truth and this store is a cached mirror.
///
/// Loading is lazy + idempotent (`load()` is safe to call repeatedly; it fetches
/// once). Saving is optimistic: the in-memory value updates immediately and the
/// backend write happens behind it, so the UI stays responsive — a write failure
/// is logged, not surfaced as a blocking error (the running session still
/// reflects the user's choice).

import * as api from "$lib/api";
import type { Preferences } from "$lib/types";
import { DEFAULT_AGENT_SELECTIONS } from "$lib/agentSelection";

const DEFAULTS: Preferences = {
  editor_command: "code",
  terminal_app: "Terminal",
  diff_style: "unified",
  show_builtins: true,
  claude_chrome_enabled: false,
  auto_reading_mode: false,
  notify_on_completion: true,
  notify_while_focused: false,
  agent_defaults: structuredClone(DEFAULT_AGENT_SELECTIONS),
};

export const preferences = $state<Preferences>({ ...DEFAULTS });

/// The last save failure, or null. Surfaced inline in Settings so a rare
/// `config.yaml` write failure isn't silent — the setting still works this
/// session (the in-memory value stands) but the user is told it may not survive
/// restart, and can report it.
///
/// `keys` records which preferences the *user touched* on the failed attempt, so
/// Settings can render the warning next to the control they used rather than in
/// whichever section happens to own the renderer.
///
/// **Any successful save clears it, even one for unrelated preferences.** That is
/// accurate, not sloppy: every write sends the whole merged object, and memory is
/// updated optimistically before the write — so a later successful save carries
/// the earlier failed value with it and does persist it.
export const saveStatus = $state<{ error: string | null; keys: string[] }>({
  error: null,
  keys: [],
});

export const preferenceLoadState = $state<{ ready: boolean }>({ ready: false });
let loadPromise: Promise<void> | null = null;
/// Serialize whole-object writes so a slower earlier save cannot land after a
/// newer one and restore stale defaults. This matters for selection editing,
/// where model and effort changes can be made in quick succession.
let saveTail: Promise<void> = Promise.resolve();
/// Fetch preferences once and share the same readiness barrier with every
/// caller. Failure settles to the built-in fallback rather than leaving
/// creation paths blocked indefinitely.
export function loadPreferences(): Promise<void> {
  if (loadPromise !== null) return loadPromise;
  loadPromise = (async () => {
    try {
      const fetched = await api.getPreferences();
      Object.assign(preferences, fetched, {
        agent_defaults: structuredClone(fetched.agent_defaults ?? DEFAULT_AGENT_SELECTIONS),
      });
    } catch (err) {
      console.warn("[switchboard] loadPreferences failed", err);
    } finally {
      preferenceLoadState.ready = true;
    }
  })();
  return loadPromise;
}

/// Apply a partial update, persisting the merged result. Updates memory first
/// (optimistic, so the session reflects the user's intent immediately), then
/// writes. On write failure the in-memory value stands but `saveStatus.error` is
/// set so Settings can surface it — the backend deliberately reports a failed
/// explicit save rather than hiding it.
export async function updatePreferences(patch: Partial<Preferences>): Promise<void> {
  await loadPreferences();
  const next: Preferences = { ...$state.snapshot(preferences), ...patch };
  // Assign in bulk rather than field-by-field: a per-field copy silently drops
  // any preference someone forgets to add here, which presents as a toggle that
  // won't move rather than as a compile error.
  Object.assign(preferences, next);
  const keys = Object.keys(patch);
  const save = saveTail.then(async () => {
    try {
      await api.setPreferences(next);
      saveStatus.error = null;
      saveStatus.keys = [];
    } catch (err) {
      saveStatus.error = err instanceof Error ? err.message : String(err);
      saveStatus.keys = keys;
    }
  });
  saveTail = save;
  await save;
}

/// Test-only reset.
export const _testing = {
  reset(options: { ready?: boolean } = {}): void {
    Object.assign(preferences, DEFAULTS, {
      agent_defaults: structuredClone(DEFAULTS.agent_defaults),
    });
    saveStatus.error = null;
    saveStatus.keys = [];
    preferenceLoadState.ready = options.ready ?? false;
    loadPromise = preferenceLoadState.ready ? Promise.resolve() : null;
    saveTail = Promise.resolve();
  },
};
