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

// Compact elapsed-duration label ("9s" / "2m 03s" / "1h 04m"), used by the
// transcript footer's live counters (elapsed and the "No response (…)"
// silence). Bare seconds under a minute — running turns are frequently that
// short, and "0m 09s" reads as a malfunction. The silence counter never hits
// that branch (it starts at one full heartbeat threshold = 60s), so its
// existing output is unchanged. Negative/NaN inputs clamp to "0s".
export function formatDuration(ms: number): string {
  const totalSec = Number.isFinite(ms) ? Math.max(0, Math.floor(ms / 1000)) : 0;
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  const pad = (n: number): string => String(n).padStart(2, "0");
  if (h > 0) return `${h}h ${pad(m)}m`;
  return m > 0 ? `${m}m ${pad(s)}s` : `${s}s`;
}

export function currentIsoTimestamp(now: Date = new Date()): string {
  return now.toISOString();
}

const RFC3339_INSTANT =
  /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})$/;

function parseRfc3339Instant(value: string): { epochSecondMs: number; nanos: number } | null {
  const match = RFC3339_INSTANT.exec(value);
  if (match === null) return null;
  const epochSecondMs = Date.parse(`${match[1]}${match[3]}`);
  if (!Number.isFinite(epochSecondMs)) return null;
  const nanos = Number((match[2] ?? "").padEnd(9, "0"));
  return { epochSecondMs, nanos };
}

type ParsedRfc3339Instant = NonNullable<ReturnType<typeof parseRfc3339Instant>>;

function compareParsedInstants(a: ParsedRfc3339Instant, b: ParsedRfc3339Instant): number {
  if (a.epochSecondMs !== b.epochSecondMs) return a.epochSecondMs < b.epochSecondMs ? -1 : 1;
  if (a.nanos !== b.nanos) return a.nanos < b.nanos ? -1 : 1;
  return 0;
}

function compareCodeUnits(a: string, b: string): number {
  return a === b ? 0 : a < b ? -1 : 1;
}

function compareIsoTimestampsForSort(a: string, b: string, descending: boolean): number {
  const parsedA = parseRfc3339Instant(a);
  const parsedB = parseRfc3339Instant(b);
  if (parsedA === null || parsedB === null) {
    if (parsedA !== null) return -1;
    if (parsedB !== null) return 1;
    return compareCodeUnits(a, b);
  }
  const chronological = compareParsedInstants(parsedA, parsedB);
  return descending ? -chronological : chronological;
}

/// Serialized timestamps must use these sort comparators rather than raw
/// string comparison: Rust emits RFC 3339 fractions at variable precision.
/// Valid instants always sort before invalid values in either direction.
export function compareIsoTimestampsAscending(a: string, b: string): number {
  return compareIsoTimestampsForSort(a, b, false);
}

export function compareIsoTimestampsDescending(a: string, b: string): number {
  return compareIsoTimestampsForSort(a, b, true);
}

/// Chronological selection helpers keep sign conventions out of "latest" and
/// "earliest" decisions. A valid candidate replaces an invalid current value;
/// an invalid candidate never replaces another value.
export function isIsoTimestampAfter(candidate: string, current: string): boolean {
  const parsedCandidate = parseRfc3339Instant(candidate);
  if (parsedCandidate === null) return false;
  const parsedCurrent = parseRfc3339Instant(current);
  return parsedCurrent === null || compareParsedInstants(parsedCandidate, parsedCurrent) > 0;
}

export function isIsoTimestampBefore(candidate: string, current: string): boolean {
  const parsedCandidate = parseRfc3339Instant(candidate);
  if (parsedCandidate === null) return false;
  const parsedCurrent = parseRfc3339Instant(current);
  return parsedCurrent === null || compareParsedInstants(parsedCandidate, parsedCurrent) < 0;
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
