/// Live workflow-run state for the run indicator. Subscribes to the
/// project-scoped `workflow:<project-id>` channel **once per loaded project**
/// (not gated to the active project, so a run in a background project keeps
/// updating), seeded on subscribe with `list_workflow_runs` for crash-surfacing.
/// Torn down with the project, mirroring the per-agent listener lifecycle.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as api from "$lib/api";
import type { ProjectId, WorkflowProgressPayload, WorkflowRunInfo } from "$lib/types";

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

/// Subscribe to a project's workflow progress channel (idempotent). Installs the
/// listener **first**, then seeds the current runs — so a run that terminalizes
/// during project load isn't missed (the listener is already live). The
/// generation guard discards the listener and the seed if the project is torn
/// down while either await is in flight.
export async function subscribeProjectWorkflows(projectId: ProjectId): Promise<void> {
  if (subscriptions.has(projectId)) return;
  const pending = pendingSubs.get(projectId);
  if (pending !== undefined) return pending;

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
      let seeded: WorkflowRunInfo[];
      try {
        seeded = await api.listWorkflowRuns(projectId);
      } catch (err) {
        console.warn("[switchboard] listWorkflowRuns failed", err);
        return;
      }
      // Discard a stale seed if torn down meanwhile. (A live progress event that
      // landed between `listen` and here is overwritten by the seed — a
      // project-open-only, millisecond window that self-heals on the next event.)
      if (generation.get(projectId) !== gen) return;
      workflowRuns[projectId] = seeded;
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
    delete workflowRuns[projectId];
  }
}

/// Re-query the authoritative 3-class run list for a project.
export async function refreshRuns(projectId: ProjectId): Promise<void> {
  try {
    workflowRuns[projectId] = await api.listWorkflowRuns(projectId);
  } catch (err) {
    console.warn("[switchboard] listWorkflowRuns failed", err);
  }
}

/// Apply a progress event entirely from its payload — no re-query. The terminal
/// event is emitted before the backend prunes its registry entry, so re-querying
/// on it could observe the run as still-live and pin it "running"; the payload
/// already carries the status, step, and reason. `running`/`failed` upsert the
/// run **in place** (so the indicator list doesn't reshuffle on every step);
/// `complete`/`cancelled` drop it. The `interrupted` class never arrives as an
/// event (a crashed run has no live process) — it surfaces only via the
/// seed-on-subscribe query.
function handleProgress(projectId: ProjectId, payload: WorkflowProgressPayload): void {
  const current = workflowRuns[projectId] ?? [];
  if (payload.status === "complete" || payload.status === "cancelled") {
    workflowRuns[projectId] = current.filter((r) => r.run_id !== payload.run_id);
    return;
  }
  // The lean progress payload carries no `steps`; preserve the snapshot the run
  // was seeded with (from `list_workflow_runs`) when updating in place. Empty
  // `steps` is therefore a normal *transient* state for a run first seen via an
  // event before a seed/refresh populates it (not only a legacy disk file) — the
  // M4 live-view work refreshes on invoke / on an unknown running event to close
  // that window before the progress view renders.
  const existing = current.find((r) => r.run_id === payload.run_id);
  const row: WorkflowRunInfo = {
    run_id: payload.run_id,
    workflow: payload.workflow,
    step: payload.step,
    total: payload.total,
    status: payload.status === "failed" ? "failed" : "running",
    reason: payload.status === "failed" ? payload.reason : null,
    steps: existing?.steps ?? [],
  };
  workflowRuns[projectId] = existing
    ? current.map((r) => (r.run_id === payload.run_id ? row : r))
    : [...current, row];
  // A run first seen via an event — started outside the launching view (a
  // background project, direct IPC), or before that view's post-invoke seed
  // resolved — has no `steps`. Fetch the authoritative snapshot once so the live
  // view renders labeled steps. Only the first event for the run hits this; later
  // events find `existing` and update in place (preserving the fetched steps).
  if (existing === undefined) void refreshRuns(projectId);
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
    for (const key of Object.keys(workflowRuns)) delete workflowRuns[key];
  },
};
