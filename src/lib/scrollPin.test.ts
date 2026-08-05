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
