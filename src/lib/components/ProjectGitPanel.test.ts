import { afterEach, describe, expect, it, vi, type Mock } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/svelte";
import ProjectGitPanel from "./ProjectGitPanel.svelte";
import type { BranchView, ProjectListing } from "$lib/types";

// The panel reads its data from the Git-view store; mock it so the component's
// rendering branches are exercised in isolation (the store's own resolution is
// covered in gitView.svelte.test.ts).
const loadProjectRepo = vi.fn<(path: string) => Promise<void>>(async () => {});
const projectBranch = vi.fn<(id: string) => unknown>();
vi.mock("$lib/state/gitView.svelte", () => ({
  loadProjectRepo: (path: string) => loadProjectRepo(path),
  projectBranch: (id: string) => projectBranch(id),
}));

afterEach(() => {
  loadProjectRepo.mockClear();
  projectBranch.mockReset();
});

const project: ProjectListing = {
  id: "p1",
  name: "alpha",
  created_at: "",
  directory: "/repos/alpha",
  available: true,
  last_activity: "",
  archived: false,
};

const branch = (over: Partial<BranchView> = {}): BranchView => ({
  name: "feature-x",
  upstream: null,
  sync: { kind: "in_sync" },
  behind_base: null,
  merged: null,
  dangling: false,
  worktree: {
    path: "/repos/alpha",
    dirty: false,
    untracked: false,
    detached_hash: null,
    warning: null,
  },
  ...over,
});

describe("ProjectGitPanel", () => {
  it("loads the project's repo and shows its branch + status badges", () => {
    (projectBranch as Mock).mockReturnValue({
      branch: branch({
        worktree: {
          path: "/repos/alpha",
          dirty: true,
          untracked: false,
          detached_hash: null,
          warning: null,
        },
      }),
      defaultBranch: "main",
    });
    render(ProjectGitPanel, { project });

    expect(loadProjectRepo).toHaveBeenCalledWith("/repos/alpha");
    expect(screen.getByTestId("project-git-branch")).toHaveTextContent("feature-x");
    // A dirty folder surfaces the changes badge (same mapping as the Git view).
    expect(screen.getByLabelText("changes")).toBeInTheDocument();
  });

  it("surfaces the behind-default-branch indicator", () => {
    (projectBranch as Mock).mockReturnValue({
      branch: branch({ behind_base: 4 }),
      defaultBranch: "main",
    });
    render(ProjectGitPanel, { project });
    expect(screen.getByLabelText("behind main")).toBeInTheDocument();
  });

  it("shows a calm 'no changes' line for a clean, in-sync branch", () => {
    (projectBranch as Mock).mockReturnValue({ branch: branch(), defaultBranch: "main" });
    render(ProjectGitPanel, { project });
    expect(screen.getByTestId("project-git-clean")).toBeInTheDocument();
    expect(screen.queryByTestId("project-git-badges")).not.toBeInTheDocument();
  });

  it("renders nothing when the project isn't a resolvable git repo", () => {
    (projectBranch as Mock).mockReturnValue(null);
    render(ProjectGitPanel, { project });
    expect(screen.queryByTestId("project-git-panel")).not.toBeInTheDocument();
    // Still attempts the load (which the store degrades gracefully).
    expect(loadProjectRepo).toHaveBeenCalledWith("/repos/alpha");
  });
});
