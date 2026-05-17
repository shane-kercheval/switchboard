import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import type { AgentRecord } from "$lib/types";

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

// Picks the most-recently-created agent. The active agent is "whichever
// the user created most recently"; a future agent switcher will replace
// this implicit rule.
// Primary sort: `created_at` descending, compared by parsed timestamp so
// the result is correct regardless of timezone suffix variation (chrono
// emits `Z` today, but `+00:00` and offset-bearing forms are valid ISO
// 8601 too — lexical comparison would silently misorder those).
// Tiebreak by id desc: since AgentId is UUID v7 (time-ordered), the
// higher id is the newer one, consistent with the primary sort.
// Throws on empty input — callers must check `agents.length > 0` first.
export function pickNewestAgent(agents: AgentRecord[]): AgentRecord {
  const sorted = [...agents].sort((a, b) => {
    const dt = Date.parse(b.created_at) - Date.parse(a.created_at);
    if (dt !== 0) return dt;
    return b.id.localeCompare(a.id);
  });
  const first = sorted[0];
  if (!first) throw new Error("pickNewestAgent called with empty agents array");
  return first;
}
