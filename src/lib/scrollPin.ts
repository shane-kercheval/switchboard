/// Pin-state tracker shared by the transcript's bottom-follow scrollers — the
/// outer transcript container and each streaming turn's capped live region.
///
/// DESIGN: input is ground truth for intent; geometry is only a location.
///
/// Three actors move `scrollTop`: the user, the engine (momentum, rubber-band
/// springs, clamps after a collapse, its own anchoring adjustments), and our
/// follow-writes. An earlier version of this module tried to attribute each
/// scroll sample to one of them from geometry deltas alone. Five successive
/// fixes each handled a measured case and met a new one, because geometry
/// fundamentally cannot say WHO moved the view — only that it moved. The
/// decisive capture (traced in the Tauri WKWebView with an interleaved
/// input/sample/write log): a quarter-second of exclusively DOWNWARD trackpad
/// momentum, a chunk growing the document, our follow-write to the new bottom,
/// and then the view 57px above that write — the trackpad's bounce spring
/// relaxing toward the OLD bottom, ignoring our write entirely. No geometry
/// rule can tell that from a user scrolling up; the wheel stream can, trivially:
/// there was no upward input.
///
/// The rules:
/// - UNPIN only on real upward input: net upward wheel deltas crossing
///   UNPIN_INPUT_EPS flip `pinned` off immediately, in the input handler,
///   before any sample or write can interfere. Accumulated as INPUT pixels,
///   not scroll positions, so follow-writes cannot reset a slow escape — the
///   original escape-velocity bug is structurally impossible here. A downward
///   input resets the accumulation (measured: macOS momentum never injects
///   opposite-sign ticks, so a genuine reversal is the only source of sign
///   changes).
/// - Engine motion can never unpin. Springs, clamps, anchoring adjustments and
///   overridden writes arrive with no matching input, classify as "none", and
///   leave both the pin and the caller's anchor/gap bookkeeping untouched. The
///   cost of a wrongly-KEPT pin is one frame of jitter that the next
///   follow-write self-heals; the cost of a wrongly-DROPPED pin is auto-follow
///   silently off until the user notices. That asymmetry is the whole design.
/// - A sample is user movement only when its direction matches pending input
///   (`sign(delta) === sign(gesture)`). Matched movement refreshes the
///   caller's reading anchor and drives re-pinning; everything else is inert.
/// - RE-PIN on matched downward movement landing within REPIN_THRESHOLD of the
///   bottom — the current bottom, or the bottom as of the previous baseline
///   (content that arrived between the gesture and its classification moves
///   the target; the user still reached the bottom they were aiming at; the
///   previous-bottom measure is clamped non-negative because landing far PAST
///   it is an engine signature).
/// - INTENT and PROVENANCE are separate inputs, because they answer different
///   questions and one of them is dangerous to fake. `notifyIntent` says "the
///   user pushed this way" — it drives the unpin accumulator and nothing else.
///   `notifyGesture` says that AND "movement is about to arrive here", arming
///   the evidence a sample matches against. Input that cannot move THIS
///   scroller must never arm the second: a wheel the nested cap consumed, or
///   a downward wheel on a view already at the bottom, would leave evidence
///   for a movement that never comes, and a later engine adjustment in the
///   same direction claims it — the sample returns "up"/"down" and the caller
///   overwrites its reading anchor from motion the user never made. That is
///   why the two exist; collapsing them back into one call reintroduces it.
/// - Input evidence survives `notifyProgrammaticWrite` but NOT indefinitely.
///   A wheel is dispatched before the engine applies its scroll, and a chunk's
///   correction routinely runs in between, so evidence must outlive both the
///   write and a movement-free sample or the gesture is erased (that erasure
///   is the original wheel-vs-scrollbar asymmetry, and it drops re-pinning
///   from the REPIN_THRESHOLD path to the flush-at-bottom one). But unbounded
///   evidence is not safe either: stale input can match a LATER engine
///   movement, and while that cannot flip the pin, the "up"/"down" it returns
///   makes the caller overwrite its reading anchor and stored gap from motion
///   the user never made — corruption whose symptom surfaces later, as a
///   restore to the wrong place. So evidence expires after GESTURE_GRACE
///   movement-free samples: the wheel's own scroll lands within a rendering
///   step, so anything later is not it. That budget is spent by SCROLL-EVENT
///   samples only, which is what makes the sentence above true: the browser
///   coalesces `scroll` to one per element per rendering update, so counting
///   those counts rendering steps. Layout and content passes sample far more
///   often — two per streamed chunk here — and aging on those made one chunk
///   arriving before the wheel landed spend the whole budget, which is the
///   "scrolled to the bottom, following did not resume" bug wearing a
///   different hat. Passes still CLASSIFY, and still consume the evidence
///   when they observe matched movement; they just do not age it.
///
/// NOT SUPPORTED, deliberately: an evidence-free escape. Scrollbar-thumb drags
/// and track clicks emit no wheel events, so nothing distinguishes them from
/// engine motion except geometry — and every attempt to make that distinction
/// failed against the measured engine:
/// - "Displacement persists across samples" was self-fulfilling: the caller
///   had to hold its follow-write still to observe, and the held write
///   guaranteed the displacement it was measuring. One stationary artifact
///   could complete a run inside a single frame (a `scroll` event, the
///   content pass and the ResizeObserver pass are three samples, not three
///   frames) and strand a reader at the top with auto-follow off.
/// - "Displacement recurs after a correction" fails against the founding
///   capture itself: the spring IS a thing that comes back after a write.
/// - Gating either on "input has been quiet" needs a measure of when momentum
///   settles. Sample counts are not that measure — their cadence is set by
///   chunk arrival and layout passes, not by spring physics — and wall-clock
///   is a guess this file has no measurement for.
/// A mouse user reaches history with the wheel, which is input and is covered.
/// The uncovered interaction is specifically dragging the thumb. Restoring it
/// needs real provenance (do pointer events fire for the thumb? the debug
/// module's probe exists to answer that), not another geometry inference.
///
/// The one concession: a downward move landing FLUSH at the bottom re-pins
/// without evidence (`gap < REPIN_EXACT`). This is a deliberate tradeoff, not
/// an inference — nothing else re-engages a thumb-dragger who returns to the
/// end, and its failure mode (an engine landing coincidentally flush re-pins a
/// reader) costs a reading position, since the next chunk then writes to the
/// bottom. Named here so it is weighed rather than trusted.
export interface ScrollGeometry {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

/// How a scroll sample was read. "up"/"down" are user movement — direction
/// matched pending input — and callers re-capture their reading anchor on
/// them; "none" is everything else (engine motion, our own writes' echoes,
/// unmatched movement) and must leave caller bookkeeping untouched.
export type ScrollAttribution = "up" | "down" | "none";

/// Where a sample came from. See `onScrollEvent`.
export type SampleSource = "scroll" | "pass";

export interface PinTracker {
  readonly pinned: boolean;
  /// Whether recorded input evidence is still waiting to be attributed.
  /// Callers read it to decide whether to sample geometry ahead of a
  /// correction (the pre-sample that keeps a chunk's follow-write from
  /// erasing the movement a pending `scroll` event is about to report).
  readonly gesturePending: boolean;
  /// Classify a scroll sample. `source` is REQUIRED and not defaulted: only
  /// `"scroll"` (the scroller's own `scroll` listener) ages input evidence,
  /// because only that is coalesced to one per rendering update. `"pass"` is
  /// a caller-driven observation — a pre-correction sample, a layout pass —
  /// which classifies and may consume evidence but must not spend its
  /// lifetime. A permissive default here is how an earlier version of this
  /// interface let a forgetful caller silently pick the unsafe branch.
  onScrollEvent(g: ScrollGeometry, source: SampleSource): ScrollAttribution;
  /// Record input DIRECTION only: it feeds the unpin accumulator and may flip
  /// the pin, but arms no movement evidence. For input this scroller cannot
  /// act on — a wheel a nested region consumed, a downward wheel at the
  /// bottom — where the direction is still the user's intent but no movement
  /// will follow (see the header's intent-vs-provenance rule).
  notifyIntent(delta: number): boolean;
  /// Record input this scroller is expected to MOVE on: `notifyIntent` plus
  /// the evidence a following sample matches against. Wheel/trackpad only;
  /// keyboard scrolling is deliberately unsupported and scrollbars emit no
  /// input (see the header). Signed like `scrollTop`: positive drives toward
  /// the bottom. Both return true if the call flipped the pin, so the caller
  /// can record the transition: the main unpin happens HERE, not in a sample,
  /// and a public state change hidden behind a void return is one the next
  /// caller gets wrong.
  notifyGesture(delta: number): boolean;
  /// Record the geometry the scroller just wrote itself, so the write's echo
  /// computes a zero delta and changes nothing. Must be called after EVERY
  /// programmatic scrollTop write.
  notifyProgrammaticWrite(g: ScrollGeometry): void;
  /// Force a pin transition the application decided on (first conversation
  /// rows pin, a send, a navigator jump). Snapshots the given geometry.
  setPinned(pinned: boolean, g: ScrollGeometry): void;
}

/// How close to the bottom matched downward movement must land to re-pin.
export const REPIN_THRESHOLD = 32;

/// How close counts as FLUSH at the bottom for the evidence-free re-pin.
export const REPIN_EXACT = 2;

/// Net upward input (event units, ~px) that turns following off. Measured: a
/// deliberate slow trackpad escape arrives as ticks of −1, −2, −3 …, so the
/// threshold must be small; measured equally: downward momentum never injects
/// upward ticks, so it can be small safely.
export const UNPIN_INPUT_EPS = 2;

/// Movement-free samples input evidence survives before expiring (header).
export const GESTURE_GRACE = 2;

export function createPinTracker(): PinTracker {
  let pinned = true;
  let lastTop = 0;
  let lastMax = 0;
  // Net signed input awaiting attribution; sign is matched against movement.
  let gesture = 0;
  let pendingGesture = false;
  // Movement-free samples this evidence has survived (see GESTURE_GRACE).
  let idleSamples = 0;
  // Cumulative upward input in the current upward run (reset by downward
  // input). Compared against UNPIN_INPUT_EPS; deliberately NOT reset by
  // samples or writes, so slow escapes accumulate across follow-writes.
  let upRun = 0;

  /// Shared by both input entry points; a closure rather than `this.` so a
  /// destructured `notifyGesture` cannot lose its binding.
  function applyIntent(delta: number): boolean {
    if (delta === 0) return false;
    if (delta > 0) {
      // Downward input ends any upward run — including input this scroller
      // cannot act on, which is the only reason the intent entry point takes
      // a signed magnitude rather than a direction.
      upRun = 0;
      return false;
    }
    upRun += -delta;
    if (!pinned || upRun <= UNPIN_INPUT_EPS) return false;
    // Unpin in the input handler, before any sample or write can get in the
    // way. This is the moment the user asked to stop following.
    pinned = false;
    return true;
  }

  function clearGesture(): void {
    gesture = 0;
    pendingGesture = false;
    idleSamples = 0;
  }

  return {
    get pinned(): boolean {
      return pinned;
    },
    get gesturePending(): boolean {
      return pendingGesture;
    },
    onScrollEvent(g: ScrollGeometry, source: SampleSource): ScrollAttribution {
      const max = g.scrollHeight - g.clientHeight;
      const gap = max - g.scrollTop;
      const delta = g.scrollTop - lastTop;
      // The bottom as of the previous baseline — the one the user was moving
      // toward if content grew under the gesture. Negative means the view
      // landed far past it, which only the engine does.
      const gapBefore = lastMax - g.scrollTop;
      let result: ScrollAttribution = "none";
      const matched = pendingGesture && delta !== 0 && Math.sign(delta) === Math.sign(gesture);
      if (matched) {
        if (delta > 0) {
          result = "down";
          upRun = 0;
          if (
            !pinned &&
            (gap < REPIN_THRESHOLD || (gapBefore >= 0 && gapBefore < REPIN_THRESHOLD))
          ) {
            pinned = true;
          }
        } else {
          result = "up";
        }
        clearGesture();
      } else if (!pinned && !pendingGesture && delta > 0 && gap < REPIN_EXACT) {
        // The named concession (header): arriving flush at the end re-engages
        // following whatever moved the view there.
        result = "down";
        upRun = 0;
        pinned = true;
      } else if (pendingGesture && delta === 0) {
        // The wheel's scroll has not landed yet — keep the evidence, briefly.
        // Only a scroll-event sample spends the budget (header).
        if (source === "scroll") {
          idleSamples += 1;
          if (idleSamples > GESTURE_GRACE) clearGesture();
        }
      } else if (pendingGesture) {
        // Movement that contradicts the input: the engine, and the evidence
        // was for a scroll that never arrived.
        clearGesture();
      }
      lastTop = g.scrollTop;
      lastMax = max;
      return result;
    },
    notifyIntent(delta: number): boolean {
      // A zero delta is not input: horizontal trackpad swipes report one.
      return applyIntent(delta);
    },
    notifyGesture(delta: number): boolean {
      if (delta === 0) return false;
      const flipped = applyIntent(delta);
      gesture += delta;
      pendingGesture = true;
      idleSamples = 0;
      return flipped;
    },
    notifyProgrammaticWrite(g: ScrollGeometry): void {
      lastTop = g.scrollTop;
      lastMax = g.scrollHeight - g.clientHeight;
      // Input evidence deliberately survives (header); the write only resets
      // the position baseline so its echo reads as zero movement.
    },
    setPinned(next: boolean, g: ScrollGeometry): void {
      pinned = next;
      clearGesture();
      upRun = 0;
      lastTop = g.scrollTop;
      lastMax = g.scrollHeight - g.clientHeight;
    },
  };
}
