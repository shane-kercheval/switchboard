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
