// Setup for the WebKit browser-test project. Loads the app stylesheet so real
// Tailwind utilities (the clip `max-height`, mask gradients, `container-type`)
// are actually applied in the page — the whole point of testing in a real
// engine. Deliberately does NOT load tests/setup.ts: those jsdom polyfills
// (ResizeObserver/matchMedia/scrollIntoView) are real here.
//
// Determinism contract for every browser test: real layout and real
// `ResizeObserver` settle ASYNCHRONOUSLY. Assert by POLLING the measured value
// (`expect.poll`, `expect.element`, or `vi.waitFor`) until it converges — never
// a fixed `sleep`/`waitForTimeout`. A fixed delay is the difference between a
// reliable suite and a flaky one; the throwaway spike only used fixed waits
// because it was throwaway.
import "../src/app.css";

// WebKit raises a benign "ResizeObserver loop completed with undelivered
// notifications" when an observer callback itself mutates layout — which our
// `measureClip`/re-anchor observers do by design. @vitest/browser's error
// catcher already declines to FAIL on it (a ResizeObserver ErrorEvent has no
// `.error`, so it's routed to `console.error` rather than reported), but the
// console.error is pure noise that reads like a failure. Filter exactly that
// message; delegate every other error to the (Vitest-patched) console.error so
// real errors still surface.
const realConsoleError = console.error.bind(console);
console.error = (...args: unknown[]): void => {
  const first = args[0];
  const message = first instanceof Error ? first.message : String(first);
  if (message.includes("ResizeObserver loop")) return;
  realConsoleError(...args);
};

// Vite's client also logs unhandled window errors. A capture-phase listener runs
// before its (bubble-phase) handler, so stopping propagation here keeps the
// benign ResizeObserver message out of the terminal entirely.
window.addEventListener(
  "error",
  (event: ErrorEvent) => {
    if (event.message.includes("ResizeObserver loop")) {
      event.stopImmediatePropagation();
      event.preventDefault();
    }
  },
  true,
);
