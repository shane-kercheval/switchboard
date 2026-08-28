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
  ActivationFailure,
  AgentRecord,
  AgentProfile,
  AgentProfileSlot,
  AgentSessionFingerprint,
  ConversationItem,
  HarnessKind,
  LoadedTurn,
  ProjectId,
  ProjectListing,
  SendId,
  SessionFingerprint,
  WorkspaceDirectoryInfo,
} from "$lib/types";
import { tick, untrack } from "svelte";
import { SvelteSet } from "svelte/reactivity";
import { harnessAvailability, settledHarnessAvailability } from "$lib/harnessAvailability.svelte";
import { AUTO_SEED_ON_NEW_PROJECT } from "$lib/harnessDisplay";
import { defaultAgentNameForProfiles } from "$lib/agentSelection";
import { loadPreferences, preferences } from "$lib/preferences.svelte";
import {
  compareIsoTimestampsDescending,
  currentIsoTimestamp,
  isIsoTimestampAfter,
  isIsoTimestampBefore,
} from "$lib/utils";
import { buildLiveSendsMap } from "$lib/state/liveSends";
import { draftAttachmentPaths } from "$lib/state/composeStore";
import { layout } from "$lib/layout.svelte";
import {
  applyAgentHydrate,
  markHydrationAttempted,
  registerAgent,
  runtimes,
  setTurnTerminalHook,
  transcripts,
  unregisterAgents,
} from "./index.svelte";
import {
  subscribeProjectWorkflows,
  unsubscribeProjectWorkflows,
} from "$lib/state/workflows.svelte";
import {
  assignAgentToFirstVisibleEmptyPane,
  createEmptyPane,
  moveAgentToPane,
  revealPane,
} from "$lib/state/transcriptPanes.svelte";

/// Per-project hydrated overlay. `items` holds only `user_message` and
/// `outcome` kinds (agent content is routed to per-agent state); `status`
/// drives a loading indicator on first activation and a project-level error
/// state when the merged-conversation load fails outright.
export type ProjectConversationState = {
  items: ConversationItem[];
  status: "pending" | "loading" | "complete" | "failed";
  /// Verbatim error text when `status === "failed"` (the merged-conversation
  /// load rejected outright). Retained — not just logged — so the transcript
  /// region can surface it with a copyable Details affordance and a Retry.
  /// Absent in every non-failed state.
  error?: string;
};

export type ActivationResult = "activated" | "superseded" | "failed";

/// The registered directories + whether registry changes persist this session.
/// `persistable === false` means an existing `workspace.yaml` couldn't be read
/// at startup — surfaced distinctly from a fresh install.
export const workspace = $state<{ directories: WorkspaceDirectoryInfo[]; persistable: boolean }>({
  directories: [],
  persistable: true,
});

/// The flat cross-directory project list, sorted desc by `last_activity`.
export const projects = $state<{ list: ProjectListing[] }>({ list: [] });

/// Project-scoped deletion lifecycle shared by every deletion surface. Pending
/// is reactive UI state; the private promise registry below provides the
/// single-flight guarantee. Errors persist until dismissed or retried so a
/// failure remains visible even if the user navigates while deletion is in
/// flight.
export const projectDeletions = $state<{
  pending: Record<ProjectId, true>;
  errors: Record<ProjectId, string>;
}>({ pending: {}, errors: {} });
// Promise identity is non-rendering bookkeeping; `projectDeletions.pending`
// is the reactive projection consumed by components.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const projectDeletionPromises = new Map<ProjectId, Promise<void>>();

export function dismissProjectDeletionError(projectId: ProjectId): void {
  delete projectDeletions.errors[projectId];
}

/// The displayed project. Display-only — switching does not stop other
/// projects' agents or tear down their event subscriptions.
///
/// `activationFailure` holds a typed kind plus diagnostic message when opening
/// the displayed project failed (locked by another process, directory went
/// unavailable, removed concurrently). It always pertains to the current
/// `activeProjectId`: cleared on every (re)activation and switch, set only on
/// the current one's failure.
/// The center pane renders a retry affordance instead of an endless loading
/// state when it's set.
///
/// `loadingProjectId` is set during a project switch so the sidebar/header can
/// paint the new selection before a large transcript is derived and rendered.
export const selection = $state<{
  activeProjectId: ProjectId | null;
  activationFailure: ActivationFailure | null;
  loadingProjectId: ProjectId | null;
}>({ activeProjectId: null, activationFailure: null, loadingProjectId: null });

/// Per-project agent rosters, populated lazily on first activation.
export const agentsByProject = $state<Record<ProjectId, AgentRecord[]>>({});

/// Harnesses whose agent failed to auto-create on the just-created project,
/// with the reason. Surfaced as a dismissible banner so a partial failure (the
/// project opens, but one expected agent is missing) is visible rather than
/// silent. Transient and event-scoped: cleared on every project (re)activation,
/// repopulated only by `createProjectAndActivate`.
export const agentCreationFailures = $state<{ harness: HarnessKind; error: string }[]>([]);

/// Set when auto-create had to seed before the login-shell PATH resolved, so the
/// agent set may be incomplete. Surfaced as a dismissible banner rather than
/// logged: a project quietly missing an agent is worse than one visibly missing
/// it, because the user has no way to know which agent should have been there.
/// Same lifecycle as `agentCreationFailures` — cleared on every (re)activation.
export const seedPathUnresolved = $state<{ value: boolean }>({ value: false });

/// Dismiss the incomplete-seeding banner.
export function dismissSeedPathUnresolved(): void {
  seedPathUnresolved.value = false;
}

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

/// Per-project session-file fingerprints captured at last hydration — the
/// baseline the staleness-refresh check diffs against to decide whether a
/// refresh-capable agent's file changed (the user continued the session in the
/// harness's own TUI). Non-reactive bookkeeping, like the guards above.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const sessionFingerprintBaseline = new Map<ProjectId, AgentSessionFingerprint[]>();

/// Projects with a staleness refresh in flight. `maybeRefreshProject` clears the
/// hydration guard before re-reading, so it can't rely on that guard for
/// re-entry protection; this self-guard keeps a second refresh from kicking off
/// a redundant concurrent re-read. Defense-in-depth — the sole caller is
/// `seq`-guarded and the keyed merge already makes a concurrent re-read
/// dup-safe — but it keeps the function safe for any future caller.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const refreshInFlight = new Set<ProjectId>();

/// Every `send_id` of a *this-session* send, taken from the **user** turns in
/// the per-agent slices. Used to keep the overlay to not-live-this-session
/// content: a re-read of the journal would otherwise re-surface a
/// `user_message`/`outcome` row for a live send, doubling the row the slice
/// already renders (`buildUnifiedRows` draws user rows from both the slices and
/// the overlay with no cross-source dedup).
///
/// **Must read user turns only.** Project hydration routes historical user
/// content to the *overlay* and only agent turns into slices — and those agent
/// turns carry the journal-joined `send_id` of the *historical* send they
/// answered. So a user turn in a slice can only have come from this-session
/// `dispatchUserTurn`; keying on `role === "user"` is the clean discriminator
/// between live and hydrated-historical sends. Collecting from agent turns too
/// would sweep up historical send ids and delete legitimate overlay prompts on
/// refresh. Empty on first hydrate (no live user turns), so this is a no-op
/// there and load-bearing only on refresh.
function liveSliceSendIds(projectId: ProjectId): Set<SendId> {
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const ids = new Set<SendId>();
  for (const agent of agentsByProject[projectId] ?? []) {
    for (const turn of transcripts[agent.id] ?? []) {
      if (turn.role === "user" && turn.send_id !== undefined) ids.add(turn.send_id);
    }
  }
  return ids;
}

/// Whether a session file changed between two fingerprints. Gated on
/// `(source_path, modified_at, byte_len)` together — a moved file (a harness's
/// candidate selection), a touched mtime, or an appended byte length each count
/// as changed; absence on one side but not the other (file appeared/vanished) is
/// also a change.
function fingerprintChanged(
  a: SessionFingerprint | null | undefined,
  b: SessionFingerprint | null | undefined,
): boolean {
  if (a == null && b == null) return false;
  if (a == null || b == null) return true;
  return (
    a.source_path !== b.source_path || a.modified_at !== b.modified_at || a.byte_len !== b.byte_len
  );
}

function sortByActivity(list: ProjectListing[]): ProjectListing[] {
  return [...list].sort((a, b) => compareIsoTimestampsDescending(a.last_activity, b.last_activity));
}

function applyActivityOverrides(list: ProjectListing[]): ProjectListing[] {
  return sortByActivity(
    list.map((project) => {
      const override = projectActivityOverrides[project.id];
      return override !== undefined && isIsoTimestampAfter(override, project.last_activity)
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

export function nextUnreadCompletedProjectId(): ProjectId | null {
  const activeId = selection.activeProjectId;
  const unread = projects.list.filter(
    (project) => project.id !== activeId && project.id in backgroundCompletedProjectIds,
  );
  if (unread.length === 0) return null;
  return unread.reduce((oldest, project) =>
    isIsoTimestampBefore(project.last_activity, oldest.last_activity) ? project : oldest,
  ).id;
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
let activationSeq = 0;

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

async function afterNextPaint(): Promise<void> {
  await tick();
  if (typeof requestAnimationFrame !== "function") {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    return;
  }
  await new Promise<void>((resolve) => {
    requestAnimationFrame(() => {
      setTimeout(resolve, 0);
    });
  });
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

/// Hide a working directory. **"Remove" no longer deletes anything.**
///
/// A directory's catalog entry is referenced by every project in it and cannot
/// be dropped while any of them exists, so removal means "stop showing me this":
/// the entry survives, its projects survive, and adding the directory back
/// unhides it. The backend still drains the projects' in-flight turns.
///
/// The **frontend lifecycle teardown** below is unchanged and still required —
/// hide-then-re-add lands on the same project ids, so without it the stale
/// memoized `loadStarted` promise would make re-activation skip
/// `open_project`/`list_agents` and leave the backend with an unloaded "active"
/// project, and the hidden agents' listeners would leak.
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
  unsubscribeProjectWorkflows(removedProjectIds);
  for (const id of removedProjectIds) {
    delete agentsByProject[id];
    delete conversations[id];
    delete backgroundCompletedProjectIds[id];
    delete projectActivityOverrides[id];
    loadStarted.delete(id);
    hydrationStarted.delete(id);
    sessionFingerprintBaseline.delete(id);
    refreshInFlight.delete(id);
  }
  previousLiveProjectSendPairs = previousLiveProjectSendPairs.filter(
    (pair) => !removedProjectIds.includes(pair.projectId),
  );
  if (activeRemoved) {
    selection.activeProjectId = null;
    selection.activationFailure = null;
    selection.loadingProjectId = null;
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
  const activation = await activateProject(summary.id);
  if (activation !== "activated") return;
  await seedAgentsForInstalledHarnesses(summary.id);
}

/// Auto-populate a freshly created project with one agent per installed harness
/// that opts into auto-seeding (`AUTO_SEED_ON_NEW_PROJECT`); excluded harnesses
/// stay dialog-only. New projects only — called solely from
/// `createProjectAndActivate`, never on activation of an existing project.
///
/// Awaits a *settled* availability probe before reading `installed()`. Two races
/// converge here: the store's startup probe is fired un-awaited and reports `[]`
/// until it resolves, and the PATH those probes search is itself resolved
/// asynchronously — a probe answered from the interim PATH can miss a CLI
/// entirely. Seeding is one-shot, so either race silently produces a project
/// with fewer agents than the user has CLIs, permanently.
///
/// Mirrors the canonical create path (`createAgent` → `registerAgent` →
/// `addAgentToProjectRoster`) per agent — a plain roster re-fetch would skip
/// `registerAgent` and leave the agents without live transcript/dispatch state.
/// Each create is independent: one failure is recorded in `agentCreationFailures`
/// (surfaced as a dismissible banner) and never aborts the rest or the open.
///
/// **Targets a captured project, not live active state.** Both `create_agent`
/// (backend active project) binds to whatever is active *at call time*. If the
/// user navigated to another project mid-seed, continuing could create agents
/// in the wrong project — so we capture the id up front and bail if it changes.
/// Frontend roster insertion uses the returned agent's authoritative project id.
/// The new-project dialog also stays non-dismissible while this runs (belt).
/// The durable backend fix is `create_agent`/`attach_agent` taking an explicit
/// `project_id` instead of reading active state — out of scope here, but the
/// same coupling affects project remove/rename.
async function seedAgentsForInstalledHarnesses(projectId: ProjectId): Promise<void> {
  if (selection.activeProjectId !== projectId) return;
  await loadPreferences();
  if (selection.activeProjectId !== projectId) return;
  // A `capturing` result means the wait expired with the PATH still unresolved,
  // so `installed()` may be short. Seeding proceeds anyway — a wedged
  // non-dismissible dialog is worse than a missing agent — but the user is told.
  if ((await settledHarnessAvailability()) === "capturing") {
    seedPathUnresolved.value = selection.activeProjectId === projectId;
  }
  for (const harness of harnessAvailability.installed()) {
    if (selection.activeProjectId !== projectId) break;
    // Every harness is auto-seeded today, so this guard takes no branch; it is
    // retained as the extension point for a harness that shouldn't be born into
    // a fresh project (see the note above `SUPPORTS_MODEL_SELECTION` in
    // `harnessDisplay.ts`). Such a harness stays selectable in the create-agent
    // dialog — it is just not seeded automatically.
    if (!AUTO_SEED_ON_NEW_PROJECT[harness]) continue;
    try {
      // Every auto-created agent is born with a known, displayed model/effort
      // (`undefined` for a no-capability harness → backend stores `None`).
      const defaults = preferences.agent_defaults[harness];
      const model = defaults.primary.model ?? undefined;
      const effort = defaults.primary.effort ?? undefined;
      const agent = await api.createAgent(
        defaultAgentNameForProfiles(harness, defaults.primary, defaults.secondary),
        harness,
        model,
        effort,
        defaults.secondary,
      );
      if (selection.activeProjectId !== projectId) break;
      await registerAgent(agent);
      if (selection.activeProjectId !== projectId) break;
      addAgentToProjectRoster(agent);
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
  replaceAgentRecord(agentId, updated);
}

export async function setAgentProfiles(
  agentId: AgentId,
  primary: AgentProfile,
  secondary: AgentProfile | null,
): Promise<void> {
  const updated = await api.setAgentProfiles(agentId, primary, secondary);
  replaceAgentRecord(agentId, updated);
}

export async function setActiveAgentProfile(
  agentId: AgentId,
  active: AgentProfileSlot,
): Promise<void> {
  const updated = await api.setActiveAgentProfile(agentId, active);
  replaceAgentRecord(agentId, updated);
}

/// One reorder per project in-flight at a time. A concurrent call (e.g. from
/// key autorepeat while a write is still pending) is dropped; the accepted call
/// keeps the previous-state snapshot clean so a failure rollback always restores
/// a backend-consistent order. Dropping is correct here: the last accepted move
/// is the intended order, and the dropped tick just means the animation runs
/// slightly slower than the key repeat rate — imperceptible.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const reorderInFlight = new Set<ProjectId>();

/// Reorder a project's roster. Roster order is the canonical display order
/// everywhere it appears (sidebar cards, compose chips and their ⌘1..9
/// numbering, pane columns and member chips), so the new order is applied
/// optimistically for immediate feedback across all of them, then reconciled
/// with the backend-persisted records. On failure the previous order is
/// restored and the error propagates to the caller (the sidebar surfaces it).
/// Concurrent calls for the same project are dropped (see `reorderInFlight`).
export async function reorderAgents(projectId: ProjectId, orderedIds: AgentId[]): Promise<void> {
  if (reorderInFlight.has(projectId)) return;
  const previous = agentsByProject[projectId];
  if (!previous) return;
  reorderInFlight.add(projectId);
  // Transient lookup, never stored or observed — reactivity not needed.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const byId = new Map(previous.map((a) => [a.id, a]));
  // Mirror the backend's permutation check (take-and-remove: a duplicate id
  // misses on its second lookup), so an exact permutation is the only input
  // that repaints the roster — anything else skips the optimistic update and
  // goes straight to the backend's authoritative rejection, with no transient
  // duplicate cards rendered in between.
  const optimistic: AgentRecord[] = [];
  for (const id of orderedIds) {
    const record = byId.get(id);
    if (record === undefined) break;
    byId.delete(id);
    optimistic.push(record);
  }
  if (optimistic.length === previous.length && orderedIds.length === previous.length) {
    agentsByProject[projectId] = optimistic;
  }
  try {
    agentsByProject[projectId] = await api.reorderAgents(projectId, orderedIds);
  } catch (e) {
    agentsByProject[projectId] = previous;
    throw e;
  } finally {
    reorderInFlight.delete(projectId);
  }
}

/// Replace an agent's record across whichever project roster holds it — located
/// across all rosters rather than assumed active, matching `renameAgent`.
function replaceAgentRecord(agentId: AgentId, updated: AgentRecord): void {
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
/// teardown** for that single project and remove its persisted layout
/// preferences. Reversible directory removal keeps those preferences, but a
/// permanently deleted project id must start clean if it is ever reused. Errors
/// propagate to the caller (the menu's inline confirm surfaces them and keeps
/// the row).
export function deleteProject(projectId: ProjectId): Promise<void> {
  const existing = projectDeletionPromises.get(projectId);
  if (existing !== undefined) return existing;

  delete projectDeletions.errors[projectId];
  projectDeletions.pending[projectId] = true;
  const operation = deleteProjectOnce(projectId)
    .catch((error: unknown) => {
      projectDeletions.errors[projectId] = error instanceof Error ? error.message : String(error);
      throw error;
    })
    .finally(() => {
      projectDeletionPromises.delete(projectId);
      delete projectDeletions.pending[projectId];
    });
  projectDeletionPromises.set(projectId, operation);
  return operation;
}

async function deleteProjectOnce(projectId: ProjectId): Promise<void> {
  // Snapshot the agent ids before the await — the roster is dropped below.
  const removedAgentIds = (agentsByProject[projectId] ?? []).map((a) => a.id);

  await api.deleteProject(projectId);

  layout.removeProjectPreferences(projectId);
  unregisterAgents(removedAgentIds);
  unsubscribeProjectWorkflows([projectId]);
  projects.list = projects.list.filter((p) => p.id !== projectId);
  delete agentsByProject[projectId];
  delete conversations[projectId];
  delete backgroundCompletedProjectIds[projectId];
  delete projectActivityOverrides[projectId];
  loadStarted.delete(projectId);
  hydrationStarted.delete(projectId);
  sessionFingerprintBaseline.delete(projectId);
  refreshInFlight.delete(projectId);
  previousLiveProjectSendPairs = previousLiveProjectSendPairs.filter(
    (pair) => pair.projectId !== projectId,
  );
  if (selection.activeProjectId === projectId) {
    selection.activeProjectId = null;
    selection.activationFailure = null;
    selection.loadingProjectId = null;
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
/// one that failed to load. On failure, records `activationFailure` (the center
/// pane shows a retry affordance instead of an endless loading state); the
/// error is cleared here on every (re)activation, so switching away or retrying
/// clears a stale failure.
export async function activateProject(projectId: ProjectId): Promise<ActivationResult> {
  const seq = ++activationSeq;
  selection.activeProjectId = projectId;
  selection.activationFailure = null;
  selection.loadingProjectId = projectId;
  delete backgroundCompletedProjectIds[projectId];
  // Auto-create failures pertain to a just-created project; switching away (or
  // re-activating) clears the banner. `createProjectAndActivate` seeds *after*
  // this, so its failures survive.
  agentCreationFailures.length = 0;
  seedPathUnresolved.value = false;
  // A re-activation is a switch back to a project whose load already ran — the
  // only time a staleness refresh applies (first activation hydrates fresh).
  const isReactivation = loadStarted.has(projectId);
  await afterNextPaint();
  if (seq !== activationSeq || selection.activeProjectId !== projectId) return "superseded";
  try {
    await ensureProjectLoaded(projectId);
    if (seq !== activationSeq || selection.activeProjectId !== projectId) return "superseded";
    await api.setActiveProject(projectId);
    if (seq !== activationSeq || selection.activeProjectId !== projectId) return "superseded";
    selection.loadingProjectId = null;
    // Pick up TUI-continued turns on switch-back. Inside the `seq` guard so a
    // superseded activation can't kick off a refresh for a project the user has
    // already navigated away from. Awaited so the refreshed turns are applied
    // before the activation resolves (tests and callers can rely on it).
    if (isReactivation) {
      await maybeRefreshProject(projectId);
      if (seq !== activationSeq || selection.activeProjectId !== projectId) return "superseded";
    }
    return "activated";
  } catch (err) {
    if (seq !== activationSeq || selection.activeProjectId !== projectId) return "superseded";
    selection.activationFailure =
      err instanceof api.ActivationFailureError
        ? { type: err.type, message: err.message }
        : { type: "other", message: err instanceof Error ? err.message : String(err) };
    selection.loadingProjectId = null;
    return "failed";
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
    // Subscribe to the project's workflow progress channel (not active-gated, so
    // a run in a background project keeps updating). Idempotent; seeds run state.
    void subscribeProjectWorkflows(projectId);
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
///
/// **`agentTurnFilter`** scopes which agents' *agent turns* are (re-)applied —
/// supplied only on a staleness **refresh** (`maybeRefreshProject`), set to the
/// refresh-capable agents. The whole project is re-read (the journal-join that
/// classifies imported-vs-journaled user content needs all agents), but agent
/// turns are merged only for refresh-capable agents: a non-refresh-capable agent
/// that ran a turn in Switchboard this session already advanced its own file,
/// and its live turn's key is `None`, so re-merging its disk copy would
/// *duplicate* the live turn (the live-vs-disk hazard the per-harness gate
/// exists to prevent). On first hydrate the filter is absent → all agents apply
/// (safe: no live turns exist at project open).
/// Outcome of a hydration attempt, for callers that must distinguish "the data
/// arrived" from "nothing changed".
///
/// **`skipped` and `failed` are not interchangeable, and neither is `applied`.**
/// A refresh deliberately preserves the previously loaded conversation when it
/// fails (see below), so `conversations[projectId].status` reads `"complete"`
/// whether the read succeeded or threw — it cannot be used as a success signal
/// by anyone downstream. `skipped` means another hydration held the guard, so
/// this caller's work may still be pending and should be retried later.
export type HydrateOutcome = "applied" | "skipped" | "failed";

export async function hydrateProject(
  projectId: ProjectId,
  agentTurnFilter?: ReadonlySet<AgentId>,
): Promise<HydrateOutcome> {
  if (hydrationStarted.has(projectId)) return "skipped";
  hydrationStarted.add(projectId);
  // A refresh (the only caller passing `agentTurnFilter`) re-reads over a
  // known-good loaded view, so it must be non-destructive: keep the current
  // conversation displayed while re-reading, and on failure leave it (and the
  // baseline) untouched — a best-effort switch-back refresh must never turn a
  // working view into a blank/error one for a transient hiccup. First
  // hydration/retry still show the loading state and surface failures.
  const isRefresh = agentTurnFilter !== undefined;
  if (!isRefresh) conversations[projectId] = { items: [], status: "loading" };
  // Capture the freshness baseline BEFORE the parse, best-effort: a file written
  // between this stat and the parse leaves the baseline behind the parsed state,
  // so the next refresh re-reads (a benign deduped no-op) rather than missing the
  // change. A failed fingerprint fetch just means no refresh baseline (full
  // reload still works) — it must not fail the hydration.
  let baseline: AgentSessionFingerprint[] | undefined;
  try {
    baseline = await api.projectSessionFingerprints(projectId);
  } catch (e) {
    console.warn("[switchboard] projectSessionFingerprints failed", {
      project_id: projectId,
      error: e,
    });
  }
  try {
    // Loading garbage-collects every staged attachment the journal doesn't
    // reference. An unsent draft's chips live in localStorage, which the backend
    // can't see, so declare their paths or the load deletes the files behind
    // chips the composer is still showing.
    const convo = await api.loadProjectConversation(projectId, draftAttachmentPaths(projectId));

    // Sends represented live in the slices this session own their rendering
    // there; drop the journal's copy of them from the overlay to avoid a doubled
    // user/outcome row. No-op on first hydrate; load-bearing on refresh.
    const liveSends = liveSliceSendIds(projectId);

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
          send_correlation: item.send_correlation ?? null,
          started_at: item.started_at,
          ended_at: item.ended_at ?? null,
          status: item.status,
          items: item.items,
          usage: item.usage ?? null,
          // Thread per-turn model/effort through this hand-built remap so the
          // footer's model survives restart — a field not copied here is silently
          // dropped (which is exactly how it went missing before).
          model: item.model ?? null,
          effort: item.effort ?? null,
          spend: item.spend ?? null,
          // Thread the stable hydration key through this hand-built remap — the
          // merge dedups on it, and a field not copied here is silently dropped.
          hydration_key: item.hydration_key ?? null,
          // Likewise the compaction-continuation link — the merge uses it to
          // collapse a continuation into the live resident that spans it.
          continuation_of: item.continuation_of ?? null,
        });
        turnsByAgent.set(item.agent_id, arr);
      } else if (item.kind === "system_marker") {
        // An agent-scoped inter-turn marker (compaction). It has no send and no
        // live counterpart (compaction is reopen-only), so it never needs the
        // live-slice dedup below — pass it straight through to the overlay.
        overlay.push(item);
      } else {
        // user_message | outcome — the project-level overlay. Skip a journaled
        // row (has a `send_id`) whose send is already live in a slice; imported
        // prompts (`send_id` null) have no live counterpart and pass through.
        if (item.send_id != null && liveSends.has(item.send_id)) continue;
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
      // On refresh, leave non-refresh-capable agents' content frozen (see the
      // doc above) — their slice and prior state are untouched.
      if (agentTurnFilter !== undefined && !agentTurnFilter.has(agentId)) continue;
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
      });
    }

    conversations[projectId] = { items: overlay, status: "complete" };
    if (baseline !== undefined) sessionFingerprintBaseline.set(projectId, baseline);
    if (!isRefresh) seedPendingForkHistory(projectId, baseline);
    // A per-agent parse failure means that agent contributed no turns even
    // though the project load succeeded — for a caller waiting on one specific
    // agent's history that is a failure, not an application.
    const filtered = agentTurnFilter;
    if (filtered !== undefined) {
      const failedAgent = convo.agents.some(
        (meta) => filtered.has(meta.agent_id) && meta.load_error != null,
      );
      if (failedAgent) return "failed";
    }
    return "applied";
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    console.warn("[switchboard] hydrateProject failed", { project_id: projectId, error: e });
    // A failed refresh keeps the prior loaded conversation (and its baseline)
    // intact — log only, so the next switch-back retries. A failed first
    // hydration/retry surfaces the error (there is no good view to protect).
    if (!isRefresh) {
      conversations[projectId] = { items: [], status: "failed", error: message };
    }
    return "failed";
  }
}

/// On re-activation of an already-loaded project, re-read its conversation if a
/// **refresh-capable** agent's session file changed since last hydration (the
/// user continued it in the harness's own TUI). The cheap `stat`-only fingerprint
/// check gates the expensive parse: when nothing changed, `loadProjectConversation`
/// is never called. The re-read merges agent turns only for refresh-capable
/// agents and is dup-safe via the stable key (see `hydrateProject`). A failed
/// fingerprint check degrades to "no refresh" (the displayed history just isn't
/// updated until the next switch).
async function maybeRefreshProject(projectId: ProjectId): Promise<void> {
  const baseline = sessionFingerprintBaseline.get(projectId);
  if (baseline === undefined) return; // not yet hydrated → nothing to refresh
  if (refreshInFlight.has(projectId)) return; // a refresh is already running
  refreshInFlight.add(projectId);
  try {
    let current: AgentSessionFingerprint[];
    try {
      current = await api.projectSessionFingerprints(projectId);
    } catch (e) {
      console.warn("[switchboard] refresh freshness check failed", {
        project_id: projectId,
        error: e,
      });
      return;
    }
    const baseByAgent = new Map(baseline.map((f) => [f.agent_id, f]));
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const refreshCapable = new Set<AgentId>();
    let anyStale = false;
    for (const f of current) {
      if (!f.refresh_capable) continue;
      refreshCapable.add(f.agent_id);
      if (fingerprintChanged(baseByAgent.get(f.agent_id)?.fingerprint, f.fingerprint)) {
        anyStale = true;
      }
    }
    // Unchanged → do NOT re-read (the parse path stays uncalled).
    if (!anyStale) return;
    hydrationStarted.delete(projectId);
    await hydrateProject(projectId, refreshCapable);
  } finally {
    refreshInFlight.delete(projectId);
  }
}

/// Re-attempt a project's conversation hydration after an outright load
/// failure. Clears the sticky `hydrationStarted` guard so `hydrateProject`
/// re-runs (the `loadStarted` open/roster guard is untouched — open succeeded;
/// only the conversation merge failed). A failed load applied nothing, so the
/// re-attempt cannot duplicate content.
export async function retryProjectHydration(projectId: ProjectId): Promise<void> {
  // Re-entrancy guard, mirroring `retryAgentHydration`. `hydrateProject` also
  // feeds each agent's turns through the per-agent append-merge
  // (`applyAgentHydrate`), so two concurrent project retries duplicate agent
  // turns just like the per-agent path — not only the `conversations` overlay
  // (which is last-write-wins and would be fine). `hydrateProject` sets status
  // `"loading"` synchronously before its await, so a racing retry sees it here.
  if (conversations[projectId]?.status === "loading") return;
  hydrationStarted.delete(projectId);
  await hydrateProject(projectId);
}

/// Append a freshly created/attached agent to its owning project's roster so
/// an async completion cannot place it in whichever project is active later.
export function addAgentToProjectRoster(agent: AgentRecord): void {
  const existing = agentsByProject[agent.project_id] ?? [];
  agentsByProject[agent.project_id] = [...existing, agent];
}

/// Branch `sourceId`'s conversation into a new agent and make it the live one.
///
/// Registration + placement only — the caller sends the branch's first message
/// immediately after, and *that* send is what materializes it as a harness
/// session (Claude cannot copy a session; a branch only exists as a turn). So
/// this must not be called speculatively: a fork with no send is an agent whose
/// session never comes into being until someone messages it.
///
/// Ordering is load-bearing and mirrors the create/attach path:
/// `registerAgent` first (it initializes the runtime *before* subscribing, so an
/// event arriving immediately after the command resolves finds somewhere to
/// land), then roster, then placement.
///
/// **Placement:** the branch gets its own visible track. Never the parent's
/// pane — they share history with identical timestamps, so co-paning renders
/// every inherited message twice. Prefer a visible empty pane the user already
/// has; otherwise create one while narrowing an untouched all-agent pane to the
/// source the user actually forked. The other idle roster agents become
/// unassigned instead of unexpectedly remaining recipients in the original
/// pane. A new pane may start *minimized* when the row is full — hence
/// `revealPane`, which also handles focus mode by making the branch the
/// maximized pane rather than dropping the user out of focus. A fork-send is an
/// explicit action with an immediate result: leaving the reply in a pane the
/// user cannot see (with compose now addressed at that unseen agent) reads as
/// "my message vanished." The parent is left exactly where it is; only the
/// compose selection moves.
export async function forkAgentIntoOwnPane(sourceId: AgentId): Promise<AgentRecord> {
  const fork = await api.forkAgent(sourceId);
  await registerAgent(fork);
  addAgentToProjectRoster(fork);
  const rosterIds = (agentsByProject[fork.project_id] ?? []).map((item) => item.id);
  let paneId = assignAgentToFirstVisibleEmptyPane(fork.project_id, rosterIds, fork.id);
  if (paneId === null) {
    paneId = createEmptyPane(fork.project_id, rosterIds, [sourceId]);
    moveAgentToPane(fork.project_id, rosterIds, fork.id, paneId);
  }
  revealPane(fork.project_id, rosterIds, paneId);
  // The branch does not exist yet — the send that follows this call is what
  // materializes it — so the transcript cue arms here, before the first turn.
  forkHistoryPending.add(fork.id);
  return fork;
}

/// A registered branch, and whether its event channel is live.
///
/// `unsubscribed` is **committed but unreachable**: the branch is durable and on
/// screen, but subscribing to its channel failed, so a turn dispatched into it
/// would spend real work on events that never arrive — and Tauri has no replay,
/// so subscribing later cannot recover them.
export type ReachableFork =
  | { kind: "ready"; fork: AgentRecord }
  | { kind: "unsubscribed"; fork: AgentRecord; message: string };

/// Register a branch of `sourceId` and classify whether it can be sent to.
///
/// Deliberately does **not** touch recipient selection, dispatch, or compose
/// state. Each caller finalizes its own composer on its own rules — plain mode
/// clears before this call and hands the text back on failure, prompt mode holds
/// everything until the send has dispatched and then retires it only if the
/// composer still matches what it captured — and folding those into a shared
/// helper is how an obsolete instance ends up retargeting a live one.
export async function createReachableFork(sourceId: AgentId): Promise<ReachableFork> {
  const fork = await forkAgentIntoOwnPane(sourceId);
  if (runtimes[fork.id]?.listener_error != null) {
    return {
      kind: "unsubscribed",
      fork,
      message: `${fork.name} was created, but Switchboard couldn't connect to its updates — your message wasn't sent. Retry from the banner above, then send again.`,
    };
  }
  return { kind: "ready", fork };
}

/// Forks whose inherited history has already been loaded. A fork's branch point
/// only materializes when its first turn runs, so its transcript shows just that
/// turn until the session file is re-read.
///
/// **Exactly one *successful* load per fork**, hence a set of the ones that
/// succeeded rather than a set of the ones attempted.
// Bookkeeping only — never read during render, so it needs no reactivity.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const forkHistoryLoaded = new Set<AgentId>();

/// Forks materialized **this session** whose inherited history hasn't arrived
/// yet — the transcript's "this is a branch, its earlier conversation is still
/// coming" cue. Reactive because it is read during render.
///
/// **Deliberately not the same set as [`forkHistoryLoaded`].** The two answer
/// different questions. The refresh's one-shot guard tracks whether the history
/// has been pulled; this tracks whether the *user needs telling* that it hasn't.
/// Armed at fork creation, and re-armed on project open for branches that never
/// materialized (see [`seedPendingForkHistory`]) — a branch whose first turn
/// failed before a restart is still waiting, and the cue would otherwise vanish
/// in exactly the case it was widened to cover.
const forkHistoryPending = new SvelteSet<AgentId>();

/// Per-project in-flight fork-history read, so concurrent branch terminals
/// serialize rather than firing overlapping project reads. Bookkeeping only —
/// never read during render.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const forkHistoryLoadInFlight = new Map<ProjectId, Promise<HydrateOutcome>>();

/// Whether `agentId` is a fork still waiting for its inherited history — read by
/// the transcript to decide whether to explain the branch's empty backlog.
export function isAwaitingForkHistory(agentId: AgentId): boolean {
  return forkHistoryPending.has(agentId);
}

/// Re-arm the branch cue for forks that never materialized, on project open.
///
/// A branch created in an earlier session whose first turn failed has no session
/// file and therefore no inherited history — the cue is exactly as true as it was
/// before the restart, and the next successful turn is what resolves it. A branch
/// that *did* materialize needs no cue: the ordinary load already brought its
/// history in.
///
/// **The signal is the session file, not the transcript.** "Fork provenance plus
/// an empty per-agent slice" looks equivalent and is not: a real session can load
/// with no agent turns (cancelled after the file was created, a parent holding
/// only a dangling prompt), which would pin a permanent "history is coming" cue
/// on a branch that already has everything. The fingerprint distinguishes them —
/// for a refresh-capable harness an absent fingerprint means the file does not
/// exist. When fingerprints are unavailable, seed nothing: a missing cue is
/// cosmetic, a false permanent one is not.
function seedPendingForkHistory(
  projectId: ProjectId,
  fingerprints: AgentSessionFingerprint[] | undefined,
): void {
  // `== null` deliberately: the probe is best-effort and its failure path can
  // yield either absence or an explicit null. Seeding must never be able to
  // break the hydration it rides along with.
  if (fingerprints == null) return;
  const byAgent = new Map(fingerprints.map((f) => [f.agent_id, f]));
  for (const agent of agentsByProject[projectId] ?? []) {
    if (agent.forked_from_session == null) continue;
    if (forkHistoryLoaded.has(agent.id)) continue;
    const fingerprint = byAgent.get(agent.id);
    if (fingerprint === undefined || !fingerprint.refresh_capable) continue;
    if (fingerprint.fingerprint == null) forkHistoryPending.add(agent.id);
  }
}

/// Load a freshly materialized fork's inherited history.
///
/// **Trigger is the outcome, not the file.** The frontend cannot stat a session
/// file, so "did this turn materialize the branch?" is approximated by
/// `Completed`, and cancelled/failed **re-arm** — a fork whose first turn was
/// cancelled may still have a complete child file (harness-behavior §3.5), so it
/// picks its history up on the next completed turn rather than never. The two
/// alternatives are worse: spending the one shot on a failed turn leaves
/// inherited history invisible until reopen, and a file-existence IPC invents a
/// backend surface for a cosmetic gain.
///
/// **Goes through the project conversation merge**, not `hydrateAgent` /
/// `retryAgentHydration`. The per-agent loader returns raw user turns, and the
/// hydrate reducer's keyed dedup covers *agent* turns only — user turns are
/// journal-overlay-owned. A per-agent reload would therefore duplicate both the
/// inherited prompts and the fork's own live prompt. `hydrateProject` replaces
/// the overlay wholesale (dup-safe) and applies agent turns for the filtered
/// agent, where the live first reply collapses against its disk copy by
/// `hydration_key`.
async function loadForkInheritedHistory(agentId: AgentId): Promise<void> {
  const agent = Object.values(agentsByProject)
    .flat()
    .find((candidate) => candidate.id === agentId);
  if (agent?.forked_from_session == null) return;
  if (forkHistoryLoaded.has(agentId)) return;
  const projectId = agent.project_id;
  // Only meaningful once the project's own hydration has settled — before that
  // the open path will read this session anyway.
  if (conversations[projectId]?.status !== "complete") return;

  // **Serialize, don't drop.** Two branches whose first turns land together
  // would otherwise both clear `hydrationStarted` and both read: the delete
  // below and `hydrateProject`'s guard check are in the same synchronous slice,
  // so neither sees the other's guard. Coalescing instead — losers returning
  // early — is *wrong here* even though it is right for `maybeRefreshProject`:
  // that one retries on the next project switch, which happens constantly,
  // whereas this retries on the next completed turn of this specific fork,
  // which may never come. A dropped load leaves that branch's inherited history
  // permanently missing behind a banner promising it. So the loser waits and
  // then runs its own read: sequential rather than concurrent, one extra read,
  // nothing lost. (Its `agentTurnFilter` names a different agent, so the
  // winner's read cannot cover it.)
  const inFlight = forkHistoryLoadInFlight.get(projectId);
  if (inFlight !== undefined) await inFlight.catch(() => undefined);

  hydrationStarted.delete(projectId);
  const load = hydrateProject(projectId, new Set([agentId]));
  forkHistoryLoadInFlight.set(projectId, load);
  let outcome: HydrateOutcome;
  try {
    outcome = await load;
  } finally {
    if (forkHistoryLoadInFlight.get(projectId) === load) {
      forkHistoryLoadInFlight.delete(projectId);
    }
  }
  // Only an `applied` read actually produced inherited history. Anything else
  // leaves the one-shot unset so the next completed turn retries. Reading
  // `conversations[projectId].status` here instead would report success for a
  // failed read too — a refresh preserves the prior `"complete"` view when it
  // throws, which is the whole reason this outcome exists.
  if (outcome === "applied") {
    forkHistoryLoaded.add(agentId);
    forkHistoryPending.delete(agentId);
  }
}

/// Wire the terminal hook that drives [`loadForkInheritedHistory`]. Called once
/// at app start; the hook is a no-op for every non-fork agent.
export function installForkHistoryRefresh(): void {
  setTurnTerminalHook((agentId, outcome) => {
    if (outcome !== "completed") return;
    void loadForkInheritedHistory(agentId);
  });
}

/// Test-only reset. Production never calls this; the module is a singleton, so
/// tests reset between runs to avoid bleed.
export const _testing = {
  reset(): void {
    forkHistoryLoaded.clear();
    forkHistoryPending.clear();
    setTurnTerminalHook(undefined);
    workspace.directories = [];
    workspace.persistable = true;
    projects.list = [];
    for (const key of Object.keys(projectDeletions.pending)) delete projectDeletions.pending[key];
    for (const key of Object.keys(projectDeletions.errors)) delete projectDeletions.errors[key];
    projectDeletionPromises.clear();
    selection.activeProjectId = null;
    selection.activationFailure = null;
    selection.loadingProjectId = null;
    previousLiveProjectSendPairs = [];
    activationSeq = 0;
    loadStarted.clear();
    hydrationStarted.clear();
    sessionFingerprintBaseline.clear();
    refreshInFlight.clear();
    for (const key of Object.keys(agentsByProject)) delete agentsByProject[key];
    for (const key of Object.keys(conversations)) delete conversations[key];
    for (const key of Object.keys(backgroundCompletedProjectIds))
      delete backgroundCompletedProjectIds[key];
    for (const key of Object.keys(projectActivityOverrides)) delete projectActivityOverrides[key];
    agentCreationFailures.length = 0;
    seedPathUnresolved.value = false;
  },
};
