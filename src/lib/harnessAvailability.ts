/// Pure helpers for rendering harness-availability state. Shared between
/// `App.svelte`'s banner stack and `CreateAgentForm`'s radio gating —
/// keeping the copy in one place enforces the "tooltip text matches
/// banner copy verbatim" design at the module level rather than relying
/// on developer discipline.
///
/// **Scope discipline**: these are pure functions mapping typed inputs
/// to user-facing strings / booleans. The `banners: HarnessBanner[]`
/// derivation logic lives in `App.svelte` (with the suppression rule);
/// don't grow this module to own that — the consumer keeps the
/// orchestration, this module owns only the text and the gate predicate.

import type { HarnessAvailability, HarnessBanner } from "./types";

const BINARY_COPY: Record<HarnessAvailability["harness"], string> = {
  claude_code: "Claude Code not found on PATH. Install from https://claude.com/code",
  codex: "Codex not found on PATH. Install from https://github.com/openai/codex",
  gemini: "Gemini CLI not found on PATH. Install from https://github.com/google-gemini/gemini-cli",
};

const AUTH_COPY: Record<"codex" | "gemini", string> = {
  codex:
    "Codex not authenticated — run `codex login` and reload Switchboard. (API-key-only auth is not supported.)",
  gemini:
    "Gemini not authenticated — run `gemini` interactively to sign in, then reload Switchboard.",
};

/// The user-facing string for a given banner. The `auth_missing` variant
/// is type-narrowed to the auth-detectable harnesses (Codex and Gemini)
/// via `HarnessBanner`; Claude's banner is `binary_missing` only.
export function bannerCopy(banner: HarnessBanner): string {
  if (banner.kind === "binary_missing") {
    return BINARY_COPY[banner.harness];
  }
  return AUTH_COPY[banner.harness];
}

/// The inline message shown next to the harness picker when the selected
/// harness is unavailable for a *real* reason (binary missing or auth
/// missing). Returns `null` for `"checking"` — we don't surface scary
/// "Checking…" copy during the brief probe window; the UI silently
/// disables submission via `isHarnessSelectable` instead.
///
/// **Decoupled from `isHarnessSelectable` on purpose**: "is the user
/// blocked?" and "what message do we show?" are different questions.
/// Conflating them (e.g., returning a non-null sentinel for checking)
/// would force the message-rendering site to filter out non-message
/// states.
export function harnessUnavailableReason(a: HarnessAvailability): string | null {
  if (a.binary === "missing") {
    return BINARY_COPY[a.harness];
  }
  if (a.auth === "missing" && (a.harness === "codex" || a.harness === "gemini")) {
    return AUTH_COPY[a.harness];
  }
  // `auth === "missing"` for a harness without file-detectable auth is
  // structurally unreachable today (Claude's variant has
  // `auth: "unsupported"`). The explicit harness guard above is
  // belt-and-suspenders symmetric with `bannerCopy`, so a future Claude
  // auth probe widening Claude's auth field cannot silently render
  // wrong copy on a Claude row.
  return null;
}

/// Whether the radio for this harness should be enabled (and, when
/// selected, whether Submit is enabled). False for `"checking"`,
/// binary-missing, OR auth-missing. The `"checking"` arm is what
/// closes the pre-probe fail-open window: until probes complete, the
/// form doesn't accept submissions for that harness.
///
/// **Note vs `harnessUnavailableReason`**: this returns false for
/// `"checking"` (block the user) while the reason function returns
/// `null` for the same state (no inline message). The asymmetry is
/// intentional — see that function's docstring.
export function isHarnessSelectable(a: HarnessAvailability): boolean {
  if (a.binary !== "available") return false;
  if (a.auth === "missing" || a.auth === "checking") return false;
  return true;
}

/// Stable `data-testid` for a banner so component tests can find each
/// one independently when the stack renders multiple at once. Co-located
/// with the copy so testid + copy stay aligned in one place.
export function bannerTestid(banner: HarnessBanner): string {
  return `banner-${banner.kind}-${banner.harness}`;
}
