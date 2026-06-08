import "@testing-library/jest-dom/vitest";

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
