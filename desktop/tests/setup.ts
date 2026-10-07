import { afterAll } from "vitest";
import "@testing-library/jest-dom/vitest";

// bits-ui's body-scroll-lock schedules a 24ms `window.setTimeout` on unmount to
// reset body styles (see bits-ui#1639). Under jsdom that timer can fire *after*
// Vitest has torn down the file's environment, so `document` is gone and the
// deferred callback throws `ReferenceError: document is not defined` as an
// uncaught exception that fails the entire run — intermittently, depending on
// teardown timing. Waiting out the delay once per file lets any pending cleanup
// run while `document` still exists. The flush must be a real wait: the timer is
// already scheduled on the real clock by the time this hook runs, so fake-timer
// flushing can't reach it. Cost is one short wait per test file (~61), not per
// test.
const BODY_SCROLL_LOCK_CLEANUP_MS = 24;
afterAll(async () => {
  await new Promise((resolve) => setTimeout(resolve, BODY_SCROLL_LOCK_CLEANUP_MS + 25));
});

// jsdom ships no `matchMedia`; the theme store needs it. Default to light
// (no dark preference); individual tests override `window.matchMedia` to
// drive `prefers-color-scheme`.
if (typeof window !== "undefined" && !window.matchMedia) {
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

// jsdom ships no `ResizeObserver`; bits-ui (via `runed`) uses it for
// positioning anything that mounts in a Portal (Tooltip / Popover content).
// No-op polyfill is sufficient — tests don't depend on observed resize
// events, just on the constructor existing so the component can mount
// and unmount cleanly.
if (typeof window !== "undefined" && !window.ResizeObserver) {
  window.ResizeObserver = class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  } as unknown as typeof ResizeObserver;
}

// jsdom doesn't implement layout, so `Element.scrollIntoView` is absent; the
// command palette calls it to keep the highlighted row visible during keyboard
// navigation. No-op polyfill is enough — tests don't assert scroll position.
if (typeof Element !== "undefined" && !Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = function (): void {};
}
