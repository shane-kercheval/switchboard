/// Shared classes for segmented controls. Standard and compact variants keep
/// distinct geometry but intentionally share one color system.

/// The control's outer wrapper. Add the layout (`inline-grid grid-cols-3`,
/// `grid grid-cols-4`, `flex`, …) alongside this.
export const SEGMENTED_CONTAINER_CLASS = "border-border bg-raised gap-1 rounded-full border p-0.5";

/// Compact control wrapper for persistent page chrome.
export const SEGMENTED_MAIN_CONTAINER_CLASS =
  "border-border bg-raised gap-1 rounded-full border p-0.5";

/// A single segment. Height/typography are fixed here (the standard `h-7`);
/// add centering (`flex items-center justify-center`) or `flex-1`, plus the
/// active/inactive color classes below.
export const SEGMENTED_ITEM_CLASS = "h-6 rounded-full px-2 text-xs font-medium transition-colors";

/// The selected segment.
export const SEGMENTED_ITEM_ACTIVE_CLASS = "bg-segment-selected text-segment-selected-fg";

/// A selectable, unselected segment.
export const SEGMENTED_ITEM_INACTIVE_CLASS = "text-muted hover:bg-panel";

/// Compact main-view segments for persistent page chrome.
export const SEGMENTED_MAIN_ITEM_CLASS =
  "flex h-5 items-center justify-center rounded-full px-2 text-[11px] font-medium transition-colors";

export const SEGMENTED_MAIN_ITEM_ACTIVE_CLASS = SEGMENTED_ITEM_ACTIVE_CLASS;

export const SEGMENTED_MAIN_ITEM_INACTIVE_CLASS = SEGMENTED_ITEM_INACTIVE_CLASS;
