/// Persisted app-layout preferences: sidebar widths + collapse state, the Git
/// view's detail-pane width, and the diff panel's file-list width.
///
/// **Global per device, not per project.** A sidebar's width expresses a fact
/// about your monitor and reading preference — it means the same thing in every
/// project, and making it per-project would reflow the whole app on every
/// project switch. (Transcript pane *fractions* are per-project because pane
/// membership is; see `state/transcriptPanes.svelte.ts`.)
///
/// Like the theme (`theme.svelte.ts`), this lives in `localStorage` rather than
/// the git-trackable `config.yaml`: layout is a device-local appearance
/// preference, and syncing it across machines via a checked-in file would be
/// wrong. Stored under a versioned envelope; anything malformed degrades to
/// defaults — layout is ergonomic, not load-bearing.
///
/// Widths are pixels, deliberately: a 280px rail should stay 280px on a bigger
/// monitor (the content area is what should grow). The pixel representation is
/// kept honest in two layers. This store sanitizes on read and write — a value
/// from a bigger monitor (or a hand-edited blob) is clamped against the current
/// viewport before it's ever returned. The *live* bound is CSS: each consumer
/// mirrors its clamp as a `max-width` (SidebarPanel, the Git detail aside, the
/// diff file list), so a mid-session window shrink caps the rendered width
/// immediately — and because the stored preference is never rewritten by that
/// shrink, the panel re-expands when the window grows back. `ResizeHandle`
/// clamps its start value to the same live bound, so a drag begins from the
/// width on screen, not the invisible stored one.

const STORAGE_KEY = "switchboard-layout";
const STORAGE_VERSION = 1;

/// Defaults match the pre-resizable Tailwind widths (`w-72` / `w-60`) so an
/// untouched install looks identical.
export const PROJECTS_SIDEBAR_DEFAULT_WIDTH = 288;
export const AGENTS_SIDEBAR_DEFAULT_WIDTH = 240;
export const DIFF_FILE_LIST_DEFAULT_WIDTH = 256;

export const SIDEBAR_MIN_WIDTH = 200;
/// The Git detail pane's minimum, shared by its drag clamp and the read clamp.
export const GIT_DETAIL_MIN_WIDTH = 360;
export const DIFF_FILE_LIST_MIN_WIDTH = 176;
export const DIFF_FILE_LIST_MAX_WIDTH = 440;

function viewportWidth(): number {
  return typeof window === "undefined" ? Number.POSITIVE_INFINITY : window.innerWidth;
}

/// Live upper bound for a sidebar: never wider than 480px or 40% of the
/// viewport — a rail, not a split view.
export function sidebarMaxWidth(): number {
  return Math.max(SIDEBAR_MIN_WIDTH, Math.min(480, Math.round(viewportWidth() * 0.4)));
}

function clampSidebarWidth(px: number): number {
  return Math.min(sidebarMaxWidth(), Math.max(SIDEBAR_MIN_WIDTH, Math.round(px)));
}

/// The detail pane's drag clamp is 85% of its live split container; on read the
/// viewport is the best available stand-in for that container.
function clampGitDetailWidth(px: number | null): number | null {
  if (px === null) return null;
  const max = Math.max(GIT_DETAIL_MIN_WIDTH, Math.round(viewportWidth() * 0.85));
  return Math.min(max, Math.max(GIT_DETAIL_MIN_WIDTH, Math.round(px)));
}

function clampDiffFileListWidth(px: number): number {
  return Math.min(DIFF_FILE_LIST_MAX_WIDTH, Math.max(DIFF_FILE_LIST_MIN_WIDTH, Math.round(px)));
}

type SidebarLayout = { width: number; open: boolean };

type LayoutState = {
  projectsSidebar: SidebarLayout;
  agentsSidebar: SidebarLayout;
  /// null = never dragged: the Git view keeps its CSS default (2/3 of the split).
  gitDetailWidth: number | null;
  diffFileListWidth: number;
};

function defaults(): LayoutState {
  return {
    projectsSidebar: { width: PROJECTS_SIDEBAR_DEFAULT_WIDTH, open: true },
    agentsSidebar: { width: AGENTS_SIDEBAR_DEFAULT_WIDTH, open: true },
    gitDetailWidth: null,
    diffFileListWidth: DIFF_FILE_LIST_DEFAULT_WIDTH,
  };
}

function parseSidebar(value: unknown, fallback: SidebarLayout): SidebarLayout {
  if (value === null || typeof value !== "object") return fallback;
  const v = value as { width?: unknown; open?: unknown };
  return {
    width:
      typeof v.width === "number" && Number.isFinite(v.width)
        ? clampSidebarWidth(v.width)
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
      gitDetailWidth?: unknown;
      diffFileListWidth?: unknown;
    };
    return {
      projectsSidebar: parseSidebar(v.projectsSidebar, base.projectsSidebar),
      agentsSidebar: parseSidebar(v.agentsSidebar, base.agentsSidebar),
      gitDetailWidth:
        typeof v.gitDetailWidth === "number" && Number.isFinite(v.gitDetailWidth)
          ? clampGitDetailWidth(v.gitDetailWidth)
          : null,
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
    state.projectsSidebar.width = clampSidebarWidth(px);
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
    state.agentsSidebar.width = clampSidebarWidth(px);
    persist();
  },
  get agentsSidebarOpen(): boolean {
    return state.agentsSidebar.open;
  },
  set agentsSidebarOpen(open: boolean) {
    state.agentsSidebar.open = open;
    persist();
  },
  get gitDetailWidth(): number | null {
    return state.gitDetailWidth;
  },
  set gitDetailWidth(px: number | null) {
    state.gitDetailWidth = clampGitDetailWidth(px);
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
