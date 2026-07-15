// Shared styling for the app's small icon buttons (sidebar toggles, the "+"
// menu triggers, theme toggle, dialog close). Named neutrally — these are used
// well beyond the sidebar now. The square footprint with a `rounded-full` hover
// gives the consistent circular hover affordance used across the app.
const ICON_BUTTON_BASE =
  "text-muted hover:text-fg inline-flex h-[26px] w-[26px] items-center justify-center rounded-full";

export const ICON_BUTTON_CLASS = `${ICON_BUTTON_BASE} hover:bg-control-hover`;

/// Compact controls that rest directly on a recessed `panel` surface brighten
/// to `raised`, producing a clearer hover than another nearby gray.
export const ICON_BUTTON_ON_PANEL_CLASS = `${ICON_BUTTON_BASE} hover:bg-raised`;

export const ICON_SIZE = 18;

/// Actions nested in a row need one stronger step than the row's own hover.
/// Selected blue rows instead use the light `raised` fill for contrast.
export const ROW_ACTION_ICON_CLASS = `${ICON_BUTTON_BASE} hover:bg-active group-data-[selected=true]:hover:bg-raised`;
