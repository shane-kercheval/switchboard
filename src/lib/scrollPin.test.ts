import { describe, expect, it } from "vitest";
import {
  createPinTracker,
  REPIN_THRESHOLD,
  SCROLL_EPS,
  type PinTracker,
  type ScrollGeometry,
} from "./scrollPin";

// The tracker is the single attribution state machine behind both the outer
// transcript scroll and each streaming turn's capped live region. These tests
// pin its boundary behavior directly — the component suites (jsdom + real
// WebKit) cover the same contracts end-to-end but cannot exercise fractional
// deltas and interleavings deterministically.

function geometry(scrollTop: number, scrollHeight = 1000, clientHeight = 500): ScrollGeometry {
  return { scrollTop, scrollHeight, clientHeight };
}

/** A tracker pinned at the bottom of the default 1000/500 geometry. */
function pinnedAtBottom(): PinTracker {
  const pin = createPinTracker();
  pin.notifyProgrammaticWrite(geometry(500));
  return pin;
}

describe("upward unpin", () => {
  it("unpins on any upward movement past the epsilon, regardless of distance from the bottom", () => {
    const pin = pinnedAtBottom();
    // 5px up — far inside the re-pin threshold, still a genuine unpin.
    expect(pin.onScrollEvent(geometry(495))).toBe("up");
    expect(pin.pinned).toBe(false);
  });

  it("unpins on a large upward scroll", () => {
    const pin = pinnedAtBottom();
    expect(pin.onScrollEvent(geometry(0))).toBe("up");
    expect(pin.pinned).toBe(false);
  });

  it("stays pinned while upward movement has not exceeded the epsilon", () => {
    const pin = pinnedAtBottom();
    expect(pin.onScrollEvent(geometry(500 - SCROLL_EPS / 2))).toBe("none");
    expect(pin.pinned).toBe(true);
  });
});

describe("upward-intent accumulation", () => {
  it("accumulates sub-epsilon upward movements until they cross the epsilon", () => {
    const pin = pinnedAtBottom();
    expect(pin.onScrollEvent(geometry(499.4))).toBe("none"); // 0.6 accumulated
    expect(pin.pinned).toBe(true);
    expect(pin.onScrollEvent(geometry(498.8))).toBe("up"); // 1.2 > epsilon
    expect(pin.pinned).toBe(false);
  });

  it("accumulates across programmatic follow-writes (streaming re-pin interleaving)", () => {
    // The adversarial loop: a sub-epsilon upward drift, then a streamed chunk
    // whose follow-write resets the position baseline, repeated. Per-event
    // epsilon comparison never unpins here; the accumulator must.
    const pin = pinnedAtBottom();
    expect(pin.onScrollEvent(geometry(499.4))).toBe("none"); // 0.6 up
    // Chunk: content grows, follow-write pins to the new bottom.
    pin.notifyProgrammaticWrite(geometry(700, 1200, 500));
    expect(pin.onScrollEvent(geometry(700, 1200, 500))).toBe("none"); // echo, inert
    expect(pin.onScrollEvent(geometry(699.4, 1200, 500))).toBe("up"); // 1.2 total
    expect(pin.pinned).toBe(false);
  });

  it("oscillating jitter self-cancels and never unpins", () => {
    const pin = pinnedAtBottom();
    for (let i = 0; i < 10; i++) {
      pin.onScrollEvent(geometry(499.5)); // 0.5 up
      pin.onScrollEvent(geometry(500)); // 0.5 down cancels it
    }
    expect(pin.pinned).toBe(true);
  });

  it("a genuine downward scroll clears pending upward intent", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin far up
    pin.onScrollEvent(geometry(99.4)); // 0.6 pending
    expect(pin.onScrollEvent(geometry(200))).toBe("down"); // reversal clears it
    expect(pin.onScrollEvent(geometry(199.4))).toBe("none"); // 0.6 again, not 1.2
    expect(pin.pinned).toBe(false);
  });

  it("accumulates sub-epsilon downward movements symmetrically", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin far up
    expect(pin.onScrollEvent(geometry(100.75))).toBe("none"); // 0.75 down pending
    expect(pin.onScrollEvent(geometry(101.5))).toBe("down"); // 1.5 crosses the epsilon
    expect(pin.pinned).toBe(false); // classified, but far from the bottom
  });

  it("re-pins from a sub-epsilon downward drift into the bottom zone", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(470)); // unpin 30px above the bottom... still >eps up
    expect(pin.pinned).toBe(false);
    expect(pin.onScrollEvent(geometry(470.75))).toBe("none");
    expect(pin.onScrollEvent(geometry(471.5))).toBe("down"); // gap 28.5 < threshold
    expect(pin.pinned).toBe(true);
  });

  it("pending intent survives a clamp", () => {
    const pin = createPinTracker();
    pin.notifyProgrammaticWrite(geometry(1500, 2000, 500));
    pin.onScrollEvent(geometry(1499.4, 2000, 500)); // 0.6 pending
    expect(pin.onScrollEvent(geometry(500, 1000, 500))).toBe("clamp"); // collapse
    expect(pin.pinned).toBe(true);
    expect(pin.onScrollEvent(geometry(499.4, 1000, 500))).toBe("up"); // 1.2 total
    expect(pin.pinned).toBe(false);
  });
});

describe("clamp attribution", () => {
  it("does not unpin when a content collapse shrinks the extent and clamps scrollTop", () => {
    const pin = createPinTracker();
    pin.notifyProgrammaticWrite(geometry(1500, 2000, 500));
    // Collapse: 2000 → 1000 content, browser clamps to the new bottom.
    expect(pin.onScrollEvent(geometry(500, 1000, 500))).toBe("clamp");
    expect(pin.pinned).toBe(true);
  });

  it("does not unpin when viewport growth shrinks the extent and clamps scrollTop", () => {
    const pin = pinnedAtBottom();
    // Viewport 500 → 800 with unchanged content: extent 500 → 200, clamp.
    expect(pin.onScrollEvent(geometry(200, 1000, 800))).toBe("clamp");
    expect(pin.pinned).toBe(true);
  });

  it("unpins on upward movement even while the extent is growing (streaming)", () => {
    const pin = pinnedAtBottom();
    expect(pin.onScrollEvent(geometry(480, 1200, 500))).toBe("up");
    expect(pin.pinned).toBe(false);
  });

  it("an engine anchoring adjustment (downward move + extent growth) is not user movement", () => {
    // WebKit compensates for content growth ABOVE the viewport by moving
    // scrollTop down in the same event the extent grows. Reading that as a
    // user scroll-down would re-pin an unpinned reader (and slam the view)
    // on any expansion above their reading position.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin far up, reading history
    pin.onScrollEvent(geometry(99.4)); // 0.6 pending intent
    // A block above expands by 800px; the engine adjusts scrollTop to match.
    expect(pin.onScrollEvent(geometry(900, 1800, 500))).toBe("anchor");
    expect(pin.pinned).toBe(false);
    // Pending intent survived the adjustment.
    expect(pin.onScrollEvent(geometry(899.4, 1800, 500))).toBe("up");
  });

  it("a gradual resize (1px shrink + 1px clamp per frame) never reads as user scroll", () => {
    // Extents are integer CSSOM values, so a 1px shrink is a real shrink. A
    // shared >1px tolerance on both axes would let each frame's clamp feed the
    // intent total and unpin after two frames of slow pane-dragging.
    const pin = pinnedAtBottom();
    for (let i = 1; i <= 5; i++) {
      expect(pin.onScrollEvent(geometry(500 - i, 1000, 500 + i))).toBe("clamp");
      expect(pin.pinned).toBe(true);
    }
    // A genuine upward scroll right after the run still unpins — the clamp
    // frames neither fed nor destroyed intent.
    expect(pin.onScrollEvent(geometry(450, 1000, 505))).toBe("up");
    expect(pin.pinned).toBe(false);
  });
});

describe("downward re-pin", () => {
  it("re-pins on a genuine downward scroll landing within the threshold", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin far up
    expect(pin.pinned).toBe(false);
    expect(pin.onScrollEvent(geometry(500 - REPIN_THRESHOLD + 1))).toBe("down");
    expect(pin.pinned).toBe(true);
  });

  it("stays unpinned on a downward scroll that stops short of the threshold", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100));
    expect(pin.onScrollEvent(geometry(300))).toBe("down");
    expect(pin.pinned).toBe(false);
  });
});

describe("programmatic writes", () => {
  it("a follow-write's echoed event is inert (zero delta) and cannot re-pin", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(495)); // unpin 5px above the bottom
    // Content grows; the caller gap-holds to 695 of a 1200/500 doc and reports it.
    pin.notifyProgrammaticWrite(geometry(695, 1200, 500));
    // The browser echoes the write as a scroll event: same geometry, no change.
    expect(pin.onScrollEvent(geometry(695, 1200, 500))).toBe("none");
    expect(pin.pinned).toBe(false);
  });

  it("setPinned forces the transition and snapshots (first-rows pin, navigator unpin)", () => {
    const pin = createPinTracker();
    // Navigator jump: unpin at an arbitrary position.
    pin.setPinned(false, geometry(200, 2000, 500));
    expect(pin.pinned).toBe(false);
    expect(pin.onScrollEvent(geometry(200, 2000, 500))).toBe("none");
    // First rows after empty: force pin regardless of position.
    pin.setPinned(true, geometry(200, 2000, 500));
    expect(pin.pinned).toBe(true);
  });
});

describe("re-pinning while the content grows", () => {
  it("a wheeled downward move counts as the user even though the extent grew", () => {
    // The streaming case: the extent grows on nearly every chunk, so a reader
    // scrolling back down races growth in almost every event. Without the
    // gesture evidence every one of those events reads as an engine anchoring
    // adjustment and the reader can never return to the bottom.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    expect(pin.pinned).toBe(false);
    // Wheel down 200px while a chunk grows the extent by 20px.
    pin.notifyGesture(200);
    expect(pin.onScrollEvent(geometry(300, 1020, 500))).toBe("down");
    expect(pin.pinned).toBe(false); // still far from the bottom
    // …and again, landing inside the threshold: this must re-pin mid-stream.
    pin.notifyGesture(240);
    expect(pin.onScrollEvent(geometry(540, 1060, 500))).toBe("down");
    expect(pin.pinned).toBe(true);
  });

  it("reads an ungestured downward move during growth as the engine anchoring", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    // 800px of growth above the viewport, 800px of compensation, no gesture.
    expect(pin.onScrollEvent(geometry(900, 1800, 500))).toBe("anchor");
    expect(pin.pinned).toBe(false);
  });

  it("does not carry evidence past a sample that saw no movement", () => {
    // Evidence for a scroll this scroller never received — a wheel consumed by
    // a nested region, say — must not attach itself to whatever engine
    // movement happens next. Nothing is lost: the no-movement sample also
    // absorbs the extent, so a real event that follows has no growth left to
    // explain and classifies without evidence.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    pin.notifyGesture(200);
    expect(pin.onScrollEvent(geometry(100))).toBe("none");
    expect(pin.gesturePending).toBe(false);
    expect(pin.onScrollEvent(geometry(900, 1800, 500))).toBe("anchor");
    expect(pin.pinned).toBe(false);
  });

  it("a misattributed engine adjustment cannot re-pin on the previous bottom", () => {
    // Belt and braces for the case above: even if evidence did survive, an
    // adjustment that lands far past the previous bottom must not satisfy the
    // re-pin leniency — an unbounded measure made every misattribution a slam.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    pin.notifyGesture(200);
    expect(pin.onScrollEvent(geometry(900, 1800, 500))).toBe("down");
    expect(pin.pinned).toBe(false); // 400px from the bottom, 400 past the old one
  });

  it("a zero delta is not input and does not arm the pre-correction sample", () => {
    // A horizontal trackpad swipe reports deltaY 0; arming on it opens the
    // sample the caller's gate exists to keep shut for engine-only movement.
    const pin = pinnedAtBottom();
    pin.notifyGesture(0);
    expect(pin.gesturePending).toBe(false);
  });

  it("a programmatic write clears pending evidence", () => {
    const pin = pinnedAtBottom();
    pin.notifyGesture(200);
    expect(pin.gesturePending).toBe(true);
    pin.notifyProgrammaticWrite(geometry(500));
    expect(pin.gesturePending).toBe(false);
    expect(pin.onScrollEvent(geometry(900, 1800, 500))).toBe("anchor");
  });

  it("spends the gesture on one classification, not every later event", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    pin.notifyGesture(200);
    expect(pin.onScrollEvent(geometry(300, 1020, 500))).toBe("down");
    // A later engine adjustment must not inherit the spent gesture.
    expect(pin.onScrollEvent(geometry(500, 1220, 500))).toBe("anchor");
    expect(pin.pinned).toBe(false);
  });

  it("re-pins on a downward move that reached the bottom the growth then moved", () => {
    // The reader wheels to the bottom of the 1000px document; the chunk that
    // arrives in the same frame makes it 1400px. Measuring only the new gap
    // (400px) would leave them unpinned one chunk short of the end, every
    // chunk, forever.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    pin.notifyGesture(400);
    expect(pin.onScrollEvent(geometry(500, 1400, 500))).toBe("down");
    expect(pin.pinned).toBe(true);
  });

  it("does not re-pin a downward move that stopped short of either bottom", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    pin.notifyGesture(300);
    expect(pin.onScrollEvent(geometry(400, 1400, 500))).toBe("down");
    expect(pin.pinned).toBe(false); // 100px short of the old bottom
  });

  it("an upward gesture does not license a downward move", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    pin.notifyGesture(-30); // wheeling up while content grows above
    expect(pin.onScrollEvent(geometry(900, 1800, 500))).toBe("anchor");
    expect(pin.pinned).toBe(false);
  });
});

describe("elastic overscroll", () => {
  it("a flick against the bottom and its spring back leave the pin alone", () => {
    // macOS rubber-banding reports positions past `max` and then decays back.
    // The excursion moves down and the spring back moves up, so scoring them
    // as user movement unpins a reader who did nothing but flick downward at
    // the end of the transcript — the reported "streaming stopped following"
    // symptom.
    const pin = pinnedAtBottom();
    for (const top of [540, 520, 505, 501.5, 500.4, 500]) {
      expect(pin.onScrollEvent(geometry(top))).toBe("elastic");
    }
    expect(pin.pinned).toBe(true);
  });

  it("a genuine scroll up right after the spring back still unpins", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(540)); // 40px excursion past the bottom
    pin.onScrollEvent(geometry(500)); // spring back
    // Inside the excursion's own settle window, an evidence-free move up is
    // indistinguishable from the undershoot — but a real escape after a
    // trackpad bounce arrives with wheel events, and those end the settle.
    pin.notifyGesture(-20);
    expect(pin.onScrollEvent(geometry(480))).toBe("up");
    expect(pin.pinned).toBe(false);
  });

  it("sizes the settle window to the excursion, so a hard flick's undershoot holds", () => {
    // The undershoot is a spring response: it scales with the energy of the
    // flick. A fixed window sized from one gentle capture would let a hard
    // flick's tail through as a scroll up — the measured bug, returning for
    // hard flicks only.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(700)); // 200px past the bottom
    pin.onScrollEvent(geometry(560));
    expect(pin.onScrollEvent(geometry(485))).toBe("elastic"); // 15px undershoot
    expect(pin.pinned).toBe(true);
  });

  it("a settle cannot swallow movement once the reader scrolls against it", () => {
    // The swallow is what makes a follow-write slam a reader who moved away:
    // while the latch holds the tracker still reports pinned, so the next
    // chunk writes them back to the bottom.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(700)); // deep excursion arms a wide window
    pin.onScrollEvent(geometry(500));
    pin.notifyGesture(-10);
    expect(pin.onScrollEvent(geometry(490))).toBe("up");
    expect(pin.pinned).toBe(false);
  });

  it("an evidence-free move outside the settle window still unpins", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(510)); // 10px excursion
    pin.onScrollEvent(geometry(500));
    expect(pin.onScrollEvent(geometry(485))).toBe("up"); // 15px > 10px window
    expect(pin.pinned).toBe(false);
  });

  it("the measured WebKit bounce, replayed frame by frame, never unpins", () => {
    // Captured from the Tauri WKWebView: a trackpad flick against the bottom
    // of a live transcript (max 153). The tail is the part that matters — the
    // spring undershoots the rest position to 150, and under a 2px settle
    // window that single event unpinned a following reader, which is the
    // reported "streaming stopped following".
    const bounce = [
      157, 161, 174, 181, 185, 187, 185, 183, 181, 178, 176, 173, 171, 169, 167, 165, 163, 162, 161,
      159, 158, 157, 156, 155, 154, 153, 152, 150,
    ];
    const pin = createPinTracker();
    pin.notifyProgrammaticWrite(geometry(153, 753, 600));
    for (const top of bounce) {
      expect(pin.onScrollEvent(geometry(top, 753, 600))).toBe("elastic");
    }
    expect(pin.pinned).toBe(true);
  });

  it("the measured top-edge bounce does not disturb an unpinned reader", () => {
    const pin = createPinTracker();
    pin.notifyProgrammaticWrite(geometry(153, 753, 600));
    pin.onScrollEvent(geometry(0, 753, 600)); // scroll to the top, unpinned
    expect(pin.pinned).toBe(false);
    for (const top of [-4, -6, -7, -8, -7, -5, -3, -1, 0, 1]) {
      expect(pin.onScrollEvent(geometry(top, 753, 600))).toBe("elastic");
    }
    expect(pin.pinned).toBe(false);
  });

  it("the elastic re-pin clears pending intent, like every other transition", () => {
    // Leftover upward drift would cross the epsilon on the next jitter event
    // and unpin one event later — auto-follow dropping out for no reason,
    // which is the symptom this re-pin exists to fix.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    pin.onScrollEvent(geometry(99.1)); // 0.9 pending upward intent
    pin.notifyGesture(460);
    expect(pin.onScrollEvent(geometry(540))).toBe("elastic");
    expect(pin.pinned).toBe(true);
    expect(pin.onScrollEvent(geometry(499.8))).not.toBe("up");
    expect(pin.pinned).toBe(true);
  });

  it("a collapse's stale out-of-range position never arms a spring window", () => {
    // Three unrelated things put scrollTop outside [0, max]. Only a spring
    // deepens while the extent holds; a collapse's position is merely stale.
    // Sizing the window from position alone let a collapse swallow — and then
    // rewind — hundreds of pixels of a reader's scrolling.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(400)); // unpin, reading history
    // Content collapses: extent 500 → 100, position still at 400.
    expect(pin.onScrollEvent(geometry(400, 600, 500))).toBe("elastic");
    // A second sample of that same stale position sees no extent change. It
    // is not deepening, so it must not be mistaken for a spring.
    expect(pin.onScrollEvent(geometry(400, 600, 500))).toBe("elastic");
    // The clamp lands, and a small genuine scroll right after must register.
    expect(pin.onScrollEvent(geometry(100, 600, 500))).toBe("elastic");
    expect(pin.onScrollEvent(geometry(90, 600, 500))).toBe("up");
    expect(pin.pinned).toBe(false);
  });

  it("a delayed engine adjustment past the bottom never arms a spring window", () => {
    // The extent growth and the position adjustment can land in separate
    // samples; the later one sees no extent change and would otherwise read
    // as a spring.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(400)); // unpin, reading history
    pin.onScrollEvent(geometry(400, 1400, 600)); // growth absorbed, position held
    // The adjustment relocates the view past the end in one jump — from
    // mid-history, where no spring could have started.
    expect(pin.onScrollEvent(geometry(900, 1400, 600))).toBe("elastic");
    expect(pin.onScrollEvent(geometry(800, 1400, 600))).toBe("elastic"); // clamp
    // A 10px scroll right after must register: the window is the floor, not
    // the 100px the adjustment overshot by.
    expect(pin.onScrollEvent(geometry(790, 1400, 600))).toBe("up");
    expect(pin.pinned).toBe(false);
  });

  it("a bounce whose first frame carries growth still sizes its window", () => {
    // Deepening is a max over the excursion, so one extent-stable frame is
    // enough — which is why the growth test is per-frame, not per-excursion.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(540, 1020, 500)); // out of range, but extent grew
    pin.onScrollEvent(geometry(560, 1020, 500)); // deepens, extent stable
    pin.onScrollEvent(geometry(505, 1020, 500)); // spring back
    expect(pin.onScrollEvent(geometry(495, 1020, 500))).toBe("elastic"); // 25px undershoot
    expect(pin.pinned).toBe(true);
  });

  it("contrary input while still out of range is not lost before re-entry", () => {
    // The reader reverses mid-excursion. Their evidence is spent by the
    // out-of-range sample, so it has to shrink the window then and there —
    // otherwise the settle swallows (and the caller rewinds) the return.
    const pin = createPinTracker();
    pin.notifyProgrammaticWrite(geometry(500));
    pin.onScrollEvent(geometry(700)); // 200px excursion
    pin.notifyGesture(-30); // reader wheels up, still out of range
    expect(pin.onScrollEvent(geometry(650))).toBe("elastic");
    expect(pin.onScrollEvent(geometry(490))).toBe("up"); // gap 10 > floor
    expect(pin.pinned).toBe(false);
  });

  it("a flick that carries an unpinned reader past the bottom re-pins", () => {
    // The momentum jump straight from history to past the end: no in-range
    // sample ever lands, so without this the reader finishes at the bottom
    // with auto-follow off.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(100)); // unpin, reading history
    pin.notifyGesture(460);
    expect(pin.onScrollEvent(geometry(540))).toBe("elastic");
    expect(pin.pinned).toBe(true);
  });

  it("an engine adjustment landing past the bottom does not re-pin", () => {
    // Growth above the viewport with a shrink below can push scrollTop past
    // `max` with no user input at all; pinning there would slam a reader who
    // never moved.
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(400)); // unpin, reading history
    expect(pin.onScrollEvent(geometry(900, 1300, 600))).toBe("elastic");
    expect(pin.pinned).toBe(false);
  });

  it("a flick against the top does not read as a downward return", () => {
    const pin = pinnedAtBottom();
    pin.onScrollEvent(geometry(0)); // unpin at the top
    expect(pin.onScrollEvent(geometry(-40))).toBe("elastic");
    expect(pin.onScrollEvent(geometry(-5))).toBe("elastic");
    expect(pin.onScrollEvent(geometry(0))).toBe("elastic");
    expect(pin.pinned).toBe(false);
  });
});
