// The tool row's verb vocabulary and icon mapping, in one place so the bold
// column stays a fixed, small set — it only scans as a column if every
// component agrees on the labels.
//
// Labels are deliberately state-invariant nouns ("Command", "Edit"): the
// status glyph (spinner / check / failed / cancelled) is the row's sole state
// signal, so encoding state into the verb duplicated it — and a noun reads
// correctly in every state, including a tool cancelled mid-flight where any
// tense would assert something false.

import {
  FilePen,
  FilePlus,
  FileText,
  FileX,
  ListChecks,
  Plug,
  Search,
  SquareTerminal,
  Wrench,
} from "@lucide/svelte";
import type { ToolCall } from "$lib/state/types";
import type { ToolFacet } from "$lib/types";
import { redactDisplay, toolInputPreview } from "$lib/toolInput";

/// All lucide icons share one component shape; alias it off a concrete icon
/// so consumers get a precise type without reaching into lucide internals.
export type ToolIconComponent = typeof Wrench;

/// The row's lifecycle state, folded from the ToolCall lifecycle fields.
/// Consumed by the status glyph only — the verb no longer encodes state.
export type ToolRowState = "running" | "done" | "failed" | "cancelled";

export function toolRowState(tool: ToolCall): ToolRowState {
  if (tool.completed_at === undefined && tool.stopped_at === undefined) return "running";
  if (tool.stop_reason === "cancelled") return "cancelled";
  if (tool.stop_reason === "failed" || tool.is_error === true) return "failed";
  return "done";
}

/// The bold normalized label for the row. `rawName` is the fallback for the
/// generic facet and for any facet discriminant this build doesn't know —
/// the Rust enum is `#[non_exhaustive]`, so an unrecognized `facet_kind`
/// must degrade to the raw tool name rather than rendering blank.
export function toolVerb(facet: ToolFacet, rawName: string): string {
  switch (facet.facet_kind) {
    case "shell":
      return "Command";
    case "edit":
      return editVerb(facet.files);
    case "write":
      return "Write";
    case "read":
      return "Read";
    case "search":
      return "Search";
    case "todo":
      return "Todos";
    case "mcp":
      return `${facet.server} · ${facet.tool}`;
    case "other":
      return rawName;
    default:
      return rawName;
  }
}

/// A single-file edit reads by its change kind: harnesses without a separate
/// write tool (Codex creates files via an apply_patch "Add File") arrive as
/// Edit facets, but the user is looking at a creation or a deletion, and the
/// label should say so. Multi-file patches stay "Edit" with per-file markers.
function editVerb(files: { change: string }[]): string {
  if (files.length === 1) {
    if (files[0]!.change === "added") return "Write";
    if (files[0]!.change === "deleted") return "Delete";
  }
  return "Edit";
}

/// The muted detail: the facet's own substance — the command that ran, the
/// file(s) touched, the pattern searched. Facet-derived rather than a raw
/// input preview so it never repeats what the verb already says (the raw tool
/// name lives in the expanded raw-input section instead). Undefined means the
/// row has nothing beyond its verb.
export function toolDetail(facet: ToolFacet, input: unknown): string | undefined {
  switch (facet.facet_kind) {
    case "shell":
      return nonEmpty(redactDisplay(oneLine(facet.command)));
    case "edit":
      return nonEmpty(facet.files.map((f) => f.path).join(", "));
    case "write":
    case "read":
      return nonEmpty(facet.path);
    case "search":
      return facet.path ? `${facet.pattern} in ${facet.path}` : nonEmpty(facet.pattern);
    case "todo":
      return todoSummary(facet.items);
    default:
      // mcp, other, and unknown discriminants: the input preview (already
      // redacted) is the only substance available.
      return toolInputPreview(input);
  }
}

function todoSummary(items: { content: string; status: string }[]): string | undefined {
  if (items.length === 0) return undefined;
  if (items.length === 1) return nonEmpty(oneLine(items[0]!.content));
  return `${items.length} items`;
}

function oneLine(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

function nonEmpty(value: string): string | undefined {
  return value === "" ? undefined : value;
}

/// Facet → left-column icon. Unknown discriminants take the generic wrench,
/// same degradation rule as the verb.
export function toolIcon(facet: ToolFacet): ToolIconComponent {
  switch (facet.facet_kind) {
    case "shell":
      return SquareTerminal;
    case "edit":
      if (facet.files.length === 1 && facet.files[0]!.change === "added") return FilePlus;
      if (facet.files.length === 1 && facet.files[0]!.change === "deleted") return FileX;
      return FilePen;
    case "write":
      return FilePlus;
    case "read":
      return FileText;
    case "search":
      return Search;
    case "todo":
      return ListChecks;
    case "mcp":
      return Plug;
    default:
      return Wrench;
  }
}
