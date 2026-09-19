/// The newest quota reading for each harness, across every agent and every
/// project.
///
/// **A quota belongs to an account, not to an agent or a project.** There is one
/// `claude login` / `codex login` per machine, and every turn any agent runs
/// reports that one account's windows. Holding the reading per agent published
/// one fact N times at N staleness levels, which is what made sibling cards
/// disagree; holding it under a project made reopening an old project restore
/// that project's older reading. One entry per harness removes both.
///
/// **This module owns the rule for which reading wins, and it is the only place
/// that rule exists.** The backend persists the map without interpreting it, so
/// there is no second copy to disagree with this one.
import { invoke } from "@tauri-apps/api/core";
import type { HarnessKind } from "$lib/types";
import { sameCodexUsageWindows } from "$lib/usageWindows";

/// One harness account's newest reading.
export type HarnessUsageReading = {
  /// The harness's own rate-limit payload, opaque here exactly as it is opaque
  /// through the adapter, the dispatcher, and IPC. Only `usageWindows.ts`
  /// interprets it.
  payload: unknown;
  /// **The ordering key**, ISO-8601. For a live event this is arrival time; for a
  /// reading recovered from a harness's own session file it is the instant the
  /// harness measured it, which is the only thing that can order several agents'
  /// restored readings against each other.
  ///
  /// Distinct from [`as_of`](#as_of), which is a staleness qualifier shown to the
  /// user and deliberately absent for a durable source. A reading needs both: one
  /// to be ranked, one to be labelled.
  observed_at: string;
  /// Model of the turn that delivered the reading, which is what names Claude's
  /// model-gated weekly window — the payload never names it itself.
  model?: string;
  /// Set only when the reading was restored from a snapshot rather than received
  /// live, so the UI can say how old it is. Absent for live and for
  /// session-file-backed readings, which are current by construction.
  as_of?: string;
  /// Whether the harness is currently refusing work because a window in this
  /// reading is exhausted.
  ///
  /// **Needed only for a harness whose payload cannot say so.** Codex records a
  /// windowless payload on a refused turn and names no window, so the verdict has
  /// to be carried beside the reading. Claude states it in the payload itself, so
  /// nothing sets this for Claude and `usageWindows.ts` reads Claude's own
  /// status instead.
  limit_reached?: boolean;
};

/// Keyed by harness. Partial because a harness contributes an entry only once one
/// of its agents has reported a reading; a harness that has never run shows no
/// card rather than an empty one.
export const harnessUsage = $state<Partial<Record<HarnessKind, HarnessUsageReading>>>({});

/// Whether `candidate` describes a strictly later observation than `stored`.
/// Ties keep what is already held: two readings stamped the same instant are
/// equally true, and replacing on a tie would churn the persisted file for no
/// gain.
function isNewer(candidate: string, stored: string | undefined): boolean {
  if (stored === undefined) return true;
  return candidate > stored;
}

/// Record a reading, keeping it only if nothing newer is already held.
///
/// The refusal verdict is carried forward **only when the new reading describes
/// the same windows**. A verdict is a judgment about a particular window, so a
/// reading describing different windows retires it along with the window it
/// judged. Without that, a stale verdict paints a freshly reset quota as spent,
/// which the reset-passed gate cannot correct because the new window is current.
/// A refused turn's own reading re-reports the same windows, which is what lets
/// the verdict survive the sequence that set it.
export function observeUsage(harness: HarnessKind, reading: HarnessUsageReading): void {
  const stored = harnessUsage[harness];
  if (!isNewer(reading.observed_at, stored?.observed_at)) return;
  const carried =
    stored?.limit_reached === true && sameCodexUsageWindows(stored.payload, reading.payload);
  harnessUsage[harness] = carried ? { ...reading, limit_reached: true } : reading;
  void persist();
}

/// Record that the harness refused a turn because a quota is exhausted.
///
/// Attaches to the reading already held rather than creating an entry: the
/// verdict is *about* a reading, and a refusal with no measurement to attach to
/// has no window to mark. A refused Codex turn always has one, since the reading
/// that preceded the refusal is still the newest window-bearing one on disk.
export function recordUsageRefusal(harness: HarnessKind): void {
  const stored = harnessUsage[harness];
  if (stored === undefined || stored.limit_reached === true) return;
  harnessUsage[harness] = { ...stored, limit_reached: true };
  void persist();
}

/// Fill in the model that delivered the newest reading, once it is known.
///
/// Claude's model-gated weekly window carries no model of its own, so the label
/// comes from the turn that delivered the reading — and that turn's `init` can
/// arrive *after* its rate-limit event. **Fills a blank only**: a reading that
/// already names a model is never relabelled, so a later turn on a different
/// model cannot rewrite history.
export function nameUsageModel(harness: HarnessKind, model: string | undefined): void {
  if (model === undefined || model === "") return;
  const stored = harnessUsage[harness];
  if (stored === undefined || stored.model !== undefined) return;
  harnessUsage[harness] = { ...stored, model };
  void persist();
}

/// Clear the refusal verdict after a turn completes.
///
/// **Only a completed turn clears it.** A cancellation is the user's own doing
/// and an unrelated failure is no evidence the quota moved, so neither is taken
/// as proof the harness is serving work again. Clearing on either reproduces, on
/// a slower clock, the flicker that came from deriving this from the last error.
export function clearUsageRefusal(harness: HarnessKind): void {
  const stored = harnessUsage[harness];
  if (stored === undefined || stored.limit_reached !== true) return;
  const { limit_reached: _limitReached, ...rest } = stored;
  harnessUsage[harness] = rest;
  void persist();
}

/// Seed the store from the persisted file at startup. Fill-if-empty per harness,
/// so a live reading that arrived while this was in flight is never replaced by
/// the older value from disk.
export async function loadPersistedUsage(): Promise<void> {
  let stored: { harnesses?: Record<string, unknown> };
  try {
    stored = await invoke<{ harnesses?: Record<string, unknown> }>("get_harness_usage");
  } catch (e) {
    // Best-effort: an unreadable file costs an empty usage section until the next
    // turn reports a reading, and must never fail app startup.
    console.warn("could not load persisted harness usage", e);
    return;
  }
  for (const [harness, value] of Object.entries(stored.harnesses ?? {})) {
    const reading = asReading(value);
    if (reading === null) continue;
    // `observeUsage` rather than a direct write, so the newest-wins rule applies
    // to the restored value too and a live reading already in memory holds.
    observeUsage(harness as HarnessKind, reading);
  }
}

/// Defensive read of one persisted entry. The file is machine-written, but it is
/// also user-editable and forward-compatible by design, so an entry that does not
/// carry the two fields every consumer needs is dropped rather than rendered.
function asReading(value: unknown): HarnessUsageReading | null {
  if (typeof value !== "object" || value === null) return null;
  const v = value as Record<string, unknown>;
  if (typeof v.observed_at !== "string") return null;
  if (v.payload === undefined) return null;
  return {
    payload: v.payload,
    observed_at: v.observed_at,
    model: typeof v.model === "string" ? v.model : undefined,
    as_of: typeof v.as_of === "string" ? v.as_of : undefined,
    limit_reached: v.limit_reached === true ? true : undefined,
  };
}

/// Write the whole map back. Whole-map because the backend holds no rule of its
/// own to merge with; failures are swallowed for the same reason a missing file
/// is tolerated.
async function persist(): Promise<void> {
  try {
    await invoke("set_harness_usage", { usage: { harnesses: { ...harnessUsage } } });
  } catch (e) {
    console.warn("could not persist harness usage", e);
  }
}

/// Test-only reset. Named under `_testing` so a production caller grepping for
/// "reset" cannot autocomplete into clearing app state.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(harnessUsage)) {
      delete harnessUsage[key as HarnessKind];
    }
  },
};
