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
