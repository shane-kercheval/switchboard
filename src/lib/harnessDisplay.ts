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

/// The canonical harness list + iteration order, derived from a type-checked
/// `Record<HarnessKind, …>` rather than hand-written. A new `HarnessKind`
/// variant can't be silently omitted: it must be added to `HARNESS_LABEL`
/// (a missing key is a type error), after which it appears here and in every
/// surface that iterates this list. **Always iterate this** instead of a literal
/// `["claude_code", …]` array — a bare array is type-legal while incomplete and
/// silently drops a harness from probes/banners/pickers. Insertion order
/// (claude → codex → gemini → antigravity) is **load-bearing**: it governs
/// auto-create sequencing (M4) and display order across banners, the picker, and
/// the status list. Reorder only if the backend's `HARNESSES` constant changes.
export const ALL_HARNESSES = Object.keys(HARNESS_LABEL) as HarnessKind[];

/// Brand/icon-derived accent colors for transcript attribution and compact
/// harness identity. Chosen from the actual icon artwork.
export const HARNESS_COLOR: Record<HarnessKind, string> = {
  claude_code: "#d97757",
  codex: "#3831ff",
  gemini: "#17b967",
  antigravity: "#3187fe",
};

/// Official setup/install docs for each harness CLI. The single source for
/// these URLs: the getting-started panel links to them, and
/// `harnessAvailability`'s binary-missing copy is built from them — so a
/// moved docs page is a one-line change here, not a hunt across the frontend.
export const HARNESS_SETUP_URL: Record<HarnessKind, string> = {
  claude_code: "https://code.claude.com/docs/en/quickstart",
  codex: "https://developers.openai.com/codex/cli",
  gemini: "https://geminicli.com/docs/get-started/installation/",
  antigravity: "https://antigravity.google/docs/cli-install",
};

/// How the user authenticates each harness, shown in the auth column of the
/// getting-started panel when a harness is installed but not signed in. These
/// are *hints* — the authoritative test is a successful send; auth is
/// otherwise discovered reactively.
export const HARNESS_LOGIN_HINT: Record<HarnessKind, string> = {
  claude_code: "run `claude auth login` to authenticate",
  codex: "run `codex login` to authenticate",
  gemini: "run `gemini` to authenticate",
  antigravity: "run `agy` to authenticate",
};

/// Default agent name for a harness — the pre-filled name in the create form
/// and the name each auto-created agent gets on a new project. A **direct**
/// slug map, deliberately not derived from a display label: these are
/// persisted, canonicalized identifiers, and `HARNESS_LABEL` is the short
/// display label (`"Claude"`) which would slug to the wrong name. All four are
/// distinct under the backend's name canonicalization, so a new project's
/// auto-created agents never self-collide.
export const HARNESS_DEFAULT_AGENT_NAME: Record<HarnessKind, string> = {
  claude_code: "claude-code",
  codex: "codex",
  gemini: "gemini",
  antigravity: "antigravity",
};
