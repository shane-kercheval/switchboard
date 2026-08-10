import type { ProjectId } from "$lib/types";

/// Persisted app-layout preferences: sidebar widths + collapse state, the Git
/// view's repository-pane width, and the diff panel's file-list width.
///
/// **Device-local, with explicit project-scoped preferences.** Sidebar widths
/// and open state express facts about the device and mean the same thing in
/// every project. The selected right-sidebar content and Pins ordering are
/// keyed by project, however: switching projects restores how that project's
/// right sidebar was last used. (Transcript pane *fractions* are also
/// per-project because pane membership is; see `state/transcriptPanes.svelte.ts`.)
///
/// Like the theme (`theme.svelte.ts`), this lives in `localStorage` rather than
/// the git-trackable `config.yaml`: layout is a device-local appearance
/// preference, and syncing it across machines via a checked-in file would be
/// wrong. Stored under a versioned envelope; anything malformed degrades to
/// defaults — layout is ergonomic, not load-bearing.
///
/// Widths are pixels, deliberately: a 280px rail should stay 280px on a bigger
/// monitor (the content area is what should grow). The store retains the user's
/// preferred width within each pane's absolute bounds. Sidebars additionally
/// apply a viewport-relative live cap in CSS, but that temporary rendered width
/// never replaces the stored preference. The Git repository and changed-files
/// panes do not have live caps: the diff canvas alone absorbs window resizing.

const STORAGE_KEY = "switchboard-layout";
const STORAGE_VERSION = 1;

/// Defaults match the pre-resizable Tailwind widths (`w-72` / `w-60`) so an
/// untouched install looks identical.
export const PROJECTS_SIDEBAR_DEFAULT_WIDTH = 288;
export const AGENTS_SIDEBAR_DEFAULT_WIDTH = 240;
export const PINS_SIDEBAR_DEFAULT_WIDTH = 360;
export const GIT_REPO_DEFAULT_WIDTH = 360;
export const DIFF_FILE_LIST_DEFAULT_WIDTH = 256;
export const RIGHT_SIDEBAR_DEFAULT_MODE: RightSidebarMode = "agents";
export const PINS_SORT_DEFAULT_MODE: PinsSortMode = "pinned_at";

export const SIDEBAR_MIN_WIDTH = 200;
export const SIDEBAR_MAX_WIDTH = 480;
export const RIGHT_SIDEBAR_MAX_WIDTH = 960;
export const GIT_REPO_MIN_WIDTH = 240;
export const GIT_REPO_MAX_WIDTH = 480;
export const DIFF_FILE_LIST_MIN_WIDTH = 176;
export const DIFF_FILE_LIST_MAX_WIDTH = 440;

function viewportWidth(): number {
  return typeof window === "undefined" ? Number.POSITIVE_INFINITY : window.innerWidth;
}

/// Live upper bound for a sidebar: never wider than 480px or 40% of the
/// viewport — a rail, not a split view.
export function sidebarMaxWidth(): number {
  return Math.max(
    SIDEBAR_MIN_WIDTH,
    Math.min(SIDEBAR_MAX_WIDTH, Math.round(viewportWidth() * 0.4)),
  );
}

/// The right sidebar doubles as a reading surface in Pins mode, so it may grow
/// beyond the narrower project-picker rail while still leaving room for the
/// transcript beside it.
export function rightSidebarMaxWidth(): number {
  return Math.max(
    SIDEBAR_MIN_WIDTH,
    Math.min(RIGHT_SIDEBAR_MAX_WIDTH, Math.round(viewportWidth() * 0.6)),
  );
}

function clampSidebarWidth(px: number, maxWidth: number): number {
  return Math.min(maxWidth, Math.max(SIDEBAR_MIN_WIDTH, Math.round(px)));
}

function clampGitRepoWidth(px: number): number {
  return Math.min(GIT_REPO_MAX_WIDTH, Math.max(GIT_REPO_MIN_WIDTH, Math.round(px)));
}

function clampDiffFileListWidth(px: number): number {
  return Math.min(DIFF_FILE_LIST_MAX_WIDTH, Math.max(DIFF_FILE_LIST_MIN_WIDTH, Math.round(px)));
}

type SidebarLayout = { width: number; open: boolean };
export type RightSidebarMode = "agents" | "pins";
export type PinsSortMode = "pinned_at" | "message_at";
type ProjectLayoutPreferences = {
  rightSidebarMode?: RightSidebarMode;
  pinsSortMode?: PinsSortMode;
};

type LayoutState = {
  projectsSidebar: SidebarLayout;
  agentsSidebar: SidebarLayout;
  pinsSidebarWidth: number;
  projectPreferences: Record<ProjectId, ProjectLayoutPreferences>;
  gitRepoWidth: number;
  diffFileListWidth: number;
};

function defaults(): LayoutState {
  return {
    projectsSidebar: { width: PROJECTS_SIDEBAR_DEFAULT_WIDTH, open: true },
    agentsSidebar: { width: AGENTS_SIDEBAR_DEFAULT_WIDTH, open: true },
    pinsSidebarWidth: PINS_SIDEBAR_DEFAULT_WIDTH,
    projectPreferences: {},
    gitRepoWidth: GIT_REPO_DEFAULT_WIDTH,
    diffFileListWidth: DIFF_FILE_LIST_DEFAULT_WIDTH,
  };
}

function parseRightSidebarMode(value: unknown): RightSidebarMode | undefined {
  if (value === "agents" || value === "pins") return value;
  return undefined;
}

function parsePinsSortMode(value: unknown): PinsSortMode | undefined {
  if (value === "pinned_at" || value === "message_at") return value;
  return undefined;
}

function parseProjectPreferences(value: unknown): Record<ProjectId, ProjectLayoutPreferences> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(
    Object.entries(value).flatMap(([projectId, stored]) => {
      if (projectId.length === 0 || stored === null || typeof stored !== "object") return [];
      const raw = stored as { rightSidebarMode?: unknown; pinsSortMode?: unknown };
      const rightSidebarMode = parseRightSidebarMode(raw.rightSidebarMode);
      const pinsSortMode = parsePinsSortMode(raw.pinsSortMode);
      if (rightSidebarMode === undefined && pinsSortMode === undefined) return [];
      return [[projectId, { rightSidebarMode, pinsSortMode }]];
    }),
  );
}

function parseSidebar(value: unknown, fallback: SidebarLayout, maxWidth: number): SidebarLayout {
  if (value === null || typeof value !== "object") return fallback;
  const v = value as { width?: unknown; open?: unknown };
  return {
    width:
      typeof v.width === "number" && Number.isFinite(v.width)
        ? clampSidebarWidth(v.width, maxWidth)
        : fallback.width,
    open: typeof v.open === "boolean" ? v.open : fallback.open,
  };
}

function readStored(): LayoutState {
  const base = defaults();
  if (typeof localStorage === "undefined") return base;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) return base;
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object") return base;
    const envelope = parsed as { version?: unknown; layout?: unknown };
    if (envelope.version !== STORAGE_VERSION) return base;
    if (envelope.layout === null || typeof envelope.layout !== "object") return base;
    const v = envelope.layout as {
      projectsSidebar?: unknown;
      agentsSidebar?: unknown;
      pinsSidebarWidth?: unknown;
      projectPreferences?: unknown;
      gitRepoWidth?: unknown;
      diffFileListWidth?: unknown;
    };
    return {
      projectsSidebar: parseSidebar(v.projectsSidebar, base.projectsSidebar, SIDEBAR_MAX_WIDTH),
      agentsSidebar: parseSidebar(v.agentsSidebar, base.agentsSidebar, SIDEBAR_MAX_WIDTH),
      pinsSidebarWidth:
        typeof v.pinsSidebarWidth === "number" && Number.isFinite(v.pinsSidebarWidth)
          ? clampSidebarWidth(v.pinsSidebarWidth, RIGHT_SIDEBAR_MAX_WIDTH)
          : base.pinsSidebarWidth,
      projectPreferences: parseProjectPreferences(v.projectPreferences),
      gitRepoWidth:
        typeof v.gitRepoWidth === "number" && Number.isFinite(v.gitRepoWidth)
          ? clampGitRepoWidth(v.gitRepoWidth)
          : base.gitRepoWidth,
      diffFileListWidth:
        typeof v.diffFileListWidth === "number" && Number.isFinite(v.diffFileListWidth)
          ? clampDiffFileListWidth(v.diffFileListWidth)
          : base.diffFileListWidth,
    };
  } catch {
    return base;
  }
}

const state = $state<LayoutState>(readStored());

function persist(): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ version: STORAGE_VERSION, layout: state }));
  } catch {
    // Quota or serialization failure — layout degrades to in-memory-only.
  }
}

export const layout = {
  get projectsSidebarWidth(): number {
    return state.projectsSidebar.width;
  },
  set projectsSidebarWidth(px: number) {
    state.projectsSidebar.width = clampSidebarWidth(px, SIDEBAR_MAX_WIDTH);
    persist();
  },
  get projectsSidebarOpen(): boolean {
    return state.projectsSidebar.open;
  },
  set projectsSidebarOpen(open: boolean) {
    state.projectsSidebar.open = open;
    persist();
  },
  get agentsSidebarWidth(): number {
    return state.agentsSidebar.width;
  },
  set agentsSidebarWidth(px: number) {
    state.agentsSidebar.width = clampSidebarWidth(px, SIDEBAR_MAX_WIDTH);
    persist();
  },
  get rightSidebarOpen(): boolean {
    return state.agentsSidebar.open;
  },
  set rightSidebarOpen(open: boolean) {
    state.agentsSidebar.open = open;
    persist();
  },
  get pinsSidebarWidth(): number {
    return state.pinsSidebarWidth;
  },
  set pinsSidebarWidth(px: number) {
    state.pinsSidebarWidth = clampSidebarWidth(px, RIGHT_SIDEBAR_MAX_WIDTH);
    persist();
  },
  rightSidebarModeFor(projectId: ProjectId): RightSidebarMode {
    // The direct indexed read subscribes Svelte to an absent project key so
    // the first project-specific write repaints its mounted consumers.
    return state.projectPreferences[projectId]?.rightSidebarMode ?? RIGHT_SIDEBAR_DEFAULT_MODE;
  },
  setRightSidebarMode(projectId: ProjectId, mode: RightSidebarMode): void {
    state.projectPreferences[projectId] = {
      ...state.projectPreferences[projectId],
      rightSidebarMode: mode,
    };
    persist();
  },
  pinsSortModeFor(projectId: ProjectId): PinsSortMode {
    return state.projectPreferences[projectId]?.pinsSortMode ?? PINS_SORT_DEFAULT_MODE;
  },
  setPinsSortMode(projectId: ProjectId, mode: PinsSortMode): void {
    state.projectPreferences[projectId] = {
      ...state.projectPreferences[projectId],
      pinsSortMode: mode,
    };
    persist();
  },
  removeProjectPreferences(projectId: ProjectId): void {
    if (state.projectPreferences[projectId] === undefined) return;
    delete state.projectPreferences[projectId];
    persist();
  },
  get gitRepoWidth(): number {
    return state.gitRepoWidth;
  },
  set gitRepoWidth(px: number) {
    state.gitRepoWidth = clampGitRepoWidth(px);
    persist();
  },
  get diffFileListWidth(): number {
    return state.diffFileListWidth;
  },
  set diffFileListWidth(px: number) {
    state.diffFileListWidth = clampDiffFileListWidth(px);
    persist();
  },
};

/// Test-only API surface. Production hydrates once at module load; tests use
/// `reset` to isolate between cases and `reloadFromStorage` to exercise the
/// restart path.
export const _testing = {
  reset(): void {
    Object.assign(state, defaults());
    if (typeof localStorage !== "undefined") localStorage.removeItem(STORAGE_KEY);
  },
  reloadFromStorage(): void {
    Object.assign(state, readStored());
  },
};
