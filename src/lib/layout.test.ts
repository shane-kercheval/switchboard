import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AGENTS_SIDEBAR_DEFAULT_WIDTH,
  DIFF_FILE_LIST_DEFAULT_WIDTH,
  DIFF_FILE_LIST_MAX_WIDTH,
  PROJECTS_SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MIN_WIDTH,
  _testing,
  layout,
  sidebarMaxWidth,
} from "./layout.svelte";

const STORAGE_KEY = "switchboard-layout";

function seed(stored: unknown): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(stored));
  _testing.reloadFromStorage();
}

function setViewportWidth(px: number): void {
  Object.defineProperty(window, "innerWidth", { value: px, configurable: true });
}

beforeEach(() => {
  setViewportWidth(1024);
  localStorage.clear();
  _testing.reset();
});

afterEach(() => {
  _testing.reset();
  vi.restoreAllMocks();
});

describe("layout store", () => {
  it("defaults with nothing stored", () => {
    expect(layout.projectsSidebarWidth).toBe(PROJECTS_SIDEBAR_DEFAULT_WIDTH);
    expect(layout.agentsSidebarWidth).toBe(AGENTS_SIDEBAR_DEFAULT_WIDTH);
    expect(layout.projectsSidebarOpen).toBe(true);
    expect(layout.agentsSidebarOpen).toBe(true);
    expect(layout.gitDetailWidth).toBeNull();
    expect(layout.diffFileListWidth).toBe(DIFF_FILE_LIST_DEFAULT_WIDTH);
  });

  it("round-trips widths and collapse state through storage", () => {
    layout.projectsSidebarWidth = 320;
    layout.agentsSidebarWidth = 300;
    layout.projectsSidebarOpen = false;
    layout.agentsSidebarOpen = false;
    layout.gitDetailWidth = 500;
    layout.diffFileListWidth = 300;
    _testing.reloadFromStorage();
    expect(layout.projectsSidebarWidth).toBe(320);
    expect(layout.agentsSidebarWidth).toBe(300);
    expect(layout.projectsSidebarOpen).toBe(false);
    expect(layout.agentsSidebarOpen).toBe(false);
    expect(layout.gitDetailWidth).toBe(500);
    expect(layout.diffFileListWidth).toBe(300);
  });

  it("clamps a sidebar width saved on a larger monitor against the current viewport", () => {
    seed({
      version: 1,
      layout: { projectsSidebar: { width: 5000, open: true } },
    });
    expect(layout.projectsSidebarWidth).toBe(sidebarMaxWidth());
    expect(layout.projectsSidebarWidth).toBeLessThanOrEqual(Math.round(1024 * 0.4));
  });

  it("sidebarMaxWidth follows the viewport but never exceeds 480", () => {
    setViewportWidth(900);
    expect(sidebarMaxWidth()).toBe(360);
    setViewportWidth(3000);
    expect(sidebarMaxWidth()).toBe(480);
  });

  it("clamps a stored git detail width against the viewport and floors it at its minimum", () => {
    seed({ version: 1, layout: { gitDetailWidth: 10_000 } });
    expect(layout.gitDetailWidth).toBe(Math.round(1024 * 0.85));
    seed({ version: 1, layout: { gitDetailWidth: 10 } });
    expect(layout.gitDetailWidth).toBe(360);
  });

  it("clamps widths on write, not only on read", () => {
    layout.projectsSidebarWidth = 5;
    expect(layout.projectsSidebarWidth).toBe(SIDEBAR_MIN_WIDTH);
    layout.diffFileListWidth = 9999;
    expect(layout.diffFileListWidth).toBe(DIFF_FILE_LIST_MAX_WIDTH);
  });

  it("setting gitDetailWidth back to null persists the unset state", () => {
    layout.gitDetailWidth = 500;
    layout.gitDetailWidth = null;
    _testing.reloadFromStorage();
    expect(layout.gitDetailWidth).toBeNull();
  });

  it("degrades a corrupt blob to defaults", () => {
    localStorage.setItem(STORAGE_KEY, "{not json");
    _testing.reloadFromStorage();
    expect(layout.projectsSidebarWidth).toBe(PROJECTS_SIDEBAR_DEFAULT_WIDTH);
    expect(layout.projectsSidebarOpen).toBe(true);
  });

  it("degrades wrong version and malformed field types to defaults", () => {
    seed({ version: 99, layout: { projectsSidebar: { width: 320, open: false } } });
    expect(layout.projectsSidebarWidth).toBe(PROJECTS_SIDEBAR_DEFAULT_WIDTH);
    seed({
      version: 1,
      layout: {
        projectsSidebar: { width: "wide", open: "yes" },
        gitDetailWidth: "big",
        diffFileListWidth: Number.NaN,
      },
    });
    expect(layout.projectsSidebarWidth).toBe(PROJECTS_SIDEBAR_DEFAULT_WIDTH);
    expect(layout.projectsSidebarOpen).toBe(true);
    expect(layout.gitDetailWidth).toBeNull();
    expect(layout.diffFileListWidth).toBe(DIFF_FILE_LIST_DEFAULT_WIDTH);
  });

  it("survives a persist failure with the in-memory value intact", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    layout.agentsSidebarWidth = 320;
    expect(layout.agentsSidebarWidth).toBe(320);
  });
});
