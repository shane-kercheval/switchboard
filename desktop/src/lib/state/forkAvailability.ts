import type { AgentRecord, ProjectId } from "$lib/types";
import { projectIsAvailable } from "$lib/types";
import { HARNESS_LABEL } from "$lib/harnessDisplay";
import { supportsComposeFork } from "$lib/harnessCapabilities";
import { agentIsWorking, runtimes, transcripts } from "$lib/state/index.svelte";
import { conversations, projects } from "$lib/state/workspace.svelte";

/// When an agent can be forked, for both places that offer Fork: the compose
/// box's Fork half and a Codex agent card's Fork item. Each returns why not as
/// user-facing copy, or `null`. What only one surface needs (the compose box's
/// mode and recipient checks, the card's compose-claim check) stays with that
/// surface.
///
/// Advisory: the backend re-checks the source's session file and that it is
/// idle when the fork is created, and refuses with its own message.

/// Whether the project's working directory is usable. A harness subprocess
/// starts in that directory, so with it deleted or unresolvable every send and
/// every fork would fail at spawn, and the app-level banner says why. A project
/// the list doesn't know is not gated: there is no information, and the backend
/// reports the truth if the folder really is gone.
export function projectFolderAvailable(projectId: ProjectId): boolean {
  const listing = projects.list.find((candidate) => candidate.id === projectId);
  return listing === undefined || projectIsAvailable(listing);
}

/// Why nothing in `projectId` can be forked right now.
export function projectForkBlock(projectId: ProjectId): string | null {
  if (!projectFolderAvailable(projectId)) {
    return "This project's folder is unavailable, so its agents can't run.";
  }
  // A fork's earlier conversation is read through the project's history load:
  // a Codex fork's right after it is created, a Claude fork's after its first
  // reply. Either read is skipped while the project is still loading, and the
  // in-flight load may have started before the fork existed, so a fork made
  // now could show no earlier conversation and no error. It also holds after a
  // failed project load until a retry succeeds.
  if (conversations[projectId]?.status !== "complete") {
    return "Fork is available once the project's history has loaded.";
  }
  return null;
}

/// Why `agent` itself can't be forked right now.
///
/// The busy check is what the backend re-checks authoritatively. A branch taken
/// mid-turn copies a conversation still being written (probe-measured: a Claude
/// send then inherits a "No response requested." placeholder instead of the
/// parent's real answer, permanently). Keep its wording in step with
/// `AppError::ForkSourceBusy`.
export function agentForkBlock(agent: AgentRecord): string | null {
  if (!supportsComposeFork(agent.harness)) {
    return `Switchboard can branch only Claude Code and Codex sessions, and ${agent.name} is ${HARNESS_LABEL[agent.harness]}.`;
  }
  if (!looksLikeItHasASession(agent)) {
    return `${agent.name} has no session to branch from yet — send it a message first.`;
  }
  if (agentIsWorking(runtimes[agent.id])) {
    return `${agent.name} is busy — its conversation must stop changing before you can branch. Wait for it to finish, or cancel it first.`;
  }
  return null;
}

/// Why `agent` can't be forked right now, project conditions first.
export function forkBlockFor(projectId: ProjectId, agent: AgentRecord): string | null {
  return projectForkBlock(projectId) ?? agentForkBlock(agent);
}

/// Whether `agent` looks like it has a harness session to branch from, or to
/// compact: "Compact all" uses the same test to skip agents never sent anything.
///
/// **A Codex session id is proof.** Switchboard records one only once Codex
/// has created the session, so a Codex agent that has one has a session even
/// when nothing of it can be shown: a Codex fork whose earlier conversation is
/// unavailable and that hasn't been sent anything has an empty transcript. A
/// Claude agent's id is assigned before any session exists, so it proves
/// nothing.
///
/// Otherwise derived from the transcript rather than fetched: an agent with any
/// turn has dispatched at least once. Deliberately **not** an
/// `agent_session_info` call: putting an IPC behind every selection change would
/// make the compose bar's request sequence depend on which agent is selected.
/// An empty transcript counts as evidence only once the agent's history has
/// loaded: while it is loading, or after a failure until the user retries, it is
/// empty for an agent that may have a long history, and "send it a message
/// first" would be false.
///
/// Advisory in both directions, and the backend's check of the session file is
/// the authority. It offers Fork for an agent whose only turn failed before a
/// session file was written, which the backend then refuses with a precise
/// error. Without a Codex session id, it refuses Fork for a real session that
/// parses to zero turns (an empty or housekeeping-only file).
export function looksLikeItHasASession(agent: AgentRecord): boolean {
  if (agent.session_locator !== null && "codex" in agent.session_locator) return true;
  if (runtimes[agent.id]?.hydration_status !== "complete") return true;
  return (transcripts[agent.id] ?? []).length > 0;
}
