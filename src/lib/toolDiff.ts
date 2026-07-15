// Synthesize a `FileDiff` from an Edit facet's before/after strings so the
// tool row reuses `DiffView` — the same renderer the Git view uses — instead
// of a second patch-shaped component.
//
// This is computed here, on the frontend, rather than in Rust at parse time:
// shipping hunks would double the wire payload and drag a `crates/git` type
// into `crates/harness`, which has no business depending on it. The sole
// consumer (the tool row) invokes this eagerly for every MOUNTED edit row —
// inline diffs are the point of that row — bounded by the facet content cap
// and the transcript's render-windowing, not by expansion.
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
import type { DiffHunk, DiffLine, EditPair, EditedFile, FileDiff } from "$lib/types";

// jsdiff default context (4) wastes rows on content the snippet already
// scopes tightly; 3 matches conventional diff output.
const CONTEXT_LINES = 3;

export function synthesizeEditDiff(file: EditedFile): FileDiff {
  return synthesizeTextEditDiff(file.path, file.edits, file.truncated);
}

/// MCP edits are remote snippet changes, not filesystem edits. Keep the
/// required `FileDiff.path` empty so this adapter cannot accidentally confer
/// file actions if the shared diff renderer gains them later.
export function synthesizeMcpTextEditDiff(
  before: string,
  after: string,
  contentTruncated: boolean,
): FileDiff {
  return synthesizeTextEditDiff("", [{ old: before, new: after }], contentTruncated);
}

function synthesizeTextEditDiff(
  path: string,
  edits: EditPair[],
  contentTruncated: boolean,
): FileDiff {
  return {
    path,
    binary: false,
    // The facet cap truncated the before/after content, so the hunks below
    // are computed from a prefix — surface DiffView's existing notice.
    truncated: contentTruncated,
    too_large: false,
    too_large_bytes: null,
    hunks: edits.flatMap((pair) => pairHunks(pair)),
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

function pairHunks(pair: EditPair): DiffHunk[] {
  const patch = structuredPatch("", "", pair.old, pair.new, undefined, undefined, {
    context: CONTEXT_LINES,
  });
  return patch.hunks.map((hunk) => {
    const lines: DiffLine[] = [];
    let oldLine = hunk.oldStart;
    let newLine = hunk.newStart;
    for (const raw of hunk.lines) {
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
    return {
      header: `@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@ · snippet-relative`,
      lines,
    };
  });
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
