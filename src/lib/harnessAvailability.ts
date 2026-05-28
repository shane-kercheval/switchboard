/// Pure helpers for rendering harness-availability state. Shared between
/// `App.svelte`'s banner stack and `CreateAgentForm`'s radio gating —
/// keeping the copy in one place enforces the "tooltip text matches
/// banner copy verbatim" design at the module level rather than relying
/// on developer discipline.
///
/// **Auth is out of scope here.** v1 surfaces auth failures reactively
/// (a logged-out harness is discovered when the user sends; the failed
/// turn carries the adapter-authored message). No proactive auth banner,
/// no picker gate. The only availability dimension the frontend tracks
/// is binary presence — a missing CLI is a real install problem that
/// must be surfaced before any send can succeed.

import type { HarnessAvailability, HarnessBanner } from "./types";

const BINARY_COPY: Record<HarnessAvailability["harness"], string> = {
  claude_code: "Claude Code not found on PATH. Install from https://claude.com/code",
  codex: "Codex not found on PATH. Install from https://github.com/openai/codex",
  gemini: "Gemini CLI not found on PATH. Install from https://github.com/google-gemini/gemini-cli",
  antigravity:
    "Antigravity CLI (agy) not found on PATH. Install from https://antigravity.google/download",
};

/// The user-facing string for a given banner.
export function bannerCopy(banner: HarnessBanner): string {
  return BINARY_COPY[banner.harness];
}

/// The inline message shown next to the harness picker when the selected
/// harness is unavailable for a real reason (binary missing). Returns
/// `null` for `"checking"` — we don't surface scary "Checking…" copy
/// during the brief probe window; the UI silently disables submission
/// via `isHarnessSelectable` instead.
///
/// **Decoupled from `isHarnessSelectable` on purpose**: "is the user
/// blocked?" and "what message do we show?" are different questions.
/// Conflating them (e.g., returning a non-null sentinel for checking)
/// would force the message-rendering site to filter out non-message
/// states.
export function harnessUnavailableReason(a: HarnessAvailability): string | null {
  return a.binary === "missing" ? BINARY_COPY[a.harness] : null;
}

/// Whether the radio for this harness should be enabled (and, when
/// selected, whether Submit is enabled). False for `"checking"` (closes
/// the pre-probe fail-open window) and `"missing"` (real install gap).
///
/// **Note vs `harnessUnavailableReason`**: this returns false for
/// `"checking"` (block the user) while the reason function returns
/// `null` for the same state (no inline message). The asymmetry is
/// intentional — see that function's docstring.
export function isHarnessSelectable(a: HarnessAvailability): boolean {
  return a.binary === "available";
}

/// Stable `data-testid` for a banner so component tests can find each
/// one independently when the stack renders multiple at once. Co-located
/// with the copy so testid + copy stay aligned in one place.
export function bannerTestid(banner: HarnessBanner): string {
  return `banner-${banner.kind}-${banner.harness}`;
}
