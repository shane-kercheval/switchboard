import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type {
  AgentRecord,
  NormalizedEvent,
  ProjectConversation,
  ProjectListing,
  ProjectSummary,
  RepoListing,
} from "$lib/types";
import { ALL_HARNESSES } from "$lib/harnessDisplay";

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
  // (keyed by HarnessKind). Drives the binary-missing banner tests.
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
  agentQueue: AgentRecord[];
  sendMessageId: string;
  loadConvoCalls: string[];
  failOpenFor: Set<string>;
  failConvoFor: Set<string>;
  failPickFor: Set<string>;
  // Git-view tracked repos returned by `list_tracked_repos` / `read_tracked_repo`.
  trackedRepos: RepoListing[];
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
    agentQueue: [],
    sendMessageId: "77777777-7777-7000-8000-777777777777",
    loadConvoCalls: [],
    failOpenFor: new Set(),
    failConvoFor: new Set(),
    failPickFor: new Set(),
    trackedRepos: [],
  };
}

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
      };
      backend.projects.push(row);
      backend.rosters.set(id, []);
      return summaryFor(id);
    }
    case "open_project": {
      const pid = args?.projectId as string;
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
      return { editor_command: null, terminal_app: "Terminal" };
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

async function mountApp() {
  const App = (await import("./App.svelte")).default;
  return render(App);
}

// Drives the new-project dialog flow (choose folder → name → submit). The
// folder is resolved from the mocked native dialog; the project is created in
// DIR_A unless `folder` overrides it.
async function createNewProjectViaDialog(name: string, folder: string = DIR_A): Promise<void> {
  openDialogMock.mockResolvedValueOnce(folder);
  await fireEvent.click(screen.getByTestId("welcome-add-project"));
  await waitFor(() => expect(screen.getByTestId("new-project-form")).toBeInTheDocument());
  await fireEvent.click(screen.getByTestId("new-project-choose-folder"));
  const nameInput = screen.getByTestId("new-project-name") as HTMLInputElement;
  await fireEvent.input(nameInput, { target: { value: name } });
  await fireEvent.click(screen.getByTestId("new-project-submit"));
}

// create_agent calls (name + harness pairs) in invocation order.
function createAgentCalls(): { name: string; harness: string }[] {
  return invokeMock.mock.calls
    .filter(([c]) => c === "create_agent")
    .map(([, a]) => a as { name: string; harness: string });
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

  it("renders a Claude binary-missing banner when only the Claude probe fails", async () => {
    backend.notInstalled.add("claude_code");
    await mountApp();
    await waitFor(() =>
      expect(screen.getByTestId("banner-binary_missing-claude_code")).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("banner-binary_missing-codex")).not.toBeInTheDocument();
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

  it("renders two binary banners simultaneously when two binaries are missing", async () => {
    backend.notInstalled.add("claude_code");
    backend.notInstalled.add("codex");
    await mountApp();
    await waitFor(() => {
      expect(screen.getByTestId("banner-binary_missing-claude_code")).toBeInTheDocument();
      expect(screen.getByTestId("banner-binary_missing-codex")).toBeInTheDocument();
    });
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

  it("new project: choosing a folder + name creates the project, activates it, and auto-seeds an agent per installed harness", async () => {
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    await waitFor(() => expect(screen.getByText("brand-new")).toBeInTheDocument());
    const createProjectCalls = invokeMock.mock.calls.filter(([c]) => c === "create_project");
    expect(createProjectCalls).toHaveLength(1);
    expect(createProjectCalls[0]?.[1]).toEqual({ name: "brand-new", directory: DIR_A });

    // All four harnesses are installed by default → one agent each, named after
    // the harness, in HARNESSES order.
    await waitFor(() => expect(createAgentCalls()).toHaveLength(4));
    expect(createAgentCalls()).toEqual([
      { name: "claude-code", harness: "claude_code" },
      { name: "codex", harness: "codex" },
      { name: "gemini", harness: "gemini" },
      { name: "antigravity", harness: "antigravity" },
    ]);
    // Auto-seeded → the roster is populated, not the empty first-agent prompt.
    await waitFor(() => expect(screen.getAllByTestId("sidebar-agent")).toHaveLength(4));
    expect(screen.queryByTestId("confirm-create-agent")).not.toBeInTheDocument();
  });

  it("new project: seeds agents only for installed harnesses", async () => {
    backend.notInstalled.add("gemini");
    backend.notInstalled.add("antigravity");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    await waitFor(() => expect(createAgentCalls()).toHaveLength(2));
    expect(createAgentCalls()).toEqual([
      { name: "claude-code", harness: "claude_code" },
      { name: "codex", harness: "codex" },
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
    await waitFor(() => expect(createAgentCalls()).toHaveLength(4));
  });

  it("new project: a failed agent create surfaces a dismissible banner and the rest still seed", async () => {
    backend.createAgentFailFor.add("codex");
    await mountApp();
    await waitFor(() => expect(screen.getByTestId("welcome-add-project")).toBeInTheDocument());

    await createNewProjectViaDialog("brand-new");

    // The other three still created; the failure is surfaced, not silent.
    await waitFor(() => expect(createAgentCalls()).toHaveLength(4));
    const banner = await screen.findByTestId("banner-agent-create-failed-codex");
    expect(banner).toHaveTextContent("Couldn't create the Codex agent");
    expect(screen.getAllByTestId("sidebar-agent")).toHaveLength(3);

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
    expect(createCalls[0]?.[1]).toEqual({ name: "claude-code", harness: "claude_code" });
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
    // And the auth-banner posture is preserved: nothing in the banner stack.
    expect(screen.queryByTestId(/^banner-auth_missing-/)).not.toBeInTheDocument();
  });

  // --- directory removal lifecycle (store-level: the `removeDirectory` +
  // teardown primitive that M8's project-delete will reuse; there is no
  // directory-removal UI in this milestone) ---

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

  it("global shortcuts are suppressed when focus is inside an editable element", async () => {
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

    // ⌘B fired from the textarea must not toggle the projects sidebar.
    await fireEvent.keyDown(textarea, { key: "b", metaKey: true });
    expect(screen.getByTestId("projects-sidebar")).toBeInTheDocument();

    // ⌘⇧B must not toggle the agents sidebar.
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    await fireEvent.keyDown(textarea, { key: "B", metaKey: true, shiftKey: true });
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();

    // ⌘, must not open settings.
    await fireEvent.keyDown(textarea, { key: ",", metaKey: true });
    expect(screen.queryByTestId("settings-view")).not.toBeInTheDocument();
  });

  // --- Git view toggle (M3 commit B) ---

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
});
