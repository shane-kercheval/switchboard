import { describe, expect, it } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, within } from "@testing-library/svelte";
import ContextBreakdown from "./ContextBreakdown.svelte";
import type { ContextReport } from "$lib/types";
import type { ContextReportRequest } from "$lib/state/types";

const REPORT: ContextReport = {
  model: "claude-fable-5-1",
  total_tokens: 25_081,
  max_tokens: 1_000_000,
  categories: [
    { name: "System prompt", tokens: 4_026, kind: "used" },
    { name: "Free space", tokens: 941_900, kind: "free" },
  ],
  mcp_tools: [
    { name: "mcp__docs__read", detail: "docs", tokens: 300 },
    { name: "mcp__notes__search_items", detail: "notes", tokens: 901 },
  ],
  skills: [{ name: "dataviz", detail: "Built-in", tokens: 480 }],
  raw: "## Context Usage\n\n**Model:** claude-fable-5-1",
};

type Overrides = {
  report?: ContextReport;
  at?: string | null;
  request?: ContextReportRequest;
};

function mount(overrides: Overrides = {}): void {
  render(ContextBreakdown, {
    props: {
      open: true,
      onClose: () => {},
      agentName: "alice",
      report: overrides.report,
      at: overrides.at ?? null,
      request: overrides.request,
    },
  });
}

describe("ContextBreakdown", () => {
  it("shows the settled empty state when no report is available", () => {
    mount();
    expect(screen.getByTestId("context-breakdown-empty")).toBeInTheDocument();
    expect(screen.queryByTestId("context-breakdown-refresh")).not.toBeInTheDocument();
    // Nothing to show the raw of, so no disclosure for it.
    expect(screen.queryByTestId("context-breakdown-raw-toggle")).not.toBeInTheDocument();
  });

  it("renders the header meter and one meter per category", () => {
    mount({ report: REPORT });
    expect(screen.getByTestId("context-breakdown-usage")).toHaveTextContent("claude-fable-5-1");
    expect(screen.getByTestId("context-breakdown-usage")).toHaveTextContent("25k / 1M");
    const categories = screen.getByTestId("context-breakdown-categories");
    expect(within(categories).getByText("System prompt")).toBeInTheDocument();
    expect(within(categories).getByText("Free space")).toBeInTheDocument();
  });

  it("groups MCP tools by server behind a collapsed section", async () => {
    mount({ report: REPORT });
    expect(screen.queryByTestId("context-breakdown-rows-mcp")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("context-breakdown-toggle-mcp"));

    const rows = screen.getByTestId("context-breakdown-rows-mcp");
    expect(within(rows).getByText("docs")).toBeInTheDocument();
    expect(within(rows).getByText("notes")).toBeInTheDocument();
    expect(within(rows).getByText("mcp__docs__read")).toBeInTheDocument();
  });

  it("says so when it could not read the report, and still shows the text", async () => {
    mount({ report: { raw: "the CLI printed something else", unparsed: true } });

    expect(screen.getByTestId("context-breakdown-unparsed")).toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("context-breakdown-raw-toggle"));
    expect(screen.getByTestId("context-breakdown-raw")).toHaveTextContent(
      "the CLI printed something else",
    );
  });

  it("says when the breakdown was measured", () => {
    // Every report gets this line, however fresh. A breakdown measures one
    // instant and each turn after it grows the context it describes, so an
    // unqualified one is a number the reader cannot place.
    mount({ report: REPORT, at: "2026-09-18T15:48:31Z" });
    expect(screen.getByTestId("context-breakdown-as-of")).toBeInTheDocument();
  });

  it("shows no time for a report that arrived without one", () => {
    // Only reachable from an older persisted shape; a missing time renders
    // nothing rather than a fabricated "just now".
    mount({ report: REPORT, at: null });
    expect(screen.queryByTestId("context-breakdown-as-of")).not.toBeInTheDocument();
  });

  it("shows a spinner while the initial analysis is queued", () => {
    mount({ request: { send_id: "s", phase: "queued" } });
    const loading = screen.getByTestId("context-breakdown-loading");
    expect(loading).toHaveTextContent("Context analysis queued…");
    expect(loading.querySelector(".animate-spin")).not.toBeNull();
    expect(screen.queryByTestId("context-breakdown-empty")).not.toBeInTheDocument();
  });

  it("shows a spinner instead of the stale report while a refresh is running", () => {
    mount({ report: REPORT, request: { send_id: "s", phase: "running" } });
    expect(screen.getByTestId("context-breakdown-loading")).toHaveTextContent(
      "Refreshing context…",
    );
    expect(screen.queryByTestId("context-breakdown-usage")).not.toBeInTheDocument();
  });

  it("names a failure and keeps the previous report beside it", () => {
    // The panel is the only surface a report failure has — there is no
    // transcript row — and blanking the breakdown would throw away the best
    // measurement still available.
    mount({
      report: REPORT,
      at: "2026-09-18T15:48:31Z",
      request: { send_id: "s", phase: "failed", error: "alice has no conversation yet" },
    });

    expect(screen.getByTestId("context-breakdown-request-note")).toHaveTextContent(
      "alice has no conversation yet",
    );
    expect(screen.getByTestId("context-breakdown-usage")).toHaveTextContent("25k / 1M");
    expect(screen.getByTestId("context-breakdown-as-of")).toBeInTheDocument();
    expect(screen.queryByTestId("context-breakdown-loading")).not.toBeInTheDocument();
  });

  it("names a cancellation and keeps the previous report", () => {
    mount({ report: REPORT, request: { send_id: "s", phase: "cancelled" } });
    expect(screen.getByTestId("context-breakdown-request-note")).toHaveTextContent("cancelled");
    expect(screen.getByTestId("context-breakdown-usage")).toBeInTheDocument();
  });

  it("says nothing about a settled successful request — the numbers are the message", () => {
    mount({ report: REPORT, request: { send_id: "s", phase: "done" } });
    expect(screen.queryByTestId("context-breakdown-request-note")).not.toBeInTheDocument();
    expect(screen.queryByTestId("context-breakdown-refresh")).not.toBeInTheDocument();
  });

  it("reveals a memory file's full path through the app's tooltip, not the browser's", async () => {
    mount({
      report: {
        ...REPORT,
        memory_files: [{ name: "/Users/example/.claude/CLAUDE.md", detail: "User", tokens: 167 }],
      },
    });
    await fireEvent.click(screen.getByTestId("context-breakdown-toggle-memory"));

    const row = within(screen.getByTestId("context-breakdown-rows-memory")).getByText("CLAUDE.md");
    expect(row).not.toHaveAttribute("title");
    // The primitive marks its trigger; a native `title` would leave it bare.
    expect(row).toHaveAttribute("data-tooltip-trigger");
  });
});
