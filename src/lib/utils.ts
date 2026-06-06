import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

// Standard shadcn-svelte utility for composing Tailwind class lists.
// Resolves conditional classes and de-duplicates conflicting utilities.
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

// Last path component of a POSIX-style absolute path. Used to derive default
// project names and to render the breadcrumb. Pure string manipulation —
// does not touch the filesystem.
export function basename(path: string): string {
  const trimmed = path.endsWith("/") ? path.slice(0, -1) : path;
  const i = trimmed.lastIndexOf("/");
  return i >= 0 ? trimmed.slice(i + 1) : trimmed;
}

function trimTrailingPathSeparators(path: string): string {
  return path.replace(/[\\/]+$/, "");
}

function isLikelyWindowsPath(path: string): boolean {
  return /^[a-z]:[\\/]/i.test(path) || path.includes("\\");
}

/// Display a path relative to the user's home directory when possible. The home
/// directory is supplied by the platform layer (`homeDir()` in Tauri), so this
/// remains portable across macOS, Linux, and Windows without hard-coded prefixes.
export function formatHomePath(path: string, home: string | null | undefined): string {
  if (home == null || home === "") return path;

  const normalizedHome = trimTrailingPathSeparators(home);
  if (normalizedHome === "") return path;

  const windows = isLikelyWindowsPath(path) || isLikelyWindowsPath(normalizedHome);
  const comparablePath = windows ? path.toLowerCase() : path;
  const comparableHome = windows ? normalizedHome.toLowerCase() : normalizedHome;

  if (comparablePath === comparableHome) return "~";

  const next = path.charAt(normalizedHome.length);
  if (comparablePath.startsWith(comparableHome) && (next === "/" || next === "\\")) {
    return `~${path.slice(normalizedHome.length)}`;
  }

  return path;
}

// Compact elapsed-duration label ("2m 03s" / "1h 04m"), used by the transcript
// footer's "No response (…)" silence counter. Seconds-precision under an hour;
// minute-precision past one hour. Negative/NaN inputs clamp to "0m 00s".
export function formatDuration(ms: number): string {
  const totalSec = Number.isFinite(ms) ? Math.max(0, Math.floor(ms / 1000)) : 0;
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  const pad = (n: number): string => String(n).padStart(2, "0");
  return h > 0 ? `${h}h ${pad(m)}m` : `${m}m ${pad(s)}s`;
}

export function currentIsoTimestamp(now: Date = new Date()): string {
  return now.toISOString();
}

// Compact "time since" label for the project list's last-activity column.
// `now` is injectable so tests stay deterministic (no wall-clock dependency).
// Falls back to a short locale date for anything older than ~4 weeks, and to
// an empty string for an unparseable input.
export function relativeTime(iso: string, now: Date = new Date()): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "";
  const seconds = Math.floor((now.getTime() - then) / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d ago`;
  const weeks = Math.floor(days / 7);
  if (weeks < 5) return `${weeks}w ago`;
  return new Date(iso).toLocaleDateString();
}
