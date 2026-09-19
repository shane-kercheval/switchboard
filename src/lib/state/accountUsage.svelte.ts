/// Self-refreshing account quota reads.
///
/// **Why a harness gets its quota asked for rather than reported to it.** Codex
/// can be asked, over its app-server protocol, for every metered limit the
/// account holds — each one named, each stating its own exhaustion. The
/// alternative is what the per-turn stream gives: one unnamed bucket under
/// identical identifiers whichever limit it describes. Asking is the only way
/// to know which quota a number belongs to.
///
/// Nothing here interprets the payload; `usageWindows.ts` does that. This module
/// owns *when* the read runs and how concurrent requests collapse.
import { invoke } from "@tauri-apps/api/core";
import { observeUsage } from "$lib/state/harnessUsage.svelte";
import { codexAccountUsageView } from "$lib/usageWindows";
import type { HarnessKind } from "$lib/types";

/// The harness this read speaks for.
///
/// A constant rather than a parameter because the command speaks a protocol only
/// Codex has. A `harness` argument would build a dispatcher with one arm and a
/// signature promising a generality no second harness can supply; callers gate
/// on `supportsAccountUsageRead` instead, which is the honest form of the
/// question and the thing that keeps this from being decided by harness name.
const ACCOUNT_USAGE_HARNESS: HarnessKind = "codex";

/// Whether a read is running, and whether anything asked for one while it was.
/// Module-level because the invariant they encode is about a shared resource —
/// the single subprocess this must not launch N copies of.
let readInFlight: Promise<void> | null = null;
let requestedSinceRead = false;

/// Ask for a fresh account reading, coalescing with any read already running.
///
/// **Drain semantics, not dedupe, and the distinction is load-bearing.** A
/// request arriving mid-read sets a flag and causes exactly one *further* read
/// once the current one lands. Answering the later request from the in-flight
/// call instead — the obvious dedupe — would serve it a number measured before
/// the turn that triggered it, systematically understating usage at exactly the
/// moment it changed.
///
/// **Assume a read is usually already running.** The measured distribution on a
/// capped account is a ~2s median against a trigger that fires at every turn
/// end, so the in-flight case is the normal path rather than a burst. The
/// follow-up flag is set on most triggers, not rarely.
///
/// **Superseding an in-flight read by discarding it is not available**: this
/// awaits the call, and Tauri propagates no cancellation, so nothing on either
/// side drops the future. That is why the backend's timeout is load-bearing
/// rather than defensive — an unbounded read against a wedged server would hold
/// this single slot for the life of the session and permanently freeze the
/// Codex meter.
///
/// Fire-and-forget: never awaited on a turn's critical path, and every failure
/// is already collapsed to "no reading" by the backend.
export function requestAccountUsageRefresh(): void {
  requestedSinceRead = true;
  if (readInFlight !== null) return;
  readInFlight = (async () => {
    try {
      while (requestedSinceRead) {
        requestedSinceRead = false;
        // Caught per pass, not around the loop. A throw while handling one
        // response must not abandon a request that arrived during it, and
        // nothing awaits this promise in production, so an escaping rejection
        // would be invisible as well as fatal.
        try {
          await readOnce();
        } catch (e) {
          console.warn("codex account usage read failed", e);
        }
      }
    } finally {
      // Structural rather than contingent: the slot is released whatever
      // happens above. Holding it forever is the permanent-freeze failure this
      // module's bound exists to prevent, and a throw is a far cheaper way to
      // reach it than a wedged server.
      readInFlight = null;
    }
  })();
}

/// One read, offered to the store.
///
/// The reading is stamped with **arrival time**, which is what ranks it above
/// anything restored from disk. `None` from the backend means "no newer number"
/// — never an error to surface — so it leaves whatever is held in place.
async function readOnce(): Promise<void> {
  let usage: unknown;
  try {
    usage = await invoke("read_codex_account_usage");
  } catch (e) {
    // The command's `Err` arm is unreachable by construction; this catch exists
    // for the IPC layer itself failing, which costs one skipped refresh.
    console.warn("could not read codex account usage", e);
    return;
  }
  if (usage === null || usage === undefined) return;
  reportDiagnostics(usage);
  observeUsage(ACCOUNT_USAGE_HARNESS, {
    payload: usage,
    observed_at: new Date().toISOString(),
  });
}

/// Conditions last reported, so a standing one logs once.
let lastDiagnostics = "";

/// Log what the reading could not be read as, once per distinct condition.
///
/// **Here rather than in the reader**, which is pure and runs inside a
/// `$derived`: a warning there would re-fire on every recompute. Deduplicated
/// the way the backend's failure log is, because the conditions worth reporting
/// are steady states — a field the server stopped emitting stays absent on every
/// refresh — and an unsuppressed line would bury the transient ones.
///
/// **An empty reading is not itself a condition.** An account whose only quota
/// is a model reserve legitimately renders nothing, and so does one whose
/// windows have all cycled. Only a structural failure to read a bucket is
/// reported, which is what separates "nothing to show" from "we could not tell".
function reportDiagnostics(usage: unknown): void {
  // `nowMs` is passed but cannot matter: see the invariant on
  // `CodexAccountDiagnostic`. A test pins it.
  const { diagnostics } = codexAccountUsageView(usage, Date.now());
  const summary = diagnostics
    .map((d) => ("slot" in d ? `${d.kind}:${d.limitId}.${d.slot}` : `${d.kind}:${d.limitId}`))
    .sort()
    .join(",");
  if (summary === lastDiagnostics) return;
  const cleared = summary === "" && lastDiagnostics !== "";
  lastDiagnostics = summary;
  if (summary !== "") {
    console.warn(`codex account usage: unreadable response (${summary})`);
  } else if (cleared) {
    // The other half of the story, and otherwise invisible: without it the log
    // shows a condition that appears never to have ended. Matches the backend's
    // failure log, which logs its recovery for the same reason.
    console.info("codex account usage: response is readable again");
  }
}

/// Test-only. Named under `_testing` so no production caller can await a
/// background refresh and make it load-bearing on a critical path.
export const _testing = {
  /// Resolve once no read is running and none is pending.
  async settled(): Promise<void> {
    while (readInFlight !== null) {
      await readInFlight;
    }
  },
  reset(): void {
    readInFlight = null;
    requestedSinceRead = false;
    lastDiagnostics = "";
  },
};
