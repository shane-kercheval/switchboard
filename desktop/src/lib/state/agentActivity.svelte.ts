/// What an agent is doing right now, as the agent card, the pane-header member
/// chips, and the card's accessible label show it. One derivation so those
/// surfaces cannot disagree.
///
/// Precedence when several apply: **working > waiting > failed > idle**.
/// Working is what the agent is doing now (a held forward to a busy agent will
/// itself queue behind that turn). Waiting outranks failed because a send is
/// already on its way, and its dispatch clears the failure.

import type { AgentId, AgentRecord, HarnessKind, ProjectId, WorkflowRunInfo } from "$lib/types";
import type { AgentRuntime, RuntimeMap, Turn } from "$lib/state/types";
import { agentIsWorking, runtimes, transcripts } from "$lib/state/index.svelte";
import {
  forwardSourceLabel,
  heldForwardsFor,
  type HeldForward,
} from "$lib/state/heldForwards.svelte";
import { workflowRuns } from "$lib/state/workflows.svelte";
import { agentsByProject } from "$lib/state/workspace.svelte";
import { canonicalizeForUniqueness } from "$lib/nameValidation";

/// Who or what a waiting agent waits on. `user` is a workflow paused for the
/// user's input; `workflow` is a run between agents (none of its agents is
/// working), named by the workflow.
export type WaitTarget =
  | { kind: "agent"; agentId: AgentId; name: string; harness?: HarnessKind }
  | { kind: "user" }
  | { kind: "workflow"; name: string };

export type AgentActivity =
  /// `startedAt` is the running turn's start; absent while the work is only
  /// accepted or queued and no turn has started.
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
  transcript: Turn[] | undefined;
  heldForwards: HeldForward[];
  /// The project's workflow runs.
  runs: WorkflowRunInfo[];
  /// Any loaded agent by id, for a forward source in another project.
  findAgent: (agentId: AgentId) => AgentRecord | undefined;
};

export function deriveAgentActivity(input: ActivityInputs): AgentActivity {
  const runtime = input.runtimes[input.agent.id];
  if (agentIsWorking(runtime)) {
    return { kind: "working", startedAt: runningTurnStart(runtime, input.transcript) };
  }
  const on = [...heldForwardTargets(input), ...workflowTargets(input)];
  if (on.length > 0) return { kind: "waiting", on: dedupe(on) };
  if (runtime?.last_error !== undefined) {
    return { kind: "failed", reason: runtime.last_error.message };
  }
  return { kind: "idle" };
}

/// The running turn's start, only while a turn is actually streaming: between
/// a turn's end and the agent going idle, and while work is only queued, the
/// card shows "Working" with no time rather than a stale one.
function runningTurnStart(
  runtime: AgentRuntime | undefined,
  transcript: Turn[] | undefined,
): string | undefined {
  if (runtime?.run_status !== "processing") return undefined;
  const turns = transcript ?? [];
  const streaming = (turn: Turn): boolean => turn.role === "agent" && turn.status === "streaming";
  const inFlight = turns.find(
    (turn) => streaming(turn) && turn.turn_id === runtime.in_flight_turn_id,
  );
  return (inFlight ?? [...turns].reverse().find(streaming))?.started_at;
}

function heldForwardTargets(input: ActivityInputs): WaitTarget[] {
  return input.heldForwards
    .filter((forward) => forward.recipients.includes(input.agent.id))
    .flatMap((forward) =>
      forward.sources.map(
        (source): WaitTarget => ({
          kind: "agent",
          agentId: source.id,
          name: forwardSourceLabel(source, input.projectId),
          harness: (input.roster.find((a) => a.id === source.id) ?? input.findAgent(source.id))
            ?.harness,
        }),
      ),
    );
}

/// A running workflow's later step names this agent. Steps name agents by
/// their resolved name (unique within a project, compared the way the backend
/// does), because the run's step info carries no agent ids.
///
/// The agent waits on the run's agents that are working now. With none working,
/// it waits on the user when the run is paused for input, else on the run.
function workflowTargets(input: ActivityInputs): WaitTarget[] {
  const key = canonicalizeForUniqueness(input.agent.name);
  const names = (step: WorkflowRunInfo["steps"][number]): string[] =>
    step.recipients.flatMap((ref) => (ref.kind === "literal" ? [ref.name] : []));
  return input.runs.flatMap((run): WaitTarget[] => {
    if (run.status !== "running") return [];
    const later = run.steps.some(
      (step, i) =>
        i > run.step &&
        (step.kind === "send" || step.kind === "for_each") &&
        names(step).some((name) => canonicalizeForUniqueness(name) === key),
    );
    if (!later) return [];
    const working = run.steps
      .slice(0, run.step + 1)
      .flatMap(names)
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
    return run.steps[run.step]?.kind === "pause"
      ? [{ kind: "user" }]
      : [{ kind: "workflow", name: run.workflow }];
  });
}

function dedupe(targets: WaitTarget[]): WaitTarget[] {
  // A local within one pure call, never stored or rendered.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const seen = new Set<string>();
  return targets.filter((target) => {
    const key =
      target.kind === "agent"
        ? `a:${target.agentId}`
        : target.kind === "workflow"
          ? `w:${target.name}`
          : "u";
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export function waitTargetName(target: WaitTarget): string {
  if (target.kind === "user") return "you";
  return target.name;
}

/// The state words for the card's accessible name, or `null` when idle. Never
/// carries the elapsed time, so the name changes only when the state does.
export function activityPhrase(activity: AgentActivity): string | null {
  switch (activity.kind) {
    case "working":
      return "working";
    case "waiting":
      return `waiting for ${activity.on.map(waitTargetName).join(", ")}`;
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
    transcript: transcripts[agent.id],
    heldForwards: heldForwardsFor(projectId),
    runs: workflowRuns[projectId] ?? [],
    findAgent: findLoadedAgent,
  });
}
