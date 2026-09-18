import type { HarnessKind } from "$lib/types";

/// Frontend mirrors of the per-harness capability predicates on
/// `HarnessKind` in `crates/core/src/harness.rs`.
///
/// **Mirrors, not the authority.** The backend refuses independently; these
/// exist so the UI never offers an action that would be refused. When a
/// predicate changes there, change it here — a stale mirror shows a menu item
/// that always errors, or hides one that would work.

/// Whether Switchboard can drive this harness's **manual context compaction**.
///
/// Claude Code only. Codex can compact, but through an app-server protocol
/// Switchboard does not speak; Antigravity has no compaction at all. Sending
/// those harnesses a `/compact` *prompt* is not a substitute and is worse than
/// refusing — the model answers with a claim that it compacted while nothing
/// did, so the user believes their context shrank when it did not.
export function supportsManualCompaction(harness: HarnessKind | undefined): boolean {
  return harness === "claude_code";
}

/// Whether Switchboard can ask this harness for a **context breakdown** — what
/// is occupying the agent's context window.
///
/// Claude Code only, and for the same reason as compaction rather than a
/// coincidentally identical one: Claude's CLI intercepts `/context` locally and
/// answers with a structured measurement, while the other two have no such
/// interception, so the command reaches the model as an ordinary message and is
/// answered with an invented breakdown. Every figure in that answer looks like a
/// measurement, which makes it worse than no panel at all.
export function supportsContextReport(harness: HarnessKind | undefined): boolean {
  return harness === "claude_code";
}
