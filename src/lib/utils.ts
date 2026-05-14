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

// Picks the most-recently-created agent. M4 introduces an agent switcher; in
// M1 the active agent is just "whichever the user created most recently."
// Tiebreak by id desc keeps the choice deterministic when timestamps tie:
// since AgentId is UUID v7 (time-ordered), descending-by-id means
// newer-by-id wins, which is consistent with the `created_at` primary sort.
// Throws on empty input — callers must check `agents.length > 0` first.
export function pickNewestAgent(agents: AgentRecord[]): AgentRecord {
  const sorted = [...agents].sort((a, b) => {
    if (a.created_at !== b.created_at) return b.created_at.localeCompare(a.created_at);
    return b.id.localeCompare(a.id);
  });
  const first = sorted[0];
  if (!first) throw new Error("pickNewestAgent called with empty agents array");
  return first;
}
