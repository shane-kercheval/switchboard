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
  /// **Also what dates the reading for the user.** There used to be a second
  /// field for that, set only when a reading came from a stream-only snapshot, on
  /// the theory that a session-file-backed reading needs no staleness qualifier
  /// because it is durable. Durable was mistaken for current: the file is re-read
  /// on every open but can itself be days old. One instant answers both questions.
  ///
  /// **Optional, because a reading can genuinely have no measured instant**: a
  /// Codex `token_count` record whose line carries no parseable timestamp yields
  /// one, and the reading is still worth keeping. Such a reading ranks below
  /// every stamped one and renders with no age line at all, rather than being
  /// given a fabricated instant that would sort correctly and then be shown to
  /// the user as a date in 1970.
  observed_at?: string;
  /// Model of the turn that delivered the reading, which is what names Claude's
  /// model-gated weekly window — the payload never names it itself.
  model?: string;
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
///
/// **Compared as instants, never as text.** The producers disagree on fractional
/// precision — chrono emits none for a whole second and six digits otherwise,
/// `toISOString` always emits three — and both shapes are present in one
/// `usage.yaml`. Lexically `"…898649Z"` loses to `"…898Z"` because a digit sorts
/// below `Z`, so a more precise stamp would always lose to a coarser one from the
/// same millisecond.
///
/// **An absent or unparseable instant ranks last**, so a reading whose age is
/// unknown can be superseded but never supersedes. Two such readings tie, and a
/// tie keeps what is already held: the first unstamped reading therefore stands
/// until a stamped one arrives, which is arbitrary but stable, and neither of two
/// undateable readings has a claim over the other.
///
/// Presumes a reading is already held. The *first* reading for a harness is taken
/// unconditionally by the caller, dated or not — otherwise an undated reading
/// could never land at all, since it does not rank above nothing.
function isNewer(candidate: string | undefined, stored: string | undefined): boolean {
  const a = candidate === undefined ? Number.NaN : Date.parse(candidate);
  const b = stored === undefined ? Number.NaN : Date.parse(stored);
  if (Number.isNaN(a)) return false;
  if (Number.isNaN(b)) return true;
  return a > b;
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
  // Nothing held yet takes the reading whatever its instant; ranking only decides
  // between two readings that both exist.
  if (stored !== undefined && !isNewer(reading.observed_at, stored.observed_at)) return;
  const carried =
    stored?.limit_reached === true && sameCodexUsageWindows(stored.payload, reading.payload);
  harnessUsage[harness] = carried ? { ...reading, limit_reached: true } : reading;
  persist();
}

/// Record that the harness refused a turn because a quota is exhausted.
///
/// Attaches to the reading already held rather than creating an entry: the
/// verdict is *about* a reading, and a refusal with no measurement to attach to
/// has no window to mark.
///
/// Usually there is one, because the reading that preceded the refusal is still
/// the newest window-bearing record on disk. **Not always**: an agent whose
/// rollout contains only the refused turn has no window-bearing record at all, so
/// no reading was ever emitted and the refusal is dropped. That shows no meter
/// rather than a wrong one, which is the direction this code takes throughout.
export function recordUsageRefusal(harness: HarnessKind): void {
  const stored = harnessUsage[harness];
  if (stored === undefined || stored.limit_reached === true) return;
  harnessUsage[harness] = { ...stored, limit_reached: true };
  persist();
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
  persist();
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
  persist();
}

/// Seed the store from the persisted file at startup.
///
/// Each restored entry is **ranked, not filled in**: it goes through the same
/// newest-wins rule as a live reading, so it supersedes what is in memory when it
/// is genuinely newer and loses when it is not. That is wanted in both
/// directions — a reading persisted yesterday should beat a rollout stamp from
/// two days ago, and a live reading that landed while this was in flight should
/// not be replaced by an older one from disk.
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
  if (v.payload === undefined) return null;
  // An **absent** instant is a shape we write ourselves, so the entry is kept and
  // ranks last. A **present but non-string** one is not: the value is emitted
  // unquoted, so its survival as a string depends on the YAML reader not
  // resolving plain scalars to a timestamp type, and if that ever changes the
  // entry should disappear loudly rather than quietly demote itself to unranked.
  if (v.observed_at !== undefined && typeof v.observed_at !== "string") return null;
  return {
    payload: v.payload,
    observed_at: typeof v.observed_at === "string" ? v.observed_at : undefined,
    model: typeof v.model === "string" ? v.model : undefined,
    limit_reached: v.limit_reached === true ? true : undefined,
  };
}

/// Whether a write is in flight, and whether the map changed while it was.
/// Module-level rather than passed around because the invariant they encode is
/// about the file, which is a single shared resource.
let writeInFlight: Promise<void> | null = null;
let mapChangedSinceWrite = false;

/// Request that the file be brought up to date with the map.
///
/// **At most one write is in flight, and it always carries current state.**
/// Writes used to be fired independently per change, which let two land out of
/// order and leave the file holding the older map — invisible until a restart,
/// and healed by the next turn, but a hazard that costs more to carry in the head
/// than the drain loop costs to read. Requests arriving during a write set a flag
/// instead of queueing, so a burst collapses to one further write rather than one
/// per change.
///
/// Whole-map every time, because the backend holds no rule of its own to merge
/// with. Failures are swallowed for the same reason a missing file is tolerated:
/// a lost write costs an empty usage section until the next turn reports a
/// reading, never a broken operation.
function persist(): void {
  mapChangedSinceWrite = true;
  if (writeInFlight !== null) return;
  writeInFlight = (async () => {
    while (mapChangedSinceWrite) {
      mapChangedSinceWrite = false;
      try {
        // Read inside the loop, never snapshotted at request time, so the write
        // carries the map as it stands now rather than as it stood when the
        // change that triggered this was made.
        await invoke("set_harness_usage", { usage: { harnesses: { ...harnessUsage } } });
      } catch (e) {
        console.warn("could not persist harness usage", e);
      }
    }
    writeInFlight = null;
  })();
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
