// Pure helpers for the diff view: map a file path to a Prism language, render
// one diff line's content as sanitized highlighted HTML, and fold a hunk's lines
// into aligned side-by-side rows. Kept out of the component so the alignment — the
// one non-trivial bit — is unit-testable on its own.

import DOMPurify from "dompurify";
import { escapeHtml, highlightCode } from "$lib/markdown";
import type { DiffLine } from "$lib/types";

// File extension → Prism language id. Only the grammars `markdown.ts` actually
// imports are worth mapping; anything else falls through to `""`, which renders as
// escaped plain text (still safe, just unhighlighted) rather than an error.
const EXT_TO_LANG: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "tsx",
  js: "javascript",
  jsx: "jsx",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  json: "json",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  sql: "sql",
  md: "markdown",
  markdown: "markdown",
  html: "markup",
  svelte: "markup",
  css: "css",
};

/// The Prism language id for a file path, or `""` when the extension has no
/// mapped grammar (the diff still renders, just without syntax colors).
export function languageForPath(path: string): string {
  const base = path.slice(path.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  if (dot <= 0) return ""; // no extension, or a dotfile with no extension
  return EXT_TO_LANG[base.slice(dot + 1).toLowerCase()] ?? "";
}

/// A byte count as a short human-readable size (decimal units, matching Finder),
/// e.g. `122 MB`. Used for the "file too large to diff" message.
export function formatFileSize(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  // Whole bytes; one decimal under 100; whole numbers above (122 MB, not 122.2 MB).
  let rounded = unit === 0 ? value : value >= 100 ? Math.round(value) : Math.round(value * 10) / 10;
  // Rounding can carry into the next unit (999.96 MB → 1000 → "1 GB", not "1000 MB").
  if (rounded >= 1000 && unit < units.length - 1) {
    rounded /= 1000;
    unit += 1;
  }
  return `${rounded} ${units[unit]}`;
}

/// Above this many characters, a single line skips Prism tokenizing and renders as
/// escaped plain text. The backend already clamps absurd lines, but Prism's cost
/// grows with line length, so this is a main-thread-safety net: highlighting a
/// multi-kilobyte line buys nothing a reader can use and risks janking the frame.
/// Set well above any real source line, so normal diffs are untouched.
const MAX_HIGHLIGHT_LINE_LENGTH = 5_000;

/// Render one diff line's content as highlighted, **sanitized** HTML for `{@html}`.
///
/// The content is whatever the user's agents wrote, rendered in a privileged
/// webview, so it is highlighted through the shared Prism path and then passed
/// through DOMPurify before it can reach the DOM — a file containing `<script>` or
/// `<img onerror=…>` becomes inert text, never an execution (the same contract as
/// the Markdown renderer). Highlighting is per line: a line is a self-contained
/// unit here, which keeps the cost bounded and avoids cross-line grammar state.
///
/// An over-long line skips highlighting and renders as escaped text — still safe
/// (escaping plus DOMPurify), just uncolored — so one pathological line can't block
/// the main thread.
export function highlightDiffLine(content: string, lang: string): string {
  if (content.length > MAX_HIGHLIGHT_LINE_LENGTH) {
    return DOMPurify.sanitize(escapeHtml(content));
  }
  return DOMPurify.sanitize(highlightCode(content, lang));
}

/// One side-by-side row: the old-side line on the left, the new-side line on the
/// right. A context line occupies both sides (same line); a pure deletion has no
/// right, a pure addition no left.
export interface SideBySideRow {
  left: DiffLine | null;
  right: DiffLine | null;
}

/// Fold a hunk's flat line list into aligned side-by-side rows.
///
/// Within a change block, a run of removed lines pairs row-by-row with the run of
/// added lines that follows (deletions left, additions right), padding the shorter
/// side with blanks. Context lines flush the pending block and then span both
/// columns. This is the standard two-column alignment; doing it over libgit2's
/// already-structured lines keeps it a small, deterministic transform.
export function toSideBySide(lines: DiffLine[]): SideBySideRow[] {
  const rows: SideBySideRow[] = [];
  let removed: DiffLine[] = [];
  let added: DiffLine[] = [];

  const flush = (): void => {
    const pairs = Math.max(removed.length, added.length);
    for (let i = 0; i < pairs; i++) {
      rows.push({ left: removed[i] ?? null, right: added[i] ?? null });
    }
    removed = [];
    added = [];
  };

  for (const line of lines) {
    if (line.origin === "context") {
      flush();
      rows.push({ left: line, right: line });
    } else if (line.origin === "removed") {
      removed.push(line);
    } else {
      added.push(line);
    }
  }
  flush();
  return rows;
}
