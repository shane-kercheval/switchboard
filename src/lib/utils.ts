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
