import type { AttachmentKind } from "$lib/types";

// Extension → attachment kind. The frontend owns this mapping: it has the
// dropped filename and assigns the numbered label, so the backend only persists
// what it's told (see the M1 contract). Extend the lists here.
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
