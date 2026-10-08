// Code reviews delivered as tool input (Claude Code's `ReportFindings`). The
// model is told the host renders the review and not to repeat it as text, so
// a successful call is answer content: it renders as a card and counts toward
// copy, matching what forward and workflows send (Rust `forward.rs`).

import type { ToolCall } from "$lib/state/types";
import type { Finding, FindingsFacet } from "$lib/types";

export type FindingsToolCall = ToolCall & { facet: FindingsFacet };

/// Whether a tool call delivered a review that counts as answer content: a
/// findings call the harness confirmed as successful. Not "is this a review
/// call" — a call still waiting on its result is false here, though it already
/// renders as a card (see `rendersAsFindingsCard`). A rejected call (the
/// model then retries), one stopped by a cancelled or failed turn, and one that
/// never got a result are excluded — the same rule as the Rust capture and
/// disk read, so copy never includes a review that forward leaves out.
export function isDeliveredReview(tool: ToolCall): tool is FindingsToolCall {
  return tool.facet.facet_kind === "findings" && tool.is_error === false;
}

/// Whether a tool call renders as the findings card rather than the generic
/// tool row: a confirmed review, or one still waiting for its result while the
/// turn is live (the input is complete at start, so the card can show at
/// once). Every other findings call keeps the generic row, which shows its
/// failed or cancelled status — so the card never shows a review that copy
/// leaves out once the turn settles.
export function rendersAsFindingsCard(tool: ToolCall, turnSettled: boolean): boolean {
  if (tool.facet.facet_kind !== "findings") return false;
  if (tool.is_error === false) return true;
  return (
    !turnSettled &&
    tool.is_error === undefined &&
    tool.completed_at === undefined &&
    tool.stopped_at === undefined
  );
}

/// `Code review · medium · 7 findings`, matching the header Rust writes into
/// the review's markdown.
export function findingsHeader(facet: FindingsFacet): string {
  const n = facet.findings.length;
  const count = n === 0 ? "no findings" : n === 1 ? "1 finding" : `${n} findings`;
  return facet.level ? `Code review · ${facet.level} · ${count}` : `Code review · ${count}`;
}

export function findingLocation(finding: Finding): string {
  return finding.line ? `${finding.file}:${finding.line}` : finding.file;
}

/// The location as a row shows it: the file name and line, without the
/// folders. Cutting a long path at its end would drop exactly the file name
/// and line, so the row keeps those and the expanded detail shows the full
/// path (two same-named files look alike until expanded).
export function findingShortLocation(finding: Finding): string {
  const name =
    finding.file
      .split("/")
      .filter((part) => part !== "")
      .at(-1) ?? finding.file;
  return finding.line ? `${name}:${finding.line}` : name;
}

/// The finding's status label: an outcome from a post-fix re-report wins over
/// the verification verdict. Unknown values (the Rust enums are
/// non-exhaustive) produce no label rather than a wrong one.
export type FindingBadge = { label: string; tone: "failed" | "warning" | "accent" | "neutral" };

export function findingBadge(finding: Finding): FindingBadge | undefined {
  switch (finding.outcome) {
    case "fixed":
      return { label: "Fixed", tone: "accent" };
    case "skipped":
      return { label: "Skipped", tone: "neutral" };
    case "no_change_needed":
      return { label: "No change needed", tone: "neutral" };
  }
  switch (finding.verdict) {
    case "CONFIRMED":
      return { label: "Confirmed", tone: "failed" };
    case "PLAUSIBLE":
      return { label: "Plausible", tone: "warning" };
  }
  return undefined;
}
