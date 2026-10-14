/// Context occupancy: how full an agent's context is after its newest finished
/// turn, or the size a compaction newer than that turn left behind. A
/// forward-looking signal ("how full will the next turn's context be"). The
/// raw operands come back beside the fraction because the meter shows both,
/// and rescanning the transcript for them could disagree with the fraction on
/// a mid-render update.
///
/// **One rule, in two places.** Agent access reports the same figure to an
/// orchestrator (`desktop/src-tauri/src/ops/context.rs`), so the rule lives
/// here as a pure function and both copies run the cases in
/// `desktop/tests/fixtures/context-occupancy.json`. A change to the rule is
/// made in both and added to that file.
///
/// `context_tokens_after_turn` is the harness-reconciled occupancy (see
/// `TurnUsage`): for Claude it sums the disjoint cache fields (cached +
/// cache-creation, which `input_tokens` alone excludes), for Codex the adapter
/// substitutes the session file's per-turn `last_token_usage` because the
/// stream reports thread-cumulative totals. Consuming the pre-reconciled value
/// keeps this formula harness-agnostic — do not re-add per-harness token
/// summation here. Both it and `context_window` must be present; otherwise the
/// bar is hidden. An impossible occupancy is hidden rather than clamped, so bad
/// telemetry never masquerades as a plausible 100%.
///
/// A compaction marker newer than that turn supersedes it. Live, a completed
/// compaction is itself a turn with usage, so this matters after a reopen,
/// when the marker is the compaction's only trace and the newest turn is the
/// reply before it: without the marker the bar would show the pre-compaction
/// size until the agent's next reply. A marker's own window is the one the
/// compaction ran with: Codex records it in the session file, and for Claude
/// the loader reattaches the window saved at compaction time. A marker with
/// none (a Claude compaction run outside Switchboard, or before that window
/// was saved) borrows the superseded turn's, which is wrong only if the
/// model's window changed between that reply and the compaction, and only
/// until the next reply.
import type { TurnUsage } from "$lib/types";
import { isIsoTimestampAfter } from "$lib/utils";

export type ContextOccupancy = { usedTokens: number; windowTokens: number; fraction: number };

/// An agent's newest compaction marker that recorded the compacted context's
/// size: when it happened, the size, and the window it ran with when known.
export type CompactedReading = { at: string; tokens: number; window: number | null };

/// The parts of a turn the rule reads.
export type OccupancyTurn = {
  role: string;
  status?: string;
  started_at: string;
  ended_at?: string | null;
  usage?: TurnUsage | null;
};

export function contextOccupancy(
  turns: readonly OccupancyTurn[],
  compacted: CompactedReading | undefined,
): ContextOccupancy | undefined {
  let latest: { at: string; usage: TurnUsage } | undefined;
  for (let i = turns.length - 1; i >= 0; i--) {
    const turn = turns[i];
    // A streaming turn has no terminal usage yet, so keep showing the last
    // completed state while it runs. Once a terminal usage record exists it
    // is authoritative: missing/invalid operands clean-hide rather than
    // falling back to an older, stale percentage.
    if (
      turn?.role !== "agent" ||
      turn.status === "streaming" ||
      turn.usage === undefined ||
      turn.usage === null
    )
      continue;
    latest = { at: turn.ended_at ?? turn.started_at, usage: turn.usage };
    break;
  }
  if (
    compacted !== undefined &&
    (latest === undefined || isIsoTimestampAfter(compacted.at, latest.at))
  ) {
    return occupancyOf(compacted.tokens, compacted.window ?? latest?.usage.context_window);
  }
  if (latest === undefined) return undefined;
  return occupancyOf(latest.usage.context_tokens_after_turn, latest.usage.context_window);
}

export function occupancyOf(
  occupancy: number | null | undefined,
  window: number | null | undefined,
): ContextOccupancy | undefined {
  if (window === undefined || window === null || window === 0) return undefined;
  if (occupancy === undefined || occupancy === null) return undefined;
  if (occupancy > window) return undefined;
  return { usedTokens: occupancy, windowTokens: window, fraction: occupancy / window };
}
