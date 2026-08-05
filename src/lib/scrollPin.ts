/// Pin-state tracker shared by the transcript's bottom-follow scrollers — the
/// outer transcript container and each streaming turn's capped live region.
/// Both need the same attribution state machine: classify every `scroll` event
/// by its geometry delta (user scrolling up, user scrolling down, or a browser
/// clamp), unpin immediately on genuine upward movement, re-pin only when the
/// user genuinely returns to the bottom, and treat the scroller's own
/// programmatic writes as inert. Keeping it here — pure, DOM-free — means the
/// two call sites cannot drift apart and boundary behavior is unit-testable
/// without component fixtures.
///
/// Attribution rules:
/// - `scrollTop` decreased while the scrollable extent (scrollHeight −
///   clientHeight) did not shrink → only the user scrolls up: the browser
///   moves `scrollTop` down on its own only by clamping, a clamp requires the
///   extent to shrink (content collapsing, or the viewport growing on a
///   resize), and the scroller's own writes report themselves via
///   `notifyProgrammaticWrite` so their echoed events compute a zero delta.
/// - Upward unpinning has NO distance threshold — the threshold applies only
///   to re-pinning near the bottom on the way back down. A distance-based
///   unpin would be reset by every follow-write during streaming, so only a
///   fast flick could ever escape a live stream.
/// - Intent is a SIGNED total that accumulates across events and across
///   programmatic writes: each non-clamp event subtracts its delta (upward
///   movement raises the total, downward movement lowers it), and crossing
///   ±epsilon classifies. Per-event epsilon comparison would silently drop
///   sub-pixel deltas — a slow trackpad drag emits ≤1px per frame, and during
///   streaming every follow-write resets the position baseline, so without
///   the accumulator the gentlest gestures could never escape a live stream
///   (the original escape-velocity bug shrunk to 1px, not fixed). The same
///   holds symmetrically for drifting back down: without downward
///   accumulation the gentlest return could never re-pin, and the caller's
///   gap/anchor bookkeeping (refreshed only on classified movement) would go
///   stale by the drift. Oscillating jitter self-cancels; classification
///   resets the total. The total deliberately survives
///   `notifyProgrammaticWrite` — follow-writes move the view, not the user's
///   intent.
/// - The two axes get DIFFERENT tolerances. Positions are fractional, so the
///   epsilon absorbs rounding there; scrollHeight/clientHeight are integer
///   CSSOM measurements, so ANY extent change of ≥1px is a real change:
///   shrink + downward move = clamp, growth + downward move = the engine's
///   own scroll-anchoring adjustment (WebKit compensates for above-viewport
///   growth despite having no *CSS* overflow-anchor support). Requiring more
///   than 1px of shrink would let a gradual pane/window resize — 1px of
///   shrink and 1px of clamp per frame — feed the intent total and unpin with
///   no user input at all.
/// - There is deliberately NO bottom-gap guard on unpinning: the gap sits
///   near zero exactly when follow-writes are fighting the user (the escape
///   case), so a guard blocks legitimate escapes, while the settle of any
///   hypothetical overscroll excursion is a downward move that re-pins.

export interface ScrollGeometry {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

/// How a scroll event was attributed. "up"/"down" are genuine user movement
/// (callers re-capture their reading anchor on these); "clamp" is the browser
/// clamping scrollTop after the extent shrank; "anchor" is the browser's own
/// scroll-anchoring adjustment after the extent grew (WebKit compensates for
/// growth above the viewport by moving scrollTop down — the reading position
/// did not move, so neither did the user); "none" is sub-epsilon noise.
export type ScrollAttribution = "up" | "down" | "clamp" | "anchor" | "none";

export interface PinTracker {
  readonly pinned: boolean;
  /// Classify a scroll event and update pin state. Call from the scroller's
  /// `scroll` listener with its current geometry.
  onScrollEvent(g: ScrollGeometry): ScrollAttribution;
  /// Record the geometry the scroller just wrote itself, so the write's echoed
  /// scroll event computes a zero delta and changes nothing. Must be called
  /// after EVERY programmatic scrollTop write — without it, a follow-write
  /// (a downward move) would read as the user scrolling down and re-pin
  /// anyone who had just unpinned within the re-pin threshold of the bottom.
  notifyProgrammaticWrite(g: ScrollGeometry): void;
  /// Force a pin transition the application decided on (first conversation
  /// rows pin, a navigator jump unpins). Snapshots the given geometry.
  setPinned(pinned: boolean, g: ScrollGeometry): void;
}

/// Absorbs fractional scroll positions (WebKit reports sub-pixel scrollTop).
export const SCROLL_EPS = 1;

/// How close to the bottom a genuine downward scroll must land to re-pin.
export const REPIN_THRESHOLD = 32;

export function createPinTracker(): PinTracker {
  let pinned = true;
  let lastTop = 0;
  let lastMax = 0;
  // Signed running intent total (px): positive = pending upward movement,
  // negative = pending downward. See the accumulation rule in the header.
  let intent = 0;

  return {
    get pinned(): boolean {
      return pinned;
    },
    onScrollEvent(g: ScrollGeometry): ScrollAttribution {
      const max = g.scrollHeight - g.clientHeight;
      const gap = max - g.scrollTop;
      const delta = g.scrollTop - lastTop;
      let result: ScrollAttribution = "none";
      if (delta < 0 && lastMax - max >= 1) {
        // A clamp, not the user (extents are integers — ≥1px of shrink is a
        // real shrink). Pending intent survives it — the clamp moved the
        // view, not the user's mind.
        result = "clamp";
      } else if (delta > 0 && max - lastMax >= 1) {
        // The browser's own scroll-anchoring adjustment: content grew above
        // the viewport and the engine moved scrollTop down to keep the
        // visible content still. Reading it as a user scroll-down would
        // re-pin (and slam) on any expansion above the viewport. A real
        // downward gesture coinciding with same-event growth is eaten here
        // once and classifies on the next quiet event. Pending intent
        // survives, as with clamps.
        result = "anchor";
      } else {
        intent -= delta;
        if (intent > SCROLL_EPS) {
          // A transient false unpin here (e.g. an overscroll excursion, if the
          // engine ever surfaces one) self-corrects: its settle is a downward
          // move landing at the bottom, which re-pins.
          pinned = false;
          intent = 0;
          result = "up";
        } else if (intent < -SCROLL_EPS) {
          pinned = gap < REPIN_THRESHOLD;
          intent = 0;
          result = "down";
        }
      }
      lastTop = g.scrollTop;
      lastMax = max;
      return result;
    },
    notifyProgrammaticWrite(g: ScrollGeometry): void {
      lastTop = g.scrollTop;
      lastMax = g.scrollHeight - g.clientHeight;
    },
    setPinned(next: boolean, g: ScrollGeometry): void {
      pinned = next;
      intent = 0;
      lastTop = g.scrollTop;
      lastMax = g.scrollHeight - g.clientHeight;
    },
  };
}
