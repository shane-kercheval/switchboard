// Shared styling for the app's small icon buttons (sidebar toggles, the "+"
// menu triggers, theme toggle, dialog close). Named neutrally — these are used
// well beyond the sidebar now. The square footprint with a `rounded-full` hover
// gives the consistent circular hover affordance used across the app.
const ICON_BUTTON_BASE =
  "text-muted hover:text-fg inline-flex h-[26px] w-[26px] items-center justify-center rounded-full";

export const ICON_BUTTON_CLASS = `${ICON_BUTTON_BASE} hover:bg-raised`;

// Same footprint as `ICON_BUTTON_CLASS`, but for buttons sitting *on* a
// `bg-raised` surface (the compose card and its menus), where `hover:bg-raised`
// would be invisible (white-on-white in light mode). Steps the hover fill down
// to `panel` so the round hover affordance still reads.
export const ICON_BUTTON_ON_RAISED_CLASS = `${ICON_BUTTON_BASE} hover:bg-panel`;

export const ICON_SIZE = 18;

/// Hover treatment for an action icon nested in a selectable row. The stronger
/// neutral default stays visible when the row itself already has `bg-hover`; a
/// selected blue row overrides it to the light `bg-raised` fill.
export const ROW_ACTION_ICON_HOVER = "hover:bg-active group-data-[selected=true]:hover:bg-raised";
