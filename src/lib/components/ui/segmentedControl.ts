/// Shared classes for segmented controls (the Settings theme picker, the
/// Add-agent mode toggle, and the harness picker) so they share one height,
/// radius, padding, and typography. Each control supplies its own layout
/// (`inline-grid` / `grid` / `flex`, column count, `flex-1` vs centered) and
/// its own active/inactive coloring on top of these bases. Mirrors the
/// `iconButton.ts` shared-class pattern.

/// The control's outer wrapper. Add the layout (`inline-grid grid-cols-3`,
/// `grid grid-cols-4`, `flex`, …) alongside this.
export const SEGMENTED_CONTAINER_CLASS = "border-border bg-panel gap-1 rounded-full border p-0.5";

/// Compact control wrapper for persistent controls that sit on light/raised
/// main chrome instead of inside a side panel.
export const SEGMENTED_MAIN_CONTAINER_CLASS =
  "border-border bg-raised gap-1 rounded-full border p-0.5";

/// A single segment. Height/typography are fixed here (the standard `h-7`);
/// add centering (`flex items-center justify-center`) or `flex-1`, plus the
/// active/inactive color classes below.
export const SEGMENTED_ITEM_CLASS = "h-6 rounded-full px-2 text-xs font-medium transition-colors";

/// The selected segment.
export const SEGMENTED_ITEM_ACTIVE_CLASS = "bg-primary text-primary-fg";

/// A selectable, unselected segment — the lighter-gray hover background is
/// part of the standard so every control's hover affordance matches.
export const SEGMENTED_ITEM_INACTIVE_CLASS = "text-muted hover:bg-raised";

/// Compact main-view segments for persistent page chrome, where the selected
/// state should read softer than dialog/settings controls.
export const SEGMENTED_MAIN_ITEM_CLASS =
  "flex h-5 items-center justify-center rounded-full px-2 text-[11px] font-medium transition-colors";

export const SEGMENTED_MAIN_ITEM_ACTIVE_CLASS = "bg-muted/75 text-primary-fg";

export const SEGMENTED_MAIN_ITEM_INACTIVE_CLASS = "text-muted hover:bg-panel";
