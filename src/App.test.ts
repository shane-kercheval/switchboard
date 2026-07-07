import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { flushSync } from "svelte";
import type {
  AgentRecord,
  NormalizedEvent,
  Prompt,
  ProjectConversation,
  ProjectListing,
  ProjectSummary,
  RepoListing,
} from "$lib/types";
import { ALL_HARNESSES } from "$lib/harnessDisplay";
// Static import so App.svelte's (large) component-tree transform happens at
// module collection, not inside the first test that calls `mountApp`. `vi.mock`
// is hoisted above all imports, so the mocked IPC/event/dialog modules still
// apply. Importing it lazily charged the cold transform (~8s locally, more on
// CI) to the first test's 15s budget, which intermittently timed it out.
import App from "./App.svelte";

// App.svelte tests focus on the workspace-level orchestration: eager registry
// load, lazy per-project activation (roster + hydration), display-only project
// switching (listeners persist), the not-persistable signal, and the
// post-restart merged-conversation render. Per-screen UI (ProjectsSidebar /
// Sidebar / UnifiedTranscript / ComposeBar) has its own component tests; these
// verify the glue.
//
// The IPC layer is mocked with a small **stateful fake backend** so flows read
// naturally (add directory → create project → activate → send) without
// asserting brittle call orders.

const listenCallbacks = new Map<string, (e: { payload: NormalizedEvent }) => void>();
function fireTo(channel: string, event: NormalizedEvent): void {
  const cb = listenCallbacks.get(channel);
  if (cb === undefined) throw new Error(`no listener registered for ${channel}`);
  cb({ payload: event });
}
const listenMock = vi.fn(async (event: string, handler: unknown): Promise<() => void> => {
  listenCallbacks.set(event, handler as (e: { payload: NormalizedEvent }) => void);
  return () => {
    listenCallbacks.delete(event);
  };
});
const openDialogMock = vi.fn(async (_options: unknown): Promise<string | null> => null);

// --- stateful fake backend ---
type Backend = {
  persistable: boolean;
  dirs: Map<string, { available: boolean }>;
  projects: ProjectListing[];
  rosters: Map<string, AgentRecord[]>;
  conversations: Map<string, ProjectConversation>;
  activeProjectId: string | null;
  probeFailures: Set<string>;
  // Harnesses `get_harness_install_status` should report as not-installed
  // (keyed by HarnessKind). Drives the Supported CLIs install-status tests.
  notInstalled: Set<string>;
  // Harnesses whose `create_agent` should reject (keyed by HarnessKind).
  // Drives the partial-auto-create-failure test.
  createAgentFailFor: Set<string>;
  // When set, `get_harness_install_status` waits on this gate before
  // resolving — lets a test hold the probe "pending" to exercise the
  // auto-create race guard (await a fresh probe before reading installed()).
  installGate: Promise<void> | null;
  // When set, `create_agent` waits on this gate before resolving — lets a
  // test park the seeding loop after its first create (call is recorded on
  // invoke) to exercise the captured-project-id bail.
  createAgentGate: Promise<void> | null;
  // When set, `open_project` waits on this project's gate before resolving — lets a test
  // observe the immediate UI state while project activation is still pending.
  openProjectGates: Map<string, Promise<void>>;
  agentQueue: AgentRecord[];
  sendMessageId: string;
  loadConvoCalls: string[];
  failOpenFor: Set<string>;
  failConvoFor: Set<string>;
  failPickFor: Set<string>;
  // Git-view tracked repos returned by `list_tracked_repos` / `read_tracked_repo`.
  trackedRepos: RepoListing[];
  openEditorFailure: string | null;
  openEditorQueue: Promise<unknown>[];
  prompts: Prompt[];
};
let backend: Backend;
let agentSeq = 0;

function freshBackend(): Backend {
  return {
    persistable: true,
    dirs: new Map(),
    projects: [],
    rosters: new Map(),
    conversations: new Map(),
    activeProjectId: null,
    probeFailures: new Set(),
    notInstalled: new Set(),
    createAgentFailFor: new Set(),
    installGate: null,
    createAgentGate: null,
    openProjectGates: new Map(),
    agentQueue: [],
    sendMessageId: "77777777-7777-7000-8000-777777777777",
    loadConvoCalls: [],
    failOpenFor: new Set(),
    failConvoFor: new Set(),
    failPickFor: new Set(),
    trackedRepos: [],
    openEditorFailure: null,
    openEditorQueue: [],
    prompts: [],
  };
}

const REVIEW_PROMPT: Prompt = {
  provider: "local",
  name: "review",
  title: "Code Review",
  description: "Review code",
  arguments: [{ name: "focus", description: "What to focus on", required: true }],
  tags: [],
};

function summaryFor(id: string): ProjectSummary {
  const row = backend.projects.find((p) => p.id === id);
  if (row === undefined) throw new Error(`open of unknown project ${id}`);
  return { id: row.id, name: row.name, created_at: row.created_at };
}

const invokeMock = vi.fn(async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
  switch (cmd) {
    case "list_workspace_directories":
      return {
        directories: [...backend.dirs].map(([path, d]) => ({ path, available: d.available })),
        persistable: backend.persistable,
      };
    case "list_projects":
      // Only projects in registered directories surface (matches the real
      // backend: the workspace registry gates which projects are listed).
      return backend.projects.filter((p) => backend.dirs.has(p.directory));
    case "pick_directory": {
      // Read-only probe: discovers projects on disk for the chosen folder
      // WITHOUT registering it (no mutation of `backend.dirs`). Mirrors the real
      // `pick_directory_impl` the preview step calls before "Add" commits.
      const path = args?.path as string;
      if (backend.failPickFor.has(path)) throw new Error("incompatible .switchboard/ version");
      const found = backend.projects.filter((p) => p.directory === path);
      return {
        path,
        has_switchboard: found.length > 0,
        projects: found.map((p) => ({ id: p.id, name: p.name, created_at: p.created_at })),
      };
    }
    case "init_directory": {
      const path = args?.path as string;
      backend.dirs.set(path, { available: true });
      return { path, has_switchboard: true, projects: [] };
    }
    case "remove_directory": {
      const path = args?.path as string;
      backend.dirs.delete(path);
      backend.projects = backend.projects.filter((p) => p.directory !== path);
      return null;
    }
    case "create_project": {
      const id = `00000000-0000-7000-8000-0000000c${(backend.projects.length + 1)
        .toString()
        .padStart(4, "0")}`;
      const row: ProjectListing = {
        id,
        name: args?.name as string,
        created_at: "2026-05-20T00:00:00Z",
        directory: args?.directory as string,
        available: true,
        last_activity: "2026-05-20T00:00:00Z",
        archived: false,
      };
      backend.projects.push(row);
      backend.rosters.set(id, []);
      return summaryFor(id);
    }
    case "open_project": {
      const pid = args?.projectId as string;
      const gate = backend.openProjectGates.get(pid);
      if (gate !== undefined) await gate;
      if (backend.failOpenFor.has(pid)) throw new Error("project is locked by another process");
      return summaryFor(pid);
    }
    case "set_active_project":
      backend.activeProjectId = args?.projectId as string;
      return null;
    case "list_agents": {
      const pid = (args?.projectId as string) ?? backend.activeProjectId;
      return backend.rosters.get(pid ?? "") ?? [];
    }
    case "create_agent":
    case "attach_agent": {
      if (cmd === "create_agent" && backend.createAgentGate) await backend.createAgentGate;
      if (cmd === "create_agent" && backend.createAgentFailFor.has(args?.harness as string)) {
        throw new Error(`disk full creating ${args?.harness as string}`);
      }
      const agent =
        backend.agentQueue.shift() ??
        ({
          id: `00000000-0000-7000-8000-00000000a${(++agentSeq).toString().padStart(3, "0")}`,
          project_id: backend.activeProjectId ?? "",
          name: args?.name as string,
          harness: args?.harness as AgentRecord["harness"],
          session_locator: null,
          created_at: "2026-05-20T00:00:01Z",
        } satisfies AgentRecord);
      const pid = backend.activeProjectId ?? "";
      backend.rosters.set(pid, [...(backend.rosters.get(pid) ?? []), agent]);
      return agent;
    }
    case "load_transcript":
      return { turns: [], warnings: [] };
    case "load_project_conversation": {
      const pid = args?.projectId as string;
      backend.loadConvoCalls.push(pid);
      if (backend.failConvoFor.has(pid)) throw new Error("conversation load failed");
      return backend.conversations.get(pid) ?? { items: [], agents: [] };
    }
    case "send_message":
      return backend.sendMessageId;
    case "list_prompts":
      return backend.prompts;
    // The getting-started panel (no-project state) probes auth + install
    // status. Honor probeFailures so a "logged out" / "not installed" case
    // can be simulated; default to authed + installed.
    case "check_claude_auth":
    case "check_codex_auth":
    case "check_gemini_auth":
    case "check_antigravity_auth":
      if (backend.probeFailures.has(cmd)) throw new Error(`auth failed: ${cmd}`);
      return null;
    case "get_harness_install_status": {
      if (backend.installGate) await backend.installGate;
      const installed = !backend.notInstalled.has(args?.harness as string);
      return { installed, version: installed ? "1.0.0" : null };
    }
    case "get_preferences":
      return {
        editor_command: null,
        terminal_app: "Terminal",
        diff_style: "side_by_side",
        show_builtins: true,
      };
    case "add_tracked_repo":
    case "remove_tracked_repo":
      return null;
    case "list_tracked_repos":
      return backend.trackedRepos;
    case "read_tracked_repo": {
      const root = args?.path as string;
      return backend.trackedRepos.find((r) => r.repo.root === root) ?? backend.trackedRepos[0];
    }
    case "fetch_repo":
      return null;
    case "branch_commits":
      return [];
    case "changed_files":
      return [];
    case "commit_changed_files":
      return { found: true, body: null, files: [] };
    case "file_diff":
    case "commit_file_diff":
      return {
        path: "",
        binary: false,
        truncated: false,
        too_large: false,
        too_large_bytes: null,
        hunks: [],
      };
    case "open_in_editor":
      if (backend.openEditorQueue.length > 0) return await backend.openEditorQueue.shift();
      if (backend.openEditorFailure !== null) throw new Error(backend.openEditorFailure);
      return null;
    default:
      throw new Error(`unexpected invoke call: ${cmd}`);
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: unknown) => listenMock(event, handler),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (options: unknown) => openDialogMock(options),
}));

const DIR_A = "/tmp/sw-a";
const DIR_B = "/tmp/sw-b";

function listing(
  over: Partial<ProjectListing> & { id: string; directory: string },
): ProjectListing {
  return {
    name: "alpha",
    created_at: "2026-05-20T00:00:00Z",
    available: true,
    last_activity: "2026-05-20T00:00:00Z",
    archived: false,
    ...over,
  };
}

function agent(over: Partial<AgentRecord> & { id: string; project_id: string }): AgentRecord {
  return {
    name: "assistant",
    harness: "claude_code",
    session_locator: { uuid: "33333333-3333-7000-8000-333333333333" },
    created_at: "2026-05-20T00:00:01Z",
    ...over,
  };
}

/// Seed a directory with one project and (optionally) a roster + conversation,
/// so a test can mount straight into a populated workspace.
function seedProject(opts: {
  projectId: string;
  directory?: string;
  agents?: AgentRecord[];
  conversation?: ProjectConversation;
  available?: boolean;
  lastActivity?: string;
  name?: string;
}): void {
  const dir = opts.directory ?? DIR_A;
  backend.dirs.set(dir, { available: opts.available ?? true });
  backend.projects.push(
    listing({
      id: opts.projectId,
      directory: dir,
      name: opts.name ?? "alpha",
      available: opts.available ?? true,
      last_activity: opts.lastActivity ?? "2026-05-20T00:00:00Z",
    }),
  );
  backend.rosters.set(opts.projectId, opts.agents ?? []);
  if (opts.conversation !== undefined) backend.conversations.set(opts.projectId, opts.conversation);
}

function mountApp() {
  return render(App);
}

// Drives the new-project dialog flow (choose folder → name → submit). The
// folder is resolved from the mocked native dialog; the project is created in
// DIR_A unless `folder` overrides it.
async function createNewProjectViaDialog(
  name: string,
  folder: string = DIR_A,
  triggerTestId = "welcome-add-project",
): Promise<void> {
  openDialogMock.mockResolvedValueOnce(folder);
  await fireEvent.click(screen.getByTestId(triggerTestId));
  await waitFor(() => expect(screen.getByTestId("new-project-form")).toBeInTheDocument());
  await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
  const nameInput = screen.getByTestId("new-project-name") as HTMLInputElement;
  await fireEvent.input(nameInput, { target: { value: name } });
  await fireEvent.click(screen.getByTestId("new-project-submit"));
}

function projectRowByName(name: string): HTMLElement {
  const row = screen
    .getAllByTestId("project-row")
    .find((candidate) => within(candidate).queryByText(name) !== null);
  if (row === undefined) throw new Error(`missing project row ${name}`);
  return row;
}

async function expectComposeFocused(): Promise<void> {
  await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("compose-textarea")));
}

// create_agent calls (name + harness pairs) in invocation order. Projects just
// the two identity fields so seed-order assertions don't couple to the
// model/effort defaults each call also carries (covered by a dedicated test).
function createAgentCalls(): { name: string; harness: string }[] {
  return invokeMock.mock.calls
    .filter(([c]) => c === "create_agent")
    .map(([, a]) => {
      const args = a as { name: string; harness: string };
      return { name: args.name, harness: args.harness };
    });
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("App", () => {
  beforeEach(() => {
    backend = freshBackend();
    agentSeq = 0;
    listenCallbacks.clear();
    invokeMock.mockClear();
    listenMock.mockClear();
    openDialogMock.mockReset();
  });

  afterEach(async () => {
    // Unmount the rendered App *before* resetting the global stores. Auto-cleanup
    // runs last (LIFO: it registers at import, this hook registers later), so
    // without an explicit unmount here the store resets fire while the tree is
    // still live — its `$derived`/`$effect`s then churn against torn-down state
    // (empty repos, null install status), which destabilizes the reactive
    // runtime and intermittently hangs the next test's mount on slow CI.
    cleanup();
    const idx = await import("$lib/state/index.svelte");
    idx._testing.reset();
    const ws = await import("$lib/state/workspace.svelte");
    ws._testing.reset();
    const ha = await import("$lib/harnessAvailability.svelte");
    ha._testing.reset();
    const compose = await import("$lib/state/composeStore");
    compose._testing.reset();
    const gv = await import("$lib/state/gitView.svelte");
    gv._testing.reset();
    const cp = await import("$lib/state/commandPalette.svelte");
    cp._testing.reset();
    const tp = await import("$lib/state/transcriptPreview.svelte");
    tp._testing.reset();
    const panes = await import("$lib/state/transcriptPanes.svelte");
    panes._testing.reset();
    const recipients = await import("$lib/state/recipientSelection.svelte");
    recipients._testing.reset();
  });

  // --- harness availability banners (workspace empty → welcome) ---

  it("renders the welcome state (Add project) with no banners when probes succeed and no projects exist", async () => {
    await mountApp();
    expect(screen.getByText("Switchboard")).toBeInTheDocument();
    expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument();
    // With no projects the picker sidebar has nothing to show, so it (and its
    // re-open toggle) hide; the welcome screen carries the Add project affordance.
    expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument();
    expect(screen.queryByTestId("projects-sidebar-toggle")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.queryByTestId(/^banner-/)).not.toBeInTheDocument());
  });

  it("shows a missing Claude CLI in the welcome status list without a global banner", async () => {
    backend.notInstalled.add("claude_code");
    await mountApp();
    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Not installed"),
    );
    expect(screen.queryByTestId(/^banner-binary_missing-/)).not.toBeInTheDocument();
  });

  it("probes auth in the no-project getting-started surface, but renders no auth banner", async () => {
    // Auth status is proactive in exactly one place — the no-project
    // getting-started panel — and reactive everywhere else. So the panel
    // *does* probe auth here (that's its job), but the removed mid-work
    // surface must stay gone: no auth banner.
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("getting-started")).toBeInTheDocument());
    // The panel probes auth asynchronously (install → auth per harness); wait
    // for at least one auth probe to fire rather than checking synchronously.
    await waitFor(() => {
      const authCalls = invokeMock.mock.calls.filter(
        ([c]) =>
          c === "check_claude_auth" ||
          c === "check_codex_auth" ||
          c === "check_gemini_auth" ||
          c === "check_antigravity_auth",
      );
      expect(authCalls.length).toBeGreaterThan(0);
    });
    // The mid-work auth banner stays removed — that posture is the point.
    expect(screen.queryByTestId(/^banner-auth_missing-/)).not.toBeInTheDocument();
  });

  it("keeps missing CLI status out of global banners", async () => {
    backend.notInstalled.add("claude_code");
    backend.notInstalled.add("codex");
    await mountApp();
    await waitFor(() => {
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Not installed");
      expect(screen.getByTestId("harness-install-codex")).toHaveTextContent("Not installed");
    });
    expect(screen.queryByTestId(/^banner-binary_missing-/)).not.toBeInTheDocument();
  });

  // --- adding projects ---

  it("add existing: opens an explanatory dialog before picking a folder (no immediate OS picker)", async () => {
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("welcome-add-project"));
    await waitFor(() => expect(screen.getByTestId("project-dialog")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("project-dialog-mode-existing"));
    // The dialog explains what to select first; the OS folder picker has not
    // been opened yet (it opens on "Choose folder…").
    await waitFor(() => expect(screen.getByTestId("add-existing-form")).toBeInTheDocument());
    expect(openDialogMock).not.toHaveBeenCalled();
    // Add is disabled until a folder with projects has been previewed.
    expect(screen.getByTestId("add-existing-add")).toBeDisabled();
  });

  it("add existing: choosing a folder previews the projects but does NOT add them until Add is pressed", async () => {
    backend.projects.push(listing({ id: "p-x", directory: DIR_A, name: "existing-proj" }));
    backend.rosters.set("p-x", []);
    openDialogMock.mockResolvedValueOnce(DIR_A);
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("welcome-add-project"));
    await waitFor(() => expect(screen.getByTestId("project-dialog")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("project-dialog-mode-existing"));
    await waitFor(() => expect(screen.getByTestId("add-existing-form")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("add-existing-choose-folder"));

    // The preview names the project and enables Add — but the read-only probe
    // ran, not a commit: the directory is unregistered and nothing is in the
    // flat list yet.
    await waitFor(() => expect(screen.getByTestId("add-existing-found")).toBeInTheDocument());
    expect(screen.getByTestId("add-existing-found")).toHaveTextContent("existing-proj");
    expect(screen.getByTestId("add-existing-add")).toBeEnabled();
    expect(invokeMock.mock.calls.some(([c]) => c === "pick_directory")).toBe(true);
    expect(invokeMock.mock.calls.some(([c]) => c === "init_directory")).toBe(false);
    expect(screen.queryByTestId("project-row")).not.toBeInTheDocument();

    // Pressing Add commits: init_directory registers the folder, the dialog
    // closes, and the project surfaces in the flat list.
    await fireEvent.click(screen.getByTestId("add-existing-add"));
    await waitFor(() =>
      expect(
        invokeMock.mock.calls.some(([c, a]) => c === "init_directory" && a?.path === DIR_A),
      ).toBe(true),
    );
    await waitFor(() => expect(screen.queryByTestId("project-dialog")).not.toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toBeInTheDocument();
  });

  it("add existing: a folder with no projects shows a 'none found' warning, leaves Add disabled, and adds nothing", async () => {
    // DIR_A has no projects on disk.
    openDialogMock.mockResolvedValueOnce(DIR_A);
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("welcome-add-project"));
    await waitFor(() => expect(screen.getByTestId("project-dialog")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("project-dialog-mode-existing"));
    await waitFor(() => expect(screen.getByTestId("add-existing-form")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("add-existing-choose-folder"));

    await waitFor(() => expect(screen.getByTestId("add-existing-none")).toBeInTheDocument());
    expect(screen.queryByTestId("add-existing-found")).not.toBeInTheDocument();
    // Nothing to add → Add stays disabled and the empty folder is never
    // registered (no accidental `.switchboard/` init).
    expect(screen.getByTestId("add-existing-add")).toBeDisabled();
    expect(invokeMock.mock.calls.some(([c]) => c === "init_directory")).toBe(false);
  });

  it("add existing: a failed probe surfaces the error, disables Add, and registers nothing", async () => {
    // e.g. an incompatible `.switchboard/` version — the read-only probe rejects.
    backend.failPickFor.add(DIR_A);
    openDialogMock.mockResolvedValueOnce(DIR_A);
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("welcome-add-project"));
    await waitFor(() => expect(screen.getByTestId("project-dialog")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("project-dialog-mode-existing"));
    await waitFor(() => expect(screen.getByTestId("add-existing-form")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("add-existing-choose-folder"));

    await waitFor(() => expect(screen.getByTestId("add-existing-error")).toBeInTheDocument());
    expect(screen.queryByTestId("add-existing-found")).not.toBeInTheDocument();
    expect(screen.getByTestId("add-existing-add")).toBeDisabled();
    expect(invokeMock.mock.calls.some(([c]) => c === "init_directory")).toBe(false);
  });

  it("add existing: a probe failure on a re-pick clears the prior preview (Add can't strand on the old folder)", async () => {
    // First pick (DIR_A) previews a project; second pick (DIR_B) fails the probe.
    backend.projects.push(listing({ id: "p-x", directory: DIR_A, name: "existing-proj" }));
    backend.rosters.set("p-x", []);
    backend.failPickFor.add(DIR_B);
    openDialogMock.mockResolvedValueOnce(DIR_A).mockResolvedValueOnce(DIR_B);
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("welcome-add-project"));
    await waitFor(() => expect(screen.getByTestId("project-dialog")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("project-dialog-mode-existing"));
    await waitFor(() => expect(screen.getByTestId("add-existing-form")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("add-existing-choose-folder"));
    await waitFor(() => expect(screen.getByTestId("add-existing-found")).toBeInTheDocument());
    expect(screen.getByTestId("add-existing-add")).toBeEnabled();

    // Re-pick a folder whose probe fails → the stale preview is gone and Add is
    // disabled, so it can't silently commit the first folder.
    await fireEvent.click(screen.getByTestId("add-existing-choose-folder"));
    await waitFor(() => expect(screen.getByTestId("add-existing-error")).toBeInTheDocument());
    expect(screen.queryByTestId("add-existing-found")).not.toBeInTheDocument();
    expect(screen.getByTestId("add-existing-add")).toBeDisabled();
    expect(invokeMock.mock.calls.some(([c]) => c === "init_directory")).toBe(false);
  });

  it("new project: choosing a folder + name creates the project, activates it, and auto-seeds an agent per auto-seed harness", async () => {
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    await waitFor(() => expect(screen.getByText("brand-new")).toBeInTheDocument());
    const createProjectCalls = invokeMock.mock.calls.filter(([c]) => c === "create_project");
    expect(createProjectCalls).toHaveLength(1);
    expect(createProjectCalls[0]?.[1]).toEqual({ name: "brand-new", directory: DIR_A });

    // All four harnesses are installed, but Gemini is excluded from auto-seeding
    // (no longer on individual plans) → one agent each for Claude/Codex/
    // Antigravity, named after the model+effort it'll run (Antigravity's
    // harness-owned model falls back to the bare harness name), in HARNESSES
    // order.
    await waitFor(() => expect(createAgentCalls()).toHaveLength(3));
    expect(createAgentCalls()).toEqual([
      { name: "opus-high", harness: "claude_code" },
      { name: "gpt-5-5-medium", harness: "codex" },
      { name: "antigravity", harness: "antigravity" },
    ]);
    // Auto-seeded → the roster is populated, not the empty first-agent prompt.
    await waitFor(() => expect(screen.getAllByTestId("sidebar-agent")).toHaveLength(3));
    expect(screen.queryByTestId("confirm-create-agent")).not.toBeInTheDocument();

    // Each seeded agent is born with its harness's default model/effort
    // (Antigravity carries neither).
    const seededArgs = invokeMock.mock.calls
      .filter(([c]) => c === "create_agent")
      .map(([, a]) => a as { name: string; harness: string; model?: string; effort?: string });
    expect(seededArgs).toEqual([
      { name: "opus-high", harness: "claude_code", model: "opus", effort: "high" },
      { name: "gpt-5-5-medium", harness: "codex", model: "gpt-5.5", effort: "medium" },
      { name: "antigravity", harness: "antigravity", model: undefined, effort: undefined },
    ]);
  });

  it("new project: an installed but auto-seed-excluded harness (Gemini) is not seeded", async () => {
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    // Gemini is installed by default in the test backend, yet never seeded.
    await waitFor(() => expect(createAgentCalls()).toHaveLength(3));
    expect(createAgentCalls().some(({ harness }) => harness === "gemini")).toBe(false);
  });

  it("new project: seeds agents only for installed harnesses", async () => {
    backend.notInstalled.add("gemini");
    backend.notInstalled.add("antigravity");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    await waitFor(() => expect(createAgentCalls()).toHaveLength(2));
    expect(createAgentCalls()).toEqual([
      { name: "opus-high", harness: "claude_code" },
      { name: "gpt-5-5-medium", harness: "codex" },
    ]);
  });

  it("new project: with no harnesses installed, creates no agents and shows the first-agent prompt", async () => {
    for (const h of ALL_HARNESSES) backend.notInstalled.add(h);
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    // Lands in a usable (empty) project — the first-agent form, not an error.
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());
    expect(createAgentCalls()).toHaveLength(0);
  });

  it("auto-create awaits a fresh probe — agents seed correctly even when the install probe is still pending at create time", async () => {
    // Hold the install probe pending so the store reports nothing installed
    // until released. The await in auto-create must close this window; without
    // it, installed() would read [] and seed zero agents.
    let release: () => void = () => {};
    backend.installGate = new Promise<void>((r) => (release = r));
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");
    // Project is created/activated, but auto-create is parked on the gated probe.
    await waitFor(() => expect(screen.getByText("brand-new")).toBeInTheDocument());
    expect(createAgentCalls()).toHaveLength(0);

    release();
    await waitFor(() => expect(createAgentCalls()).toHaveLength(3));
  });

  it("new project: a failed agent create surfaces a dismissible banner and the rest still seed", async () => {
    backend.createAgentFailFor.add("codex");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    // The other two still created; the failure is surfaced, not silent.
    await waitFor(() => expect(createAgentCalls()).toHaveLength(3));
    const banner = await screen.findByTestId("banner-agent-create-failed-codex");
    expect(banner).toHaveTextContent("Couldn't create the Codex agent");
    expect(screen.getAllByTestId("sidebar-agent")).toHaveLength(2);

    // Dismissible.
    await fireEvent.click(screen.getByTestId("banner-agent-create-failed-codex-dismiss"));
    expect(screen.queryByTestId("banner-agent-create-failed-codex")).not.toBeInTheDocument();
  });

  it("activating an existing project never auto-creates agents", async () => {
    seedProject({
      projectId: "p-existing",
      directory: DIR_A,
      name: "existing",
      agents: [agent({ id: "ag-1", project_id: "p-existing", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("existing"));
    await waitFor(() => expect(screen.getByTestId("unified-transcript")).toBeInTheDocument());
    expect(createAgentCalls()).toHaveLength(0);
  });

  it("the New Project dialog is non-dismissible while agents are seeding", async () => {
    // Hold the install probe so seeding parks mid-flight and the dialog stays
    // busy — the modal must not be dismissable into a half-created project.
    let release: () => void = () => {};
    backend.installGate = new Promise<void>((r) => (release = r));
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    // Parked on the gated probe: dialog still up, and the ✕ is gone (the
    // escape/outside paths are suppressed by the same `dismissible={false}`).
    expect(screen.getByTestId("new-project-form")).toBeInTheDocument();
    expect(screen.queryByTestId("dialog-close")).not.toBeInTheDocument();

    release();
    await waitFor(() => expect(screen.queryByTestId("new-project-form")).not.toBeInTheDocument());
  });

  it("seeding bails if the active project changes mid-flight (no agents scattered into the wrong project)", async () => {
    // Defense-in-depth for the active-project coupling: park the seed loop on
    // its first create (which proves activation completed and the id was
    // captured), flip the active project, then release. The captured-id guard
    // must stop the loop — only the single in-flight create lands (the
    // documented TOCTOU sliver the dialog belt covers), not all four.
    let release: () => void = () => {};
    backend.createAgentGate = new Promise<void>((r) => (release = r));
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");
    // First create issued and parked → activation is done, id captured.
    await waitFor(() => expect(createAgentCalls()).toHaveLength(1));

    // Active project changes out from under the parked seed.
    const ws = await import("$lib/state/workspace.svelte");
    ws.selection.activeProjectId = "some-other-project";

    release();
    await waitFor(() => expect(screen.queryByTestId("new-project-form")).not.toBeInTheDocument());
    // Guard bailed after the in-flight create — three more were prevented.
    expect(createAgentCalls()).toHaveLength(1);
  });

  it("new project: failed activation does not auto-seed agents", async () => {
    backend.failOpenFor.add("00000000-0000-7000-8000-0000000c0001");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    await waitFor(() => expect(screen.getByTestId("activation-error")).toBeInTheDocument());
    expect(createAgentCalls()).toHaveLength(0);
  });

  it("new project: superseded activation does not auto-seed agents into another project", async () => {
    let releaseCreated!: () => void;
    backend.openProjectGates.set(
      "00000000-0000-7000-8000-0000000c0002",
      new Promise<void>((resolve) => {
        releaseCreated = resolve;
      }),
    );
    seedProject({
      projectId: "p-other",
      directory: DIR_B,
      name: "other",
      agents: [agent({ id: "ag-other", project_id: "p-other", name: "other-agent" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new", DIR_A, "add-project");
    await waitFor(() =>
      expect(projectRowByName("brand-new")).toHaveAttribute("data-active", "true"),
    );

    await fireEvent.click(within(projectRowByName("other")).getByText("other"));
    await waitFor(() => expect(projectRowByName("other")).toHaveAttribute("data-active", "true"));
    await waitFor(() => expect(backend.activeProjectId).toBe("p-other"));

    releaseCreated();
    await waitFor(() => expect(screen.queryByTestId("new-project-form")).not.toBeInTheDocument());

    expect(createAgentCalls()).toHaveLength(0);
    expect(backend.activeProjectId).toBe("p-other");
    expect(backend.rosters.get("p-other")).toHaveLength(1);
  });

  it("switching projects clears the auto-create failure banner", async () => {
    backend.createAgentFailFor.add("codex");
    seedProject({ projectId: "p-other", directory: DIR_B, name: "other", agents: [] });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");
    await screen.findByTestId("banner-agent-create-failed-codex");

    await fireEvent.click(screen.getByText("other"));
    await waitFor(() =>
      expect(screen.queryByTestId("banner-agent-create-failed-codex")).not.toBeInTheDocument(),
    );
  });

  it("surfaces the not-persistable state distinctly from a fresh install", async () => {
    backend.persistable = false;
    backend.dirs.set(DIR_A, { available: true });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("not-persistable-banner")).toBeInTheDocument());
    // The distinguishing signal is the banner above the welcome surface — not a
    // bare welcome with no warning. (With no projects the picker sidebar hides.)
    expect(screen.getByText("Switchboard")).toBeInTheDocument();
  });

  // --- workspace: project list + lazy activation ---

  it("eager-loads the flat project list across directories without hydrating transcripts", async () => {
    seedProject({ projectId: "p-a", directory: DIR_A, name: "alpha" });
    seedProject({ projectId: "p-b", directory: DIR_B, name: "beta" });
    await mountApp();

    await waitFor(() => {
      expect(screen.getAllByTestId("project-row")).toHaveLength(2);
    });
    // Eager registry only — no project's conversation is hydrated at startup.
    expect(backend.loadConvoCalls).toEqual([]);
  });

  it("first activation loads the roster + hydrates once; re-activation does not re-hydrate", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({ projectId: "p-b", directory: DIR_B, name: "beta" });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row").length).toBe(2));

    const rowA = screen.getByText("alpha");
    await fireEvent.click(rowA);
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    // Hydration is fire-and-forget just after the roster renders.
    await waitFor(() => expect(backend.loadConvoCalls).toEqual(["p-a"]));

    // Switch to B then back to A — A must not re-hydrate.
    await fireEvent.click(screen.getByText("beta"));
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(backend.loadConvoCalls.filter((p) => p === "p-a")).toHaveLength(1);
  });

  it("focuses the compose box after switching projects", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-2", project_id: "p-b", name: "helper" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row")).toHaveLength(2));

    await fireEvent.click(within(projectRowByName("alpha")).getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    await expectComposeFocused();

    await fireEvent.click(within(projectRowByName("beta")).getByText("beta"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    await expectComposeFocused();
  });

  it("selects a project row and clears the center pane while activation is pending", async () => {
    let releaseOpen!: () => void;
    backend.openProjectGates.set(
      "p-a",
      new Promise<void>((resolve) => {
        releaseOpen = resolve;
      }),
    );
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    const row = screen.getByTestId("project-row");

    await fireEvent.click(within(row).getByText("alpha"));

    expect(row).toHaveAttribute("data-active", "true");
    await waitFor(() => expect(screen.getByTestId("project-loading")).toBeInTheDocument());
    expect(screen.queryByTestId("compose-textarea")).not.toBeInTheDocument();
    // The loading layout reserves the agents sidebar's width and the compose
    // bar's height with empty shells, so the centered spinner doesn't jump
    // (sideways or vertically) once the real layout mounts for the next
    // loading state.
    expect(screen.getByTestId("project-loading-sidebar-shell")).toBeInTheDocument();
    expect(screen.getByTestId("project-loading-sidebar-shell")).toHaveClass("w-60");
    expect(screen.getByTestId("project-loading-compose-shell")).toBeInTheDocument();

    releaseOpen();
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
  });

  it("dehighlights the previous project row as soon as a new activation starts", async () => {
    let releaseBeta!: () => void;
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-2", project_id: "p-b", name: "helper" })],
    });
    backend.openProjectGates.set(
      "p-b",
      new Promise<void>((resolve) => {
        releaseBeta = resolve;
      }),
    );
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row")).toHaveLength(2));

    const alphaRow = projectRowByName("alpha");
    const betaRow = projectRowByName("beta");
    await fireEvent.click(within(alphaRow).getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(alphaRow).toHaveAttribute("data-active", "true");

    await fireEvent.click(within(betaRow).getByText("beta"));

    expect(betaRow).toHaveAttribute("data-active", "true");
    expect(alphaRow).toHaveAttribute("data-active", "false");
    expect(screen.getByTestId("project-loading")).toBeInTheDocument();
    expect(screen.queryByTestId("compose-textarea")).not.toBeInTheDocument();

    releaseBeta();
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
  });

  it("ignores stale activation completion after switching to another project", async () => {
    let releaseAlpha!: () => void;
    backend.openProjectGates.set(
      "p-a",
      new Promise<void>((resolve) => {
        releaseAlpha = resolve;
      }),
    );
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-2", project_id: "p-b", name: "helper" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row")).toHaveLength(2));

    const alphaRow = screen
      .getAllByTestId("project-row")
      .find((row) => within(row).queryByText("alpha") !== null);
    const betaRow = screen
      .getAllByTestId("project-row")
      .find((row) => within(row).queryByText("beta") !== null);
    if (alphaRow === undefined || betaRow === undefined) throw new Error("missing project rows");

    await fireEvent.click(within(alphaRow).getByText("alpha"));
    await waitFor(() => expect(alphaRow).toHaveAttribute("data-active", "true"));
    await fireEvent.click(within(betaRow).getByText("beta"));
    await waitFor(() => expect(betaRow).toHaveAttribute("data-active", "true"));
    await waitFor(() => expect(backend.activeProjectId).toBe("p-b"));

    releaseAlpha();
    await new Promise((resolve) => setTimeout(resolve, 10));

    expect(backend.activeProjectId).toBe("p-b");
    expect(betaRow).toHaveAttribute("data-active", "true");
  });

  it("Cmd+G selects the oldest unread completed project", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-a", project_id: "p-a", name: "alpha-agent" })],
      lastActivity: "2026-05-20T09:45:00Z",
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-b", project_id: "p-b", name: "beta-agent" })],
      lastActivity: "2026-05-20T09:46:00Z",
    });
    seedProject({
      projectId: "p-c",
      directory: "/tmp/sw-c",
      name: "charlie",
      agents: [agent({ id: "ag-c", project_id: "p-c", name: "charlie-agent" })],
      lastActivity: "2026-05-20T09:47:00Z",
    });
    seedProject({
      projectId: "p-d",
      directory: "/tmp/sw-d",
      name: "delta",
      agents: [agent({ id: "ag-d", project_id: "p-d", name: "delta-agent" })],
      lastActivity: "2026-05-20T09:48:00Z",
    });
    seedProject({
      projectId: "p-e",
      directory: "/tmp/sw-e",
      name: "echo",
      agents: [agent({ id: "ag-e", project_id: "p-e", name: "echo-agent" })],
      lastActivity: "2026-05-20T09:49:00Z",
    });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row")).toHaveLength(5));
    await fireEvent.click(screen.getByText("echo"));
    await waitFor(() => expect(backend.activeProjectId).toBe("p-e"));

    const ws = await import("$lib/state/workspace.svelte");
    ws.backgroundCompletedProjectIds["p-a"] = true;
    ws.backgroundCompletedProjectIds["p-b"] = true;
    ws.backgroundCompletedProjectIds["p-c"] = true;
    await waitFor(() => expect(projectRowByName("alpha")).toHaveTextContent("alpha"));

    await fireEvent.keyDown(window, { key: "g", metaKey: true });

    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));
    expect(projectRowByName("alpha")).toHaveAttribute("data-active", "true");
    expect(screen.queryAllByTestId("project-completed")).toHaveLength(2);
  });

  it("Cmd+G does nothing when no project has an unread completed response", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-a", project_id: "p-a", name: "alpha-agent" })],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-b", project_id: "p-b", name: "beta-agent" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row")).toHaveLength(2));
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));

    await fireEvent.keyDown(window, { key: "g", metaKey: true });

    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(backend.activeProjectId).toBe("p-a");
    expect(projectRowByName("alpha")).toHaveAttribute("data-active", "true");
  });

  it("Cmd+G exits Git view before jumping to the unread project", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-a", project_id: "p-a", name: "alpha-agent" })],
      lastActivity: "2026-05-20T09:45:00Z",
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-b", project_id: "p-b", name: "beta-agent" })],
      lastActivity: "2026-05-20T09:46:00Z",
    });
    backend.trackedRepos = [
      {
        repo: {
          root: DIR_B,
          name: "beta-repo",
          default_branch: "main",
          available: true,
          is_bare: false,
          local_branches: [],
          remote_branches: [],
          detached_worktrees: [],
        },
        linked_projects: {},
      } satisfies RepoListing,
    ];
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row")).toHaveLength(2));
    await fireEvent.click(screen.getByText("beta"));
    await waitFor(() => expect(backend.activeProjectId).toBe("p-b"));

    await fireEvent.click(screen.getByTestId("view-toggle-git"));
    await waitFor(() => expect(screen.getByTestId("git-view")).toBeInTheDocument());

    const ws = await import("$lib/state/workspace.svelte");
    ws.backgroundCompletedProjectIds["p-a"] = true;

    await fireEvent.keyDown(window, { key: "g", metaKey: true });

    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));
    expect(screen.queryByTestId("git-view")).not.toBeInTheDocument();
    expect(projectRowByName("alpha")).toHaveAttribute("data-active", "true");
  });

  it("switching projects is display-only: a backgrounded agent's listener still updates state", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-2", project_id: "p-b", name: "helper" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row").length).toBe(2));

    // Activate A (registers ag-1's listener), then switch to B.
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("beta"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    // A background turn for ag-1 (in directory A) arrives AFTER the switch.
    // Its listener must still be alive and update state — proving switching
    // didn't unregister it.
    const turnId = "88888888-8888-7000-8000-888888888888";
    fireTo(`agent:ag-1`, {
      type: "turn_start",
      turn_id: turnId,
      message_id: backend.sendMessageId,
      send_id: backend.sendMessageId,
      started_at: "2026-05-20T01:00:00Z",
    });
    fireTo(`agent:ag-1`, { type: "content_chunk", turn_id: turnId, kind: "text", text: "bg work" });

    // Switch back to A; the background turn's content is present.
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() =>
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("bg work"),
    );
  });

  // --- first agent + add agent ---

  it("a project with no agents shows the create-agent form; creating one renders the transcript", async () => {
    seedProject({ projectId: "p-a", directory: DIR_A, name: "alpha", agents: [] });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("confirm-create-agent")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    await waitFor(() => {
      expect(screen.getByTestId("unified-transcript")).toBeInTheDocument();
      expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
      expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    });
    const createCalls = invokeMock.mock.calls.filter(([c]) => c === "create_agent");
    expect(createCalls).toHaveLength(1);
    // The form preselects and submits Claude's defaults.
    expect(createCalls[0]?.[1]).toEqual({
      name: "opus-high",
      harness: "claude_code",
      model: "opus",
      effort: "high",
    });
  });

  it("add agent via the sidebar modal registers a listener and appends to the roster", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.agentQueue.push(
      agent({
        id: "ag-2",
        project_id: "p-a",
        name: "second",
        harness: "codex",
        session_locator: null,
      }),
    );
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());

    const listenBefore = listenMock.mock.calls.length;
    await fireEvent.click(screen.getByTestId("sidebar-add-agent"));
    await waitFor(() => expect(screen.getByTestId("dialog-content")).toBeInTheDocument());
    const modal = screen.getByTestId("dialog-content");
    const nameInput = within(modal).getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "second" } });
    await fireEvent.click(within(modal).getByTestId("harness-codex"));
    await fireEvent.click(within(modal).getByTestId("confirm-create-agent"));

    await waitFor(() => {
      const names = screen.getAllByTestId("agent-name");
      expect(names.some((n) => n.textContent === "second")).toBe(true);
    });
    expect(listenMock.mock.calls.length).toBe(listenBefore + 1);
    expect(listenMock.mock.calls.at(-1)?.[0]).toBe("agent:ag-2");
  });

  // --- post-restart merged conversation ---

  it("renders the merged post-restart conversation: grouped user message, agent content, and failed/cancelled markers", async () => {
    const convo: ProjectConversation = {
      items: [
        {
          kind: "user_message",
          id: "s-1",
          send_id: "s-1",
          agent_ids: ["ag-1"],
          text: "do the thing",
          at: "2026-05-20T00:00:00Z",
        },
        {
          kind: "agent_turn",
          turn_id: "t-done",
          agent_id: "ag-1",
          started_at: "2026-05-20T00:00:00Z",
          ended_at: "2026-05-20T00:00:02Z",
          status: "complete",
          items: [{ item_kind: "text", kind: "text", text: "did the thing" }],
          usage: null,
        },
        // A failed send and a cancelled send, each at its own send's instant.
        {
          kind: "user_message",
          id: "s-2",
          send_id: "s-2",
          agent_ids: ["ag-1"],
          text: "second ask",
          at: "2026-05-20T00:01:00Z",
        },
        {
          kind: "outcome",
          turn_id: "t-fail",
          send_id: "s-2",
          agent_id: "ag-1",
          status: "failed",
          reason: "boom",
          at: "2026-05-20T00:01:00Z",
        },
        {
          kind: "user_message",
          id: "s-3",
          send_id: "s-3",
          agent_ids: ["ag-1"],
          text: "third ask",
          at: "2026-05-20T00:02:00Z",
        },
        {
          kind: "outcome",
          turn_id: "t-cancel",
          send_id: "s-3",
          agent_id: "ag-1",
          status: "cancelled",
          reason: "user",
          at: "2026-05-20T00:02:00Z",
        },
      ],
      agents: [
        { agent_id: "ag-1", meta: null, last_rate_limit: null, warnings: [], load_error: null },
      ],
    };
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
      conversation: convo,
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));

    await waitFor(() => {
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("did the thing");
    });
    const transcript = screen.getByTestId("unified-transcript");
    expect(transcript).toHaveTextContent("do the thing");
    // The failed and cancelled markers render (from the journal overlay).
    expect(within(transcript).getByTestId("outcome-failed")).toBeInTheDocument();
    expect(within(transcript).getByTestId("outcome-cancelled")).toBeInTheDocument();

    // The user message renders once per send (grouped), not duplicated.
    const userRows = within(transcript)
      .getAllByTestId("turn")
      .filter((el) => el.getAttribute("data-role") === "user");
    expect(userRows).toHaveLength(3);

    // A marker never sorts above the prompt that caused it: within the
    // rendered order, each user message precedes its same-instant marker.
    const order = transcript.textContent ?? "";
    expect(order.indexOf("second ask")).toBeLessThan(order.indexOf("boom"));
    expect(order.indexOf("third ask")).toBeLessThan(order.indexOf("cancelled"));
  });

  // --- E2E send ---

  it("E2E send: send → turn_start → content_chunk → turn_end → agent_idle renders and unblocks Send", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    await waitFor(() => {
      const sendCalls = invokeMock.mock.calls.filter(([c]) => c === "send_message");
      expect(sendCalls).toHaveLength(1);
      expect(sendCalls[0]?.[1]).toMatchObject({ agentId: "ag-1", prompt: "hi" });
      // A frontend-minted send_id rides along on every send.
      expect(typeof (sendCalls[0]?.[1] as { sendId?: unknown }).sendId).toBe("string");
    });

    const turnId = "88888888-8888-7000-8000-888888888888";
    const channel = "agent:ag-1";
    fireTo(channel, {
      type: "turn_start",
      turn_id: turnId,
      message_id: backend.sendMessageId,
      send_id: backend.sendMessageId,
      started_at: "2026-05-20T00:00:00Z",
    });
    fireTo(channel, { type: "content_chunk", turn_id: turnId, kind: "text", text: "hello back" });
    fireTo(channel, {
      type: "turn_end",
      turn_id: turnId,
      outcome: { status: "completed" },
      ended_at: "2026-05-20T00:00:01Z",
      usage: null,
    });
    fireTo(channel, { type: "agent_idle", agent_id: "ag-1" });

    await waitFor(() =>
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("hello back"),
    );
    await fireEvent.input(textarea, { target: { value: "again" } });
    await waitFor(() => expect(screen.getByTestId("compose-send")).not.toBeDisabled());
  });

  it("E2E reactive auth: AuthFailure turn renders the authored message in the transcript (no banner)", async () => {
    // Reactive-auth surface: with no proactive auth banner or picker gate,
    // a logged-out harness must still be discoverable — by sending. The
    // adapter authors the AuthFailure message, and the transcript renders
    // any failed turn's error text verbatim. This test exercises the full
    // path (compose → send → turn_start → turn_end(AuthFailure)) and
    // asserts the user sees the authored copy where they actually look
    // (the transcript), not a banner.
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant", harness: "codex" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const textarea = screen.getByTestId("compose-textarea") as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: "hi" } });
    await fireEvent.click(screen.getByTestId("compose-send"));

    const turnId = "99999999-9999-7000-8000-999999999999";
    const channel = "agent:ag-1";
    fireTo(channel, {
      type: "turn_start",
      turn_id: turnId,
      message_id: backend.sendMessageId,
      send_id: backend.sendMessageId,
      started_at: "2026-05-20T00:00:00Z",
    });
    fireTo(channel, {
      type: "turn_end",
      turn_id: turnId,
      outcome: {
        status: "failed",
        kind: "auth_failure",
        message: "Codex authentication required — run `codex login`",
      },
      ended_at: "2026-05-20T00:00:01Z",
      usage: null,
    });
    fireTo(channel, { type: "agent_idle", agent_id: "ag-1" });

    // The authored message lands in the transcript — same path any failed
    // turn takes (the absence of a per-kind render is intentional).
    await waitFor(() =>
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent(
        "Codex authentication required — run `codex login`",
      ),
    );
    // And the auth-banner posture is preserved: auth failures render in the transcript only.
    expect(screen.queryByTestId(/^banner-auth_missing-/)).not.toBeInTheDocument();
  });

  // --- directory removal lifecycle (store-level: the `removeDirectory` +
  // teardown primitive; there is no directory-removal UI yet) ---

  it("removeDirectory drops the directory's projects and tears down its agents' listeners", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({ projectId: "p-b", directory: DIR_B, name: "beta" });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row").length).toBe(2));

    // Activate p-a so ag-1's listener is registered, then remove its directory.
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(listenCallbacks.has("agent:ag-1")).toBe(true);

    const ws = await import("$lib/state/workspace.svelte");
    await ws.removeDirectory(DIR_A);

    await waitFor(() => {
      const names = screen.getAllByTestId("project-row").map((r) => r.textContent);
      expect(names.some((n) => n?.includes("beta"))).toBe(true);
      expect(names.some((n) => n?.includes("alpha"))).toBe(false);
    });
    // Teardown: the removed agent's listener is gone (no leak), and the backend
    // drain command fired.
    expect(listenCallbacks.has("agent:ag-1")).toBe(false);
    expect(
      invokeMock.mock.calls.some(([c, a]) => c === "remove_directory" && a?.path === DIR_A),
    ).toBe(true);
  });

  it("remove-then-re-add reuses no stale state: re-activation re-opens the project", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const ws = await import("$lib/state/workspace.svelte");
    await ws.removeDirectory(DIR_A);
    await waitFor(() => expect(screen.queryByTestId("project-row")).not.toBeInTheDocument());

    // Re-add the same directory + project id (ids persist on disk). The stale
    // memoized load must NOT be reused: re-activation re-opens the project.
    const openCallsBefore = invokeMock.mock.calls.filter(([c]) => c === "open_project").length;
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await ws.loadWorkspace();

    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    const openCallsAfter = invokeMock.mock.calls.filter(([c]) => c === "open_project").length;
    expect(openCallsAfter).toBeGreaterThan(openCallsBefore);
  });

  it("a failed activation shows a center error + retry, not an endless loading state", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.failOpenFor.add("p-a");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("activation-error")).toBeInTheDocument());
    expect(screen.queryByTestId("project-loading")).not.toBeInTheDocument();

    // Recover and retry — the project opens.
    backend.failOpenFor.delete("p-a");
    await fireEvent.click(screen.getByTestId("activation-retry"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(screen.queryByTestId("activation-error")).not.toBeInTheDocument();
  });

  it("an unreadable workspace with no recovered directories shows the not-persistable banner, not welcome", async () => {
    backend.persistable = false; // empty dirs + not persistable
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("not-persistable-banner")).toBeInTheDocument());
    expect(screen.queryByText("Open working directory")).not.toBeInTheDocument();
  });

  it("a whole-project conversation load failure shows the load-failed banner; live sends still work", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.failConvoFor.add("p-a");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    await waitFor(() => expect(screen.getByTestId("transcript-load-failed")).toBeInTheDocument());
  });

  it("a per-agent transcript load error surfaces in the sidebar without blanking the project", async () => {
    const convo: ProjectConversation = {
      items: [
        {
          kind: "agent_turn",
          turn_id: "t-ok",
          agent_id: "ag-ok",
          started_at: "2026-05-20T00:00:00Z",
          ended_at: "2026-05-20T00:00:01Z",
          status: "complete",
          items: [{ item_kind: "text", kind: "text", text: "healthy history" }],
          usage: null,
        },
      ],
      agents: [
        { agent_id: "ag-ok", meta: null, last_rate_limit: null, warnings: [], load_error: null },
        {
          agent_id: "ag-bad",
          meta: null,
          last_rate_limit: null,
          warnings: [],
          load_error: "corrupt sidecar",
        },
      ],
    };
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-ok", project_id: "p-a", name: "healthy" }),
        agent({
          id: "ag-bad",
          project_id: "p-a",
          name: "broken",
          harness: "codex",
          session_locator: null,
        }),
      ],
      conversation: convo,
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));

    await waitFor(() =>
      expect(screen.getByTestId("unified-transcript")).toHaveTextContent("healthy history"),
    );
    await waitFor(() =>
      expect(screen.getByTestId("agent-hydration-error")).toHaveTextContent("corrupt sidecar"),
    );
    // The same per-agent failure also surfaces as pinned transcript-region
    // chrome (where the user is looking), naming the agent — without blanking
    // the healthy agent's history.
    expect(screen.getByTestId("agent-hydration-failed")).toHaveTextContent(
      "Couldn't load broken's history",
    );
  });

  // --- sidebar collapse / expand ---

  it("projects sidebar: toggle collapses and re-opens; the control moves between sidebar and title bar", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    // The toggle is inside the sidebar when open.
    const sidebar = screen.getByTestId("projects-sidebar");
    expect(within(sidebar).getByTestId("projects-sidebar-toggle")).toBeInTheDocument();

    // Collapse — same testid, but now lives in the center title bar.
    await fireEvent.click(within(sidebar).getByTestId("projects-sidebar-toggle"));
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());
    // Re-open control is now in the DOM (in the title bar).
    expect(screen.getByTestId("projects-sidebar-toggle")).toBeInTheDocument();

    // Re-open.
    await fireEvent.click(screen.getByTestId("projects-sidebar-toggle"));
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
  });

  it("agents sidebar: toggle hides and re-shows the sidebar while the title bar persists", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());
    expect(screen.getByTestId("agents-sidebar-toggle")).toBeInTheDocument();

    // Hide.
    await fireEvent.click(screen.getByTestId("agents-sidebar-toggle"));
    await waitFor(() => expect(screen.queryByTestId("sidebar")).not.toBeInTheDocument());
    // Title bar (breadcrumb) and compose area remain usable.
    expect(screen.getByTestId("breadcrumb")).toBeInTheDocument();
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();

    // Re-show.
    await fireEvent.click(screen.getByTestId("agents-sidebar-toggle"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());
  });

  // --- compact transcript header control ---

  it("shows the compact transcript toggle for an open project with agents and hides it in Git view", async () => {
    seedProject({
      projectId: "p-a",
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() =>
      expect(screen.getByTestId("transcript-compact-toggle")).toBeInTheDocument(),
    );
    const paneAdd = screen.getByTestId("app-pane-add");
    const compact = screen.getByTestId("transcript-compact-toggle");
    const projectsToggle = screen.getByTestId("view-toggle-projects");
    const tabStrip = screen.getByTestId("app-pane-tab-strip");
    expect(tabStrip).not.toContainElement(paneAdd);
    expect(tabStrip).not.toContainElement(compact);
    expect(paneAdd.compareDocumentPosition(compact) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(
      compact.compareDocumentPosition(projectsToggle) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).not.toBe(0);

    // The Git view is a full takeover — the compact control is irrelevant there.
    await fireEvent.click(screen.getByTestId("view-toggle-git"));
    await waitFor(() =>
      expect(screen.queryByTestId("transcript-compact-toggle")).not.toBeInTheDocument(),
    );
  });

  it("does not show the compact toggle for a project with no agents", async () => {
    seedProject({ projectId: "p-a", name: "alpha", agents: [] });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("breadcrumb")).toHaveTextContent("alpha"));
    expect(screen.queryByTestId("transcript-compact-toggle")).not.toBeInTheDocument();
  });

  it("inverts compact mode from the header (default compact → expand)", async () => {
    seedProject({
      projectId: "p-a",
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    const btn = await screen.findByTestId("transcript-compact-toggle");
    // Compact is on by default, so the action offered is "Expand transcript".
    expect(btn).toHaveAttribute("aria-label", "Expand transcript");

    await fireEvent.click(btn);
    await waitFor(() =>
      expect(screen.getByTestId("transcript-compact-toggle")).toHaveAttribute(
        "aria-label",
        "Compact transcript",
      ),
    );
  });

  it("covers the transcript with a busy overlay while expand/collapse re-renders", async () => {
    seedProject({
      projectId: "p-a",
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    const btn = await screen.findByTestId("transcript-compact-toggle");
    // The overlay lives beside the transcript pane, which mounts only after
    // activation resolves — toggling before that would have nothing to cover.
    await waitFor(() => expect(screen.getByTestId("unified-transcript")).toBeInTheDocument());

    // Dispatch synchronously and flush before yielding to the event loop —
    // the overlay's whole lifetime fits inside one await chain in jsdom, so
    // the transient "up" phase is only observable via flushSync.
    const clicked = fireEvent.click(btn);
    flushSync();
    // The overlay is up immediately — the mutation itself is deferred behind
    // rAFs so the blur+spinner can paint before the heavy re-render blocks.
    expect(screen.getByTestId("transcript-busy-overlay")).toBeInTheDocument();
    expect(
      screen.getByTestId("transcript-busy-overlay").querySelector(".animate-spin"),
    ).not.toBeNull();
    await clicked;

    // It comes down once the re-render has flushed, with the mode flipped.
    await waitFor(() => expect(screen.queryByTestId("transcript-busy-overlay")).toBeNull());
    expect(screen.getByTestId("transcript-compact-toggle")).toHaveAttribute(
      "aria-label",
      "Compact transcript",
    );
  });

  it("reads as a reset when manual overrides exist and clears them on click", async () => {
    seedProject({
      projectId: "p-a",
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("project-row")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await screen.findByTestId("transcript-compact-toggle");

    const tp = await import("$lib/state/transcriptPreview.svelte");
    tp.toggleKey("p-a", "agent:x", false); // a manual per-unit override
    await waitFor(() =>
      expect(screen.getByTestId("transcript-compact-toggle")).toHaveAttribute(
        "aria-label",
        "Reset compact transcript",
      ),
    );

    await fireEvent.click(screen.getByTestId("transcript-compact-toggle"));
    // Reset → overrides cleared and compact stays enabled, so the action returns
    // to "Expand transcript".
    await waitFor(() =>
      expect(screen.getByTestId("transcript-compact-toggle")).toHaveAttribute(
        "aria-label",
        "Expand transcript",
      ),
    );
    expect(tp.hasOverrides("p-a")).toBe(false);
  });

  it("keeps compact state per project (header reflects the active project)", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-2", project_id: "p-b", name: "helper" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getAllByTestId("project-row").length).toBe(2));

    // Turn compact off for alpha.
    await fireEvent.click(screen.getByText("alpha"));
    await fireEvent.click(await screen.findByTestId("transcript-compact-toggle"));
    await waitFor(() =>
      expect(screen.getByTestId("transcript-compact-toggle")).toHaveAttribute(
        "aria-label",
        "Compact transcript",
      ),
    );

    // beta is untouched → still default-on ("Expand transcript").
    await fireEvent.click(screen.getByText("beta"));
    await waitFor(() =>
      expect(screen.getByTestId("transcript-compact-toggle")).toHaveAttribute(
        "aria-label",
        "Expand transcript",
      ),
    );

    // Back to alpha → its own off state is remembered.
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() =>
      expect(screen.getByTestId("transcript-compact-toggle")).toHaveAttribute(
        "aria-label",
        "Compact transcript",
      ),
    );
  });

  it("settings button toggles settings without changing the selected project", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");

    await fireEvent.click(
      within(screen.getByTestId("projects-sidebar")).getByTestId("settings-button"),
    );
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");
    expect(screen.getByText("Theme")).toBeInTheDocument();
    expect(screen.getByText("Shortcuts")).toBeInTheDocument();

    await fireEvent.click(
      within(screen.getByTestId("projects-sidebar")).getByTestId("settings-button"),
    );
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();

    await fireEvent.click(
      within(screen.getByTestId("projects-sidebar")).getByTestId("settings-button"),
    );
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("settings-close"));
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
  });

  it("settings button label reflects open/closed state for assistive technology", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const settingsBtn = within(screen.getByTestId("projects-sidebar")).getByTestId(
      "settings-button",
    );
    expect(settingsBtn).toHaveAttribute("aria-label", "Open settings");

    await fireEvent.click(settingsBtn);
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    expect(settingsBtn).toHaveAttribute("aria-label", "Close settings");

    await fireEvent.click(settingsBtn);
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
    expect(settingsBtn).toHaveAttribute("aria-label", "Open settings");
  });

  it("settings is reachable from the title bar when the projects sidebar is collapsed", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    // Collapse the sidebar — the settings button moves to the center title bar.
    const sidebar = screen.getByTestId("projects-sidebar");
    await fireEvent.click(within(sidebar).getByTestId("projects-sidebar-toggle"));
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());

    // Settings button is now in the title bar with the correct closed-state label.
    const titleBarBtn = screen.getByTestId("settings-button");
    expect(titleBarBtn).toHaveAttribute("aria-label", "Open settings");

    // Opening and closing works from the title bar.
    await fireEvent.click(titleBarBtn);
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    expect(titleBarBtn).toHaveAttribute("aria-label", "Close settings");

    await fireEvent.click(titleBarBtn);
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
  });

  it("global shortcuts toggle sidebars and open settings", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    await fireEvent.keyDown(window, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());
    await fireEvent.keyDown(window, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");

    await fireEvent.keyDown(window, { key: "B", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.queryByTestId("sidebar")).not.toBeInTheDocument());
    await fireEvent.keyDown(window, { key: "B", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());

    await fireEvent.keyDown(window, { key: ",", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
    expect(screen.getByTestId("project-row")).toHaveAttribute("data-active", "true");

    await fireEvent.keyDown(window, { key: ",", metaKey: true });
    await waitFor(() => expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument());
    expect(screen.getByTestId("compose-textarea")).toBeInTheDocument();
  });

  it("adds an empty pane from the app header", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("app-pane-add")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("app-pane-add"));

    const layout = panes.layoutFor("p-a", ["ag-1", "ag-2"]);
    expect(layout.panes).toHaveLength(2);
    expect(layout.panes[1]!.members).toEqual([]);
  });

  it("narrows pane one to the compose-selected agents on the first split, leaving the rest unassigned", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
        agent({ id: "ag-3", project_id: "p-a", name: "carol" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("app-pane-add")).toBeInTheDocument());

    selection.setRecipients("p-a", ["ag-1", "ag-3"]);
    await fireEvent.click(screen.getByTestId("app-pane-add"));

    const roster = ["ag-1", "ag-2", "ag-3"];
    const layout = panes.layoutFor("p-a", roster);
    expect(layout.panes).toHaveLength(2);
    expect(layout.panes[0]!.members).toEqual(["ag-1", "ag-3"]);
    expect(layout.panes[1]!.members).toEqual([]);
    expect(panes.unassignedAgentIds("p-a", roster)).toEqual(["ag-2"]);
    // The compose selection itself is untouched.
    expect(selection.selectionFor("p-a")).toEqual(["ag-1", "ag-3"]);
  });

  it("restores and switches minimized panes from the app header", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const state = await import("$lib/state/index.svelte");
    const recipients = await import("$lib/state/recipientSelection.svelte");
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("app-pane-add")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2"];
    const pane2 = panes.moveAgentToNewPane("p-a", roster, "ag-2");
    state.runtimes["ag-2"] = {
      ...(state.runtimes["ag-2"] ?? {
        agent_id: "ag-2",
        run_status: "idle",
        hydration_status: "complete",
        pending_sends: [],
      }),
      run_status: "starting",
      pending_sends: [{ send_id: "send-bob", user_turn_id: "user-bob" }],
    };
    panes.minimizePane("p-a", roster, pane2);
    await waitFor(() => expect(screen.getByTestId("app-pane-minimized-tab")).toBeInTheDocument());
    expect(screen.getByTestId("app-pane-minimized-tab")).toHaveTextContent(/^Pane 2$/);
    expect(
      within(screen.getByTestId("app-pane-minimized-tab")).getByRole("status", {
        name: "Pane 2 has running agents",
      }),
    ).toBeInTheDocument();

    state.runtimes["ag-2"] = {
      ...state.runtimes["ag-2"]!,
      run_status: "idle",
      pending_sends: [],
    };
    await waitFor(() =>
      expect(
        within(screen.getByTestId("app-pane-minimized-tab")).getByRole("status", {
          name: "Pane 2 activity ended",
        }),
      ).toBeInTheDocument(),
    );

    await fireEvent.click(screen.getByTestId("app-pane-minimized-tab"));
    // Revealing a pane is deferred behind the transcript-busy spinner (two
    // animation frames), so poll for the layout change rather than asserting
    // synchronously right after the click.
    await waitFor(() => expect(panes.layoutFor("p-a", roster).minimized).toEqual([]));

    panes.maximizePane("p-a", roster, panes.layoutFor("p-a", roster).panes[0]!.id);
    await waitFor(() =>
      expect(screen.getByTestId("app-pane-minimized-tab")).toHaveTextContent(/^Pane 2$/),
    );
    expect(
      within(screen.getByTestId("app-pane-minimized-tab")).queryByRole("status", {
        name: "Pane 2 activity ended",
      }),
    ).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("app-pane-minimized-tab"));
    await waitFor(() => {
      expect(panes.layoutFor("p-a", roster).maximized).toBe(pane2);
      expect(recipients.selectionFor("p-a")).toEqual(["ag-2"]);
    });

    panes.maximizePane("p-a", roster, pane2);
    await waitFor(() =>
      expect(screen.getByTestId("app-pane-minimized-tab")).toHaveTextContent("Pane 1"),
    );
    await fireEvent.click(screen.getByTestId("app-pane-minimized-tab"));
    await waitFor(() => {
      expect(panes.layoutFor("p-a", roster).maximized).toBe(
        panes.layoutFor("p-a", roster).panes[0]!.id,
      );
      expect(recipients.selectionFor("p-a")).toEqual(["ag-1"]);
    });

    expect(screen.queryByTestId("app-pane-restore-all")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("pane-maximize"));
    // Maximize/restore is deferred behind the busy spinner — poll for the result.
    await waitFor(() => expect(panes.layoutFor("p-a", roster).maximized).toBeNull());
  });

  it("does not corrupt the original project's pane layout when the user switches project before the deferred reveal runs", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const ws = await import("$lib/state/workspace.svelte");
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [
        agent({ id: "ag-3", project_id: "p-b", name: "carol" }),
        agent({ id: "ag-4", project_id: "p-b", name: "dave" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(within(projectRowByName("alpha")).getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("app-pane-add")).toBeInTheDocument());

    // Park ag-2 in its own pane and minimize it, so project A has a non-default
    // layout (ag-2 lives in pane 2) that corruption would be visible against.
    const rosterA = ["ag-1", "ag-2"];
    const pane2 = panes.moveAgentToNewPane("p-a", rosterA, "ag-2");
    panes.minimizePane("p-a", rosterA, pane2);
    await waitFor(() => expect(screen.getByTestId("app-pane-minimized-tab")).toBeInTheDocument());

    // Clicking the tab schedules the reveal behind the spinner's two animation
    // frames. Synchronously switch to project B before those frames fire (no
    // awaited gap, so no timer can run in between): `activateProject` sets the
    // active project id synchronously. When the deferred action lands, B is
    // active. Under the bug, the closure read the live roster (B's) and
    // `reconcileLayout` would prune ag-2 from A's pane and persist the loss.
    fireEvent.click(screen.getByTestId("app-pane-minimized-tab"));
    void ws.activateProject("p-b");
    expect(ws.selection.activeProjectId).toBe("p-b");

    // Advance well past the reveal's two deferred frames so its action has run
    // (and, per the guard, been dropped) before asserting.
    for (let i = 0; i < 4; i++) await new Promise(requestAnimationFrame);

    // A's saved layout is untouched: ag-2 still belongs to pane 2 (the captured
    // roster prevented the prune) and pane 2 is still minimized (the staleness
    // guard dropped the now-irrelevant reveal).
    const layoutA = panes.layoutFor("p-a", rosterA);
    expect(layoutA.panes.find((p) => p.id === pane2)?.members).toEqual(["ag-2"]);
    expect(layoutA.minimized).toContain(pane2);
  });

  it("keeps pane tab completion markers across project switches", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const state = await import("$lib/state/index.svelte");
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    seedProject({
      projectId: "p-b",
      directory: DIR_B,
      name: "beta",
      agents: [agent({ id: "ag-3", project_id: "p-b", name: "beta-agent" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(within(projectRowByName("alpha")).getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("app-pane-add")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2"];
    const pane2 = panes.moveAgentToNewPane("p-a", roster, "ag-2");
    state.runtimes["ag-2"] = {
      ...(state.runtimes["ag-2"] ?? {
        agent_id: "ag-2",
        run_status: "idle",
        hydration_status: "complete",
        pending_sends: [],
      }),
      run_status: "starting",
      pending_sends: [{ send_id: "send-bob", user_turn_id: "user-bob" }],
    };
    panes.minimizePane("p-a", roster, pane2);
    await waitFor(() =>
      expect(
        within(screen.getByTestId("app-pane-minimized-tab")).getByRole("status", {
          name: "Pane 2 has running agents",
        }),
      ).toBeInTheDocument(),
    );

    await fireEvent.click(within(projectRowByName("beta")).getByText("beta"));
    await waitFor(() => expect(screen.queryByTestId("app-pane-minimized-tab")).toBeNull());
    state.runtimes["ag-2"] = {
      ...state.runtimes["ag-2"]!,
      run_status: "idle",
      pending_sends: [],
    };

    await fireEvent.click(within(projectRowByName("alpha")).getByText("alpha"));
    await waitFor(() =>
      expect(
        within(screen.getByTestId("app-pane-minimized-tab")).getByRole("status", {
          name: "Pane 2 activity ended",
        }),
      ).toBeInTheDocument(),
    );
  });

  it("⌘⌥1..N targets pane N once split, and is inert with a single pane", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    panes._testing.reset();
    selection._testing.reset();

    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2"];

    // Single pane: the chord is inert (no pane to disambiguate).
    selection.setRecipients("p-a", ["ag-1"]);
    await fireEvent.keyDown(window, { key: "1", code: "Digit1", metaKey: true, altKey: true });
    expect(selection.selectionFor("p-a")).toEqual(["ag-1"]);

    // Split, then ⌘⌥2 targets the new pane and ⌘⌥1 the leftmost — full
    // replace semantics, same meaning as clicking the pane header.
    panes.moveAgentToNewPane("p-a", roster, "ag-2");
    await fireEvent.keyDown(window, { key: "2", code: "Digit2", metaKey: true, altKey: true });
    expect(selection.selectionFor("p-a")).toEqual(["ag-2"]);
    await fireEvent.keyDown(window, { key: "1", code: "Digit1", metaKey: true, altKey: true });
    expect(selection.selectionFor("p-a")).toEqual(["ag-1"]);

    // ⌘1 (no Alt) stays the per-agent chip toggle — no collision: it toggles
    // alice out of the set rather than re-targeting a pane.
    await fireEvent.keyDown(window, { key: "1", metaKey: true });
    expect(selection.selectionFor("p-a")).toEqual([]);

    // An empty pane keeps its positional number but is not a send target:
    // ⌘⌥N on it is a no-op rather than clearing the recipient set.
    const pane1 = panes.layoutFor("p-a", roster).panes[0]!.id;
    panes.moveAgentToPane("p-a", roster, "ag-2", pane1); // empties pane 2
    selection.setRecipients("p-a", ["ag-1"]);
    await fireEvent.keyDown(window, { key: "2", code: "Digit2", metaKey: true, altKey: true });
    expect(selection.selectionFor("p-a")).toEqual(["ag-1"]);

    panes._testing.reset();
    selection._testing.reset();
  });

  it("⌘⇧[ / ⌘⇧] cycle the targeted pane by position", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    panes._testing.reset();
    selection._testing.reset();
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2"];
    panes.moveAgentToNewPane("p-a", roster, "ag-2"); // pane 1: alice, pane 2: bob
    selection.setRecipients("p-a", ["ag-1"]);

    // ⌘⇧] → next pane (bob); again wraps back to alice. `code`, not `key`
    // (Shift+bracket produces "{"/"}").
    await fireEvent.keyDown(window, {
      key: "}",
      code: "BracketRight",
      metaKey: true,
      shiftKey: true,
    });
    expect(selection.selectionFor("p-a")).toEqual(["ag-2"]);
    await fireEvent.keyDown(window, {
      key: "}",
      code: "BracketRight",
      metaKey: true,
      shiftKey: true,
    });
    expect(selection.selectionFor("p-a")).toEqual(["ag-1"]);

    // ⌘⇧[ → previous pane (wraps from alice to bob).
    await fireEvent.keyDown(window, {
      key: "{",
      code: "BracketLeft",
      metaKey: true,
      shiftKey: true,
    });
    expect(selection.selectionFor("p-a")).toEqual(["ag-2"]);

    panes._testing.reset();
    selection._testing.reset();
  });

  it("⌘⇧] shows the transcript spinner when cycling onto a hidden pane", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    panes._testing.reset();
    selection._testing.reset();
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2"];
    panes.moveAgentToNewPane("p-a", roster, "ag-2"); // pane 1: alice, pane 2: bob
    const pane2 = panes.layoutFor("p-a", roster).panes[1]!.id;
    // Maximize pane 1 → pane 2 is hidden, so cycling onto it remounts the
    // transcript and should show the busy spinner (the reported bug: it didn't).
    panes.maximizePane("p-a", roster, panes.layoutFor("p-a", roster).panes[0]!.id);

    await fireEvent.keyDown(window, {
      key: "}",
      code: "BracketRight",
      metaKey: true,
      shiftKey: true,
    });

    // Spinner shows up front, before the deferred reveal runs.
    expect(screen.getByTestId("transcript-busy-overlay")).toBeInTheDocument();

    // Then the cycle applies: pane 2 is targeted + re-maximized, overlay clears.
    await waitFor(() => expect(selection.selectionFor("p-a")).toEqual(["ag-2"]));
    expect(panes.layoutFor("p-a", roster).maximized).toBe(pane2);
    await waitFor(() => expect(screen.queryByTestId("transcript-busy-overlay")).toBeNull());

    panes._testing.reset();
    selection._testing.reset();
  });

  it("maximizing a pane shows the transcript spinner", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    panes._testing.reset();
    selection._testing.reset();
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2"];
    panes.moveAgentToNewPane("p-a", roster, "ag-2"); // 2 panes
    const pane1 = panes.layoutFor("p-a", roster).panes[0]!.id;
    await waitFor(() => expect(screen.getAllByTestId("pane-maximize")).toHaveLength(2));

    await fireEvent.click(screen.getAllByTestId("pane-maximize")[0]!);
    expect(screen.getByTestId("transcript-busy-overlay")).toBeInTheDocument();
    await waitFor(() => expect(panes.layoutFor("p-a", roster).maximized).toBe(pane1));
    await waitFor(() => expect(screen.queryByTestId("transcript-busy-overlay")).toBeNull());

    panes._testing.reset();
    selection._testing.reset();
  });

  it("Restore all shows the transcript spinner", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    panes._testing.reset();
    selection._testing.reset();
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
        agent({ id: "ag-3", project_id: "p-a", name: "carol" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2", "ag-3"];
    panes.moveAgentToNewPane("p-a", roster, "ag-2");
    panes.moveAgentToNewPane("p-a", roster, "ag-3"); // 3 panes
    // Maximize one → the other two become header tabs → "Restore all" appears.
    panes.maximizePane("p-a", roster, panes.layoutFor("p-a", roster).panes[0]!.id);
    await waitFor(() => expect(screen.getByTestId("app-pane-restore-all")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("app-pane-restore-all"));
    expect(screen.getByTestId("transcript-busy-overlay")).toBeInTheDocument();
    await waitFor(() => expect(panes.layoutFor("p-a", roster).maximized).toBeNull());
    await waitFor(() => expect(screen.queryByTestId("transcript-busy-overlay")).toBeNull());

    panes._testing.reset();
    selection._testing.reset();
  });

  it("Restore all appears with minimized panes and brings them all back", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    panes._testing.reset();
    selection._testing.reset();
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
        agent({ id: "ag-3", project_id: "p-a", name: "carol" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2", "ag-3"];
    panes.moveAgentToNewPane("p-a", roster, "ag-2");
    panes.moveAgentToNewPane("p-a", roster, "ag-3"); // 3 panes
    const initial = panes.layoutFor("p-a", roster).panes;
    // Minimize two panes (no maximize) — the reported bug: Restore all was hidden.
    panes.minimizePane("p-a", roster, initial[1]!.id);
    panes.minimizePane("p-a", roster, initial[2]!.id);

    await waitFor(() => expect(screen.getByTestId("app-pane-restore-all")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("app-pane-restore-all"));

    // Restore all expands everything: no minimized, no maximized left.
    await waitFor(() => {
      const layout = panes.layoutFor("p-a", roster);
      expect(layout.minimized).toEqual([]);
      expect(layout.maximized).toBeNull();
    });

    panes._testing.reset();
    selection._testing.reset();
  });

  it("⌘⌥N reveals the targeted pane: restores a minimized one, replaces a maximized one", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    panes._testing.reset();
    selection._testing.reset();

    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2"];
    const p2 = panes.moveAgentToNewPane("p-a", roster, "ag-2");
    const p1 = panes.layoutFor("p-a", roster).panes[0]!.id;

    // Minimized, nothing maximized: the chord restores the pane into the row
    // — a send must never stream into an invisible pane.
    panes.minimizePane("p-a", roster, p2);
    await fireEvent.keyDown(window, { key: "2", code: "Digit2", metaKey: true, altKey: true });
    expect(selection.selectionFor("p-a")).toEqual(["ag-2"]);
    expect(panes.layoutFor("p-a", roster).minimized).toEqual([]);

    // Another pane maximized: the chord keeps focus mode but swaps which pane
    // holds it.
    panes.maximizePane("p-a", roster, p1);
    await fireEvent.keyDown(window, { key: "2", code: "Digit2", metaKey: true, altKey: true });
    expect(panes.layoutFor("p-a", roster).maximized).toBe(p2);
    expect(selection.selectionFor("p-a")).toEqual(["ag-2"]);

    panes._testing.reset();
    selection._testing.reset();
  });

  it("⌘⌥N is fully inert while targeting is locked — no retarget, no reveal", async () => {
    const panes = await import("$lib/state/transcriptPanes.svelte");
    const selection = await import("$lib/state/recipientSelection.svelte");
    panes._testing.reset();
    selection._testing.reset();

    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [
        agent({ id: "ag-1", project_id: "p-a", name: "alice" }),
        agent({ id: "ag-2", project_id: "p-a", name: "bob" }),
      ],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const roster = ["ag-1", "ag-2"];
    const p2 = panes.moveAgentToNewPane("p-a", roster, "ag-2");
    panes.minimizePane("p-a", roster, p2);
    selection.setRecipients("p-a", ["ag-1"]);

    // Locked (prompt-render window): the gesture is atomic — neither the
    // recipient set nor pane visibility may change, or the revealed pane
    // would imply a target the refused write never made.
    selection.setTargetingLocked("p-a", true);
    await fireEvent.keyDown(window, { key: "2", code: "Digit2", metaKey: true, altKey: true });
    expect(selection.selectionFor("p-a")).toEqual(["ag-1"]);
    expect(panes.layoutFor("p-a", roster).minimized).toEqual([p2]);

    // Unlocked: the same chord retargets and reveals.
    selection.setTargetingLocked("p-a", false);
    await fireEvent.keyDown(window, { key: "2", code: "Digit2", metaKey: true, altKey: true });
    expect(selection.selectionFor("p-a")).toEqual(["ag-2"]);
    expect(panes.layoutFor("p-a", roster).minimized).toEqual([]);

    panes._testing.reset();
    selection._testing.reset();
  });

  it("opens the command palette with ⌘⇧P and runs a palette action", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    // Closed by default; ⌘⇧P opens it even though the welcome/sidebar own focus.
    expect(screen.queryByTestId("command-palette")).toBeNull();
    await fireEvent.keyDown(window, { key: "P", code: "KeyP", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("command-palette")).toBeInTheDocument());

    // Filter to the projects-sidebar toggle and run it; the sidebar hides.
    await fireEvent.input(screen.getByTestId("command-palette-search"), {
      target: { value: "projects sidebar" },
    });
    await fireEvent.click(screen.getByTestId("command-option-nav.toggle-projects-sidebar"));
    await waitFor(() => expect(screen.queryByTestId("command-palette")).toBeNull());
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());
  });

  it("switches projects from the command palette", async () => {
    seedProject({ projectId: "p-a", directory: DIR_A, name: "alpha" });
    seedProject({ projectId: "p-b", directory: DIR_B, name: "beta" });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    await fireEvent.keyDown(window, { key: "P", code: "KeyP", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("command-palette")).toBeInTheDocument());

    await fireEvent.input(screen.getByTestId("command-palette-search"), {
      target: { value: "beta" },
    });
    await fireEvent.click(screen.getByTestId("command-option-switch.p-b"));
    await waitFor(() => expect(backend.activeProjectId).toBe("p-b"));
  });

  it("opens the palette from the header button", async () => {
    seedProject({ projectId: "p-a", directory: DIR_A, name: "alpha" });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("command-palette-button")).toBeInTheDocument());

    expect(screen.queryByTestId("command-palette")).toBeNull();
    await fireEvent.click(screen.getByTestId("command-palette-button"));
    await waitFor(() => expect(screen.getByTestId("command-palette")).toBeInTheDocument());
  });

  it("suppresses other window shortcuts while the palette is open", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    await fireEvent.keyDown(window, { key: "P", code: "KeyP", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("command-palette")).toBeInTheDocument());

    // ⌘B would normally toggle the projects sidebar; while the palette owns the
    // keyboard it must be a no-op.
    await fireEvent.keyDown(window, { key: "b", metaKey: true });
    await Promise.resolve();
    expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument();
    expect(screen.getByTestId("command-palette")).toBeInTheDocument();

    // ⌘⇧P closes it again, and the suppressed shortcut works normally afterward.
    await fireEvent.keyDown(window, { key: "P", code: "KeyP", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.queryByTestId("command-palette")).toBeNull());
    await fireEvent.keyDown(window, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());
  });

  it("⌘N opens the add-project dialog", async () => {
    seedProject({ projectId: "p-a", directory: DIR_A, name: "alpha" });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    await fireEvent.keyDown(window, { key: "n", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("new-project-form")).toBeInTheDocument());
  });

  it("⌘N does not open the add-project dialog while the Git view is showing", async () => {
    seedProject({ projectId: "p-a", directory: DIR_A, name: "alpha" });
    backend.trackedRepos = [
      {
        repo: {
          root: DIR_A,
          name: "alpha-repo",
          default_branch: "main",
          available: true,
          is_bare: false,
          local_branches: [],
          remote_branches: [],
          detached_worktrees: [],
        },
        linked_projects: {},
      } satisfies RepoListing,
    ];
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("view-toggle-git"));
    await waitFor(() => expect(screen.getByTestId("git-view")).toBeInTheDocument());

    // In the Git view ⌘N is Add-repo (handled by GitView), so App must NOT open
    // the add-project dialog.
    await fireEvent.keyDown(window, { key: "n", metaKey: true });
    await Promise.resolve();
    expect(screen.queryByTestId("new-project-form")).toBeNull();
  });

  it("⌘⇧N opens the add-agent modal when a project is active, and no-ops otherwise", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    // No active project yet → ⌘⇧N does nothing.
    await fireEvent.keyDown(window, { key: "N", code: "KeyN", metaKey: true, shiftKey: true });
    await Promise.resolve();
    expect(screen.queryByTestId("dialog-content")).toBeNull();

    // Activate the project, then ⌘⇧N opens the add-agent modal.
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());
    await fireEvent.keyDown(window, { key: "N", code: "KeyN", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("dialog-content")).toBeInTheDocument());
    expect(
      within(screen.getByTestId("dialog-content")).getByTestId("agent-name"),
    ).toBeInTheDocument();
  });

  it("opens the active project in the editor with the editor shortcut", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const rowButton = projectRowByName("alpha").querySelector("button");
    if (rowButton === null) throw new Error("expected project row button");
    await fireEvent.click(rowButton);
    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));

    await fireEvent.keyDown(window, { key: "E", code: "KeyE", metaKey: true, shiftKey: true });

    expect(invokeMock).toHaveBeenCalledWith("open_in_editor", { path: DIR_A });
  });

  it("shows a visible error when the editor shortcut fails", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.openEditorFailure = "editor missing";
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const rowButton = projectRowByName("alpha").querySelector("button");
    if (rowButton === null) throw new Error("expected project row button");
    await fireEvent.click(rowButton);
    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));

    await fireEvent.keyDown(window, { key: "E", code: "KeyE", metaKey: true, shiftKey: true });

    await waitFor(() =>
      expect(screen.getByTestId("banner-open-editor-failed")).toHaveTextContent("editor missing"),
    );
    await fireEvent.click(screen.getByTestId("banner-open-editor-failed-dismiss"));
    expect(screen.queryByTestId("banner-open-editor-failed")).not.toBeInTheDocument();
  });

  it("clears the editor shortcut error after a later successful shortcut", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.openEditorFailure = "editor missing";
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const rowButton = projectRowByName("alpha").querySelector("button");
    if (rowButton === null) throw new Error("expected project row button");
    await fireEvent.click(rowButton);
    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));

    await fireEvent.keyDown(window, { key: "E", code: "KeyE", metaKey: true, shiftKey: true });
    await waitFor(() =>
      expect(screen.getByTestId("banner-open-editor-failed")).toBeInTheDocument(),
    );

    backend.openEditorFailure = null;
    await fireEvent.keyDown(window, { key: "E", code: "KeyE", metaKey: true, shiftKey: true });

    await waitFor(() =>
      expect(screen.queryByTestId("banner-open-editor-failed")).not.toBeInTheDocument(),
    );
  });

  it("ignores stale editor shortcut failures after a newer shortcut succeeds", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    const firstOpen = deferred<unknown>();
    backend.openEditorQueue = [firstOpen.promise, Promise.resolve(null)];
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const rowButton = projectRowByName("alpha").querySelector("button");
    if (rowButton === null) throw new Error("expected project row button");
    await fireEvent.click(rowButton);
    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));

    await fireEvent.keyDown(window, { key: "E", code: "KeyE", metaKey: true, shiftKey: true });
    await fireEvent.keyDown(window, { key: "E", code: "KeyE", metaKey: true, shiftKey: true });
    firstOpen.reject(new Error("editor missing"));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(screen.queryByTestId("banner-open-editor-failed")).not.toBeInTheDocument();
  });

  it("global shortcuts work when focus is inside the compose box", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    const textarea = screen.getByTestId("compose-textarea");
    textarea.focus();

    // ⌘B from the composer toggles the projects sidebar.
    await fireEvent.keyDown(textarea, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());
    await fireEvent.keyDown(textarea, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    // ⌘⇧B from the composer toggles the agents sidebar.
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    await fireEvent.keyDown(textarea, { key: "B", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.queryByTestId("sidebar")).not.toBeInTheDocument());
    await fireEvent.keyDown(textarea, { key: "B", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());

    // ⌘, from the composer opens settings.
    await fireEvent.keyDown(textarea, { key: ",", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
  });

  it("global shortcuts work when focus is inside a prompt field", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.prompts = [REVIEW_PROMPT];
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-prompt-button")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("compose-prompt-button"));
    await waitFor(() =>
      expect(screen.getByTestId("prompt-option-local:review")).toBeInTheDocument(),
    );
    await fireEvent.click(screen.getByTestId("prompt-option-local:review"));
    const promptField = await screen.findByTestId("prompt-arg-focus");
    promptField.focus();

    // ⌘B from a prompt field toggles the projects sidebar just like the plain composer.
    await fireEvent.keyDown(promptField, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument());
    await fireEvent.keyDown(promptField, { key: "b", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    // ⌘⇧B from a prompt field toggles the agents sidebar.
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    await fireEvent.keyDown(promptField, { key: "B", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.queryByTestId("sidebar")).not.toBeInTheDocument());
    await fireEvent.keyDown(promptField, { key: "B", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("sidebar")).toBeInTheDocument());

    // ⌘, from a prompt field opens settings.
    await fireEvent.keyDown(promptField, { key: ",", metaKey: true });
    await waitFor(() => expect(screen.getByTestId("settings-view")).toBeInTheDocument());
  });

  it("global shortcuts are suppressed inside non-composer editable elements", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    await fireEvent.keyDown(input, { key: "b", metaKey: true });
    expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument();

    await fireEvent.keyDown(input, { key: ",", metaKey: true });
    expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument();
    input.remove();
  });

  // --- Git view toggle ---

  it("toggles to the Git view (center takeover, sidebar hidden) and back", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.trackedRepos = [
      {
        repo: {
          root: DIR_A,
          name: "alpha-repo",
          default_branch: "main",
          available: true,
          is_bare: false,
          local_branches: [],
          remote_branches: [],
          detached_worktrees: [],
        },
        linked_projects: {},
      } satisfies RepoListing,
    ];
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    // Toggle to Git: the view takes over the center pane and the Projects
    // sidebar hides (full-width takeover, decision D1).
    await fireEvent.click(screen.getByTestId("view-toggle-git"));
    await waitFor(() => expect(screen.getByTestId("git-view")).toBeInTheDocument());
    expect(screen.queryByTestId("projects-sidebar")).not.toBeInTheDocument();
    // The tracked repo loaded via list_tracked_repos.
    await waitFor(() => expect(screen.getByTestId("git-repo")).toBeInTheDocument());

    // Toggle back to Projects: the view is gone, the sidebar returns.
    await fireEvent.click(screen.getByTestId("view-toggle-projects"));
    await waitFor(() => expect(screen.queryByTestId("git-view")).not.toBeInTheDocument());
    expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument();
  });

  it("shows project loading immediately when returning from Git view to an active project", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.trackedRepos = [
      {
        repo: {
          root: DIR_A,
          name: "alpha-repo",
          default_branch: "main",
          available: true,
          is_bare: false,
          local_branches: [],
          remote_branches: [],
          detached_worktrees: [],
        },
        linked_projects: {},
      } satisfies RepoListing,
    ];
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());
    await fireEvent.click(screen.getByText("alpha"));
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("view-toggle-git"));
    await waitFor(() => expect(screen.getByTestId("git-view")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("view-toggle-projects"));

    expect(screen.queryByTestId("git-view")).not.toBeInTheDocument();
    expect(screen.getByTestId("project-loading")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("compose-textarea")).toBeInTheDocument());
    await expectComposeFocused();
  });

  it("opens the active project in Git view with the project shortcut", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.trackedRepos = [
      {
        repo: {
          root: DIR_A,
          name: "alpha-repo",
          default_branch: "main",
          available: true,
          is_bare: false,
          local_branches: [
            {
              name: "main",
              upstream: "origin/main",
              sync: { kind: "in_sync" },
              behind_base: null,
              merged: null,
              dangling: false,
              worktree: {
                path: DIR_A,
                dirty: false,
                untracked: false,
                detached_hash: null,
                warning: null,
              },
            },
          ],
          remote_branches: [{ name: "origin/main", merged: null, behind_base: null }],
          detached_worktrees: [],
        },
        linked_projects: {
          [DIR_A]: [{ id: "p-a", name: "alpha", directory: DIR_A }],
        },
      } satisfies RepoListing,
    ];
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const rowButton = projectRowByName("alpha").querySelector("button");
    if (rowButton === null) throw new Error("expected project row button");
    await fireEvent.click(rowButton);
    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));

    await fireEvent.keyDown(window, { key: "f", code: "KeyF", metaKey: true, shiftKey: true });

    await waitFor(() => expect(screen.getByTestId("git-view")).toBeInTheDocument());
    const branch = await waitFor(() =>
      document.querySelector('[data-testid="git-branch"][data-branch="main"]'),
    );
    expect(branch).not.toBeNull();
    expect(within(branch as HTMLElement).getByTestId("branch-select")).toHaveAttribute(
      "data-selected",
      "true",
    );
  });

  it("opens the selected Git branch worktree in the editor with the editor shortcut", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    backend.trackedRepos = [
      {
        repo: {
          root: DIR_A,
          name: "alpha-repo",
          default_branch: "main",
          available: true,
          is_bare: false,
          local_branches: [
            {
              name: "main",
              upstream: "origin/main",
              sync: { kind: "in_sync" },
              behind_base: null,
              merged: null,
              dangling: false,
              worktree: {
                path: DIR_A,
                dirty: false,
                untracked: false,
                detached_hash: null,
                warning: null,
              },
            },
          ],
          remote_branches: [{ name: "origin/main", merged: null, behind_base: null }],
          detached_worktrees: [],
        },
        linked_projects: {
          [DIR_A]: [{ id: "p-a", name: "alpha", directory: DIR_A }],
        },
      } satisfies RepoListing,
    ];
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    const rowButton = projectRowByName("alpha").querySelector("button");
    if (rowButton === null) throw new Error("expected project row button");
    await fireEvent.click(rowButton);
    await waitFor(() => expect(backend.activeProjectId).toBe("p-a"));

    await fireEvent.keyDown(window, { key: "f", code: "KeyF", metaKey: true, shiftKey: true });
    await waitFor(() => expect(screen.getByTestId("git-view")).toBeInTheDocument());
    await waitFor(() =>
      expect(
        document
          .querySelector('[data-testid="git-branch"][data-branch="main"]')
          ?.querySelector('[data-testid="branch-select"]'),
      ).toHaveAttribute("data-selected", "true"),
    );

    await fireEvent.keyDown(window, { key: "E", code: "KeyE", metaKey: true, shiftKey: true });

    expect(invokeMock).toHaveBeenCalledWith("open_in_editor", { path: DIR_A });
  });

  it("opens the selected detached Git worktree in the editor with the editor shortcut", async () => {
    seedProject({
      projectId: "p-a",
      directory: DIR_A,
      name: "alpha",
      agents: [agent({ id: "ag-1", project_id: "p-a", name: "assistant" })],
    });
    const detachedPath = `${DIR_A}/detached`;
    backend.trackedRepos = [
      {
        repo: {
          root: DIR_A,
          name: "alpha-repo",
          default_branch: "main",
          available: true,
          is_bare: false,
          local_branches: [],
          remote_branches: [],
          detached_worktrees: [
            {
              path: detachedPath,
              dirty: true,
              untracked: false,
              detached_hash: "abc1234",
              warning: null,
            },
          ],
        },
        linked_projects: {},
      } satisfies RepoListing,
    ];
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("view-toggle-git"));
    await waitFor(() => expect(screen.getByTestId("git-view")).toBeInTheDocument());
    const detachedRow = await screen.findByTestId("git-detached-worktree");
    await fireEvent.click(within(detachedRow).getByTestId("worktree-select"));
    await waitFor(() =>
      expect(within(detachedRow).getByTestId("worktree-select")).toHaveAttribute(
        "data-selected",
        "true",
      ),
    );

    await fireEvent.keyDown(window, { key: "E", code: "KeyE", metaKey: true, shiftKey: true });

    expect(invokeMock).toHaveBeenCalledWith("open_in_editor", { path: detachedPath });
  });
});
