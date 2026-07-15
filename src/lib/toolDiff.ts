// Synthesize a `FileDiff` from an Edit facet's before/after strings so the
// tool row reuses `DiffView` — the same renderer the Git view uses — instead
// of a second patch-shaped component.
//
// This is computed here, on the frontend, rather than in Rust at parse time:
// shipping hunks would double the wire payload and drag a `crates/git` type
// into `crates/harness`, which has no business depending on it. The sole
// consumer (the tool row) attempts this for every MOUNTED edit row — inline
// diffs are the point of that row. Collapsed synthesis has a short computation
// deadline; only a comparison that actually exceeds it waits for expansion and
// the yielding async path. Facet content caps and transcript render-windowing
// bound the source and number of mounted rows independently.
//
// Line numbers are **snippet-relative**: the facet carries no absolute file
// offsets (neither Claude's `Edit` input nor Codex's `apply_patch` grammar
// includes them), so each hunk header says so rather than presenting relative
// numbers as if they were file positions. Note the header text and per-line
// numbers are currently display-inert: the sole consumer renders through
// DiffView's `compact` mode, which hides both. They are kept correct because
// the `FileDiff`/`DiffLine` types require them and a future non-compact
// consumer inherits honest values — but that path has not been visually
// exercised yet.

import { structuredPatch } from "diff";
import type { StructuredPatch } from "diff";
import type { DiffHunk, DiffLine, EditPair, EditedFile, FileDiff } from "$lib/types";

// jsdiff default context (4) wastes rows on content the snippet already
// scopes tightly; 3 matches conventional diff output.
const CONTEXT_LINES = 3;

// Collapsed rows get one short synchronous attempt across the whole tool row. Actual diff
// complexity is a better predictor than source bytes or line count: large
// additions and sparse edits are cheap, while much smaller unrelated inputs
// can be expensive. Twenty-five milliseconds fits this transcript's chunked
// update cadence while still aborting comparisons that would visibly stall it.
export const COLLAPSED_EDIT_DIFF_TIMEOUT_MS = 25;

// Expansion is explicit permission to prepare the exceptional diff, but it
// must still yield to streaming and interaction. The async jsdiff path does
// that; this generous ceiling only guards pathological capped inputs from
// consuming work indefinitely.
export const EXPANDED_EDIT_DIFF_TIMEOUT_MS = 5_000;

type MonotonicClock = () => number;

export interface ExpandedDiffCoordinator {
  run(
    task: (timeoutMs: number) => Promise<FileDiff | undefined>,
    signal?: AbortSignal,
  ): Promise<FileDiff | undefined>;
  cancel(): void;
}

function monotonicNow(): number {
  return performance.now();
}

/**
 * Serialize expanded comparisons behind one row-wide deadline. A single row
 * may contain many files, but expanding it is one user action and must not
 * multiply main-thread work by granting every file a fresh safety ceiling.
 */
export function createExpandedDiffCoordinator(
  timeoutMs = EXPANDED_EDIT_DIFF_TIMEOUT_MS,
  now: MonotonicClock = monotonicNow,
): ExpandedDiffCoordinator {
  let tail: Promise<void> = Promise.resolve();
  let startedAt: number | undefined;
  let cancelled = false;

  return {
    run(task, signal) {
      const result = tail.then(async () => {
        if (cancelled || signal?.aborted === true) return undefined;
        startedAt ??= now();
        const remainingMs = timeoutMs - (now() - startedAt);
        if (remainingMs <= 0) return undefined;
        return task(remainingMs);
      });
      // A failed comparison makes only its own row unavailable; it must not
      // strand later queued files behind a rejected promise.
      tail = result.then(
        () => undefined,
        () => undefined,
      );
      return result;
    },
    cancel() {
      cancelled = true;
    },
  };
}

export function synthesizeEditDiff(file: EditedFile): FileDiff;
export function synthesizeEditDiff(file: EditedFile, timeoutMs: number): FileDiff | undefined;
export function synthesizeEditDiff(file: EditedFile, timeoutMs?: number): FileDiff | undefined {
  return synthesizeTextEditDiff(file.path, file.edits, file.truncated, timeoutMs);
}

export function synthesizeEditDiffAsync(
  file: EditedFile,
  timeoutMs = EXPANDED_EDIT_DIFF_TIMEOUT_MS,
): Promise<FileDiff | undefined> {
  return synthesizeTextEditDiffAsync(file.path, file.edits, file.truncated, timeoutMs);
}

/**
 * Attempt all filesystem previews under one absolute deadline. Once a file
 * cannot finish, later files are deferred without starting more comparisons;
 * completed earlier files keep their useful previews.
 */
export function synthesizeCollapsedEditDiffs(
  files: EditedFile[],
  timeoutMs = COLLAPSED_EDIT_DIFF_TIMEOUT_MS,
  now: MonotonicClock = monotonicNow,
): Array<FileDiff | undefined> {
  const deadline = now() + timeoutMs;
  const results: Array<FileDiff | undefined> = [];
  let deferred = false;
  for (const file of files) {
    if (deferred || file.edits.length === 0) {
      results.push(undefined);
      continue;
    }
    const result = synthesizeTextEditDiff(
      file.path,
      file.edits,
      file.truncated,
      undefined,
      deadline,
      now,
    );
    results.push(result);
    deferred = result === undefined;
  }
  return results;
}

/// MCP edits are remote snippet changes, not filesystem edits. Keep the
/// required `FileDiff.path` empty so this adapter cannot accidentally confer
/// file actions if the shared diff renderer gains them later.
export function synthesizeMcpTextEditDiff(
  before: string,
  after: string,
  contentTruncated: boolean,
): FileDiff;
export function synthesizeMcpTextEditDiff(
  before: string,
  after: string,
  contentTruncated: boolean,
  timeoutMs: number,
): FileDiff | undefined;
export function synthesizeMcpTextEditDiff(
  before: string,
  after: string,
  contentTruncated: boolean,
  timeoutMs?: number,
): FileDiff | undefined {
  return synthesizeTextEditDiff("", [{ old: before, new: after }], contentTruncated, timeoutMs);
}

export function synthesizeMcpTextEditDiffAsync(
  before: string,
  after: string,
  contentTruncated: boolean,
  timeoutMs = EXPANDED_EDIT_DIFF_TIMEOUT_MS,
): Promise<FileDiff | undefined> {
  return synthesizeTextEditDiffAsync(
    "",
    [{ old: before, new: after }],
    contentTruncated,
    timeoutMs,
  );
}

function synthesizeTextEditDiff(
  path: string,
  edits: EditPair[],
  contentTruncated: boolean,
  timeoutMs?: number,
  sharedDeadline?: number,
  now: MonotonicClock = monotonicNow,
): FileDiff | undefined {
  const deadline = sharedDeadline ?? (timeoutMs === undefined ? undefined : now() + timeoutMs);
  const hunks: DiffHunk[] = [];
  for (const pair of edits) {
    const pairResult = pairHunks(pair, deadline, now);
    if (pairResult === undefined) return undefined;
    hunks.push(...pairResult);
  }
  return textEditFileDiff(path, hunks, contentTruncated);
}

async function synthesizeTextEditDiffAsync(
  path: string,
  edits: EditPair[],
  contentTruncated: boolean,
  timeoutMs: number,
  now: MonotonicClock = monotonicNow,
): Promise<FileDiff | undefined> {
  const deadline = now() + timeoutMs;
  const hunks: DiffHunk[] = [];
  // Run pairs sequentially. Starting several async Myers comparisons together
  // would multiply CPU work even though each one yields to the event loop.
  for (const pair of edits) {
    const pairResult = await pairHunksAsync(pair, deadline, now);
    if (pairResult === undefined) return undefined;
    hunks.push(...pairResult);
  }
  return textEditFileDiff(path, hunks, contentTruncated);
}

function textEditFileDiff(path: string, hunks: DiffHunk[], contentTruncated: boolean): FileDiff {
  return {
    path,
    binary: false,
    // The facet cap truncated the before/after content, so the hunks below
    // are computed from a prefix — surface DiffView's existing notice.
    truncated: contentTruncated,
    too_large: false,
    too_large_bytes: null,
    hunks,
  };
}

/// Dedicated Write facets do not carry prior file content. The product treats
/// them as file creation because that is overwhelmingly the common case; a
/// rare overwrite will therefore still be presented as an all-added diff.
/// Build only the requested prefix so a collapsed large write does not pay to
/// allocate diff rows that are not mounted.
export function synthesizeWriteDiff(
  path: string,
  content: string,
  truncated: boolean,
  maxLines?: number,
): { diff: FileDiff; hiddenLines: number } {
  const lines = content === "" ? [] : content.split("\n");
  if (content.endsWith("\n")) lines.pop();
  const visibleLines = maxLines === undefined ? lines : lines.slice(0, maxLines);
  const diffLines: DiffLine[] = visibleLines.map((line, index) => ({
    origin: "added",
    old_lineno: null,
    new_lineno: index + 1,
    content: line,
  }));
  return {
    diff: {
      path,
      binary: false,
      truncated,
      too_large: false,
      too_large_bytes: null,
      hunks:
        diffLines.length === 0
          ? []
          : [
              {
                header: `@@ -0,0 +1,${lines.length} @@`,
                lines: diffLines,
              },
            ],
    },
    hiddenLines: lines.length - visibleLines.length,
  };
}

/// MCP text creation uses the same all-added representation as a file write,
/// but deliberately carries no path because the target names a remote record.
export function synthesizeMcpTextCreationDiff(
  content: string,
  contentTruncated: boolean,
  maxLines?: number,
): { diff: FileDiff; hiddenLines: number } {
  return synthesizeWriteDiff("", content, contentTruncated, maxLines);
}

function remainingTime(deadline: number | undefined, now: MonotonicClock): number | undefined {
  if (deadline === undefined) return undefined;
  return deadline - now();
}

function pairHunks(
  pair: EditPair,
  deadline: number | undefined,
  now: MonotonicClock,
): DiffHunk[] | undefined {
  const timeoutMs = remainingTime(deadline, now);
  if (timeoutMs !== undefined && timeoutMs <= 0) return undefined;
  const patch =
    timeoutMs === undefined
      ? structuredPatch("", "", pair.old, pair.new, undefined, undefined, {
          context: CONTEXT_LINES,
        })
      : structuredPatch("", "", pair.old, pair.new, undefined, undefined, {
          context: CONTEXT_LINES,
          timeout: timeoutMs,
        });
  return patch === undefined ? undefined : patchHunks(patch, deadline, now);
}

function pairHunksAsync(
  pair: EditPair,
  deadline: number,
  now: MonotonicClock,
): Promise<DiffHunk[] | undefined> {
  const timeoutMs = remainingTime(deadline, now);
  if (timeoutMs === undefined || timeoutMs <= 0) return Promise.resolve(undefined);
  return new Promise((resolve) => {
    structuredPatch("", "", pair.old, pair.new, undefined, undefined, {
      context: CONTEXT_LINES,
      timeout: timeoutMs,
      callback: (patch) =>
        resolve(patch === undefined ? undefined : patchHunks(patch, deadline, now)),
    });
  });
}

function patchHunks(
  patch: StructuredPatch,
  deadline?: number,
  now: MonotonicClock = monotonicNow,
): DiffHunk[] | undefined {
  const result: DiffHunk[] = [];
  for (const hunk of patch.hunks) {
    if (deadline !== undefined && now() >= deadline) return undefined;
    const lines: DiffLine[] = [];
    let oldLine = hunk.oldStart;
    let newLine = hunk.newStart;
    for (const [index, raw] of hunk.lines.entries()) {
      // Conversion is normally tiny, but it is still part of the collapsed
      // deadline. Check periodically without adding a clock read per line.
      if (index % 256 === 0 && deadline !== undefined && now() >= deadline) return undefined;
      // "\ No newline at end of file" markers carry no content to render and
      // have no line number on either side.
      if (raw.startsWith("\\")) continue;
      const content = raw.slice(1);
      if (raw.startsWith("+")) {
        lines.push({ origin: "added", old_lineno: null, new_lineno: newLine, content });
        newLine += 1;
      } else if (raw.startsWith("-")) {
        lines.push({ origin: "removed", old_lineno: oldLine, new_lineno: null, content });
        oldLine += 1;
      } else {
        lines.push({ origin: "context", old_lineno: oldLine, new_lineno: newLine, content });
        oldLine += 1;
        newLine += 1;
      }
    }
    result.push({
      header: `@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@ · snippet-relative`,
      lines,
    });
  }
  if (deadline !== undefined && now() >= deadline) return undefined;
  return result;
}

/// Keep the first `maxLines` diff lines (across hunks) for the collapsed inline
/// preview, returning the trimmed `FileDiff` and how many lines were hidden.
/// A partially-kept hunk keeps its header but only its leading lines. Returns
/// the diff unchanged (`hiddenLines: 0`) when it already fits — the caller then
/// shows no fade and no "expand" affordance.
export function truncateDiff(
  diff: FileDiff,
  maxLines: number,
): { diff: FileDiff; hiddenLines: number } {
  const total = diff.hunks.reduce((n, hunk) => n + hunk.lines.length, 0);
  if (total <= maxLines) return { diff, hiddenLines: 0 };

  const hunks: DiffHunk[] = [];
  let remaining = maxLines;
  for (const hunk of diff.hunks) {
    if (remaining <= 0) break;
    if (hunk.lines.length <= remaining) {
      hunks.push(hunk);
      remaining -= hunk.lines.length;
    } else {
      hunks.push({ ...hunk, lines: hunk.lines.slice(0, remaining) });
      remaining = 0;
    }
  }
  return { diff: { ...diff, hunks }, hiddenLines: total - maxLines };
}
