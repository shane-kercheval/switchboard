import { describe, expect, it } from "vitest";
import { breakdownView } from "$lib/contextBreakdown";
import type { ContextReport } from "$lib/types";

const REPORT: ContextReport = {
  model: "claude-fable-5-1",
  total_tokens: 25_081,
  max_tokens: 1_000_000,
  categories: [
    { name: "System prompt", tokens: 4_026, kind: "used" },
    { name: "MCP tools (deferred)", tokens: 39_100, kind: "deferred" },
  ],
  mcp_tools: [
    { name: "mcp__docs__read", detail: "docs", tokens: 300 },
    { name: "mcp__docs__update", detail: "docs", tokens: 275 },
    { name: "mcp__notes__search_items", detail: "notes", tokens: 901 },
  ],
  memory_files: [{ name: "/Users/example/.claude/CLAUDE.md", detail: "User", tokens: 167 }],
  agents: [{ name: "Jenny", detail: "userSettings", tokens: 368 }],
  skills: [
    { name: "coding-guidelines", detail: "User", tokens: 30, approximate: true },
    { name: "dataviz", detail: "Built-in", tokens: 480 },
  ],
  raw: "## Context Usage",
};

function section(report: ContextReport, key: string) {
  const view = breakdownView(report);
  if (view === null) throw new Error("expected a view");
  const found = view.sections.find((s) => s.key === key);
  if (found === undefined) throw new Error(`expected a ${key} section: ${JSON.stringify(view)}`);
  return found;
}

describe("breakdownView", () => {
  it("returns null only when there is no report at all", () => {
    expect(breakdownView(undefined)).toBeNull();
    expect(breakdownView({ raw: "anything", unparsed: true })).not.toBeNull();
  });

  it("derives the header meter from the report's own totals", () => {
    const view = breakdownView(REPORT);
    expect(view?.model).toBe("claude-fable-5-1");
    expect(view?.usage).toEqual({
      usedTokens: 25_081,
      windowTokens: 1_000_000,
      fraction: 25_081 / 1_000_000,
    });
  });

  it("computes each category's fraction against the window", () => {
    // The CLI reports no per-category percentage; this is the arithmetic it
    // does to print its own table.
    const view = breakdownView(REPORT);
    expect(view?.categories[0]).toEqual({
      name: "System prompt",
      tokens: 4_026,
      fraction: 4_026 / 1_000_000,
      detail: "4k",
    });
  });

  it("shows a category with no window as a count rather than a scaleless bar", () => {
    const view = breakdownView({
      categories: [{ name: "Messages", tokens: 10 }],
      raw: "",
    });
    expect(view?.usage).toBeNull();
    expect(view?.categories[0]?.fraction).toBeNull();
    expect(view?.categories[0]?.detail).toBe("10");
  });

  it("renders no header meter for a window of zero", () => {
    // A zero window makes every fraction non-finite. The meter primitive
    // clean-hides those, so the bar would vanish while the panel still claimed
    // to be showing occupancy.
    const view = breakdownView({ total_tokens: 10, max_tokens: 0, raw: "" });
    expect(view?.usage).toBeNull();
  });

  it("groups MCP tools by server with per-server totals", () => {
    // The CLI prints one flat table across every server — 80 rows in a real
    // account — which hides the thing worth knowing: which server is expensive.
    const mcp = section(REPORT, "mcp");
    expect(mcp.tokens).toBe(300 + 275 + 901);
    expect(mcp.groups.map((g) => [g.label, g.tokens])).toEqual([
      ["docs", 575],
      ["notes", 901],
    ]);
    expect(mcp.groups[0]?.rows.map((r) => r.name)).toEqual([
      "mcp__docs__read",
      "mcp__docs__update",
    ]);
  });

  it("drops the per-row server name that the group heading already carries", () => {
    expect(section(REPORT, "mcp").groups[0]?.rows[0]?.detail).toBeNull();
  });

  it("abbreviates a memory path to its basename and keeps the whole one for hover", () => {
    const row = section(REPORT, "memory").groups[0]?.rows[0];
    expect(row?.name).toBe("CLAUDE.md");
    expect(row?.title).toBe("/Users/example/.claude/CLAUDE.md");
  });

  it("leaves a name that is not a path whole, with nothing to reveal on hover", () => {
    const row = section(REPORT, "agents").groups[0]?.rows[0];
    expect(row?.name).toBe("Jenny");
    expect(row?.title).toBeNull();
  });

  it("carries the CLI's rounding marker onto a count it guessed", () => {
    // Without the tilde a count the harness rounded reads as one it measured.
    const rows = section(REPORT, "skills").groups[0]?.rows;
    expect(rows?.[0]?.tokens).toBe("~30");
    expect(rows?.[1]?.tokens).toBe("480");
  });

  it("omits a section the report has nothing for", () => {
    const view = breakdownView({ ...REPORT, agents: [], skills: undefined });
    expect(view?.sections.map((s) => s.key)).toEqual(["mcp", "memory"]);
  });

  it("keeps the raw text and the unparsed flag for a report it could not read", () => {
    const view = breakdownView({ raw: "the CLI printed something else", unparsed: true });
    expect(view?.unparsed).toBe(true);
    expect(view?.raw).toBe("the CLI printed something else");
    expect(view?.categories).toEqual([]);
    expect(view?.sections).toEqual([]);
  });

  it("keeps two same-named rows as two rows", () => {
    // Merging would make the section's total disagree with the counts the
    // harness reported, and a user who copied a bundled skill to customize it
    // is the ordinary way two entries share a name.
    const view = breakdownView({
      ...REPORT,
      skills: [
        { name: "deep-research", tokens: 16 },
        { name: "deep-research", tokens: 16 },
      ],
    });
    const skills = view?.sections.find((s) => s.key === "skills");
    expect(skills?.groups[0]?.rows).toHaveLength(2);
    expect(skills?.tokens).toBe(32);
  });
});
