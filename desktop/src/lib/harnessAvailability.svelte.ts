/// Single source of truth for harness install status (presence on PATH +
/// best-effort version), shared by create-form gating, the Settings +
/// blank-state "Supported CLIs" list, and auto-create. Fetched once at startup
/// and refreshed at natural moments
/// (settings/blank-state mount, window re-focus) so every surface agrees by
/// construction instead of each probing the backend independently.
///
/// **Install only — never auth.** v1 keeps auth reactive (a logged-out harness
/// is discovered on send and surfaced as a transcript turn), so auth stays a
/// per-component probe. Folding it in here would conflate two axes with
/// different lifecycles: install is a cheap, cacheable, globally-shared PATH
/// probe; auth is a per-harness login hint. The pure rendering helpers
/// (`isHarnessSelectable`, `harnessUnavailableReason`, banner copy) live in the
/// sibling `harnessAvailability.ts`.

import * as api from "./api";
import { ALL_HARNESSES } from "./harnessDisplay";
import type {
  BinaryState,
  HarnessAvailability,
  HarnessInstallStatus,
  HarnessKind,
  HarnessProbeError,
  PathSource,
  RecheckOutcome,
} from "./types";

/// A per-harness slot: an answer, a probe that couldn't run, or not yet asked.
type Slot = HarnessInstallStatus | HarnessProbeError | null;

/// `null` = not yet probed. Derives to `"checking"` so gating fails closed
/// during the startup probe window (matches the prior per-harness `BinaryState`
/// that initialized to `"checking"`).
const status = $state<Record<HarnessKind, Slot>>({
  claude_code: null,
  codex: null,
  antigravity: null,
});

/// Bumped whenever previously-issued probes must stop being authoritative: a
/// recheck (which deliberately changes what a probe returns) and `_testing.reset()`.
/// A refresh snapshots this at start and drops any per-key write whose snapshot
/// is stale.
///
/// This used to be test-only, on the documented assumption that probes were
/// idempotent. Recheck broke that assumption — an ordinary refresh started
/// before it can carry results from the *old* PATH and, resolving last, revert a
/// successful recheck to "Not installed".
let generation = 0;

/// Bumped only by `_testing.reset()`. A superseded refresh retries (see
/// `refreshHarnessAvailability`), but a refresh superseded by *teardown* must
/// stay dead — retrying would re-probe into the next test's mocks, recreating
/// the cross-test pollution `reset` exists to prevent. Never changes in
/// production.
let teardownEpoch = 0;

function deriveBinary(s: Slot): BinaryState {
  if (s === null) return "checking";
  // Carried through rather than flattened to `missing`: every gating surface
  // needs to fail closed (which it does — `isHarnessSelectable` only accepts
  // `available`), but none of them should *explain* a failed check as an absent
  // CLI. Flattening here is what let the create-agent form tell users to install
  // something they already had.
  if (s === "probe_failed") return "probe_failed";
  // A result taken while the PATH is still being captured is provisional, so it
  // reads as "checking" rather than a confident "not installed". A *positive*
  // provisional result is kept: finding the CLI on the interim PATH is proof it
  // exists, and the completion event will refresh either way.
  if (s.path_source === "capturing" && !s.installed) return "checking";
  return s.installed ? "available" : "missing";
}

/// True when detection is running against a PATH we couldn't read from the
/// user's shell. Results are real but may miss a CLI installed somewhere
/// unusual, so the UI says so rather than asserting "not installed".
///
/// Deliberately ignores probe failures: those say nothing about the PATH, and
/// letting one claim `fallback` would blame the wrong subsystem in the very
/// message added to make this diagnosable.
export function isPathDegraded(): boolean {
  return ALL_HARNESSES.some((harness) => harnessStatus(harness)?.path_source === "fallback");
}

/// The install answer for a harness, or `null` when there isn't one (unprobed,
/// or the probe itself failed).
function harnessStatus(harness: HarnessKind): HarnessInstallStatus | null {
  const slot = status[harness];
  return slot === null || slot === "probe_failed" ? null : slot;
}

export const harnessAvailability = {
  /// Raw install status (presence + version), or `null` while unprobed.
  /// Read by the "Supported CLIs" list (which also wants the version).
  status(harness: HarnessKind): HarnessInstallStatus | null {
    return harnessStatus(harness);
  },
  /// The gating/banner view: `{ harness, binary }`, where `binary` is
  /// `checking` until the first probe resolves.
  availability(harness: HarnessKind): HarnessAvailability {
    return { harness, binary: deriveBinary(status[harness]) };
  },
  /// Harnesses known to be installed — what auto-create iterates to seed one
  /// agent per installed harness. Anything unprobed, failed, or answered from a
  /// not-yet-resolved PATH is excluded, so this returns `[]` until real answers
  /// land. A caller that needs a *definitive* answer must
  /// `await settledHarnessAvailability()` — awaiting an ordinary refresh is not
  /// enough, because the PATH those probes search is itself resolved
  /// asynchronously and an interim one can miss a CLI entirely.
  installed(): HarnessKind[] {
    return ALL_HARNESSES.filter((harness) => harnessStatus(harness)?.installed === true);
  },
};

/// Probe all harnesses and update the store. A per-harness failure is recorded
/// as `probe_failed` — distinct from "absent" — so one failing probe can't
/// reject the whole refresh, leave a harness stuck in `checking`, or send the
/// user to install something they already have.
///
/// Concurrent ordinary refreshes are safe: each write of a given key produces
/// the same value, so last-writer-per-key is fine. The probe is *not*
/// unconditionally idempotent, though — a recheck changes the PATH underneath it
/// — which is what the `generation` guard exists for.
///
/// **Retries when superseded, so awaiting this means the store was written.**
/// Callers that read the store right after awaiting (auto-create's `installed()`
/// pass, recheck's outcome reporting) would otherwise read stale slots in a
/// realistic ordering: the backend settling its PATH produces both an invoke
/// reply and a `harness_path_resolved` event, and when the reply is processed
/// first, the event handler's generation bump lands mid-refresh and drops every
/// write of the very refresh the caller is waiting on. The retry re-snapshots
/// and re-probes against the new generation; bounded because each supersession
/// requires another bump (a capture settling or a user recheck), which cannot
/// happen often enough back-to-back to matter beyond a few attempts.
export async function refreshHarnessAvailability(): Promise<void> {
  const startedEpoch = teardownEpoch;
  for (let attempt = 0; attempt < 3 && startedEpoch === teardownEpoch; attempt += 1) {
    // Awaited, not merely initiated: `listen` is an async IPC round-trip, so
    // calling it does not mean the backend has a subscriber. A capture
    // publishing in that window would lose its event and leave the list
    // provisional. With registration complete first, a publish either lands
    // after it (event delivered) or before the probe (the probe reads the
    // settled PATH and no event is needed).
    // Snapshotted before the await, not after: `started` must record when this
    // attempt was *requested*, so an invalidation landing while we wait on
    // listener registration still supersedes it. Reading it afterwards would
    // let a superseded attempt adopt the new generation and publish anyway.
    const started = generation;
    await ensurePathListener();
    await Promise.all(
      ALL_HARNESSES.map(async (harness) => {
        try {
          const result = await api.getHarnessInstallStatus(harness);
          if (started === generation) status[harness] = result;
        } catch (err) {
          console.warn(`[switchboard] harness install probe failed for ${harness}:`, err);
          // "The check failed", not "the CLI is absent" — and emphatically not
          // a claim about the PATH.
          if (started === generation) status[harness] = "probe_failed";
        }
      }),
    );
    if (started === generation) {
      syncProvisionalBackstop();
      return;
    }
  }
}

/// Force a full re-detection: discard the backend's cached PATH, wait for it to
/// be re-read from the login shell, then re-probe. Returns where the new PATH
/// came from so the caller can tell the user when the re-read didn't work.
///
/// The ordinary refresh can't recover from a failed capture on its own — it
/// probes against the same cached PATH and gets the same answer — so the
/// user-triggered Refresh button needs this stronger form. It also *waits*: the user
/// is watching a spinner, and reporting a provisional answer as final is the one
/// thing this must not do.
///
/// Bumping the generation first supersedes any refresh already in flight, which
/// would otherwise be carrying results from the PATH we just discarded. Statuses
/// are cleared in the same step so the list reads "Checking…" for the duration
/// rather than showing a stale "Not installed" under a "Refreshing…" button.
export async function recheckHarnessAvailability(): Promise<RecheckOutcome> {
  generation += 1;
  for (const harness of ALL_HARNESSES) status[harness] = null;
  let outcome: RecheckOutcome;
  try {
    outcome = { kind: "path", source: await api.recheckHarnessInstalls() };
  } catch (err) {
    // Not fatal: fall through to an ordinary refresh so the user still gets a
    // fresh answer, just without the forced re-read. Reported as `failed`
    // rather than defaulting to a `PathSource` — guessing "fallback" here would
    // tell the user their shell is the problem when the re-read never ran.
    console.warn("[switchboard] harness PATH re-read failed:", err);
    outcome = { kind: "failed" };
  }
  await refreshHarnessAvailability();
  return outcome;
}

/// Await a definitive PATH, then probe. For callers that get one shot and can't
/// retroactively fix a wrong answer — auto-create runs once per project
/// creation, and the completion event refreshes the store without going back to
/// create the agents it skipped.
export async function settledHarnessAvailability(): Promise<PathSource> {
  let source: PathSource = "capturing";
  try {
    source = await api.awaitHarnessPath();
  } catch (err) {
    // Degrade to an ordinary refresh: a provisional answer beats none.
    console.warn("[switchboard] waiting for the harness PATH failed:", err);
  }
  await refreshHarnessAvailability();
  return source;
}

/// Attach the PATH-resolved listener, once, and resolve when registration has
/// actually completed.
///
/// Owned by the store rather than by a component because the guarantee has to
/// hold for *every* probe path — the status list mounts and probes on its own,
/// so a listener wired only in `App`'s mount would still race it. A capture that
/// publishes before the listener attaches loses its event entirely, leaving the
/// list provisional until something else re-probes.
///
/// The handler bumps the generation first: a publish is exactly the moment
/// previously-issued probes stop being authoritative, and without it a slow
/// probe from before the publish can resolve last and overwrite the correction.
let pathListener: Promise<() => void> | null = null;

function ensurePathListener(): Promise<() => void> {
  pathListener ??= api
    .listenHarnessPathResolved(() => {
      generation += 1;
      void refreshHarnessAvailability();
    })
    .catch((err: unknown) => {
      // Cleared rather than cached, so the next refresh retries. A failed
      // `listen` most likely means the IPC bridge is down — in which case the
      // probes are failing too — but that is a likelihood, not a contract, and
      // caching the failure would leave every later capture provisional until
      // the backstop for the rest of the session even after the bridge recovers.
      // No backoff ladder: refreshes are already rate-limited by their triggers
      // (mount, window focus, the completion event, the backstop).
      console.warn("[switchboard] harness PATH listener failed to attach:", err);
      pathListener = null;
      return () => {};
    });
  return pathListener;
}

/// Longest we leave a harness reading "Checking…" on the strength of a
/// provisional result before re-asking the backend. The completion event is the
/// normal path; this is the backstop for an event that never arrives (a listener
/// that failed to attach, a publish that raced registration). Without it a lost
/// event parks the list on a spinner indefinitely, which is a worse failure than
/// the one the event was added to fix.
const PROVISIONAL_RESULT_BACKSTOP_MS = 30_000;

/// Arm the backstop while any harness is provisional; cancel it once none are.
///
/// Driven by store state after every refresh rather than scheduled once at app
/// startup: a one-shot timer only covers provisional results produced during
/// launch, leaving any later ones (after a Recheck, or a listener that never
/// attached) with no correction path at all.
///
/// Re-probes rather than reinterpreting the snapshot — a slow-but-healthy
/// capture must not be relabelled as a final fallback answer without asking.
let backstopTimer: ReturnType<typeof setTimeout> | null = null;

function syncProvisionalBackstop(): void {
  const provisional = ALL_HARNESSES.some(
    (harness) => harnessStatus(harness)?.path_source === "capturing",
  );
  if (!provisional) {
    clearProvisionalBackstop();
    return;
  }
  if (backstopTimer !== null) return;
  backstopTimer = setTimeout(() => {
    backstopTimer = null;
    console.warn("[switchboard] harness PATH still unresolved; re-probing");
    void refreshHarnessAvailability();
  }, PROVISIONAL_RESULT_BACKSTOP_MS);
}

function clearProvisionalBackstop(): void {
  if (backstopTimer !== null) {
    clearTimeout(backstopTimer);
    backstopTimer = null;
  }
}

/// Test-only reset so suites don't leak probed state across cases.
export const _testing = {
  reset(): void {
    generation += 1;
    teardownEpoch += 1;
    for (const harness of ALL_HARNESSES) status[harness] = null;
    void pathListener?.then((unlisten) => unlisten());
    pathListener = null;
    // Otherwise every suite that probes leaves a live 30s handle behind.
    clearProvisionalBackstop();
  },
};
