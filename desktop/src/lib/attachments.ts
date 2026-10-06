import type { AttachmentKind } from "$lib/types";

// Extension → attachment kind. The frontend owns this mapping: it has the
// dropped filename and assigns the numbered label, so the backend only persists
// what it's told (the backend persists the label as given). Extend the lists here.
//
// `classifyKind` never returns `"unknown"` — that variant is the wire fallback
// for a kind a *newer* build wrote into the journal (see `AttachmentKind` in
// types.ts); this build only ever emits image/text/file.
const IMAGE_EXTENSIONS = new Set([
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "svg",
  "bmp",
  "heic",
  "heif",
  "avif",
  "ico",
  "tiff",
]);

const TEXT_EXTENSIONS = new Set([
  "txt",
  "md",
  "markdown",
  "json",
  "jsonl",
  "csv",
  "tsv",
  "log",
  "yaml",
  "yml",
  "toml",
  "xml",
  "html",
  "css",
  "rs",
  "ts",
  "tsx",
  "js",
  "jsx",
  "py",
  "go",
  "rb",
  "java",
  "kt",
  "c",
  "h",
  "cpp",
  "hpp",
  "sh",
  "bash",
  "zsh",
  "sql",
  "ini",
  "env",
  "svelte",
  "vue",
]);

function extensionOf(filename: string): string {
  const base = filename.split(/[\\/]/).pop() ?? filename;
  const dot = base.lastIndexOf(".");
  // `dot >= 0` includes dotfiles: `.env` → `env` → text (wanted). `.gitignore`
  // has no second dot, so it returns `gitignore`, which isn't in any set and
  // falls through to `file` — also correct.
  return dot >= 0 ? base.slice(dot + 1).toLowerCase() : "";
}

/// Classify a dropped file by its extension. Drives the chip label prefix
/// (`image-1`, `text-1`, `file-1`) and whether the transcript renders a
/// thumbnail.
export function classifyKind(filename: string): Exclude<AttachmentKind, "unknown"> {
  const ext = extensionOf(filename);
  if (IMAGE_EXTENSIONS.has(ext)) return "image";
  if (TEXT_EXTENSIONS.has(ext)) return "text";
  return "file";
}

/// The next label for a newly attached file of `kind`, given the attachments
/// already staged in the same draft.
///
/// **Derived from the labels present, not from a running counter.** A counter
/// would live in the composer, and the composer is remounted on every project
/// switch and Git-view toggle — it would restart at 1 and mint a second `image-1`
/// beside a restored one. That is user-visible, because the label is what the user
/// types into the message (`look at @image-1`) to reference the file.
///
/// Max-suffix + 1, so labels stay monotonic within a draft: removing a chip never
/// renumbers the survivors, and a removed chip's number is never reused while a
/// higher one survives. Emptying the draft entirely does restart at 1 — no
/// surviving label is left for a new one to collide with.
///
/// A label that doesn't parse (hand-edited storage, or a format this build
/// predates) is skipped: `parseInt` yields `NaN`, and every comparison against
/// `NaN` is false, so it can never win the maximum. No explicit guard needed.
export function nextLabel(kind: AttachmentKind, existing: readonly { label: string }[]): string {
  const prefix = `${kind}-`;
  let max = 0;
  for (const { label } of existing) {
    if (!label.startsWith(prefix)) continue;
    const n = Number.parseInt(label.slice(prefix.length), 10);
    if (n > max) max = n;
  }
  return `${prefix}${max + 1}`;
}
