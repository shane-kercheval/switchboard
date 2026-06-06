/// Single source of truth for harness install status (presence on PATH +
/// best-effort version), shared by the binary-missing banner stack, the
/// create-form gating, the Settings + blank-state "Supported CLIs" list, and
/// auto-create. Fetched once at startup and refreshed at natural moments
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
import type { BinaryState, HarnessAvailability, HarnessInstallStatus, HarnessKind } from "./types";

/// `null` = not yet probed. Derives to `"checking"` so gating fails closed
/// during the startup probe window (matches the prior per-harness `BinaryState`
/// that initialized to `"checking"`).
const status = $state<Record<HarnessKind, HarnessInstallStatus | null>>({
  claude_code: null,
  codex: null,
  gemini: null,
  antigravity: null,
});

/// Bumped by `_testing.reset()`. A refresh snapshots this at start and drops any
/// per-key write whose snapshot is stale, so a probe that resolves *after* a
/// reset can't repollute the freshly-cleared store. Without this, an un-awaited
/// startup probe from one test outlives it and leaks "missing" status into the
/// next test (a phantom banner → timeout → cascade). Production never resets, so
/// the epoch stays `0` and this guard is inert there.
let epoch = 0;

function deriveBinary(s: HarnessInstallStatus | null): BinaryState {
  if (s === null) return "checking";
  return s.installed ? "available" : "missing";
}

export const harnessAvailability = {
  /// Raw install status (presence + version), or `null` while unprobed.
  /// Read by the "Supported CLIs" list (which also wants the version).
  status(harness: HarnessKind): HarnessInstallStatus | null {
    return status[harness];
  },
  /// The gating/banner view: `{ harness, binary }`, where `binary` is
  /// `checking` until the first probe resolves.
  availability(harness: HarnessKind): HarnessAvailability {
    return { harness, binary: deriveBinary(status[harness]) };
  },
  /// Harnesses known to be installed — what auto-create iterates to seed one
  /// agent per installed harness. Unprobed (`null`) harnesses are excluded, so
  /// this returns `[]` until the first probe resolves. A caller that needs a
  /// definitive answer (e.g. auto-create at project-creation time) must
  /// `await refreshHarnessAvailability()` first rather than relying on the
  /// un-awaited startup probe having completed — otherwise it can race the
  /// startup window and silently see nothing installed.
  installed(): HarnessKind[] {
    return ALL_HARNESSES.filter((harness) => status[harness]?.installed === true);
  },
};

/// Probe all harnesses and update the store. Per-harness failures degrade to
/// "not installed" (same as the status list's own catch) so one failing probe
/// can't leave a harness stuck in `checking` or reject the whole refresh; the
/// swallowed error is logged so an unexpected backend throw stays diagnosable
/// (vs. a genuinely-absent CLI, which looks identical in the UI).
///
/// Concurrent callers are safe *because the probe is idempotent* — each write
/// of a given key produces the same value, so last-writer-per-key is fine and
/// no locking is needed. This holds only while the probe stays side-effect-free
/// and stable; if it ever becomes non-idempotent, revisit.
export async function refreshHarnessAvailability(): Promise<void> {
  const started = epoch;
  await Promise.all(
    ALL_HARNESSES.map(async (harness) => {
      try {
        const result = await api.getHarnessInstallStatus(harness);
        if (started === epoch) status[harness] = result;
      } catch (err) {
        console.warn(`[switchboard] harness install probe failed for ${harness}:`, err);
        if (started === epoch) status[harness] = { installed: false, version: null };
      }
    }),
  );
}

/// Test-only reset so suites don't leak probed state across cases.
export const _testing = {
  reset(): void {
    epoch += 1;
    for (const harness of ALL_HARNESSES) status[harness] = null;
  },
};
