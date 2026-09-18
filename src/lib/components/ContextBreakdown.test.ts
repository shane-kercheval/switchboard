import { describe, expect, it, vi } from "vitest";
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
  asOf?: string | null;
  request?: ContextReportRequest;
  onRefresh?: () => void;
};

function mount(overrides: Overrides = {}): { onRefresh: ReturnType<typeof vi.fn> } {
  const onRefresh = vi.fn();
  render(ContextBreakdown, {
    props: {
      open: true,
      onClose: () => {},
      agentName: "alice",
      report: overrides.report,
      asOf: overrides.asOf ?? null,
      request: overrides.request,
      onRefresh: overrides.onRefresh ?? onRefresh,
    },
  });
  return { onRefresh };
}

describe("ContextBreakdown", () => {
  it("offers to analyze when nothing has measured the agent yet", () => {
    mount();
    expect(screen.getByTestId("context-breakdown-empty")).toBeInTheDocument();
    expect(screen.getByTestId("context-breakdown-refresh")).toHaveTextContent("Analyze context");
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

  it("qualifies a report restored from disk with its age", () => {
    mount({ report: REPORT, asOf: "2026-09-18T15:48:31Z" });
    expect(screen.getByTestId("context-breakdown-as-of")).toBeInTheDocument();
  });

  it("presents a live report without an age qualifier", () => {
    mount({ report: REPORT, asOf: null });
    expect(screen.queryByTestId("context-breakdown-as-of")).not.toBeInTheDocument();
  });

  it("says a queued request is waiting on the current turn, and refuses a second click", () => {
    // One slot: a second dispatch while the first is in flight would orphan the
    // first request's correlation, leaving the panel tracking a run it can no
    // longer match an event to.
    mount({ report: REPORT, request: { send_id: "s", phase: "queued" } });
    const button = screen.getByTestId("context-breakdown-refresh");
    expect(button).toHaveTextContent("Queued — runs after the current turn");
    expect(button).toBeDisabled();
  });

  it("disables the button while the report is running", () => {
    mount({ report: REPORT, request: { send_id: "s", phase: "running" } });
    expect(screen.getByTestId("context-breakdown-refresh")).toBeDisabled();
  });

  it("names a failure and keeps the previous report beside it", () => {
    // The panel is the only surface a report failure has — there is no
    // transcript row — and blanking the breakdown would throw away the best
    // measurement still available.
    mount({
      report: REPORT,
      asOf: "2026-09-18T15:48:31Z",
      request: { send_id: "s", phase: "failed", error: "alice has no conversation yet" },
    });

    expect(screen.getByTestId("context-breakdown-request-note")).toHaveTextContent(
      "alice has no conversation yet",
    );
    expect(screen.getByTestId("context-breakdown-usage")).toHaveTextContent("25k / 1M");
    expect(screen.getByTestId("context-breakdown-as-of")).toBeInTheDocument();
    expect(screen.getByTestId("context-breakdown-refresh")).toBeEnabled();
  });

  it("names a cancellation and keeps the previous report", () => {
    mount({ report: REPORT, request: { send_id: "s", phase: "cancelled" } });
    expect(screen.getByTestId("context-breakdown-request-note")).toHaveTextContent("cancelled");
    expect(screen.getByTestId("context-breakdown-usage")).toBeInTheDocument();
  });

  it("says nothing about a settled successful request — the numbers are the message", () => {
    mount({ report: REPORT, request: { send_id: "s", phase: "done" } });
    expect(screen.queryByTestId("context-breakdown-request-note")).not.toBeInTheDocument();
    expect(screen.getByTestId("context-breakdown-refresh")).toHaveTextContent("Refresh");
  });

  it("asks for a new report when refreshed", async () => {
    const { onRefresh } = mount({ report: REPORT });
    await fireEvent.click(screen.getByTestId("context-breakdown-refresh"));
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });
});
