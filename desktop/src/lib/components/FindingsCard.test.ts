import { describe, expect, it, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, fireEvent, within } from "@testing-library/svelte";
import type { Finding, FindingsFacet } from "$lib/types";
import FindingsCard from "./FindingsCard.svelte";

const copyTextMock = vi.hoisted(() => vi.fn(async (_text: string): Promise<void> => undefined));
vi.mock("$lib/native", () => ({
  copyText: (text: string) => copyTextMock(text),
}));

beforeEach(() => {
  copyTextMock.mockClear();
});

const FULL: Finding = {
  file: "src/app.py",
  line: 12,
  summary: "Off-by-one in the loop bound.",
  short_summary: "Off-by-one in loop bound",
  failure_scenario: "A list of 3 items processes only 2.",
  category: "correctness",
  verdict: "CONFIRMED",
  outcome: null,
};

const REQUIRED_ONLY: Finding = {
  file: "README.md",
  line: null,
  summary: "Outdated install command.",
  short_summary: null,
  failure_scenario: "Following the README fails on a fresh machine.",
  category: null,
  verdict: null,
  outcome: null,
};

function facet(findings: Finding[], level: string | null = "low"): FindingsFacet {
  return { facet_kind: "findings", level, findings, text: "REVIEW MARKDOWN" };
}

describe("FindingsCard", () => {
  it("shows the header with the level and finding count", () => {
    const { getByTestId } = render(FindingsCard, { facet: facet([FULL, REQUIRED_ONLY]) });
    expect(getByTestId("findings-header")).toHaveTextContent("Code review · low · 2 findings");
  });

  it("omits the level when the model gave none, and counts one finding singularly", () => {
    const { getByTestId } = render(FindingsCard, { facet: facet([FULL], null) });
    expect(getByTestId("findings-header")).toHaveTextContent("Code review · 1 finding");
  });

  it("shows only the header for a review with no findings", () => {
    const { getByTestId, queryByTestId } = render(FindingsCard, { facet: facet([]) });
    expect(getByTestId("findings-header")).toHaveTextContent("Code review · low · no findings");
    expect(queryByTestId("findings-list")).toBeNull();
    expect(queryByTestId("findings-toggle-all")).toBeNull();
  });

  it("titles a row with the short summary when present, else the summary", () => {
    const { getAllByTestId } = render(FindingsCard, { facet: facet([FULL, REQUIRED_ONLY]) });
    const [first, second] = getAllByTestId("finding-row");
    expect(within(first!).getByTestId("finding-title")).toHaveTextContent(
      "Off-by-one in loop bound",
    );
    // Collapsed rows show the file name and line without the folders.
    expect(within(first!).getByTestId("finding-location")).toHaveTextContent(/^app\.py:12$/);
    expect(within(second!).getByTestId("finding-title")).toHaveTextContent(
      "Outdated install command.",
    );
    expect(within(second!).getByTestId("finding-location")).toHaveTextContent(/^README\.md$/);
  });

  it("expands a row to the full summary and failure scenario", async () => {
    const { getAllByTestId } = render(FindingsCard, { facet: facet([FULL]) });
    const row = getAllByTestId("finding-row")[0]!;
    expect(within(row).queryByTestId("finding-detail")).toBeNull();

    await fireEvent.click(within(row).getByTestId("finding-toggle"));

    const detail = within(row).getByTestId("finding-detail");
    expect(within(row).getByTestId("finding-location")).toHaveTextContent(/^src\/app\.py:12$/);
    expect(detail).not.toHaveTextContent("src/app.py:12");
    expect(detail).toHaveTextContent("Off-by-one in the loop bound.");
    const scenario = within(detail).getByTestId("finding-scenario");
    expect(scenario).toHaveTextContent("Failure scenario");
    expect(scenario).toHaveTextContent("A list of 3 items processes only 2.");
    expect(within(row).getByTestId("finding-toggle")).toHaveAttribute("aria-expanded", "true");
  });

  it("does not repeat the summary in the detail when it is already the row title", async () => {
    const { getAllByTestId } = render(FindingsCard, { facet: facet([REQUIRED_ONLY]) });
    const row = getAllByTestId("finding-row")[0]!;
    await fireEvent.click(within(row).getByTestId("finding-toggle"));
    const detail = within(row).getByTestId("finding-detail");
    expect(detail).not.toHaveTextContent("Outdated install command.");
    expect(detail).toHaveTextContent("Following the README fails on a fresh machine.");
  });

  it("expands all findings from a mixed state and then collapses them all", async () => {
    const { getAllByTestId, getByTestId, queryAllByTestId } = render(FindingsCard, {
      facet: facet([FULL, REQUIRED_ONLY]),
    });
    const rows = getAllByTestId("finding-toggle");
    await fireEvent.click(rows[0]!);

    const toggleAll = getByTestId("findings-toggle-all");
    expect(toggleAll).toHaveAccessibleName("Expand all findings");
    await fireEvent.click(toggleAll);

    expect(getAllByTestId("finding-detail")).toHaveLength(2);
    for (const row of rows) expect(row).toHaveAttribute("aria-expanded", "true");
    expect(toggleAll).toHaveAccessibleName("Collapse all findings");

    await fireEvent.click(rows[1]!);
    expect(toggleAll).toHaveAccessibleName("Expand all findings");
    await fireEvent.click(toggleAll);
    await fireEvent.click(toggleAll);

    expect(queryAllByTestId("finding-detail")).toHaveLength(0);
    for (const row of rows) expect(row).toHaveAttribute("aria-expanded", "false");
    expect(toggleAll).toHaveAccessibleName("Expand all findings");
  });

  it("drops code-span backticks from a row title and renders them in the detail", async () => {
    const finding: Finding = {
      ...REQUIRED_ONLY,
      summary: "Ignoring `ios/` drops `short_summary` checks.",
      failure_scenario: "Run `make check`.",
    };
    const { getByTestId } = render(FindingsCard, { facet: facet([finding]) });
    expect(getByTestId("finding-title")).toHaveTextContent(
      "Ignoring ios/ drops short_summary checks.",
    );
    await fireEvent.click(getByTestId("finding-toggle"));
    const code = getByTestId("finding-scenario").querySelector("code");
    expect(code).toHaveTextContent("make check");
  });

  it("renders rows without an expand control when not expandable", () => {
    const { getAllByTestId, queryByTestId } = render(FindingsCard, {
      facet: facet([FULL]),
      expandable: false,
    });
    expect(getAllByTestId("finding-row")).toHaveLength(1);
    expect(queryByTestId("finding-toggle")).toBeNull();
    expect(queryByTestId("findings-toggle-all")).toBeNull();
  });

  it.each([
    [{ outcome: "fixed" }, "Fixed", "text-accent"],
    [{ outcome: "skipped" }, "Skipped", undefined],
    [{ outcome: "no_change_needed" }, "No change needed", undefined],
    [{ verdict: "CONFIRMED" }, "Confirmed", "text-status-failed"],
    [{ verdict: "PLAUSIBLE" }, "Plausible", "text-warning"],
    [{ verdict: "CONFIRMED", outcome: "fixed" }, "Fixed", "text-accent"],
  ] as const)("labels %o as %s", (fields, label, tone) => {
    const finding: Finding = { ...REQUIRED_ONLY, ...fields };
    const { getByTestId } = render(FindingsCard, { facet: facet([finding]) });
    const badge = getByTestId("finding-badge");
    expect(badge).toHaveTextContent(label);
    if (tone) expect(badge).toHaveClass(tone);
  });

  it("shows no badge without a verdict or outcome, and the category as a tag", () => {
    const { queryByTestId, getByTestId } = render(FindingsCard, {
      facet: facet([{ ...REQUIRED_ONLY, category: "efficiency" }]),
    });
    expect(queryByTestId("finding-badge")).toBeNull();
    expect(getByTestId("finding-category")).toHaveTextContent("efficiency");
  });

  it("copies the review's markdown as built in Rust", async () => {
    const { getByTestId } = render(FindingsCard, { facet: facet([FULL]) });
    await fireEvent.click(getByTestId("findings-copy"));
    expect(copyTextMock).toHaveBeenCalledWith("REVIEW MARKDOWN");
  });
});
