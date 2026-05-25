import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The store reads localStorage + matchMedia at module load and holds module
// state, so each test sets the environment, then `vi.resetModules()` +
// dynamic import to get a freshly-evaluated store.
//
// `captured` holds the `change` listener `initTheme` registers, so a test can
// simulate the OS flipping light↔dark by invoking it directly.
let captured: ((event: { matches: boolean }) => void) | null = null;

function setSystemPrefersDark(dark: boolean): void {
  captured = null;
  window.matchMedia = ((query: string) => ({
    matches: dark,
    media: query,
    onchange: null,
    addEventListener: (_type: string, handler: (event: { matches: boolean }) => void) => {
      captured = handler;
    },
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

async function loadStore(): Promise<typeof import("./theme.svelte")> {
  vi.resetModules();
  return import("./theme.svelte");
}

beforeEach(() => {
  localStorage.clear();
  document.documentElement.classList.remove("dark");
  setSystemPrefersDark(false);
});

afterEach(() => {
  document.documentElement.classList.remove("dark");
});

describe("theme store", () => {
  it("defaults to system mode when nothing is stored", async () => {
    const { theme } = await loadStore();
    expect(theme.mode).toBe("system");
  });

  it("reads a persisted mode on load", async () => {
    localStorage.setItem("switchboard-theme", "dark");
    const { theme } = await loadStore();
    expect(theme.mode).toBe("dark");
  });

  it("set('dark') adds the dark class and persists", async () => {
    const { theme } = await loadStore();
    theme.set("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(true);
    expect(localStorage.getItem("switchboard-theme")).toBe("dark");
    expect(theme.resolved).toBe("dark");
  });

  it("set('light') removes the dark class", async () => {
    document.documentElement.classList.add("dark");
    const { theme } = await loadStore();
    theme.set("light");
    expect(document.documentElement.classList.contains("dark")).toBe(false);
    expect(theme.resolved).toBe("light");
  });

  it("system mode follows the OS preference", async () => {
    setSystemPrefersDark(true);
    const { theme } = await loadStore();
    theme.set("system");
    expect(theme.resolved).toBe("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(true);
  });

  it("initTheme applies the stored mode at startup", async () => {
    localStorage.setItem("switchboard-theme", "dark");
    const { initTheme } = await loadStore();
    initTheme();
    expect(document.documentElement.classList.contains("dark")).toBe(true);
  });

  it("follows a live OS change while on system mode", async () => {
    const { theme, initTheme } = await loadStore();
    theme.set("system");
    initTheme();
    expect(theme.resolved).toBe("light");

    captured?.({ matches: true });
    expect(theme.resolved).toBe("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(true);
  });

  it("ignores a live OS change when the mode is pinned", async () => {
    const { theme, initTheme } = await loadStore();
    theme.set("light");
    initTheme();

    captured?.({ matches: true });
    expect(theme.resolved).toBe("light");
    expect(document.documentElement.classList.contains("dark")).toBe(false);
  });

  it("falls back to system for an invalid persisted value", async () => {
    localStorage.setItem("switchboard-theme", "bogus");
    const { theme } = await loadStore();
    expect(theme.mode).toBe("system");
  });
});
