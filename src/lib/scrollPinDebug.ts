/// Opt-in diagnostics for the scroll-pin state machine ($lib/scrollPin.ts).
///
/// Every rule in that module is an inference about what the engine did, and
/// twice now a rule that read as obviously correct was wrong in a way only
/// measurement caught: WKWebView turned out to report rubber-band positions
/// tens of pixels outside [0, max], and its spring undershoots the resting
/// position on the way back. Re-instrumenting by hand each time costs an edit,
/// a rebuild and a strip; this keeps the probes in the tree, inert unless a
/// developer asks for them.
///
/// This ships in the production bundle on purpose: the questions above are
/// about the real WebView under real use, and a release build that cannot be
/// asked them is a build that cannot be diagnosed. It costs one flag check per
/// scroll sample when off.
///
/// Enable from the WebView console (no rebuild, no reload):
///
///   switchboardPinDebug.enable()   // persists in localStorage
///   switchboardPinDebug.summary()  // attribution counts per scroller
///   switchboardPinDebug.disable()
///
/// What it answers, in the order the questions are worth asking:
/// - Does the engine's scroll-anchoring adjustment ("anchor") EVER fire here?
///   That rule discards downward movement, which is what broke re-pinning
///   mid-stream, and the gesture-evidence machinery exists solely to override
///   it. A zero count over real use means both can go.
/// - What actually unpins a following reader? Every pin transition is logged
///   with the geometry that produced it, so an auto-follow drop-out names its
///   own cause instead of being reconstructed from memory.
/// - Does WebKit dispatch pointer events for scrollbar-thumb drags? If it
///   does, drags can carry input provenance the way the wheel does; if it
///   doesn't, that limit is a fact rather than an assumption.

import type { ScrollAttribution, ScrollGeometry } from "$lib/scrollPin";

/// Which scroller a sample came from: `"outer"` for the transcript itself, or
/// `"cap:<preview key>"` for one streaming fan-out column's capped live region.
/// Columns pin independently, so they are counted independently.
export type PinScope = "outer" | `cap:${string}`;

/// How many samples of each shape print before only the counts keep rising.
/// The open question this module exists to answer is a FREQUENCY one, and
/// unbounded printing would answer it destructively: a single bounce is ~30
/// lines, and a per-frame attribution during streaming is hundreds of console
/// writes a second into the Web Inspector, which drops frames in WKWebView and
/// changes the scroll timing under study. Counts are exact regardless; use
/// `verbose()` when the individual samples are what you need.
const PRINT_LIMIT = 5;

const FLAG = "switchboard-debug-pin";

function readFlag(): boolean {
  try {
    return localStorage.getItem(FLAG) === "1";
  } catch {
    // No storage (test environments, restrictive contexts) means no debugging.
    return false;
  }
}

let enabled = readFlag();
let verbose = false;
const counts = new Map<string, number>();
const printed = new Map<string, number>();

function bump(key: string): number {
  const next = (counts.get(key) ?? 0) + 1;
  counts.set(key, next);
  return next;
}

function shouldPrint(key: string): boolean {
  if (verbose) return true;
  const seen = (printed.get(key) ?? 0) + 1;
  printed.set(key, seen);
  return seen <= PRINT_LIMIT;
}

function geo(g: ScrollGeometry): string {
  const max = g.scrollHeight - g.clientHeight;
  return `top=${g.scrollTop.toFixed(1)} max=${max} gap=${(max - g.scrollTop).toFixed(1)}`;
}

/// Record one classified sample. Counts everything; prints pin transitions
/// always (they are rare, and they are what users report) and the first few of
/// each engine attribution.
export function debugSample(
  scope: PinScope,
  attribution: ScrollAttribution,
  g: ScrollGeometry,
  pinnedBefore: boolean,
  pinnedAfter: boolean,
): void {
  if (!enabled) return;
  const key = `${scope}:${attribution}`;
  bump(key);
  const transition = pinnedBefore !== pinnedAfter;
  if (!transition) {
    if (attribution !== "anchor" && attribution !== "elastic") return;
    if (!shouldPrint(key)) return;
  }
  const pin = transition ? ` PIN ${pinnedBefore}->${pinnedAfter}` : "";
  console.log(`[pin] ${scope} ${attribution} ${geo(g)}${pin}`);
}

/// Record real input the scroller received, so a sample that follows can be
/// read against what the user actually did. Counted always, printed only in
/// verbose mode — wheel events arrive per frame.
export function debugInput(scope: PinScope, kind: string): void {
  if (!enabled) return;
  const key = `${scope}:input:${kind}`;
  bump(key);
  if (verbose) console.log(`[pin] ${key}`);
}

/// Probe for scrollbar-thumb drags: are they dispatched to the DOM at all, and
/// do they carry coordinates that identify them? A press whose x lands beyond
/// `clientWidth` is on the scrollbar, not the content. Returns undefined (and
/// attaches nothing) unless debugging is on at mount.
///
/// `disable()` silences these handlers immediately; it does not detach them —
/// they go at unmount. That narrower contract is deliberate: a detach registry
/// for a developer probe buys nothing a reload doesn't.
export function attachPointerProbe(el: HTMLElement): (() => void) | undefined {
  if (!enabled) return undefined;
  let onScrollbar = false;
  let moves = 0;
  const down = (event: PointerEvent): void => {
    if (!enabled) return;
    onScrollbar = event.offsetX >= el.clientWidth;
    moves = 0;
    console.log(
      `[pin] outer pointerdown x=${event.offsetX.toFixed(0)} clientWidth=${el.clientWidth} onScrollbar=${onScrollbar}`,
    );
  };
  const move = (event: PointerEvent): void => {
    if (!enabled || !onScrollbar || moves >= 3) return;
    moves += 1;
    console.log(`[pin] outer pointermove(scrollbar) y=${event.clientY.toFixed(0)}`);
  };
  const up = (): void => {
    onScrollbar = false;
  };
  el.addEventListener("pointerdown", down, { passive: true });
  el.addEventListener("pointermove", move, { passive: true });
  el.addEventListener("pointerup", up, { passive: true });
  return () => {
    el.removeEventListener("pointerdown", down);
    el.removeEventListener("pointermove", move);
    el.removeEventListener("pointerup", up);
  };
}

interface PinDebugConsole {
  enable(): void;
  disable(): void;
  /// Print every sample instead of the first few of each shape. Perturbs the
  /// timing it measures; use for a specific interaction, not a session.
  verbose(on: boolean): void;
  summary(): Record<string, number>;
  reset(): void;
}

declare global {
  var switchboardPinDebug: PinDebugConsole | undefined;
}

if (typeof globalThis !== "undefined") {
  globalThis.switchboardPinDebug = {
    enable(): void {
      enabled = true;
      try {
        localStorage.setItem(FLAG, "1");
      } catch {
        // Session-only debugging is still useful.
      }
      console.log("[pin] debugging on — reload to arm the pointer probe");
    },
    verbose(on: boolean): void {
      verbose = on;
    },
    disable(): void {
      enabled = false;
      verbose = false;
      try {
        localStorage.removeItem(FLAG);
      } catch {
        // Nothing persisted, nothing to remove.
      }
    },
    summary(): Record<string, number> {
      return Object.fromEntries([...counts.entries()].sort(([a], [b]) => a.localeCompare(b)));
    },
    reset(): void {
      counts.clear();
      printed.clear();
    },
  };
}
