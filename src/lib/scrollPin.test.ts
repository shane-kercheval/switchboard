import { describe, expect, it } from "vitest";
import {
  createPinTracker,
  GESTURE_GRACE,
  UNPIN_INPUT_EPS,
  type PinTracker,
  type ScrollGeometry,
} from "./scrollPin";

// The tracker is the single pin-intent state machine behind the outer
// transcript scroll and each streaming turn's capped live region. Input is
// ground truth for intent; geometry is only a location — see the module
// header for the measured capture that forced that design. These tests pin
// the contract directly; the WebKit browser suite covers it end-to-end.

function geometry(scrollTop: number, scrollHeight = 1000, clientHeight = 500): ScrollGeometry {
  return { scrollTop, scrollHeight, clientHeight };
}

/** A tracker pinned at the bottom of the default 1000/500 geometry. */
function pinnedAtBottom(): PinTracker {
  const pin = createPinTracker();
  pin.notifyProgrammaticWrite(geometry(500));
  return pin;
}

describe("input-driven unpin", () => {
  it("unpins immediately once net upward input crosses the epsilon", () => {
    const pin = pinnedAtBottom();
    pin.notifyGesture(-1);
    expect(pin.pinned).toBe(true); // 1 <= eps
    pin.notifyGesture(-2);
    expect(pin.pinned).toBe(false); // 3 > eps, no sample needed
  });

  it("accumulates a slow escape across follow-writes (measured tick sizes)", () => {
    // The measured deliberate slow escape arrives as ticks of -1, -2, -3.
    // Input accumulation survives writes by construction, so the original
    // escape-velocity bug (every follow-write resetting the user's progress)
    // is structurally impossible.
    const pin = pinnedAtBottom();
    pin.notifyGesture(-1);
    pin.notifyProgrammaticWrite(geometry(700, 1200, 500)); // chunk follow-write
    pin.onScrollEvent(geometry(700, 1200, 500), "scroll"); // write echo, inert
    pin.notifyGesture(-2);
    expect(pin.pinned).toBe(false);
  });

  it("a downward input resets the upward run", () => {
    const pin = pinnedAtBottom();
    pin.notifyGesture(-2);
    pin.notifyGesture(40); // reversal: user drives down again
    pin.notifyGesture(-2);
    expect(pin.pinned).toBe(true); // 2 <= eps, run restarted
  });

  it("zero deltas are not input", () => {
    const pin = pinnedAtBottom();
    pin.notifyGesture(0);
    expect(pin.gesturePending).toBe(false);
  });

  it("reports whether the input flipped the pin", () => {
    const pin = pinnedAtBottom();
    expect(pin.notifyGesture(-1)).toBe(false); // below the epsilon
    expect(pin.notifyGesture(-2)).toBe(true); // crossed it: the transition
    expect(pin.notifyGesture(-50)).toBe(false); // already unpinned
    expect(pin.notifyGesture(40)).toBe(false);
  });

  it("exposes the constants the component and tests share", () => {
    expect(UNPIN_INPUT_EPS).toBeGreaterThan(0);
    expect(UNPIN_INPUT_EPS).toBeLessThan(4); // must catch -1,-2 tick escapes
  });
});

describe("engine motion can never unpin", () => {
  it("the measured spring-back after a mid-momentum follow-write stays pinned", () => {
    // The decisive capture: exclusively DOWNWARD momentum (+53..+40), a chunk
    // grows the document, our write pins to the new bottom, and the engine's
    // bounce spring then reports the view 57px above the write. No upward
    // input existed; this must not unpin.
    const pin = createPinTracker();
    pin.notifyProgrammaticWrite(geometry(4580, 5180, 600));
    for (const [d, top] of [
      [53, 4642],
      [50, 4644],
      [46, 4643],
      [43, 4637],
      [41, 4632],
    ] as const) {
      pin.notifyGesture(d);
      pin.onScrollEvent(geometry(top, 5180, 600), "scroll");
      expect(pin.pinned).toBe(true);
    }
    // Chunk: content grows 104px, follow-write to the new bottom.
    pin.notifyProgrammaticWrite(geometry(4684, 5284, 600));
    pin.notifyGesture(40); // momentum still downward
    // The spring ignores the write and relaxes toward the old bottom.
    expect(pin.onScrollEvent(geometry(4627, 5284, 600), "scroll")).toBe("none");
    expect(pin.pinned).toBe(true);
  });

  it("a rubber-band excursion and its settle are inert", () => {
    const pin = pinnedAtBottom();
    for (const top of [540, 520, 505, 501, 500, 497, 500]) {
      expect(pin.onScrollEvent(geometry(top), "scroll")).toBe("none");
    }
    expect(pin.pinned).toBe(true);
  });

  it("a collapse clamp is inert", () => {
    const pin = createPinTracker();
    pin.notifyProgrammaticWrite(geometry(1500, 2000, 500));
    // Content collapses 2000 -> 1000; browser clamps to the new bottom.
    expect(pin.onScrollEvent(geometry(500, 1000, 500), "scroll")).toBe("none");
    expect(pin.pinned).toBe(true);
  });

  it("an engine anchoring adjustment is inert", () => {
    const pin = pinnedAtBottom();
    pin.notifyGesture(-5); // unpin, reading history
    expect(pin.pinned).toBe(false);
    pin.onScrollEvent(geometry(100), "scroll");
    // Growth above the viewport; engine moves scrollTop down to compensate.
    expect(pin.onScrollEvent(geometry(900, 1800, 500), "scroll")).toBe("none");
    expect(pin.pinned).toBe(false);
  });

  it("movement opposing the pending input is the engine, not the user", () => {
    // A write raced by momentum: input says down, the view moved up.
    const pin = pinnedAtBottom();
    pin.notifyGesture(40);
    expect(pin.onScrollEvent(geometry(450), "scroll")).toBe("none");
    expect(pin.pinned).toBe(true);
  });
});

describe("re-pin", () => {
  function unpinnedReadingHistory(): PinTracker {
    const pin = pinnedAtBottom();
    pin.notifyGesture(-400);
    pin.onScrollEvent(geometry(100), "scroll");
    expect(pin.pinned).toBe(false);
    return pin;
  }

  it("matched downward movement landing within the threshold re-pins", () => {
    const pin = unpinnedReadingHistory();
    pin.notifyGesture(380);
    expect(pin.onScrollEvent(geometry(480), "scroll")).toBe("down"); // gap 20 < 32
    expect(pin.pinned).toBe(true);
  });

  it("matched downward movement stopping short stays unpinned", () => {
    const pin = unpinnedReadingHistory();
    pin.notifyGesture(200);
    expect(pin.onScrollEvent(geometry(300), "scroll")).toBe("down");
    expect(pin.pinned).toBe(false);
  });

  it("re-pins against the previous bottom when growth raced the gesture", () => {
    // The user scrolled to the bottom of the document they were looking at; a
    // chunk arrived before the scroll event. They reached their target.
    const pin = unpinnedReadingHistory();
    pin.notifyGesture(400);
    expect(pin.onScrollEvent(geometry(500, 1400, 500), "scroll")).toBe("down");
    expect(pin.pinned).toBe(true);
  });

  it("landing far past the previous bottom is not a re-pin (engine signature)", () => {
    const pin = unpinnedReadingHistory();
    pin.notifyGesture(200);
    // 400px past where the bottom was, 400px short of where it is now.
    expect(pin.onScrollEvent(geometry(900, 1800, 500), "scroll")).toBe("down");
    expect(pin.pinned).toBe(false);
  });

  it("input evidence survives movement-free samples, then expires", () => {
    // It must outlive the pre-correction sample that runs before the engine
    // applies the wheel's scroll — otherwise the gesture is erased and re-pin
    // drops to the flush-at-bottom path. It must NOT outlive that window:
    // stale evidence matching a later engine movement makes the caller
    // overwrite its reading anchor from motion the user never made.
    const pin = unpinnedReadingHistory();
    pin.notifyGesture(400);
    for (let i = 0; i < GESTURE_GRACE; i++) {
      expect(pin.onScrollEvent(geometry(100), "scroll")).toBe("none"); // no movement yet
    }
    expect(pin.onScrollEvent(geometry(480), "scroll")).toBe("down"); // the real movement
    expect(pin.pinned).toBe(true);
  });

  it("expired evidence cannot be claimed by a later engine movement", () => {
    const pin = unpinnedReadingHistory();
    pin.notifyGesture(400);
    for (let i = 0; i < GESTURE_GRACE + 1; i++) {
      pin.onScrollEvent(geometry(100), "scroll");
    }
    expect(pin.gesturePending).toBe(false);
    // A later engine adjustment moves the view downward on its own.
    expect(pin.onScrollEvent(geometry(300), "scroll")).toBe("none");
    expect(pin.pinned).toBe(false);
  });

  it("movement contradicting the input discards the evidence", () => {
    const pin = unpinnedReadingHistory();
    pin.notifyGesture(400); // user asks to go down
    expect(pin.onScrollEvent(geometry(50), "scroll")).toBe("none"); // engine moved it up
    expect(pin.gesturePending).toBe(false);
  });

  it("input evidence survives a follow-write that lands first", () => {
    // Wheel is dispatched before the engine applies the scroll; a chunk's
    // correction runs in between. Clearing evidence at the write would erase
    // the gesture and the re-pin (the original wheel-vs-scrollbar asymmetry).
    const pin = unpinnedReadingHistory();
    pin.notifyGesture(400);
    pin.notifyProgrammaticWrite(geometry(100, 1100, 500)); // gap-hold write
    expect(pin.onScrollEvent(geometry(570, 1100, 500), "scroll")).toBe("down");
    expect(pin.pinned).toBe(true);
  });

  it("an evidence-free arrival flush at the bottom re-pins (scrollbar drag)", () => {
    const pin = unpinnedReadingHistory();
    expect(pin.onScrollEvent(geometry(499.5), "scroll")).toBe("down"); // gap 0.5 < 2
    expect(pin.pinned).toBe(true);
  });

  it("an evidence-free downward move short of the bottom does not re-pin", () => {
    const pin = unpinnedReadingHistory();
    expect(pin.onScrollEvent(geometry(300), "scroll")).toBe("none");
    expect(pin.pinned).toBe(false);
  });
});

describe("no evidence-free escape", () => {
  it("sustained displacement with no input never unpins", () => {
    // Deliberately unsupported (see the module header): every geometry-only
    // rule tried here either proved itself by suppressing the correction that
    // would falsify it, or fell to the measured spring, which is exactly a
    // displacement that returns after a write.
    const pin = pinnedAtBottom();
    for (let i = 0; i < 10; i++) {
      expect(pin.onScrollEvent(geometry(300 - i), "scroll")).toBe("none");
    }
    expect(pin.pinned).toBe(true);
  });

  it("a mid-flush remount clamp leaves a follower following", () => {
    // The artifact that used to strand a reader at the top: scrollTop reads 0
    // for a pass with the extent already recovered, and no write intervenes.
    const pin = pinnedAtBottom();
    for (let i = 0; i < 5; i++) {
      expect(pin.onScrollEvent(geometry(0), "scroll")).toBe("none");
    }
    expect(pin.pinned).toBe(true);
  });

  it("the measured spring's return after a write does not unpin", () => {
    // The founding capture, and the reason "displacement recurs after a
    // correction" cannot be evidence: the spring returns after every write.
    const pin = createPinTracker();
    pin.notifyProgrammaticWrite(geometry(4580, 5180, 600));
    for (let round = 0; round < 3; round++) {
      pin.notifyProgrammaticWrite(geometry(4684 + round, 5284 + round, 600));
      expect(pin.onScrollEvent(geometry(4627, 5284 + round, 600), "scroll")).toBe("none");
    }
    expect(pin.pinned).toBe(true);
  });
});

describe("programmatic writes and forced transitions", () => {
  it("a write's echo is inert", () => {
    const pin = pinnedAtBottom();
    pin.notifyGesture(-5);
    expect(pin.pinned).toBe(false);
    pin.notifyProgrammaticWrite(geometry(695, 1200, 500));
    expect(pin.onScrollEvent(geometry(695, 1200, 500), "scroll")).toBe("none");
    expect(pin.pinned).toBe(false);
  });

  it("setPinned forces the transition and clears accumulated state", () => {
    const pin = createPinTracker();
    pin.setPinned(false, geometry(200, 2000, 500)); // navigator jump
    expect(pin.pinned).toBe(false);
    expect(pin.onScrollEvent(geometry(200, 2000, 500), "scroll")).toBe("none");
    pin.notifyGesture(-1);
    pin.setPinned(true, geometry(200, 2000, 500)); // first rows after empty
    expect(pin.pinned).toBe(true);
    pin.notifyGesture(-2);
    expect(pin.pinned).toBe(true); // upRun was reset; 2 <= eps
  });
});
