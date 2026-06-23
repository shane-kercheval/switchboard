import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import WorkflowMenu from "./WorkflowMenu.svelte";
import type { WorkflowListing } from "$lib/types";

function listing(over: Partial<WorkflowListing> = {}): WorkflowListing {
  return {
    name: "review-and-aggregate",
    is_builtin: true,
    description: "Fan out reviews then aggregate",
    inputs: [],
    invocable: true,
    parse_error: null,
    ...over,
  };
}

function setup(workflows: WorkflowListing[]) {
  const onpick = vi.fn();
  const oncopy = vi.fn();
  const onopenfolder = vi.fn();
  const onclose = vi.fn();
  render(WorkflowMenu, { props: { workflows, onpick, oncopy, onopenfolder, onclose } });
  return { onpick, oncopy, onopenfolder, onclose };
}

describe("WorkflowMenu", () => {
  it("tags a built-in read-only and offers copy; picking enters it", async () => {
    const { onpick, oncopy } = setup([listing()]);
    const key = "builtin:review-and-aggregate";
    expect(screen.getByTestId(`workflow-builtin-tag-${key}`)).toBeInTheDocument();
    // The copy action is icon-only — its accessible name is the only text
    // affordance, so it carries the discoverability/a11y contract.
    expect(screen.getByTestId(`workflow-copy-${key}`)).toHaveAccessibleName("Copy to my workflows");

    await fireEvent.click(screen.getByTestId(`workflow-copy-${key}`));
    expect(oncopy).toHaveBeenCalledTimes(1);
    expect(onpick).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByTestId(`workflow-option-${key}`));
    expect(onpick.mock.calls[0]?.[0]).toMatchObject({ name: "review-and-aggregate" });
  });

  it("shows a parse error and does not pick that row", async () => {
    const { onpick } = setup([
      listing({ name: "broken", is_builtin: false, parse_error: "bad yaml", invocable: false }),
    ]);
    const key = "dir:broken";
    expect(screen.getByTestId(`workflow-parse-error-${key}`)).toHaveTextContent("bad yaml");
    await fireEvent.click(screen.getByTestId(`workflow-option-${key}`));
    expect(onpick).not.toHaveBeenCalled();
  });

  it("flags a non-invocable workflow and refuses to pick it", async () => {
    const { onpick } = setup([listing({ name: "iterate", is_builtin: false, invocable: false })]);
    const key = "dir:iterate";
    expect(screen.getByTestId(`workflow-not-invocable-${key}`)).toHaveTextContent(
      "step type not supported",
    );
    await fireEvent.click(screen.getByTestId(`workflow-option-${key}`));
    expect(onpick).not.toHaveBeenCalled();
  });

  it("filters via the search field and shows the empty state", async () => {
    setup([
      listing(),
      listing({ name: "other", is_builtin: false, description: "something else" }),
    ]);
    await fireEvent.input(screen.getByTestId("workflow-menu-search"), {
      target: { value: "aggregate" },
    });
    expect(screen.getByTestId("workflow-option-builtin:review-and-aggregate")).toBeInTheDocument();
    expect(screen.queryByTestId("workflow-option-dir:other")).toBeNull();

    await fireEvent.input(screen.getByTestId("workflow-menu-search"), {
      target: { value: "zzz" },
    });
    expect(screen.getByTestId("workflow-menu-empty")).toHaveTextContent("No matching workflows");
  });

  it("offers an open-folder action", async () => {
    const { onopenfolder } = setup([listing()]);
    await fireEvent.click(screen.getByTestId("workflow-menu-open-folder"));
    expect(onopenfolder).toHaveBeenCalledTimes(1);
  });
});
