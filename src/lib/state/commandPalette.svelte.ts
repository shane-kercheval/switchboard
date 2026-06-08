// Command-palette state: the open flag and a small contribution registry.
//
// The palette (⌘⇧P) is a global modal owned by `App.svelte`, but its action
// list is context-aware: the always-available navigation/project commands are
// built in `App` (it holds the active-project + sidebar state), while the Git
// view contributes its own commands while it is mounted (`setCommandSource` in
// an effect, `clearCommandSource` on teardown). Keeping a view's commands
// co-located with the state and error handling they close over avoids lifting
// component-local UI state into a store just to reach it from the palette.
//
// `open` lives here (not as `App` local state) so the other window-level
// keyboard handlers — `ComposeBar`'s recipient chords already gate on an open
// dialog, but `GitView`'s and `App`'s do not — can suppress themselves while
// the palette owns the keyboard.

/// One actionable row in the palette. `shortcut` is a token list for
/// `platform.shortcut()` (e.g. `["mod", "shift", "p"]`), rendered OS-aware on
/// the right of the row. A `disabled` command renders greyed and is skipped by
/// keyboard navigation and activation — kept visible so the shortcut still
/// teaches even when the action isn't currently available.
export type Command = {
  id: string;
  title: string;
  /// Section header the row is grouped under (e.g. "Navigation", "Git").
  group: string;
  /// Extra text folded into the substring match alongside `title` + `group`.
  keywords?: string;
  /// Token list for `platform.shortcut()`, e.g. `["mod", "shift", "P"]`.
  /// Modifier tokens (`mod`/`shift`/`alt`/`ctrl`) are case-insensitive, but a
  /// literal letter key falls through verbatim — so write letters UPPERCASE
  /// (`"P"`, not `"p"`) or the chord renders lowercase.
  shortcut?: string[];
  disabled?: boolean;
  run: () => void | Promise<void>;
};

export const palette = $state<{ open: boolean }>({ open: false });

export function openPalette(): void {
  palette.open = true;
}

export function closePalette(): void {
  palette.open = false;
}

export function togglePalette(): void {
  palette.open = !palette.open;
}

/// Contributed command sources, in registration order. A source replaces its
/// prior entry on re-set (the Git view re-sets on every relevant state change so
/// its `disabled` flags and closures stay current), so the same `key` never
/// duplicates.
const sources = $state<{ entries: { key: string; commands: Command[] }[] }>({ entries: [] });

export function setCommandSource(key: string, commands: Command[]): void {
  const idx = sources.entries.findIndex((e) => e.key === key);
  if (idx === -1) {
    sources.entries = [...sources.entries, { key, commands }];
  } else {
    sources.entries = sources.entries.map((e) => (e.key === key ? { key, commands } : e));
  }
}

export function clearCommandSource(key: string): void {
  sources.entries = sources.entries.filter((e) => e.key !== key);
}

/// The flattened contributed commands, in source-registration order. Read
/// reactively from `.svelte` consumers.
export function contributedCommands(): Command[] {
  return sources.entries.flatMap((e) => e.commands);
}

/// Test-only reset.
export const _testing = {
  reset(): void {
    palette.open = false;
    sources.entries = [];
  },
};
