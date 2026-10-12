/// Live workflow-run state for the run indicator. Subscribes to the
/// project-scoped `workflow:<project-id>` channel **once per loaded project**
/// (not gated to the active project, so a run in a background project keeps
/// updating), seeded on subscribe with `list_workflow_runs` for crash-surfacing.
/// Torn down with the project, mirroring the per-agent listener lifecycle.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as api from "$lib/api";
import type { ProjectId, WorkflowProgressPayload, WorkflowRunInfo } from "$lib/types";
import { recordWorkflowTerminal } from "./sendCompletion";

/// Runs per project, keyed by project id — the source for the run indicator.
export const workflowRuns = $state<Record<ProjectId, WorkflowRunInfo[]>>({});

/// Per-project unlisten handles. Plain `Map` (not reactive) — handles, not state.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const subscriptions = new Map<ProjectId, UnlistenFn>();
/// In-flight subscribe promises, so two overlapping `subscribe` calls for one
/// project don't double-subscribe (same guard as `registerAgent`).
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const pendingSubs = new Map<ProjectId, Promise<void>>();
/// Per-project subscription generation, bumped on every subscribe and on
/// unsubscribe. A subscribe captures its generation and, after each await
/// (`listen`, then the seed query), aborts if the generation has moved — so an
/// unsubscribe (or re-subscribe) that lands while a subscribe is in flight can't
/// install a leaked listener or commit a stale seed for a torn-down project.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const generation = new Map<ProjectId, number>();

/// Every run this session has seen end, per project, with how it ended.
///
/// **A fetched run list can be older than an end the window already saw.**
/// The backend announces a run's end before it drops the run from its live
/// registry, and the list reports every live run as running, so a list read
/// just after the end (the refresh a dismissal triggers, say) still says
/// running, and no event is left to correct it. Every fetched list is
/// therefore corrected by this: a completed or cancelled run is dropped, a
/// failed one keeps its failure. A run the list no longer has stays gone.
// Bookkeeping only — never read during render.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const ended = new Map<ProjectId, Map<string, WorkflowProgressPayload>>();

/// The run-list fetch in flight per project: the progress that arrived during
/// it, replayed over its answer, and whether a caller arrived during it and so
/// needs one more fetch after it.
type RunFetch = { done: Promise<void>; events: WorkflowProgressPayload[]; again: boolean };
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const fetches = new Map<ProjectId, RunFetch>();

/// Subscribe to a project's workflow progress channel (idempotent). Installs the
/// listener **first**, then seeds the current runs — so a run that terminalizes
/// during project load isn't missed (the listener is already live). The
/// generation guard discards the listener and the seed if the project is torn
/// down while either await is in flight.
export async function subscribeProjectWorkflows(projectId: ProjectId): Promise<void> {
  // The in-flight subscribe first: the listener is installed before the seed
  // lands, and a caller about to start a run must wait for the seed too.
  const pending = pendingSubs.get(projectId);
  if (pending !== undefined) return pending;
  if (subscriptions.has(projectId)) return;

  const gen = (generation.get(projectId) ?? 0) + 1;
  generation.set(projectId, gen);
  const promise = (async () => {
    try {
      const channel = `workflow:${projectId}`;
      const unlisten = await listen<WorkflowProgressPayload>(channel, (event) => {
        handleProgress(projectId, event.payload);
      });
      // Torn down (or re-subscribed) while `listen` was in flight — don't install.
      if (generation.get(projectId) !== gen) {
        unlisten();
        return;
      }
      subscriptions.set(projectId, unlisten);
      await refreshRuns(projectId);
    } finally {
      pendingSubs.delete(projectId);
    }
  })();
  pendingSubs.set(projectId, promise);
  return promise;
}

/// Tear down workflow subscriptions for the given projects (called alongside the
/// per-agent teardown when a directory/project is removed). Bumps the generation
/// so any in-flight subscribe aborts instead of resurrecting the project.
export function unsubscribeProjectWorkflows(projectIds: ProjectId[]): void {
  for (const projectId of projectIds) {
    generation.set(projectId, (generation.get(projectId) ?? 0) + 1);
    const unlisten = subscriptions.get(projectId);
    if (unlisten !== undefined) {
      unlisten();
      subscriptions.delete(projectId);
    }
    pendingSubs.delete(projectId);
    ended.delete(projectId);
    fetches.delete(projectId);
    delete workflowRuns[projectId];
  }
}

/// Re-query the authoritative 3-class run list for a project, corrected by
/// what the window already knows: the ends it has seen, and the progress that
/// arrived while the query ran. Without that, the answer, which is read before
/// it lands, could put back a run that ended meanwhile. One query per project
/// at a time; a caller arriving during one gets one more after it. A failed
/// query is logged and leaves the list as it was.
export function refreshRuns(projectId: ProjectId): Promise<void> {
  const inFlight = fetches.get(projectId);
  if (inFlight !== undefined) {
    inFlight.again = true;
    return inFlight.done;
  }
  const fetch: RunFetch = { done: Promise.resolve(), events: [], again: false };
  fetch.done = (async () => {
    try {
      for (;;) {
        fetch.again = false;
        fetch.events = [];
        const gen = generation.get(projectId);
        let listed: WorkflowRunInfo[];
        try {
          listed = await api.listWorkflowRuns(projectId);
        } catch (err) {
          console.warn("[switchboard] listWorkflowRuns failed", err);
          return;
        }
        // Torn down meanwhile: commit nothing for a project that is gone.
        if (generation.get(projectId) !== gen) return;
        workflowRuns[projectId] = correctRuns(projectId, listed, fetch.events);
        if (!fetch.again) return;
      }
    } finally {
      if (fetches.get(projectId) === fetch) fetches.delete(projectId);
    }
  })();
  fetches.set(projectId, fetch);
  return fetch.done;
}

/// A fetched list made current: ends the window has seen applied, then the
/// progress that arrived during the fetch replayed in order.
function correctRuns(
  projectId: ProjectId,
  listed: WorkflowRunInfo[],
  during: WorkflowProgressPayload[],
): WorkflowRunInfo[] {
  const seen = ended.get(projectId);
  let runs = listed;
  if (seen !== undefined) {
    runs = runs.flatMap((run) => {
      const end = seen.get(run.run_id);
      if (end === undefined) return [run];
      if (end.status !== "failed") return [];
      return [{ ...run, status: "failed" as const, reason: end.reason, step: end.step }];
    });
  }
  for (const payload of during) runs = applyProgress(runs, payload);
  return runs;
}

/// One progress event applied to a run list, with no side effects, so a
/// replay over a fetched list does exactly what the live event did.
/// `running`/`failed` upsert the run **in place** (so the indicator list
/// doesn't reshuffle on every step); `complete`/`cancelled` drop it.
///
/// The lean progress payload carries no `steps`; the snapshot the run was
/// seeded with (from `list_workflow_runs`) is preserved when updating in place.
/// Empty `steps` is therefore a normal *transient* state for a run first seen
/// via an event before a seed/refresh populates it.
function applyProgress(
  runs: WorkflowRunInfo[],
  payload: WorkflowProgressPayload,
): WorkflowRunInfo[] {
  if (payload.status === "complete" || payload.status === "cancelled") {
    return runs.filter((r) => r.run_id !== payload.run_id);
  }
  const existing = runs.find((r) => r.run_id === payload.run_id);
  const row: WorkflowRunInfo = {
    run_id: payload.run_id,
    workflow: payload.workflow,
    step: payload.step,
    total: payload.total,
    status: payload.status === "failed" ? "failed" : "running",
    reason: payload.status === "failed" ? payload.reason : null,
    steps: existing?.steps ?? [],
  };
  return existing ? runs.map((r) => (r.run_id === payload.run_id ? row : r)) : [...runs, row];
}

/// Apply a progress event from its payload. The terminal event is emitted
/// before the backend prunes its registry entry, so re-querying on it could
/// observe the run as still-live; the payload already carries the status,
/// step, and reason. The interrupted class never arrives as an event (a crashed
/// run has no live process) — it surfaces only via the seed-on-subscribe query.
function handleProgress(projectId: ProjectId, payload: WorkflowProgressPayload): void {
  if (payload.status !== "running") {
    // Record the run's outcome for the project's completion notification, before
    // the drop below. The drop is what flips the project idle, and today that
    // reaches the flush only through the activity observer's *scheduled* effect —
    // so the reverse order would work too. Don't rely on that: if the idle push
    // ever becomes synchronous (calling `noteProjectStates` from here for
    // promptness is the obvious optimization), recording after the drop would let
    // the flush run without this outcome and silently omit it. A `failed` run is
    // *retained* rather than dropped, but it has still terminalized, so it is
    // recorded here either way.
    recordWorkflowTerminal(projectId, payload.workflow, payload.status);
    // Bookkeeping only — never read during render.
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const seen = ended.get(projectId) ?? new Map<string, WorkflowProgressPayload>();
    seen.set(payload.run_id, payload);
    ended.set(projectId, seen);
  }
  fetches.get(projectId)?.events.push(payload);
  const current = workflowRuns[projectId] ?? [];
  const known = current.some((r) => r.run_id === payload.run_id);
  workflowRuns[projectId] = applyProgress(current, payload);
  // A run first seen via an event — started outside the launching view (a
  // background project, another client), or before that view's post-invoke
  // seed resolved — has no `steps`. Fetch the authoritative snapshot once so
  // the live view renders labeled steps. Only the first event for the run hits
  // this; later events find it and update in place (preserving the fetched
  // steps).
  if (!known && (payload.status === "running" || payload.status === "failed")) {
    void refreshRuns(projectId);
  }
}

/// Cancel a live run (fire-and-forget on the backend; the channel event clears it).
export async function cancelRun(runId: string): Promise<void> {
  await api.cancelWorkflowRun(runId);
}

/// Abandon a failed/interrupted run, then refresh so it drops from the per-project
/// state (which clears the held failed view and the sidebar failure badge).
export async function abandonRun(projectId: ProjectId, runId: string): Promise<void> {
  await api.abandonWorkflowRun(projectId, runId);
  await refreshRuns(projectId);
}

/// Test-only reset: drop all subscriptions and run state.
export const _testing = {
  reset(): void {
    for (const unlisten of subscriptions.values()) unlisten();
    subscriptions.clear();
    pendingSubs.clear();
    generation.clear();
    ended.clear();
    fetches.clear();
    for (const key of Object.keys(workflowRuns)) delete workflowRuns[key];
  },
};
