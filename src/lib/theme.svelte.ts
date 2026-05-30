/// Light/dark theme controller. The token layer in `app.css` keys dark mode
/// off a `.dark` class on `<html>`; this module owns that class.
///
/// Three modes: `light` / `dark` pin a theme; `system` follows the OS via
/// `prefers-color-scheme` and re-applies live when the OS preference flips.
/// The chosen mode persists to localStorage so it survives reloads. All
/// browser-API access is guarded so the store is import-safe under jsdom and
/// any non-DOM context.

export type ThemeMode = "light" | "dark" | "system";

const STORAGE_KEY = "switchboard-theme";

function prefersDark(): boolean {
  return (
    typeof window !== "undefined" &&
    window.matchMedia?.("(prefers-color-scheme: dark)").matches === true
  );
}

function readStored(): ThemeMode {
  if (typeof localStorage === "undefined") return "system";
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "light" || stored === "dark" || stored === "system" ? stored : "system";
}

let mode = $state<ThemeMode>(readStored());
// Reactive mirror of the OS preference. Kept in `$state` (not read live from
// `matchMedia` per access) so `resolved` re-derives — and any UI reading it
// re-renders — when the OS flips light↔dark while we're on `system`.
let systemDark = $state(prefersDark());

function resolved(): "light" | "dark" {
  if (mode === "system") return systemDark ? "dark" : "light";
  return mode;
}

function apply(): void {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("dark", resolved() === "dark");
}

export const theme = {
  get mode(): ThemeMode {
    return mode;
  },
  /// Resolved appearance after `system` is collapsed — what the UI actually shows.
  get resolved(): "light" | "dark" {
    return resolved();
  },
  set(next: ThemeMode): void {
    mode = next;
    if (typeof localStorage !== "undefined") localStorage.setItem(STORAGE_KEY, next);
    apply();
  },
};

/// Apply the stored mode and start tracking OS changes. Call once at startup.
export function initTheme(): void {
  apply();
  if (typeof window !== "undefined" && window.matchMedia) {
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", (event) => {
      systemDark = event.matches;
      if (mode === "system") apply();
    });
  }
}
