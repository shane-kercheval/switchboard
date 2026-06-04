// Workspace-level app state: the set of working directories, the flat
// cross-directory project list, the displayed project, per-project agent
// rosters, and the per-project hydrated conversation overlay.
//
// **Ownership split (decompose model — see `./unified.ts`).** This module owns
// the *project-level* overlay: journal-sourced historical **user messages**
// (grouped by `send_id`) and **outcome markers** (failed/cancelled). It does
// NOT own agent-turn content — that lives in the per-agent `transcripts` /
// `runtimes` maps in `./index.svelte`, both for live streaming and for hydrated
// history (regrouped from `load_project_conversation`'s `agent_turn` items and
// fed through the existing per-agent hydrate path, so the right sidebar's
// per-agent cost/context derivation keeps working).
//
// **Eager registry, lazy everything else.** `loadWorkspace` eagerly fetches the
// cheap, lock-free registry (directory list + flat project list). Per-project
// agent rosters, the inter-process project lock, listener registration, and
// transcript hydration are all deferred to first activation (`activateProject`)
// — locking every project at startup would scale lock count with total project
// count and stop a second process from opening anything.
//
// **Switching is display-only.** `activateProject` sets `selection.activeProjectId`
// immediately; it never tears down listeners, so a backgrounded project's
// agents keep streaming into their per-agent state. (Nothing streams across a
// restart — harness subprocesses die with the app — so "background keeps
// running" is strictly a within-session statement.) The backend
// `set_active_project` is re-issued on every switch because `create_agent` /
// `attach_agent` target the backend's active project.

import * as api from "$lib/api";
import type {
  AgentId,
  AgentRecord,
  ConversationItem,
  HarnessKind,
  LoadedTurn,
  ProjectId,
  ProjectListing,
  SendId,
  WorkspaceDirectoryInfo,
} from "$lib/types";
import { untrack } from "svelte";
import { harnessAvailability, refreshHarnessAvailability } from "$lib/harnessAvailability.svelte";
import { HARNESS_DEFAULT_AGENT_NAME } from "$lib/harnessDisplay";
import { currentIsoTimestamp } from "$lib/utils";
import { buildLiveSendsMap } from "$lib/state/liveSends";
import {
  applyAgentHydrate,
  markHydrationAttempted,
  registerAgent,
  runtimes,
  transcripts,
  unregisterAgents,
} from "./index.svelte";

/// Per-project hydrated overlay. `items` holds only `user_message` and
/// `outcome` kinds (agent content is routed to per-agent state); `status`
/// drives a loading indicator on first activation and a project-level error
/// state when the merged-conversation load fails outright.
export type ProjectConversationState = {
  items: ConversationItem[];
  status: "pending" | "loading" | "complete" | "failed";
};

/// The registered directories + whether registry changes persist this session.
/// `persistable === false` means an existing `workspace.yaml` couldn't be read
/// at startup — surfaced distinctly from a fresh install.
export const workspace = $state<{ directories: WorkspaceDirectoryInfo[]; persistable: boolean }>({
  directories: [],
  persistable: true,
});

/// The flat cross-directory project list, sorted desc by `last_activity`.
export const projects = $state<{ list: ProjectListing[] }>({ list: [] });

/// The displayed project. Display-only — switching does not stop other
/// projects' agents or tear down their event subscriptions.
///
/// `activationError` holds the message when opening the displayed project
/// failed (locked by another process, directory went unavailable, removed
/// concurrently). It always pertains to the current `activeProjectId`: cleared
/// on every (re)activation and switch, set only on the current one's failure.
/// The center pane renders a retry affordance instead of an endless loading
/// state when it's set.
export const selection = $state<{
  activeProjectId: ProjectId | null;
  activationError: string | null;
}>({ activeProjectId: null, activationError: null });

/// Per-project agent rosters, populated lazily on first activation.
export const agentsByProject = $state<Record<ProjectId, AgentRecord[]>>({});

/// Harnesses whose agent failed to auto-create on the just-created project,
/// with the reason. Surfaced as a dismissible banner so a partial failure (the
/// project opens, but one expected agent is missing) is visible rather than
/// silent. Transient and event-scoped: cleared on every project (re)activation,
/// repopulated only by `createProjectAndActivate`.
export const agentCreationFailures = $state<{ harness: HarnessKind; error: string }[]>([]);

/// Per-project hydrated conversation overlays, keyed by project id.
export const conversations = $state<Record<ProjectId, ProjectConversationState>>({});

/// Projects that completed live work while the user was not viewing them.
export const backgroundCompletedProjectIds = $state<Record<ProjectId, true>>({});

/// Session-local response-completion activity. The backend listing remains the
/// durable baseline; this overlay preserves live completions observed after the
/// workspace registry was loaded, including across later registry refreshes.
export const projectActivityOverrides = $state<Record<ProjectId, string>>({});

/// First-activation guard: holds the in-flight load promise per project so
/// concurrent activations share one load, and so re-activation is a pure
/// display switch (roster + hydration already done).
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const loadStarted = new Map<ProjectId, Promise<void>>();

/// Per-project hydration guard. Sticky across success AND failure for the
/// session — parsers mint fresh `turn_id`s at parse time, so re-hydrating the
/// same project would duplicate its agent turns (same rationale as the
/// per-agent `hydrationAttempted` set).
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const hydrationStarted = new Set<ProjectId>();

function sortByActivity(list: ProjectListing[]): ProjectListing[] {
  return [...list].sort((a, b) => b.last_activity.localeCompare(a.last_activity));
}

function applyActivityOverrides(list: ProjectListing[]): ProjectListing[] {
  return sortByActivity(
    list.map((project) => {
      const override = projectActivityOverrides[project.id];
      return override !== undefined && override > project.last_activity
        ? { ...project, last_activity: override }
        : project;
    }),
  );
}

export function recordProjectsActivityLocally(projectIds: ProjectId[], at: string): void {
  if (projectIds.length === 0) return;
  let changed = false;
  for (const id of projectIds) {
    if (!projects.list.some((project) => project.id === id)) continue;
    projectActivityOverrides[id] = at;
    changed = true;
  }
  if (changed) projects.list = applyActivityOverrides(projects.list);
}

export function liveProjectSends(projectId: ProjectId): Map<SendId, AgentId[]> {
  return buildLiveSendsMap(agentsByProject[projectId] ?? [], runtimes, transcripts);
}

type LiveProjectSendPair = {
  key: string;
  projectId: ProjectId;
  sendId: SendId;
  agentId: AgentId;
};

let previousLiveProjectSendPairs: LiveProjectSendPair[] = [];

function liveProjectSendPairs(): LiveProjectSendPair[] {
  const pairs: LiveProjectSendPair[] = [];
  for (const projectId of Object.keys(agentsByProject)) {
    for (const [sendId, agentIds] of liveProjectSends(projectId)) {
      for (const agentId of agentIds) {
        pairs.push({ key: `${projectId}:${sendId}:${agentId}`, projectId, sendId, agentId });
      }
    }
  }
  return pairs;
}

function projectIdsInPairs(pairs: LiveProjectSendPair[]): ProjectId[] {
  const projectIds: ProjectId[] = [];
  for (const pair of pairs) {
    if (!projectIds.includes(pair.projectId)) projectIds.push(pair.projectId);
  }
  return projectIds;
}

export function startProjectActivityObserver(
  getNow: () => string = currentIsoTimestamp,
): () => void {
  return $effect.root(() => {
    $effect(() => {
      const nowLivePairs = liveProjectSendPairs();
      const previousBusy = projectIdsInPairs(previousLiveProjectSendPairs);
      const nowBusy = projectIdsInPairs(nowLivePairs);
      const completed: ProjectId[] = [];
      const backgroundCompleted: ProjectId[] = [];
      for (const pair of previousLiveProjectSendPairs) {
        if (nowLivePairs.some((nowPair) => nowPair.key === pair.key)) continue;
        if (!completed.includes(pair.projectId)) completed.push(pair.projectId);
      }
      for (const id of previousBusy) {
        if (nowBusy.includes(id)) continue;
        if (id !== selection.activeProjectId) backgroundCompleted.push(id);
      }
      previousLiveProjectSendPairs = nowLivePairs;
      untrack(() => {
        if (completed.length > 0) recordProjectsActivityLocally(completed, getNow());
        for (const id of backgroundCompleted) backgroundCompletedProjectIds[id] = true;
      });
    });
  });
}

/// Fetch the eager registry: the directory list (incl. empty directories + the
/// persistability signal) and the flat project list. Called at startup and
/// after any add/remove/create that changes the registry.
export async function loadWorkspace(): Promise<void> {
  const [dirs, projectList] = await Promise.all([
    api.listWorkspaceDirectories(),
    api.listProjects(),
  ]);
  workspace.directories = dirs.directories;
  workspace.persistable = dirs.persistable;
  projects.list = applyActivityOverrides(projectList);
}

/// Add a working directory to the workspace and refresh the registry.
export async function addDirectory(path: string): Promise<void> {
  await api.initDirectory(path);
  await loadWorkspace();
}

/// Remove a working directory: drains its projects' in-flight turns and
/// releases their locks on the backend (leaving `.switchboard/` on disk), and
/// performs the matching **frontend lifecycle teardown** so a remove-then-re-add
/// of the same project ids (ids are persisted on disk and survive removal)
/// starts clean. Without the teardown, the stale memoized `loadStarted` promise
/// would make re-activation skip `open_project`/`list_agents` and leave the
/// backend with an unloaded "active" project, and the removed agents' listeners
/// would leak.
export async function removeDirectory(path: string): Promise<void> {
  // Snapshot the affected project + agent ids BEFORE the await — `loadWorkspace`
  // (below) will drop these projects from the list, so capture them now.
  const removedProjectIds = projects.list.filter((p) => p.directory === path).map((p) => p.id);
  const removedAgentIds = removedProjectIds.flatMap((id) =>
    (agentsByProject[id] ?? []).map((a) => a.id),
  );
  const activeRemoved = removedProjectIds.includes(selection.activeProjectId ?? "");

  await api.removeDirectory(path);

  // Backend drop succeeded — tear down the matching frontend state.
  unregisterAgents(removedAgentIds);
  for (const id of removedProjectIds) {
    delete agentsByProject[id];
    delete conversations[id];
    delete backgroundCompletedProjectIds[id];
    delete projectActivityOverrides[id];
    loadStarted.delete(id);
    hydrationStarted.delete(id);
  }
  previousLiveProjectSendPairs = previousLiveProjectSendPairs.filter(
    (pair) => !removedProjectIds.includes(pair.projectId),
  );
  if (activeRemoved) {
    selection.activeProjectId = null;
    selection.activationError = null;
  }
  await loadWorkspace();
}

/// Create a project in `directory`, refresh the registry, and activate it.
/// Registers the folder first (idempotent `init_directory`): `create_project`
/// requires its target directory to already be a loaded workspace directory, so
/// a brand-new folder must be added before the project can be created in it.
export async function createProjectAndActivate(name: string, directory: string): Promise<void> {
  await api.initDirectory(directory);
  const summary = await api.createProject(name, directory);
  await loadWorkspace();
  // Activation must complete first: `create_agent` targets the backend's active
  // project, and `activateProject` issues `set_active_project`. It also clears
  // `agentCreationFailures`, so the seeding below starts from a clean slate.
  await activateProject(summary.id);
  await seedAgentsForInstalledHarnesses();
}

/// Auto-populate a freshly created project with one agent per installed harness.
/// New projects only — called solely from `createProjectAndActivate`, never on
/// activation of an existing project.
///
/// Awaits a fresh availability probe before reading `installed()`: the store's
/// startup probe is fired un-awaited and reports `[]` until it resolves, so a
/// project created inside that window would otherwise seed zero agents. The
/// probe is idempotent and cheap, so awaiting it here closes the race.
///
/// Mirrors the canonical create path (`createAgent` → `registerAgent` →
/// `addAgentToActiveProject`) per agent — a plain roster re-fetch would skip
/// `registerAgent` and leave the agents without live transcript/dispatch state.
/// Each create is independent: one failure is recorded in `agentCreationFailures`
/// (surfaced as a dismissible banner) and never aborts the rest or the open.
///
/// **Targets a captured project, not live active state.** Both `create_agent`
/// (backend active project) and `addAgentToActiveProject` (frontend
/// `selection.activeProjectId`) bind to whatever is active *at call time*. If
/// the user navigated to another project mid-seed, continuing would scatter
/// agents into the wrong project — so we capture the id up front and bail if it
/// changes. The new-project dialog also stays non-dismissible while this runs
/// (belt). The durable fix is `create_agent`/`attach_agent` taking an explicit
/// `project_id` instead of reading active state — out of scope here, but the
/// same coupling affects M5's remove/rename.
async function seedAgentsForInstalledHarnesses(): Promise<void> {
  const projectId = selection.activeProjectId;
  await refreshHarnessAvailability();
  for (const harness of harnessAvailability.installed()) {
    if (selection.activeProjectId !== projectId) break;
    try {
      const agent = await api.createAgent(HARNESS_DEFAULT_AGENT_NAME[harness], harness);
      await registerAgent(agent);
      addAgentToActiveProject(agent);
    } catch (err) {
      // Don't strand a banner on a project the user already left.
      if (selection.activeProjectId !== projectId) break;
      agentCreationFailures.push({
        harness,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }
}

/// Remove an agent everywhere. The backend tears down its actor (cancelling any
/// in-flight turn), drops its registry record, and deletes Switchboard's
/// per-agent sidecars; on success we drop it from whichever project roster holds
/// it and unregister its live per-agent state (event listener, transcript,
/// runtime). The agent is located across all rosters rather than assumed to be
/// in the active project, mirroring the backend's agent-id → own-project
/// resolution. A dangling recipient preselect needs no cleanup — `ComposeBar`
/// filters its selection against the live roster. Errors propagate to the caller
/// (the menu surfaces them and keeps the agent).
export async function removeAgent(agentId: AgentId): Promise<void> {
  await api.removeAgent(agentId);
  for (const [projectId, agents] of Object.entries(agentsByProject)) {
    if (agents.some((a) => a.id === agentId)) {
      agentsByProject[projectId] = agents.filter((a) => a.id !== agentId);
    }
  }
  unregisterAgents([agentId]);
}

/// Rename an agent. The backend re-validates format + uniqueness (the frontend
/// pre-check is UX only) and returns the updated record, which replaces the old
/// one in whichever project roster holds it — located across all rosters rather
/// than assumed to be active, mirroring the backend's agent-id → own-project
/// resolution and matching `removeAgent`. The agent's live per-agent state
/// (`transcripts` / `runtimes`) is keyed by id and carries no name, so nothing
/// else needs updating. Errors propagate to the caller (the inline editor
/// surfaces them and stays in edit mode).
export async function renameAgent(agentId: AgentId, newName: string): Promise<void> {
  const updated = await api.renameAgent(agentId, newName);
  for (const [projectId, agents] of Object.entries(agentsByProject)) {
    if (agents.some((a) => a.id === agentId)) {
      agentsByProject[projectId] = agents.map((a) => (a.id === agentId ? updated : a));
    }
  }
}

/// Rename a project. The backend re-validates format + per-directory uniqueness
/// (the frontend pre-check is UX only) and returns the updated listing, which
/// replaces the matching row in `projects.list` in place. The name renders
/// everywhere from `projects.list` (sidebar row + breadcrumb derive from it), so
/// no other state needs touching. Rename doesn't change `last_activity`, so the
/// list order is preserved. Errors propagate to the caller (the inline editor
/// surfaces them and stays in edit mode).
export async function renameProject(projectId: ProjectId, newName: string): Promise<void> {
  const updated = await api.renameProject(projectId, newName);
  projects.list = projects.list.map((p) => (p.id === projectId ? updated : p));
}

/// Permanently delete one project's Switchboard state. The backend drains its
/// agents and removes its on-disk state (never the working directory or harness
/// session files); on success we perform the matching **frontend lifecycle
/// teardown** for that single project — the same set `removeDirectory` clears,
/// scoped to one id — so a project id reused later (ids persist on disk) starts
/// clean and no listeners/state leak. Errors propagate to the caller (the menu's
/// inline confirm surfaces them and keeps the row).
export async function deleteProject(projectId: ProjectId): Promise<void> {
  // Snapshot the agent ids before the await — the roster is dropped below.
  const removedAgentIds = (agentsByProject[projectId] ?? []).map((a) => a.id);

  await api.deleteProject(projectId);

  unregisterAgents(removedAgentIds);
  projects.list = projects.list.filter((p) => p.id !== projectId);
  delete agentsByProject[projectId];
  delete conversations[projectId];
  delete backgroundCompletedProjectIds[projectId];
  delete projectActivityOverrides[projectId];
  loadStarted.delete(projectId);
  hydrationStarted.delete(projectId);
  previousLiveProjectSendPairs = previousLiveProjectSendPairs.filter(
    (pair) => pair.projectId !== projectId,
  );
  if (selection.activeProjectId === projectId) {
    selection.activeProjectId = null;
    selection.activationError = null;
  }
}

/// Archive or unarchive a project (user-global view-state). The backend flips
/// the flag in `workspace.yaml`; on success we mirror it onto the matching
/// `projects.list` row so the `Active | Archived` filter updates immediately
/// without a relist. Display-only — never touches the project's agents. Errors
/// propagate to the caller (the menu surfaces them and keeps the current state).
export async function setProjectArchived(projectId: ProjectId, archived: boolean): Promise<void> {
  await api.setProjectArchived(projectId, archived);
  projects.list = projects.list.map((p) => (p.id === projectId ? { ...p, archived } : p));
}

/// Dismiss the auto-create failure banner for one harness.
export function dismissAgentCreationFailure(harness: HarnessKind): void {
  const idx = agentCreationFailures.findIndex((f) => f.harness === harness);
  if (idx !== -1) agentCreationFailures.splice(idx, 1);
}

/// Display the given project. The switch is immediate (responsive); the backend
/// work happens behind it. Loads the roster + hydrates the conversation on
/// first activation (once), then issues `set_active_project` — but only after
/// open/list/register succeed, so the backend's active project never points at
/// one that failed to load. On failure, records `activationError` (the center
/// pane shows a retry affordance instead of an endless loading state); the
/// error is cleared here on every (re)activation, so switching away or retrying
/// clears a stale failure.
export async function activateProject(projectId: ProjectId): Promise<void> {
  selection.activeProjectId = projectId;
  selection.activationError = null;
  delete backgroundCompletedProjectIds[projectId];
  // Auto-create failures pertain to a just-created project; switching away (or
  // re-activating) clears the banner. `createProjectAndActivate` seeds *after*
  // this, so its failures survive.
  agentCreationFailures.length = 0;
  try {
    await ensureProjectLoaded(projectId);
    await api.setActiveProject(projectId);
  } catch (err) {
    selection.activationError = err instanceof Error ? err.message : String(err);
  }
}

function ensureProjectLoaded(projectId: ProjectId): Promise<void> {
  const existing = loadStarted.get(projectId);
  if (existing !== undefined) return existing;
  const load = (async () => {
    await api.openProject(projectId);
    const agents = await api.listAgents(projectId);
    agentsByProject[projectId] = agents;
    await Promise.all(agents.map((a) => registerAgent(a)));
    void hydrateProject(projectId);
  })();
  loadStarted.set(projectId, load);
  // Allow a retry if the load (open/lock/roster) failed — a transient failure
  // shouldn't permanently wedge the project as un-activatable.
  load.catch(() => loadStarted.delete(projectId));
  return load;
}

/// Hydrate a project's conversation: split the merged backend shape into the
/// per-project overlay (user messages + outcome markers) and per-agent
/// hydration (agent-turn content regrouped by `agent_id` and fed through the
/// existing per-agent hydrate path). Per-agent `load_error` marks just that
/// agent's hydration failed; the rest of the project still renders. Idempotent
/// + sticky via `hydrationStarted`.
export async function hydrateProject(projectId: ProjectId): Promise<void> {
  if (hydrationStarted.has(projectId)) return;
  hydrationStarted.add(projectId);
  conversations[projectId] = { items: [], status: "loading" };
  try {
    const convo = await api.loadProjectConversation(projectId);

    const overlay: ConversationItem[] = [];
    // Function-local computation scratch — recreated each call, never observed
    // reactively (the reactive sinks are `conversations` and the per-agent
    // `transcripts`/`runtimes`), so a plain Map/Set is correct here.
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const turnsByAgent = new Map<AgentId, LoadedTurn[]>();
    for (const item of convo.items) {
      if (item.kind === "agent_turn") {
        const arr = turnsByAgent.get(item.agent_id) ?? [];
        arr.push({
          role: "agent",
          turn_id: item.turn_id,
          agent_id: item.agent_id,
          send_id: item.send_id ?? null,
          started_at: item.started_at,
          ended_at: item.ended_at ?? null,
          status: item.status,
          items: item.items,
          usage: item.usage ?? null,
        });
        turnsByAgent.set(item.agent_id, arr);
      } else {
        // user_message | outcome — the project-level overlay.
        overlay.push(item);
      }
    }

    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const metaByAgent = new Map(convo.agents.map((m) => [m.agent_id, m]));
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const agentIds = new Set<AgentId>([
      ...turnsByAgent.keys(),
      ...convo.agents.map((a) => a.agent_id),
    ]);
    for (const agentId of agentIds) {
      // Hydrating through `applyAgentHydrate` (or recording the failure) counts
      // as this agent's one hydration for the session — mark it so the
      // per-agent `hydrateAgent` path won't later re-parse and duplicate turns.
      markHydrationAttempted(agentId);
      const meta = metaByAgent.get(agentId);
      if (meta?.load_error != null) {
        // This agent's transcript failed to load — record the error (surfaced
        // in the sidebar, distinct from a failed turn) but keep the rest of the
        // project rendering.
        const rt = runtimes[agentId];
        if (rt !== undefined) {
          runtimes[agentId] = {
            ...rt,
            hydration_status: "failed",
            hydration_error: meta.load_error,
          };
        }
        continue;
      }
      applyAgentHydrate(agentId, {
        turns: turnsByAgent.get(agentId) ?? [],
        meta: meta?.meta ?? null,
        last_rate_limit: meta?.last_rate_limit ?? null,
        last_rate_limit_as_of: meta?.last_rate_limit_as_of ?? null,
        warnings: meta?.warnings,
      });
    }

    conversations[projectId] = { items: overlay, status: "complete" };
  } catch (e) {
    console.warn("[switchboard] hydrateProject failed", { project_id: projectId, error: e });
    conversations[projectId] = { items: [], status: "failed" };
  }
}

/// Append a freshly created/attached agent to the active project's roster so
/// the sidebar and transcript pick it up without a full reload.
export function addAgentToActiveProject(agent: AgentRecord): void {
  const projectId = selection.activeProjectId;
  if (projectId === null) {
    console.error("[switchboard] addAgentToActiveProject with no active project");
    return;
  }
  const existing = agentsByProject[projectId] ?? [];
  agentsByProject[projectId] = [...existing, agent];
}

/// Test-only reset. Production never calls this; the module is a singleton, so
/// tests reset between runs to avoid bleed.
export const _testing = {
  reset(): void {
    workspace.directories = [];
    workspace.persistable = true;
    projects.list = [];
    selection.activeProjectId = null;
    selection.activationError = null;
    previousLiveProjectSendPairs = [];
    loadStarted.clear();
    hydrationStarted.clear();
    for (const key of Object.keys(agentsByProject)) delete agentsByProject[key];
    for (const key of Object.keys(conversations)) delete conversations[key];
    for (const key of Object.keys(backgroundCompletedProjectIds))
      delete backgroundCompletedProjectIds[key];
    for (const key of Object.keys(projectActivityOverrides)) delete projectActivityOverrides[key];
    agentCreationFailures.length = 0;
  },
};
