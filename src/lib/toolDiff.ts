// Synthesize a `FileDiff` from an Edit facet's before/after strings so the
// tool row reuses `DiffView` — the same renderer the Git view uses — instead
// of a second patch-shaped component.
//
// This is computed here, on the frontend, lazily (callers invoke it only when
// a row is expanded) rather than in Rust at parse time: hunking eagerly would
// do the work for rows nobody expands, double the wire payload, and drag a
// `crates/git` type into `crates/harness`, which has no business depending
// on it.
//
// Line numbers are **snippet-relative**: the facet carries no absolute file
// offsets (neither Claude's `Edit` input nor Codex's `apply_patch` grammar
// includes them), so each hunk header says so rather than presenting relative
// numbers as if they were file positions.

import { structuredPatch } from "diff";
import type { DiffHunk, DiffLine, EditPair, EditedFile, FileDiff } from "$lib/types";

// jsdiff default context (4) wastes rows on content the snippet already
// scopes tightly; 3 matches conventional diff output.
const CONTEXT_LINES = 3;

export function synthesizeEditDiff(file: EditedFile): FileDiff {
  return {
    path: file.path,
    binary: false,
    // The facet cap truncated the before/after content, so the hunks below
    // are computed from a prefix — surface DiffView's existing notice.
    truncated: file.truncated,
    too_large: false,
    too_large_bytes: null,
    hunks: file.edits.flatMap((pair) => pairHunks(pair)),
  };
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
