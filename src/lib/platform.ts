// OS-aware keyboard-shortcut display. macOS uses symbol glyphs (⌘ ⇧ ↵);
// Windows/Linux spell them out (Ctrl, Shift, Enter). `mod` is the primary
// modifier — Command on macOS, Control elsewhere.
const isMac =
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent || "");

const MAC_SYMBOLS: Record<string, string> = {
  mod: "⌘",
  shift: "⇧",
  alt: "⌥",
  ctrl: "⌃",
  enter: "↵",
  esc: "Esc",
};

const KEY_WORDS: Record<string, string> = {
  mod: "Ctrl",
  shift: "Shift",
  alt: "Alt",
  ctrl: "Ctrl",
  enter: "Enter",
  esc: "Esc",
};

/// Format a chord for display, OS-aware. Tokens: `mod`/`shift`/`alt`/`ctrl`/
/// `enter`/`esc` (resolved per-OS) or any literal key (passed through). Chords
/// join with "+" on both platforms (⌘+⇧+A, ⌘+2 / Ctrl+Shift+A, Ctrl+2); a
/// single token like "Esc" has no separator. Example: `shortcut("mod", "enter")`
/// → "⌘+↵" on macOS, "Ctrl+Enter" elsewhere.
export function shortcut(...tokens: string[]): string {
  const symbols = isMac ? MAC_SYMBOLS : KEY_WORDS;
  return tokens.map((t) => symbols[t.toLowerCase()] ?? t).join("+");
}

export { isMac };
