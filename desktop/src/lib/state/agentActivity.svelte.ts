/// What an agent is doing right now, as the agent card, the pane-header member
/// chips, and the card's accessible label show it. One derivation so those
/// surfaces cannot disagree.
///
/// Precedence when several apply: **working > waiting > failed > idle**.
/// Working is what the agent is doing now (a held forward to a busy agent will
/// itself queue behind that turn). Waiting outranks failed because a send is
/// already on its way, and its turn's start clears the failure.

import type { AgentId, AgentRecord, HarnessKind, ProjectId, WorkflowRunInfo } from "$lib/types";
import type { RuntimeMap } from "$lib/state/types";
import { agentIsWorking, runtimes } from "$lib/state/index.svelte";
import {
  forwardSourceLabel,
  heldForwardsFor,
  type HeldForward,
} from "$lib/state/heldForwards.svelte";
import { workflowRuns } from "$lib/state/workflows.svelte";
import { agentsByProject } from "$lib/state/workspace.svelte";
import { canonicalizeForUniqueness } from "$lib/nameValidation";

/// Who or what a waiting agent waits on. `workflow` is a run whose current
/// step has no working agent, named by the workflow.
export type WaitTarget =
  | { kind: "agent"; agentId: AgentId; name: string; harness?: HarnessKind }
  | { kind: "workflow"; name: string };

export type AgentActivity =
  /// `startedAt` is the running turn's start; absent while the work is only
  /// queued, and between a turn's end and the agent going idle.
  | { kind: "working"; startedAt?: string }
  | { kind: "waiting"; on: WaitTarget[] }
  | { kind: "failed"; reason: string }
  | { kind: "idle" };

export type ActivityInputs = {
  agent: AgentRecord;
  projectId: ProjectId;
  /// The agent's own project roster, in order.
  roster: AgentRecord[];
  runtimes: RuntimeMap;
  heldForwards: HeldForward[];
  /// The project's workflow runs.
  runs: WorkflowRunInfo[];
  /// Any loaded agent by id, for a forward source in another project.
  findAgent: (agentId: AgentId) => AgentRecord | undefined;
};

export function deriveAgentActivity(input: ActivityInputs): AgentActivity {
  const runtime = input.runtimes[input.agent.id];
  if (agentIsWorking(runtime)) {
    return { kind: "working", startedAt: runtime?.turn_started_at };
  }
  const on = [...heldForwardTargets(input), ...workflowTargets(input)];
  if (on.length > 0) return { kind: "waiting", on: dedupe(on) };
  if (runtime?.last_error !== undefined) {
    return { kind: "failed", reason: runtime.last_error.message };
  }
  return { kind: "idle" };
}

/// The sources a held forward to this agent is likely still waiting on, an
/// estimate: the backend waits on the turn each source was running when the
/// forward began, which the window doesn't record. A source counts while its
/// `run_status` is not idle (a send queued behind a finished turn is not held
/// for), so one that finished and started another turn is still named. A
/// source whose runtime this window doesn't hold, in an unopened project, is
/// kept, though it may already be idle. If none is left while the forward is
/// still held, all are named, idle ones included, so the card doesn't read
/// idle just before the forward dispatches.
function heldForwardTargets(input: ActivityInputs): WaitTarget[] {
  return input.heldForwards
    .filter((forward) => forward.recipients.includes(input.agent.id))
    .flatMap((forward) => {
      const pending = forward.sources.filter((source) => {
        const runtime = input.runtimes[source.id];
        return runtime === undefined || runtime.run_status !== "idle";
      });
      return (pending.length > 0 ? pending : forward.sources).map(
        (source): WaitTarget => ({
          kind: "agent",
          agentId: source.id,
          name: forwardSourceLabel(source, input.projectId),
          harness: (input.roster.find((a) => a.id === source.id) ?? input.findAgent(source.id))
            ?.harness,
        }),
      );
    });
}

/// A running workflow's later send step names this agent. Steps name agents by
/// the names bound when the run started (compared the way the backend does),
/// because the run's step info carries no agent ids; an agent renamed mid-run
/// therefore reads idle until its own step.
///
/// The agent waits on the working agents of the run's current step, which are
/// the ones a wait step waits for. With none working, it waits on the run.
function workflowTargets(input: ActivityInputs): WaitTarget[] {
  const key = canonicalizeForUniqueness(input.agent.name);
  const names = (step: WorkflowRunInfo["steps"][number]): string[] =>
    step.recipients.flatMap((ref) => (ref.kind === "literal" ? [ref.name] : []));
  return input.runs.flatMap((run): WaitTarget[] => {
    if (run.status !== "running") return [];
    const later = run.steps.some(
      (step, i) =>
        i > run.step &&
        step.kind === "send" &&
        names(step).some((name) => canonicalizeForUniqueness(name) === key),
    );
    if (!later) return [];
    const current = run.steps[run.step];
    const working = (current === undefined ? [] : names(current))
      .map((name) =>
        input.roster.find(
          (a) => canonicalizeForUniqueness(a.name) === canonicalizeForUniqueness(name),
        ),
      )
      .filter(
        (a): a is AgentRecord =>
          a !== undefined && a.id !== input.agent.id && agentIsWorking(input.runtimes[a.id]),
      );
    if (working.length > 0) {
      return working.map((a) => ({
        kind: "agent",
        agentId: a.id,
        name: a.name,
        harness: a.harness,
      }));
    }
    return [{ kind: "workflow", name: run.workflow }];
  });
}

function dedupe(targets: WaitTarget[]): WaitTarget[] {
  // A local within one pure call, never stored or rendered.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const seen = new Set<string>();
  return targets.filter((target) => {
    const key = target.kind === "agent" ? `a:${target.agentId}` : `w:${target.name}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/// The state words for the card's accessible name, or `null` when idle. Never
/// carries the elapsed time, so the name changes only when the state does.
export function activityPhrase(activity: AgentActivity): string | null {
  switch (activity.kind) {
    case "working":
      return "working";
    case "waiting":
      return `waiting for ${activity.on.map((target) => target.name).join(", ")}`;
    case "failed":
      return "last turn failed";
    case "idle":
      return null;
  }
}

function findLoadedAgent(agentId: AgentId): AgentRecord | undefined {
  for (const roster of Object.values(agentsByProject)) {
    const found = roster.find((a) => a.id === agentId);
    if (found !== undefined) return found;
  }
  return undefined;
}

/// The live activity of `agent` in `projectId`, read from the stores. Reactive
/// when read inside an effect or template.
export function agentActivity(
  projectId: ProjectId,
  agent: AgentRecord,
  roster: AgentRecord[],
): AgentActivity {
  return deriveAgentActivity({
    agent,
    projectId,
    roster,
    runtimes,
    heldForwards: heldForwardsFor(projectId),
    runs: workflowRuns[projectId] ?? [],
    findAgent: findLoadedAgent,
  });
}
