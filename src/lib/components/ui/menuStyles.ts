/// Class strings shared by `DropdownMenu`, `DropdownMenuItem`, and
/// `DropdownMenuSub`. A submenu is the same surface as the menu it hangs off,
/// and a sub-trigger is the same row as any other item — pasting the strings
/// into each component is how the two drift apart (a submenu that scrolls
/// differently, a trigger that highlights differently) without any test
/// noticing.

/// The popover surface, for both root content and sub-content.
///
/// `max-h` + scroll: menu content is caller-supplied and unbounded (the forward
/// picker lists every agent, every pane, and every other project), so without
/// containment a long menu runs past the viewport and the clipped rows are
/// unreachable — the same dead-end as a row that can't take focus.
///
/// Two available-height variables, chained: bits-ui namespaces its floating CSS
/// vars per component, and sub-content is a plain `menu` while root content is a
/// `dropdown-menu`. Sub-content is portaled to `<body>`, not nested inside the
/// root content, so it inherits nothing and each surface sees exactly one of the
/// two defined. The final `28rem` is the no-variable case.
///
/// NOTE: the fallbacks cover bits-ui *removing* a variable, not it emitting a bad
/// value — on the first positioning pass the property is defined as the literal
/// `undefinedpx`, which makes the whole declaration invalid and drops `max-height`
/// rather than falling back. Harmless (content is parked off-screen while
/// measuring), but the fallback is not the safety net it looks like.
export const MENU_CONTENT_CLASS: string =
  "border-border/90 bg-raised z-50 min-w-44 overflow-y-auto rounded-lg border p-1 text-[13px] shadow-[0_10px_28px_rgba(0,0,0,0.10)] outline-none focus:outline-none " +
  "max-h-[min(28rem,var(--bits-dropdown-menu-content-available-height,var(--bits-menu-content-available-height,28rem)))]";

/// One selectable row, for both `DropdownMenuItem` and a submenu's trigger.
export const MENU_ITEM_CLASS: string =
  "text-fg flex w-full items-center rounded-md px-2.5 py-1.5 text-left leading-5 outline-none select-none " +
  "data-highlighted:bg-hover cursor-pointer " +
  "data-disabled:text-muted/50 data-disabled:cursor-not-allowed";
