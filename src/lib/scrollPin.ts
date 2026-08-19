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
/// - The re-pin threshold is measured against the current bottom OR the bottom
///   as of the previous sample. Growth that lands between a gesture and its
///   classification would otherwise move the target away from a reader who
///   scrolled all the way down mid-stream, leaving them a chunk's height short
///   of re-pinning. The second measure is deliberately NOT allowed to go
///   negative: landing far PAST the previous bottom is the signature of an
///   engine adjustment (which can move `scrollTop` further than the extent
///   grew), and an unbounded measure would make any misattributed adjustment
///   re-pin unconditionally — the slam this module exists to prevent.
/// - Geometry cannot tell a downward gesture that races an extent change from
///   the engine's own adjustment — both are "scrollTop moved down while the
///   extent moved" — and the extent grows on nearly every streamed chunk, so
///   attributing that shape to the engine unconditionally means a reader can
///   never scroll back down to re-pin mid-stream. Real input breaks the tie:
///   `notifyGesture` reports the signed deltas the scroller received from real
///   input, and a pending downward gesture vetoes the "anchor" attribution.
///   The engine never adjusts in response to a gesture, so the veto costs
///   nothing when the movement really was an adjustment (no gesture, no veto).
/// - Gesture evidence is spent by the NEXT sample of any kind, and cleared by
///   `notifyProgrammaticWrite`. Letting it survive a sample that saw no
///   movement would leave it attached to whatever came next — an engine
///   adjustment several frames later, misread as the user. Nothing is lost by
///   spending it early: every sample also absorbs the extent into `lastMax`,
///   so the user's real event arrives with no growth left to explain and
///   classifies without needing evidence at all.
/// - There is deliberately NO bottom-gap guard on unpinning: the gap sits
///   near zero exactly when follow-writes are fighting the user (the escape
///   case), so a guard blocks legitimate escapes. Elastic overscroll — where
///   the engine reports a position OUTSIDE [0, max] and then springs back —
///   is handled by range, not by gap: the excursion and its settle are both
///   engine motion. The settle moves UP, so without this a flick against the
///   bottom (macOS rubber-banding) would unpin a following reader. Nothing
///   but a wheel/trackpad gesture can overscroll — a scrollbar thumb clamps
///   at the end — so the modalities that carry no input evidence also cannot
///   arm this.
/// - Rubber-banding is MEASURED, not assumed. Captured from the Tauri
///   WKWebView with a trackpad flick against the bottom of a live transcript
///   (max 153): `scrollTop` runs 157 → 187 (34px past the end), decays back,
///   and then UNDERSHOOTS the rest position to 150 before settling. The top
///   edge behaves symmetrically (down to −8). Two consequences the numbers
///   dictate: the settle window has to cover the undershoot, not just the
///   out-of-range leg (a 2px window let 150 through as a user scroll-up, and
///   that single event was the reported "streaming stopped following"); and
///   an excursion must contribute NOTHING to the intent total — its net
///   displacement is real (3px up, here) but it is the engine's, so
///   accumulating it would simply unpin one event later.
/// - The settle window is DERIVED from the excursion, not a constant. The
///   undershoot is a spring response, so it scales with the energy in the
///   flick: the captured 34px excursion undershot 3px, and a hard flick
///   200px past the end would undershoot proportionally more. Any fixed
///   number is therefore a single-sample extrapolation, and too small a one
///   reopens the measured bug — the settle's tail classifies as a scroll up
///   and auto-follow drops out. So the window is the deepest out-of-range
///   distance the current excursion reached (floored at SETTLE_MIN for the
///   rounding-sized case), which self-scales and guesses nothing.
/// - Only a move that STARTS FROM THE EDGE and then DEEPENS with a stable
///   extent sizes that window, because that is the rubber band's signature and
///   nothing else's. Three unrelated things put `scrollTop` outside [0, max]:
///   a spring (which stretches from the edge the view already sits at,
///   travelling further out over several frames while the extent holds), a
///   collapse (whose position is merely stale, waiting for the clamp), and an
///   engine adjustment that relocates the view past the end. Each clause rules
///   one out. Position alone would let a collapse arm a window hundreds of
///   pixels wide. A snapshot of the extent is not enough either: a second
///   sample of a stale out-of-range position sees no extent change and reads
///   as a spring. And neither excludes an adjustment that lands out of range
///   in one jump from mid-history — only the from-the-edge test catches that,
///   because a reader in the middle of the transcript cannot be rubber-banding
///   against its end.
/// - The residue: a bounce whose deepening frames ALL coincide with content
///   growth gets only SETTLE_MIN. On the transcript that is unlikely (chunks
///   do not land every frame, and one growth-free frame is enough). Inside a
///   live cap it is likelier — the cap exists only while its turn streams and
///   is where the chunks land — so a hard flick in a streaming column can
///   settle against the floor and unpin on its own undershoot. The debug
///   counters (elastic samples per cap against chunk cadence) size that risk;
///   it is not worth a weaker rule to avoid.
/// - Erring wide is safe because contrary input shrinks the settle to its
///   floor: a gesture opposing the excursion means the reader is driving, so
///   the window collapses to SETTLE_MIN and any movement past it classifies
///   normally. Shrinking rather than disabling matters — one frame's wheel
///   sign should not be able to unpin a reader through an entire settle,
///   including the undershoot. Only a trackpad or wheel gesture can produce a
///   bounce at all (a scrollbar thumb clamps at the end), so a genuine escape
///   after a bounce always arrives carrying evidence. The residue is an
///   evidence-free escape — a scrollbar grab inside the settle of a flick —
///   bounded by the next event outside the window.
/// - NOTE the earlier claim that `notifyProgrammaticWrite` bounds this was
///   backwards: the write that clears the latch is the follow-write that
///   would slam a swallowed reader to the bottom. It is the failure, not the
///   mitigation.

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
/// did not move, so neither did the user); "elastic" is an overscroll
/// excursion past an edge and its spring back; "none" is sub-epsilon noise.
export type ScrollAttribution = "up" | "down" | "clamp" | "anchor" | "elastic" | "none";

export interface PinTracker {
  readonly pinned: boolean;
  /// Classify a scroll event and update pin state. Call from the scroller's
  /// `scroll` listener with its current geometry.
  onScrollEvent(g: ScrollGeometry): ScrollAttribution;
  /// Whether recorded input evidence is still waiting to be attributed. The
  /// tracker owns this so its call sites cannot hold divergent ideas of when a
  /// gesture is outstanding; they read it to decide whether to sample geometry
  /// ahead of a correction.
  readonly gesturePending: boolean;
  /// Record real input the scroller received — wheel only; see the caller for
  /// why keyboard scrolling is deliberately unsupported and why scrollbar
  /// drags carry no evidence. Signed like `scrollTop`: positive
  /// drives the view toward the bottom. Deltas accumulate so opposing input
  /// within one frame nets out the way the engine will apply it, but ONLY the
  /// sign of the net is ever read — magnitudes exist for that cancellation and
  /// nothing else, so their units need not be pixels (wheel deltas are not,
  /// under `DOM_DELTA_LINE`). Zero deltas are ignored. Evidence that the next
  /// movement is the user's rather than the engine's; spent by the next
  /// `onScrollEvent`.
  notifyGesture(delta: number): void;
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

/// Floor for the derived settle window (see the header): covers an excursion
/// that only just cleared OVERSCROLL_EPS, whose undershoot is rounding-sized.
const SETTLE_MIN = 4;

/// How far outside [0, max] a reported position must sit to count as an
/// elastic overscroll excursion rather than measurement rounding. Positions
/// are exact but the extent is derived from two integer-rounded lengths, so
/// an at-rest bottom can read up to ~1px past `max`; a measured rubber-band
/// excursion is tens of pixels (see the header).
const OVERSCROLL_EPS = 2;

export function createPinTracker(): PinTracker {
  let pinned = true;
  let lastTop = 0;
  let lastMax = 0;
  // Signed running intent total (px): positive = pending upward movement,
  // negative = pending downward. See the accumulation rule in the header.
  let intent = 0;
  // Net signed input received since the last sample: positive = toward the
  // bottom. See the veto rule in the header.
  let gesture = 0;
  let pendingGesture = false;
  // Which edge the last event overshot, if any: 1 = past the bottom, -1 = past
  // the top, 0 = in range. Lets the spring back into range be recognized as
  // the tail of the same engine excursion.
  let overshoot = 0;
  // Settle window for the current excursion: the deepest out-of-range distance
  // reached by a spring-shaped (deepening, extent-stable) move. Reset with the
  // latch. `deepestSeen` tracks every out-of-range position regardless of
  // provenance, so a repeated sample of a stale one cannot read as deepening.
  let depth = 0;
  let deepestSeen = 0;
  // Whether the current excursion began from the edge it overshot.
  let fromEdge = false;

  return {
    get pinned(): boolean {
      return pinned;
    },
    get gesturePending(): boolean {
      return pendingGesture;
    },
    onScrollEvent(g: ScrollGeometry): ScrollAttribution {
      const max = g.scrollHeight - g.clientHeight;
      const gap = max - g.scrollTop;
      const delta = g.scrollTop - lastTop;
      const growth = max - lastMax;
      const edge = g.scrollTop > max + OVERSCROLL_EPS ? 1 : g.scrollTop < -OVERSCROLL_EPS ? -1 : 0;
      if (edge !== 0) {
        if (edge !== overshoot) {
          // A fresh excursion, including the first frame of any: never let one
          // edge's window size the other's. A spring can only stretch from the
          // edge the view already sits at, so an excursion that begins
          // anywhere else is the engine relocating the view and never sizes a
          // window.
          depth = 0;
          deepestSeen = 0;
          fromEdge = (edge === 1 ? lastMax - lastTop : lastTop) <= SETTLE_MIN;
        }
        const outward = edge === 1 ? g.scrollTop - max : -g.scrollTop;
        if (edge === 1 ? gesture < 0 : gesture > 0) {
          // The reader is driving against the excursion (see the header).
          depth = 0;
        } else if (fromEdge && Math.abs(growth) < 1 && outward > deepestSeen + SCROLL_EPS) {
          depth = Math.max(depth, outward);
        }
        deepestSeen = Math.max(deepestSeen, outward);
      }
      // The spring back is still engine motion after the view re-enters range:
      // it keeps travelling in the direction that returns it, and it
      // overshoots the rest position on the way. The window that covers that
      // undershoot is the excursion's own depth (header). Contrary input ends
      // the settle by both routes — it collapses the window to the floor while
      // the view is still out of range, and it fails this test once back in
      // range — because a reader driving the other way is not a spring.
      const settleWindow = Math.max(SETTLE_MIN, depth);
      const springing =
        edge === 0 &&
        ((overshoot === 1 && delta < 0 && gap <= settleWindow && gesture >= 0) ||
          (overshoot === -1 && delta > 0 && g.scrollTop <= settleWindow && gesture <= 0));
      overshoot = edge !== 0 ? edge : springing ? overshoot : 0;
      if (overshoot === 0) {
        depth = 0;
        deepestSeen = 0;
      }
      let result: ScrollAttribution = "none";
      if (edge !== 0 || springing) {
        // Elastic overscroll: the engine carried the view past an edge and is
        // returning it. Neither leg is the user reaching for content — at the
        // bottom the excursion reads as a downward move and the spring back as
        // an upward one, so scoring them would re-pin and then immediately
        // unpin a reader who merely flicked against the end of the transcript.
        // Pending intent survives untouched, as with clamps: the excursion
        // contributes no displacement of its own (header).
        result = "elastic";
        // One transition IS the user's: a flick that carries an unpinned
        // reader from history clear past the bottom means they asked for the
        // end of the transcript, and no in-range sample will say so on their
        // behalf. Only with input evidence — an engine adjustment can also
        // land past `max` (growth above the viewport alongside a shrink
        // below), and pinning on that would slam a reader who never moved.
        if (edge === 1 && gesture > 0) {
          pinned = true;
          intent = 0;
        }
      } else if (delta < 0 && -growth >= 1) {
        // A clamp, not the user (extents are integers — ≥1px of shrink is a
        // real shrink). Pending intent survives it — the clamp moved the
        // view, not the user's mind.
        result = "clamp";
      } else if (delta > 0 && growth >= 1 && gesture <= 0) {
        // The browser's own scroll-anchoring adjustment: content grew above
        // the viewport and the engine moved scrollTop down to keep the
        // visible content still. Reading it as a user scroll-down would
        // re-pin (and slam) on any expansion above the viewport. The
        // adjustment can exceed the extent growth (growth above the viewport
        // alongside a shrink below it), so its size proves nothing — only the
        // absence of a downward gesture does. Pending intent survives, as
        // with clamps.
        result = "anchor";
      } else {
        intent -= delta;
        if (intent > SCROLL_EPS) {
          pinned = false;
          intent = 0;
          result = "up";
        } else if (intent < -SCROLL_EPS) {
          // Re-pin against the bottom the user was actually moving toward:
          // content that arrived between the gesture and this sample would
          // otherwise have pushed the target out from under them, so a reader
          // who scrolled to the end of a streaming turn would land short of
          // the threshold and keep following nothing. `lastMax` is the extent
          // as of the previous sample (every correction reports one), so this
          // is "the bottom of the document they were looking at". Landing PAST
          // that bottom is not evidence of reaching anything — see the header.
          const gapBefore = lastMax - g.scrollTop;
          pinned = gap < REPIN_THRESHOLD || (gapBefore >= 0 && gapBefore < REPIN_THRESHOLD);
          intent = 0;
          result = "down";
        }
      }
      lastTop = g.scrollTop;
      lastMax = max;
      gesture = 0;
      pendingGesture = false;
      return result;
    },
    notifyGesture(delta: number): void {
      // A zero delta is not input: a horizontal trackpad swipe reports one,
      // and arming on it would open the pre-correction sample that the
      // caller's gate exists to keep shut for engine-only movement.
      if (delta === 0) return;
      gesture += delta;
      pendingGesture = true;
    },
    notifyProgrammaticWrite(g: ScrollGeometry): void {
      lastTop = g.scrollTop;
      lastMax = g.scrollHeight - g.clientHeight;
      overshoot = 0;
      depth = 0;
      deepestSeen = 0;
      gesture = 0;
      pendingGesture = false;
    },
    setPinned(next: boolean, g: ScrollGeometry): void {
      pinned = next;
      intent = 0;
      gesture = 0;
      pendingGesture = false;
      lastTop = g.scrollTop;
      lastMax = g.scrollHeight - g.clientHeight;
      overshoot = 0;
      depth = 0;
      deepestSeen = 0;
    },
  };
}
