import { describe, it, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import DirectorySelector from "./DirectorySelector.svelte";
import type { DirectoryInfo, ProjectSummary } from "$lib/types";

// The three render branches of DirectorySelector are user-visible CTAs in
// the M1 acceptance flow. A regression that wired up the wrong callback or
// rendered the wrong branch would silently take the user down the wrong
// path; these tests cover that boundary.

function info(path: string, has_switchboard: boolean, projects: ProjectSummary[]): DirectoryInfo {
  return { path, has_switchboard, projects };
}

const NO_SWITCHBOARD = info("/tmp/sw-test", false, []);
const SWITCHBOARD_EMPTY = info("/tmp/sw-test", true, []);
const SWITCHBOARD_WITH_PROJECTS = info("/tmp/sw-test", true, [
  {
    id: "11111111-1111-7000-8000-111111111111",
    name: "alpha",
    created_at: "2026-05-13T00:00:00Z",
  },
  {
    id: "22222222-2222-7000-8000-222222222222",
    name: "beta",
    created_at: "2026-05-13T01:00:00Z",
  },
]);

function noopHandlers() {
  return {
    onInitAndCreate: vi.fn(),
    onCreateProject: vi.fn(),
    onSelectProject: vi.fn(),
    onCancel: vi.fn(),
  };
}

describe("DirectorySelector", () => {
  it("no .switchboard/ branch: shows Initialize CTA wired to onInitAndCreate", async () => {
    const h = noopHandlers();
    render(DirectorySelector, { props: { info: NO_SWITCHBOARD, ...h } });

    // Default project name suggests the directory's basename.
    const nameInput = screen.getByTestId("initial-project-name") as HTMLInputElement;
    expect(nameInput.value).toBe("sw-test");

    await fireEvent.click(screen.getByTestId("confirm-init"));
    expect(h.onInitAndCreate).toHaveBeenCalledExactlyOnceWith("sw-test");
    expect(h.onCreateProject).not.toHaveBeenCalled();
    expect(h.onSelectProject).not.toHaveBeenCalled();
  });

  it("initialized + empty projects branch: shows Create-project CTA wired to onCreateProject", async () => {
    const h = noopHandlers();
    render(DirectorySelector, { props: { info: SWITCHBOARD_EMPTY, ...h } });

    expect(screen.queryByTestId("confirm-init")).not.toBeInTheDocument();
    // The visible CTA in this branch is "Create project" — find it by role+name.
    const createButton = screen.getByRole("button", { name: /create project/i });
    await fireEvent.click(createButton);
    expect(h.onCreateProject).toHaveBeenCalledExactlyOnceWith("sw-test");
    expect(h.onInitAndCreate).not.toHaveBeenCalled();
  });

  it("initialized + non-empty branch: lists projects, row click calls onSelectProject", async () => {
    const h = noopHandlers();
    render(DirectorySelector, { props: { info: SWITCHBOARD_WITH_PROJECTS, ...h } });

    const rows = screen.getAllByTestId("project-row");
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent("alpha");
    expect(rows[1]).toHaveTextContent("beta");

    await fireEvent.click(rows[1]!); // pick beta
    expect(h.onSelectProject).toHaveBeenCalledExactlyOnceWith(
      SWITCHBOARD_WITH_PROJECTS.projects[1],
    );
    expect(h.onCreateProject).not.toHaveBeenCalled();
  });

  it("initialized + non-empty branch: 'Create another project' reveals form wired to onCreateProject", async () => {
    const h = noopHandlers();
    render(DirectorySelector, { props: { info: SWITCHBOARD_WITH_PROJECTS, ...h } });

    // Before clicking, the input is hidden.
    expect(screen.queryByRole("button", { name: /^create$/i })).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: /create another project/i }));

    // Input is now visible — type a name and submit.
    const inputs = screen.getAllByRole("textbox") as HTMLInputElement[];
    const newProjectInput = inputs[inputs.length - 1]!;
    await fireEvent.input(newProjectInput, { target: { value: "gamma" } });
    await fireEvent.click(screen.getByRole("button", { name: /^create$/i }));
    expect(h.onCreateProject).toHaveBeenCalledExactlyOnceWith("gamma");
    expect(h.onSelectProject).not.toHaveBeenCalled();
  });

  it.each([
    ["no .switchboard/", NO_SWITCHBOARD],
    ["initialized + empty projects", SWITCHBOARD_EMPTY],
    ["initialized + non-empty projects", SWITCHBOARD_WITH_PROJECTS],
  ])("Cancel button calls onCancel — %s branch", async (_label, info) => {
    const h = noopHandlers();
    const { unmount } = render(DirectorySelector, { props: { info, ...h } });
    // In all three branches the Cancel button is the last one rendered.
    const cancels = screen.getAllByRole("button", { name: /cancel/i });
    await fireEvent.click(cancels[cancels.length - 1]!);
    expect(h.onCancel).toHaveBeenCalledOnce();
    unmount();
  });
});
