// Pure index math for sidebar agent reordering. Extracted from the Sidebar so
// the drag's drop-index computation is unit-testable without real layout —
// jsdom reports zero-size rects, so the component supplies real geometry at
// runtime and these functions stay geometry-source-agnostic.

/// Distance (px) a grip pointer must travel before the press becomes a drag.
/// Below it, the gesture resolves to the grip's normal click (collapse
/// toggle), so an imprecise click never accidentally reorders.
export const DRAG_SLOP_PX = 5;

/// `items` with the element at `from` moved to occupy index `to`. Identity and
/// out-of-range moves return the input order unchanged (callers boundary-check
/// for UX — disabled menu items — so this is the safety net, not the gate).
export function movedOrder<T>(items: readonly T[], from: number, to: number): T[] {
  const next = [...items];
  if (from === to || from < 0 || to < 0 || from >= items.length || to >= items.length) {
    return next;
  }
  const moved = next.splice(from, 1)[0];
  if (moved === undefined) return [...items];
  next.splice(to, 0, moved);
  return next;
}

/// The index a dragged card should occupy, given the vertical midpoints of the
/// OTHER cards in display order: the count of midpoints above the pointer.
/// Midpoint-crossing (rather than edge-crossing) keeps the swap stable — after
/// two cards trade places, the pointer sits past the neighbor's new midpoint,
/// so a one-pixel jitter can't oscillate the order.
export function dropIndexForPointer(otherMidpoints: readonly number[], pointerY: number): number {
  let index = 0;
  for (const mid of otherMidpoints) {
    if (pointerY > mid) index++;
  }
  return index;
}
