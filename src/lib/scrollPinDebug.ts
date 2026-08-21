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
/// What it answers:
/// - What actually flipped the pin? Every transition dumps the interleaved
///   tail of inputs, samples and writes that led up to it — the three streams
///   read TOGETHER are what separates the user from the engine, which is the
///   whole design of the tracker.
/// - Does the pin ever flip without matching input? Under the input-primary
///   design nothing should: engine motion classifies inert, so a transition
///   with no input in the tail is a bug, and the tail shows it.
/// - Do scrollbar-thumb drags reach the DOM as pointer events? That decides
///   whether the deliberately-unsupported evidence-free escape can come back
///   with real provenance instead of geometry inference. The probe logs raw
///   press coordinates because macOS overlay scrollbars are painted over the
///   content, so no `offsetX`-vs-`clientWidth` test can identify one.

import type { ScrollAttribution, ScrollGeometry } from "$lib/scrollPin";

/// Which scroller a sample came from: `"outer"` for the transcript itself, or
/// `"cap:<preview key>"` for one streaming fan-out column's capped live region.
/// Columns pin independently, so they are counted independently.
export type PinScope = "outer" | `cap:${string}`;

/// In verbose mode, how many samples of each shape print before only the
/// counts keep rising. Unbounded printing perturbs the scroll timing under
/// study (hundreds of console writes a second drops frames in WKWebView).
/// Counts are exact regardless.
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
/// The tracker's position/extent baseline, mirrored so a printed line can show
/// the DELTA and the EXTENT CHANGE the tracker actually branched on. Samples
/// are not the whole story: `notifyProgrammaticWrite` resets that baseline too,
/// and during streaming a write lands on every chunk. Reconstructing deltas
/// from samples alone therefore produced impossible lines — a `+15` delta
/// attributed to an upward scroll — because the write in between was invisible.
/// `debugWrite` keeps this in step; a mirror that drifts is worse than none.
const previous = new Map<PinScope, ScrollGeometry>();
/// Samples since the last write, so a transition line says whether the tracker
/// was measuring against a write's baseline or a real scroll's.
const sinceWrite = new Map<PinScope, number>();

/// Ring buffer of every input, sample and write in the order they happened,
/// with timestamps. The three streams have to be read TOGETHER: a sample's
/// numbers cannot say who moved the view, only that it moved, so the question
/// "was there a wheel event just before this?" is what separates the user from
/// the engine. Buffered rather than printed live because printing every event
/// floods the inspector and perturbs the scroll timing under study; the tail is
/// dumped only when the pin actually flips, which is rare and is exactly the
/// moment worth reading.
interface TraceEntry {
  t: number;
  text: string;
}
const trace: TraceEntry[] = [];
const TRACE_MAX = 400;
const TRACE_TAIL = 24;

function record(text: string): void {
  trace.push({ t: performance.now(), text });
  if (trace.length > TRACE_MAX) trace.shift();
}

function dumpTail(reason: string, count = TRACE_TAIL): void {
  const start = Math.max(0, trace.length - count);
  const slice = trace.slice(start);
  const base = slice[0]?.t ?? 0;
  const body = slice.map((e) => `  +${(e.t - base).toFixed(1)}ms  ${e.text}`).join("\n");
  console.log(`[pin] ===== ${reason} =====\n${body}`);
}

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

function geo(scope: PinScope, g: ScrollGeometry): string {
  const max = g.scrollHeight - g.clientHeight;
  const prev = previous.get(scope);
  const delta = prev === undefined ? 0 : g.scrollTop - prev.scrollTop;
  const growth = prev === undefined ? 0 : max - (prev.scrollHeight - prev.clientHeight);
  return [
    `top=${g.scrollTop.toFixed(1)}`,
    `max=${max}`,
    `gap=${(max - g.scrollTop).toFixed(1)}`,
    `delta=${delta.toFixed(1)}`,
    `growth=${growth}`,
    `sinceWrite=${sinceWrite.get(scope) ?? "-"}`,
  ].join(" ");
}

/// Mirror a programmatic write's effect on the tracker's baseline. Must be
/// called after EVERY `notifyProgrammaticWrite`, for the same reason that call
/// itself is mandatory: the next sample's delta is measured from here.
export function debugWrite(scope: PinScope, g: ScrollGeometry): void {
  if (!enabled) return;
  bump(`${scope}:write`);
  record(`${scope} WRITE  ${geo(scope, g)}`);
  previous.set(scope, { ...g });
  sinceWrite.set(scope, 0);
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
  // Formatted ONCE, before the baseline moves. Computing it again afterwards
  // measured every sample against itself, so every live verbose line printed
  // `delta=0 growth=0` while the ring buffer held the true values — a field
  // that reads as data and is always zero is worse than no field.
  const line = `${scope} ${attribution.padEnd(7)} ${geo(scope, g)}`;
  const pin = transition ? ` PIN ${pinnedBefore}->${pinnedAfter}` : "";
  record(`${line}${pin}`);
  previous.set(scope, { ...g });
  sinceWrite.set(scope, (sinceWrite.get(scope) ?? 0) + 1);
  if (transition) {
    // The whole reason this module exists: dump what led up to it.
    dumpTail(`${scope} ${pinnedBefore ? "UNPIN" : "REPIN"} (${attribution})`);
    return;
  }
  if (verbose && shouldPrint(key)) console.log(`[pin] ${line}`);
}

/// Dump the tail for a pin transition the SAMPLE path cannot see. The main
/// unpin now happens in `notifyGesture`, before any sample exists, so a
/// facility that only watched samples was blind to the design's central
/// event — it promised "every transition dumps its history" and missed the
/// one that matters most.
export function debugTransition(scope: PinScope, reason: string, g: ScrollGeometry): void {
  if (!enabled) return;
  bump(`${scope}:transition`);
  record(`${scope} ${reason} ${geo(scope, g)}`);
  previous.set(scope, { ...g });
  dumpTail(`${scope} ${reason}`);
}

/// Record a decision the CALLER made — a forced pin, a write it chose to skip.
/// Without these the trace shows only what the tracker saw, never what the
/// application did about it, and a follow-write that never happened is
/// indistinguishable from one that happened and was overridden.
export function debugNote(scope: PinScope, label: string, value?: number): void {
  if (!enabled) return;
  bump(`${scope}:note:${label}`);
  record(`${scope} NOTE   ${label}${value === undefined ? "" : ` ${value.toFixed(1)}`}`);
}

/// Record real input the scroller received, with its magnitude. The magnitude
/// matters: a 3px upward tick in the middle of a downward flick is a different
/// event from a deliberate 100px scroll away, and only the raw number tells
/// them apart.
export function debugInput(scope: PinScope, kind: string, value: number): void {
  if (!enabled) return;
  bump(`${scope}:input:${kind}`);
  record(`${scope} INPUT  ${kind} deltaY=${value.toFixed(1)}`);
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
    // NOT a scrollbar test. macOS paints overlay scrollbars OVER the content,
    // so `clientWidth` includes the strip under the thumb and an `offsetX >=
    // clientWidth` check can never fire — an earlier version of this probe
    // reported "content presses only" for exactly that reason, which was the
    // broken check talking, not a finding. Log the raw numbers and judge a
    // press in the rightmost ~15px by eye.
    onScrollbar = event.offsetX >= el.clientWidth - 20;
    moves = 0;
    record(
      `outer POINTERDOWN x=${event.offsetX.toFixed(0)} clientWidth=${el.clientWidth} rectWidth=${el.getBoundingClientRect().width.toFixed(0)}`,
    );
  };
  const move = (event: PointerEvent): void => {
    if (!enabled || !onScrollbar || moves >= 3) return;
    moves += 1;
    record(`outer POINTERMOVE(near right edge) y=${event.clientY.toFixed(0)}`);
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
  /// Dump the recorded tail on demand, for when something looked wrong but the
  /// pin never flipped.
  tail(count?: number): void;
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
    tail(count = TRACE_TAIL): void {
      dumpTail("tail on request", count);
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
      previous.clear();
      sinceWrite.clear();
      trace.length = 0;
    },
  };
}
