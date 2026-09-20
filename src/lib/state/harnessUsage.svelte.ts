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
import { claudeStoredWindows, windowResetsAt, type StoredUsageWindow } from "$lib/usageWindows";
import type { HarnessKind, TurnId } from "$lib/types";

/// One harness account's newest reading.
export type HarnessUsageReading = {
  /// The harness's own rate-limit payload, opaque here exactly as it is opaque
  /// through the adapter, the dispatcher, and IPC. Only `usageWindows.ts`
  /// interprets it.
  ///
  /// **What it is *not* the source of, for a harness whose readings are partial:**
  /// a Claude payload's `unifiedWindows` is only the delivering reading's partial
  /// snapshot. Meter values, labels, and tones come from `windows` below; reading
  /// the raw container for them reinstates the defect that made per-window
  /// retention necessary. The container is still live for two narrower jobs — it
  /// decides whether the bare fallback line may render, and it is what recovers
  /// windows from a file written before they were held individually.
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
  /// The windows this account holds, keyed by the harness's own window key, each
  /// with the context of the reading that delivered *it*.
  ///
  /// **Present only for a harness whose readings are partial**, and that is what
  /// selects the merge rule below — the policy follows the data rather than a
  /// harness name. A reading that names every limit it has (Codex's account read)
  /// carries no map and replaces wholesale; one that names only the windows a
  /// turn's model touched (Claude's `rate_limit_event`) carries a map and merges
  /// into what is held. `reportsPartialUsageWindows` records why.
  windows?: Record<string, StoredUsageWindow>;
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

/// Merge windows the reading names, and take its account-level fields if it is
/// the newest one.
///
/// **Two scopes, two rules, because a reading can be authoritative about one and
/// silent about the other.** `payload` holds account-level state — Claude's
/// overage escalation, the no-window-map fallback — and only the newest reading
/// speaks for that. `windows` are merged per key: a reading that does not mention
/// a window is not evidence the window is gone, so the held one survives with its
/// own value, its own instant, and its own flags. Replacing wholesale is what let
/// a turn on one model delete a still-blocking cap on another.
///
/// An older reading can therefore still contribute a window the store has never
/// seen while losing the account-level fields — which is the point, since that is
/// exactly a restored reading from the agent that ran the gated model.
///
/// **Nothing is carried across readings.** A reading used to drag a separately
/// recorded refusal verdict forward whenever it described the same windows,
/// because Codex's per-turn payload could not say which quota had refused. Both
/// harnesses now state exhaustion inside the payload, and a retained window keeps
/// the judgement made about *it* rather than inheriting one made about another.
export function observeUsage(harness: HarnessKind, reading: HarnessUsageReading): void {
  const stored = harnessUsage[harness];
  // Nothing held yet takes the reading whatever its instant; ranking only decides
  // between two readings that both exist.
  if (stored === undefined) {
    harnessUsage[harness] = reading;
    persist();
    return;
  }
  const windows = mergeWindows(stored.windows, reading.windows);
  if (isNewer(reading.observed_at, stored.observed_at)) {
    harnessUsage[harness] = { ...reading, windows };
  } else if (windows !== stored.windows) {
    harnessUsage[harness] = { ...stored, windows };
  } else {
    return;
  }
  persist();
}

/// Whether `candidate` describes a later state of a window than `held` does.
///
/// **Two tiers, and the reading level has only the second one.** A reading can be
/// ordered solely by when we heard it; a *window* additionally carries the vendor's
/// own reset, which states which generation of the window it is rather than when we
/// learned of it. Generation is the stronger signal, so it decides first and the
/// instant decides only when both resets say the same thing or cannot be read.
///
/// **Directional, which is the whole point.** An earlier reset is positive evidence
/// that the arriving window is the *superseded* instance, so it is skipped rather
/// than falling through to instant ranking — otherwise a dated-but-stale reading
/// beats an undated current one under the absent-ranks-last rule, which is right in
/// general and wrong when the reset proves which instance is older. A symmetric
/// "any different reset wins" test let a project's hours-old snapshot overwrite a
/// live window and, once its elapsed reset dropped it at render, take the whole
/// Claude row off the card.
///
/// Both directions are needed. Utilization only ever climbs *within* a window, so a
/// retained value understates and retention is safe; across a reissue that
/// invariant does not hold, which is why a later reset must land even from a
/// reading that would lose on its instant.
///
/// **The premise, recorded because nothing in the payload enforces it: for a given
/// key, a larger reset means a later generation.** A vendor moving a reset
/// *backward* — a corrected allowance — is therefore not handled: that reading is
/// read as the older instance and skipped, so the key holds its value until the
/// stale reset elapses and the window drops, up to its own duration. Unobserved,
/// and indistinguishable from an older instance using what the payload gives us, so
/// it is a stated limitation rather than a case to detect.
function supersedesWindow(candidate: StoredUsageWindow, held: StoredUsageWindow): boolean {
  const a = windowResetsAt(candidate);
  const b = windowResetsAt(held);
  if (a !== null && b !== null && a !== b) return a > b;
  return isNewer(candidate.observed_at, held.observed_at);
}

/// Whether a window `supersedesWindow` just rejected was nonetheless the newer
/// measurement of the two.
///
/// **That combination is impossible under the premise the ordering rests on, which
/// is the whole reason it is worth reporting.** A window's reset is supposed to
/// advance across generations, so a newer measurement cannot carry an earlier
/// reset. If one ever does, the premise is false — and this is the only place that
/// can notice, because the claim compares two readings at least a window's duration
/// apart and no test, live or otherwise, spans that. Only the running app does.
///
/// **Call-site predicate: it does not re-test the reset direction, because nothing
/// could reach that test.** Arriving here means the candidate lost, and a candidate
/// with the *later* reset never loses; an equal or unreadable reset defers to the
/// very instant comparison below, which then cannot have favoured the candidate. So
/// a true answer already implies both resets were readable and the candidate's was
/// the earlier one, which is what lets the caller report them.
///
/// The ordinary rejection is silent here: a project whose saved reading predates a
/// window rolling over is older on *both* axes, which is the common case and no
/// contradiction at all.
///
/// **Both instants must parse.** `isNewer` ranks an absent instant last by
/// convention rather than by evidence, so a dated candidate against an undated held
/// window is not a disagreement and must not be reported as one.
function wasNewerMeasurement(candidate: StoredUsageWindow, held: StoredUsageWindow): boolean {
  const measured = Date.parse(candidate.observed_at ?? "");
  const heldMeasured = Date.parse(held.observed_at ?? "");
  return !Number.isNaN(measured) && !Number.isNaN(heldMeasured) && measured > heldMeasured;
}

/// Window keys under a standing contradiction, so one logs once rather than on
/// every reading until the stale reset elapses.
///
/// A plain object rather than a `Set`, which the lint rule would want reactive:
/// nothing renders from this and nothing should re-derive when it changes. It is log
/// bookkeeping, the same role `lastDiagnostics` plays for the account read.
const resetRegressions: Record<string, true> = {};

/// Fold the windows a reading names into the held set, per key.
///
/// Returns the held map by identity when nothing changed, so the caller can skip
/// a write and a reactive update.
function mergeWindows(
  held: Record<string, StoredUsageWindow> | undefined,
  incoming: Record<string, StoredUsageWindow> | undefined,
): Record<string, StoredUsageWindow> | undefined {
  if (incoming === undefined) return held;
  const merged = { ...held };
  let changed = false;
  for (const [key, window] of Object.entries(incoming)) {
    const h = merged[key];
    if (h !== undefined && !supersedesWindow(window, h)) {
      if (wasNewerMeasurement(window, h) && resetRegressions[key] === undefined) {
        resetRegressions[key] = true;
        // Carries both resets and both instants because the point of the line is to
        // describe a shape we have never seen. No recovery line is logged the way
        // the account read's diagnostics do: there the useful fact is that a
        // condition ended, here it is that it happened at all.
        console.warn(
          `harness usage: ${key} reset moved backward — a reading measured at ` +
            `${window.observed_at} states reset ${windowResetsAt(window)} while one ` +
            `measured at ${h.observed_at} states ${windowResetsAt(h)}. The window is ` +
            `held at the later reset; ordering assumes a reset only advances.`,
        );
      }
      continue;
    }
    // The condition ends when this key next takes a window, which is the only
    // signal available — the store never prunes, so a held window with a stale
    // reset would otherwise keep the mark for the life of the session.
    delete resetRegressions[key];
    merged[key] = window;
    changed = true;
  }
  return changed ? merged : held;
}

/// Name the windows turn `turnId` contributed, once that turn's model is known.
///
/// Claude's model-gated weekly window carries no model of its own, so the label
/// comes from the turn that delivered it — and that turn's `init` can arrive
/// *after* its rate-limit event.
///
/// **A narrow fallback, not the normal route.** In the refusal ordering actually
/// observed, the gated window arrives in a *second* rate-limit event that follows
/// `session_meta`, so it is labelled at ingest and never reaches here. This exists
/// for the inverted order recorded on a compaction stream.
///
/// **Fills only blanks, and only ones this turn contributed.** Both halves are
/// load-bearing, and the turn rather than the agent is the grain that works.
/// Without the blank check a later reading would rewrite history. With only an
/// agent check, a turn that died before reporting its model leaves a blank that
/// the same agent's *next* turn fills with a different model — which
/// `runtimeReducer` already refuses one layer down by clearing the per-turn model
/// at every turn start.
///
/// **An unknown turn grants nothing, and one check covers both sides.** Rejecting
/// an absent `turnId` up front is what makes the strict comparison below sufficient
/// to reject a window carrying no turn: with the caller's turn known to be a real
/// id, an unattributed window can never match it. Checking both separately reads as
/// belt-and-braces but leaves two expressions where either alone is load-bearing,
/// so neither can be pinned by a test. What must not happen is absent matching
/// absent, which would make every unlabelled window eligible to any fill — worse
/// than the race being fixed.
///
/// A window whose turn is unknown — restored from disk, where a later turn's model
/// would be a guess about an older measurement — is therefore never filled. It
/// renders unlabelled rather than wrong, which is the rule this surface follows
/// throughout.
export function nameUsageModel(
  harness: HarnessKind,
  turnId: TurnId | undefined,
  model: string | undefined,
): void {
  if (model === undefined || model === "") return;
  if (turnId === undefined) return;
  const stored = harnessUsage[harness];
  if (stored?.windows === undefined) return;
  let windows: Record<string, StoredUsageWindow> | undefined;
  for (const [key, window] of Object.entries(stored.windows)) {
    if (window.model !== undefined) continue;
    if (window.turn_id !== turnId) continue;
    windows = { ...(windows ?? stored.windows), [key]: { ...window, model } };
  }
  if (windows === undefined) return;
  harnessUsage[harness] = { ...stored, windows };
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
/// **An entry written before the Codex account read is kept, not migrated.**
/// It is tolerated because the two payload shapes cannot be confused: the old
/// one is the rollout's `rate_limits` object and structurally cannot carry
/// `rateLimitsByLimitId`, so the account view reads it as nothing and the
/// section renders empty until a fresh read supersedes it on arrival-time
/// ranking. That is normally the startup refresh, but it depends on the read
/// succeeding — offline, or with no Codex installed, the stale entry simply
/// keeps rendering nothing. No version check or shape sniff is needed anywhere.
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
  const windows = v.windows === undefined ? legacyWindows(v) : asStoredWindows(v.windows);
  if (windows === null) return null;
  return {
    payload: v.payload,
    observed_at: typeof v.observed_at === "string" ? v.observed_at : undefined,
    windows,
  };
}

/// Read a persisted window map, or `null` to reject the whole entry.
///
/// **One severity, and it is loud.** The file is machine-written, so a map that is
/// not the shape we write is not evidence of a window worth salvaging; the entry
/// is discarded and the next turn rebuilds it. Repairing it field by field would
/// quietly demote a window to unlabelled or unranked, which is the failure mode
/// the reading-level instant check already refuses for the same reason.
///
/// Unknown fields are ignored rather than rejected, so a later version may add one
/// without this dropping every entry an older build wrote — but they are also not
/// preserved, matching the reading-level whitelist.
function asStoredWindows(value: unknown): Record<string, StoredUsageWindow> | undefined | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const windows: Record<string, StoredUsageWindow> = {};
  for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
    if (typeof entry !== "object" || entry === null) return null;
    const e = entry as Record<string, unknown>;
    if (e.window === undefined) return null;
    const observedAt = e.observed_at;
    const model = e.model;
    if (observedAt !== undefined && typeof observedAt !== "string") return null;
    if (model !== undefined && typeof model !== "string") return null;
    windows[key] = {
      window: e.window,
      status: e.status,
      rate_limit_type: e.rate_limit_type,
      surpassed_threshold: e.surpassed_threshold,
      is_using_overage: e.is_using_overage,
      observed_at: observedAt,
      model,
      // **`turn_id` is written to the file and deliberately not read back**, which
      // is the one asymmetry here. It authorizes the late label fill, a repair
      // inside one live turn; a restored one could only ever authorize a stale fill
      // in a later session, naming a window from a turn that had nothing to do with
      // measuring it. Dropped here rather than filtered at write time, because
      // `persist` writing the map verbatim is its own invariant and turning it into
      // a projection would put a second rule in the one place that must stay dumb.
      // Turn ids are unique, so a match would already be vanishingly unlikely —
      // "unlikely" is not the standard the rest of this file holds.
      //
      // **The omission is the safeguard, and `does not restore permission to label a
      // window` is what keeps it here.** Every other field on a stored window is
      // read back, so this line's absence is the only thing standing between a
      // restored file and a stale fill; completing the list for symmetry would
      // reopen the mislabel silently. That test is the reason it cannot.
    };
  }
  return Object.keys(windows).length > 0 ? windows : undefined;
}

/// Recover windows from an entry written before they were held individually.
///
/// Lifted from the stored payload rather than migrated by a version check, which
/// works because the extraction is shape-driven: a Claude payload yields its
/// windows and any other yields none, so the same call serves every harness.
///
/// **Deliberately carries no instant and no contributing agent.** The
/// reading-level timestamp dated the reading, not each window, and using it here
/// would date one window by when another was measured; an absent contributor is
/// what stops a later turn's `init` labelling a measurement it knows nothing
/// about. Only the age line and a missing model label are unavailable — the window
/// still renders and still retires, because both read the payload's own reset.
function legacyWindows(v: Record<string, unknown>): Record<string, StoredUsageWindow> | undefined {
  return claudeStoredWindows(v.payload, {
    model: typeof v.model === "string" ? v.model : undefined,
  });
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
    for (const key of Object.keys(resetRegressions)) {
      delete resetRegressions[key];
    }
  },
};
