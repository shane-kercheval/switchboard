/// Pure derivation for the context-breakdown panel: turns a `ContextReport`
/// into the rows the panel renders.
///
/// Separate from the component for the reason `agentEnvironment.ts` is: the
/// grouping, the totals and the "is there anything to show" question are
/// testable arithmetic, and a component test that had to mount a dialog to
/// check a per-server sum would be testing the wrong thing.
import { formatTokens } from "$lib/utils";
import type { ContextItem, ContextReport } from "$lib/types";

/// One category meter.
export type CategoryRow = {
  name: string;
  tokens: number;
  /// `tokens / max_tokens`, or `null` when the report carries no window size —
  /// the row then shows its token count and no bar, rather than a bar with no
  /// scale.
  fraction: number | null;
  detail: string;
};

/// One expandable list of item rows.
export type BreakdownSection = {
  key: string;
  label: string;
  /// Total tokens across the section, for the collapsed line.
  tokens: number;
  groups: BreakdownGroup[];
};

/// A named run of rows. Flat sections use a single group with a `null` label;
/// the MCP section uses one group per server, which is what makes an 80-row
/// table readable.
export type BreakdownGroup = {
  label: string | null;
  tokens: number;
  rows: BreakdownRow[];
};

export type BreakdownRow = {
  name: string;
  detail: string | null;
  /// The token count, already carrying the CLI's `~` when it was rounded.
  tokens: string;
  /// Full text for a row whose `name` is displayed abbreviated (a memory file's
  /// path), so the component can put it on hover. `null` when `name` is whole.
  title: string | null;
};

export type BreakdownView = {
  model: string | null;
  /// The header meter. `null` when the report carries no totals — the panel
  /// then shows only the raw text.
  usage: { usedTokens: number; windowTokens: number; fraction: number } | null;
  categories: CategoryRow[];
  sections: BreakdownSection[];
  raw: string;
  unparsed: boolean;
};

function sumTokens(items: ContextItem[]): number {
  return items.reduce((total, item) => total + item.tokens, 0);
}

/// `31` → `"31"`; a rounded `31` → `"~31"`. The tilde is the CLI's own marker
/// and is carried through rather than dropped, so a count the harness guessed
/// never reads as one it measured.
function formatItemTokens(item: { tokens: number; approximate?: boolean }): string {
  const formatted = formatTokens(item.tokens);
  return item.approximate === true ? `~${formatted}` : formatted;
}

function toRow(item: ContextItem, abbreviate: boolean): BreakdownRow {
  const whole = item.name;
  const shown = abbreviate ? (whole.split("/").pop() ?? whole) : whole;
  return {
    name: shown,
    detail: item.detail ?? null,
    tokens: formatItemTokens(item),
    title: shown === whole ? null : whole,
  };
}

/// One group per distinct `detail`, in first-seen order. Used for MCP tools,
/// where `detail` is the server: the CLI prints one flat table of every tool
/// across every server, which at 80 rows is unreadable and, worse, hides the
/// thing the user actually wants — which *server* is expensive.
function groupByDetail(items: ContextItem[]): BreakdownGroup[] {
  const groups: BreakdownGroup[] = [];
  const byLabel = new Map<string, BreakdownGroup>();
  for (const item of items) {
    const label = item.detail ?? "";
    let group = byLabel.get(label);
    if (group === undefined) {
      group = { label: item.detail ?? null, tokens: 0, rows: [] };
      byLabel.set(label, group);
      groups.push(group);
    }
    group.tokens += item.tokens;
    // The server name is the group's heading, so repeating it on every row
    // would be noise.
    group.rows.push({ ...toRow(item, false), detail: null });
  }
  return groups;
}

function flatSection(
  key: string,
  label: string,
  items: ContextItem[] | undefined,
  abbreviate = false,
): BreakdownSection | null {
  if (items === undefined || items.length === 0) return null;
  return {
    key,
    label,
    tokens: sumTokens(items),
    groups: [
      {
        label: null,
        tokens: sumTokens(items),
        rows: items.map((item) => toRow(item, abbreviate)),
      },
    ],
  };
}

/// Build the panel's view, or `null` when there is no report at all.
///
/// Never returns `null` for a report it could not parse: that case still has
/// raw text to show, and showing it is the whole point of retaining it.
export function breakdownView(report: ContextReport | undefined): BreakdownView | null {
  if (report === undefined) return null;
  const windowTokens = report.max_tokens ?? null;
  const usedTokens = report.total_tokens ?? null;
  const usage =
    windowTokens !== null && usedTokens !== null && windowTokens > 0
      ? { usedTokens, windowTokens, fraction: usedTokens / windowTokens }
      : null;

  const categories: CategoryRow[] = (report.categories ?? []).map((category) => ({
    name: category.name,
    tokens: category.tokens,
    // Derived here rather than read from the report: the CLI reports no
    // per-category percentage at all, and this is the same arithmetic it does
    // to print its own table.
    fraction: windowTokens !== null && windowTokens > 0 ? category.tokens / windowTokens : null,
    detail: formatItemTokens(category),
  }));

  const mcp = report.mcp_tools ?? [];
  const sections: BreakdownSection[] = [
    mcp.length === 0
      ? null
      : { key: "mcp", label: "MCP tools", tokens: sumTokens(mcp), groups: groupByDetail(mcp) },
    flatSection("agents", "Custom agents", report.agents),
    // Abbreviated to the basename with the full path on hover: a memory file's
    // path is long and its leading directories are the same for every row.
    flatSection("memory", "Memory files", report.memory_files, true),
    flatSection("skills", "Skills", report.skills),
  ].filter((section): section is BreakdownSection => section !== null);

  return {
    model: report.model ?? null,
    usage,
    categories,
    sections,
    raw: report.raw,
    unparsed: report.unparsed === true,
  };
}
