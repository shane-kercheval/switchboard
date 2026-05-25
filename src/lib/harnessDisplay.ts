/// Display-only lookup tables for harness rendering. Separate from
/// `harnessAvailability.ts` (which is probe-state copy and gate predicates)
/// because the concerns are distinct: this module answers "what does the
/// label / badge for harness X look like?" — pure presentation, no probe
/// state. The split keeps a future change to one (e.g., adding an icon to
/// every label) from forcing test updates in the other.
///
/// **No `default` arm.** Each map is typed `Record<HarnessKind, string>`,
/// which makes exhaustiveness compile-time enforced. A future harness
/// landing without a frontend update fails type-check at the map literal,
/// not at runtime with a gray "?". Mirrors the `#[non_exhaustive]`
/// discipline on the Rust side: adding a variant forces a deliberate
/// downstream update rather than silently degrading.

import type { HarnessKind } from "./types";

export const HARNESS_LABEL: Record<HarnessKind, string> = {
  claude_code: "Claude",
  codex: "Codex",
  gemini: "Gemini",
  antigravity: "Antigravity",
};

/// Token-driven harness badge classes (soft background + strong foreground),
/// consumed by the `Badge` primitive. Themes correctly in light and dark via
/// the semantic `harness-*` tokens.
export const HARNESS_BADGE_TOKEN: Record<HarnessKind, string> = {
  claude_code: "bg-harness-claude-soft text-harness-claude",
  codex: "bg-harness-codex-soft text-harness-codex",
  gemini: "bg-harness-gemini-soft text-harness-gemini",
  antigravity: "bg-harness-antigravity-soft text-harness-antigravity",
};
