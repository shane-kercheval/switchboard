import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import type { DirectoryInfo, ProjectSummary } from "$lib/types";

// The form picks a folder via the native dialog, probes it via `pick_directory`,
// and commits via the workspace actions — all mocked here so the test exercises
// the form's own logic (validation, gating, which action fires) in isolation.
const openMock = vi.fn<() => Promise<string | null>>();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: () => openMock(),
}));

const pickDirectoryMock = vi.fn<(path: string) => Promise<DirectoryInfo>>();
vi.mock("$lib/api", () => ({
  pickDirectory: (path: string) => pickDirectoryMock(path),
}));

const createProjectAndActivateMock = vi.fn<(name: string, dir: string) => Promise<void>>();
const addDirectoryMock = vi.fn<(path: string) => Promise<void>>();
vi.mock("$lib/state/workspace.svelte", () => ({
  createProjectAndActivate: (name: string, dir: string) => createProjectAndActivateMock(name, dir),
  addDirectory: (path: string) => addDirectoryMock(path),
}));

import CreateProjectForm from "./CreateProjectForm.svelte";

function summary(name: string): ProjectSummary {
  return { id: `id-${name}`, name, created_at: "2026-05-29T00:00:00Z" };
}

function info(path: string, projects: ProjectSummary[]): DirectoryInfo {
  return { path, has_switchboard: projects.length > 0, projects };
}

function renderForm(): { onClose: ReturnType<typeof vi.fn>; onCreated: ReturnType<typeof vi.fn> } {
  const onClose = vi.fn();
  const onCreated = vi.fn();
  render(CreateProjectForm, { props: { onClose, onCreated } });
  return { onClose, onCreated };
}

beforeEach(() => {
  openMock.mockReset();
  pickDirectoryMock.mockReset();
  createProjectAndActivateMock.mockReset();
  createProjectAndActivateMock.mockResolvedValue(undefined);
  addDirectoryMock.mockReset();
  addDirectoryMock.mockResolvedValue(undefined);
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("CreateProjectForm — new project", () => {
  it("creates: choose folder → name → submit calls createProjectAndActivate with the canonical path", async () => {
    openMock.mockResolvedValue("/picked/a");
    pickDirectoryMock.mockResolvedValue(info("/canonical/a", []));
    const { onClose, onCreated } = renderForm();

    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    // Folder is usable immediately; wait for the probe to resolve the canonical path.
    await waitFor(() => expect(pickDirectoryMock).toHaveBeenCalledWith("/picked/a"));

    const name = screen.getByTestId("new-project-name") as HTMLInputElement;
    await fireEvent.input(name, { target: { value: "brand-new" } });
    await fireEvent.click(screen.getByTestId("new-project-submit"));

    expect(createProjectAndActivateMock).toHaveBeenCalledWith("brand-new", "/canonical/a");
    await waitFor(() => expect(onCreated).toHaveBeenCalledTimes(1));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("creates when Enter is pressed in the name field", async () => {
    openMock.mockResolvedValue("/picked/a");
    pickDirectoryMock.mockResolvedValue(info("/canonical/a", []));
    const { onClose, onCreated } = renderForm();

    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    await waitFor(() => expect(pickDirectoryMock).toHaveBeenCalledWith("/picked/a"));

    const name = screen.getByTestId("new-project-name") as HTMLInputElement;
    await fireEvent.input(name, { target: { value: "brand-new" } });
    await fireEvent.keyDown(name, { key: "Enter" });

    expect(createProjectAndActivateMock).toHaveBeenCalledWith("brand-new", "/canonical/a");
    await waitFor(() => expect(onCreated).toHaveBeenCalledTimes(1));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("defaults the name to the folder basename when empty", async () => {
    openMock.mockResolvedValue("/picked/my-repo");
    pickDirectoryMock.mockResolvedValue(info("/picked/my-repo", []));
    renderForm();

    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    const name = screen.getByTestId("new-project-name") as HTMLInputElement;
    await waitFor(() => expect(name.value).toBe("my-repo"));
  });

  it("disables Create and flags the input on an invalid name", async () => {
    openMock.mockResolvedValue("/picked/a");
    pickDirectoryMock.mockResolvedValue(info("/picked/a", []));
    renderForm();

    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    const name = screen.getByTestId("new-project-name") as HTMLInputElement;
    await fireEvent.input(name, { target: { value: "my repo" } });

    expect(screen.getByTestId("new-project-submit")).toBeDisabled();
    expect(screen.getByTestId("new-project-name-error")).toBeInTheDocument();
    expect(name).toHaveAttribute("aria-invalid", "true");
  });

  it("disables Create on a canonical duplicate from the probed folder", async () => {
    openMock.mockResolvedValue("/picked/a");
    pickDirectoryMock.mockResolvedValue(info("/picked/a", [summary("feature-a")]));
    renderForm();

    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    await waitFor(() => expect(pickDirectoryMock).toHaveBeenCalled());

    const name = screen.getByTestId("new-project-name") as HTMLInputElement;
    await fireEvent.input(name, { target: { value: "Feature_A" } }); // collides with feature-a
    await waitFor(() =>
      expect(screen.getByTestId("new-project-name-error")).toHaveTextContent(
        "A project named 'feature-a' already exists",
      ),
    );
    expect(screen.getByTestId("new-project-submit")).toBeDisabled();
  });

  it("keeps create usable when the probe fails (backend stays authoritative)", async () => {
    openMock.mockResolvedValue("/picked/a");
    pickDirectoryMock.mockRejectedValue(new Error("incompatible .switchboard/ version"));
    renderForm();

    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    await waitFor(() => expect(screen.getByTestId("new-project-error")).toBeInTheDocument());

    const name = screen.getByTestId("new-project-name") as HTMLInputElement;
    await fireEvent.input(name, { target: { value: "brand-new" } });
    // Folder was set before the probe failed, so Create is still allowed.
    expect(screen.getByTestId("new-project-submit")).not.toBeDisabled();
  });

  it("ignores an out-of-order folder probe so the newer pick wins", async () => {
    // Pick folder A, then folder B before A's probe resolves; resolve A LAST.
    // A's stale result must not overwrite B's canonical path / siblings.
    let resolveA!: (v: DirectoryInfo) => void;
    let resolveB!: (v: DirectoryInfo) => void;
    const aProbe = new Promise<DirectoryInfo>((r) => {
      resolveA = r;
    });
    const bProbe = new Promise<DirectoryInfo>((r) => {
      resolveB = r;
    });
    pickDirectoryMock.mockImplementation((path: string) =>
      path === "/picked/a" ? aProbe : bProbe,
    );
    renderForm();

    openMock.mockResolvedValueOnce("/picked/a");
    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    await waitFor(() => expect(pickDirectoryMock).toHaveBeenCalledWith("/picked/a"));

    openMock.mockResolvedValueOnce("/picked/b");
    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    await waitFor(() => expect(pickDirectoryMock).toHaveBeenCalledWith("/picked/b"));

    // Newer pick (B) resolves first and is applied; then the stale A resolves —
    // with a sibling that would *wrongly* flag a duplicate if A overwrote B.
    resolveB(info("/canonical/b", []));
    await tick();
    resolveA(info("/canonical/a", [summary("from-b")]));
    await tick();

    const name = screen.getByTestId("new-project-name") as HTMLInputElement;
    await fireEvent.input(name, { target: { value: "from-b" } });
    // Not flagged as a duplicate (A's stale siblings were discarded)...
    expect(screen.queryByTestId("new-project-name-error")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("new-project-submit"));
    // ...and the canonical path is B's, not A's.
    expect(createProjectAndActivateMock).toHaveBeenCalledWith("from-b", "/canonical/b");
  });

  it("surfaces a create failure and keeps the dialog open", async () => {
    openMock.mockResolvedValue("/picked/a");
    pickDirectoryMock.mockResolvedValue(info("/picked/a", []));
    createProjectAndActivateMock.mockRejectedValueOnce(new Error("disk full"));
    const { onClose } = renderForm();

    await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
    await waitFor(() => expect(pickDirectoryMock).toHaveBeenCalled());
    const name = screen.getByTestId("new-project-name") as HTMLInputElement;
    await fireEvent.input(name, { target: { value: "brand-new" } });
    await fireEvent.click(screen.getByTestId("new-project-submit"));

    await waitFor(() =>
      expect(screen.getByTestId("new-project-error")).toHaveTextContent("disk full"),
    );
    expect(onClose).not.toHaveBeenCalled();
  });
});

describe("CreateProjectForm — add existing", () => {
  async function switchToExisting(): Promise<void> {
    await fireEvent.click(screen.getByTestId("project-dialog-mode-existing"));
    await waitFor(() => expect(screen.getByTestId("add-existing-form")).toBeInTheDocument());
  }

  it("previews discovered projects and Add commits via addDirectory", async () => {
    openMock.mockResolvedValue("/picked/b");
    pickDirectoryMock.mockResolvedValue(info("/canonical/b", [summary("existing-proj")]));
    const { onClose } = renderForm();
    await switchToExisting();

    await fireEvent.click(screen.getByTestId("add-existing-choose-folder"));
    await waitFor(() => expect(screen.getByTestId("add-existing-found")).toBeInTheDocument());
    expect(screen.getByTestId("add-existing-found")).toHaveTextContent("existing-proj");
    expect(screen.getByTestId("add-existing-add")).toBeEnabled();

    await fireEvent.click(screen.getByTestId("add-existing-add"));
    expect(addDirectoryMock).toHaveBeenCalledWith("/canonical/b");
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("shows the empty state and disables Add when the folder has no projects", async () => {
    openMock.mockResolvedValue("/picked/empty");
    pickDirectoryMock.mockResolvedValue(info("/picked/empty", []));
    renderForm();
    await switchToExisting();

    await fireEvent.click(screen.getByTestId("add-existing-choose-folder"));
    await waitFor(() => expect(screen.getByTestId("add-existing-none")).toBeInTheDocument());
    expect(screen.queryByTestId("add-existing-found")).not.toBeInTheDocument();
    expect(screen.getByTestId("add-existing-add")).toBeDisabled();
    expect(addDirectoryMock).not.toHaveBeenCalled();
  });

  it("surfaces a probe failure and disables Add", async () => {
    openMock.mockResolvedValue("/picked/bad");
    pickDirectoryMock.mockRejectedValue(new Error("incompatible .switchboard/ version"));
    renderForm();
    await switchToExisting();

    await fireEvent.click(screen.getByTestId("add-existing-choose-folder"));
    await waitFor(() => expect(screen.getByTestId("add-existing-error")).toBeInTheDocument());
    expect(screen.getByTestId("add-existing-add")).toBeDisabled();
    expect(addDirectoryMock).not.toHaveBeenCalled();
  });
});

describe("CreateProjectForm — chrome", () => {
  it("Cancel calls onClose", async () => {
    const { onClose } = renderForm();
    await fireEvent.click(screen.getByTestId("new-project-cancel"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
