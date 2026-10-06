/// Attachment copies in flight, per project — the files the compose bar has
/// started staging (a drop's copy, a large paste's write) but whose chips have
/// not landed yet.
///
/// Project-scoped and in memory rather than component-local because the compose
/// bar is remounted on every project switch: a replacement bar mounted while a
/// copy is still running must still refuse to send, or the message goes out
/// without the file and the late result is discarded. Not persisted — an
/// in-flight copy does not survive the app, so neither should its reservation.
///
/// Each copy is tracked by its own id, never by file name: a copy the user
/// stopped waiting for can still finish later, and if the same file was dropped
/// again meanwhile, its completion must release only its own entry — not the
/// retry's — or Send turns on while the retry is still copying.
///
/// Three facts per copy: it is *running* until it settles, it *blocks* sends
/// until the user stops waiting for it, and its *name* stays reserved while it
/// runs (so a paste numbered after it gets a distinct name even if the stopped
/// one lands late). Every settled copy bumps the project's completion counter,
/// which the mounted bar watches to pick up a chip a copy landed from a dead
/// instance.
import type { ProjectId } from "$lib/types";

type StagingEntry = { id: string; name: string; blocking: boolean };

export type StagingHandle = {
  /// The copy settled, success or failure. Idempotent.
  release(): void;
  /// Whether the user stopped waiting for this copy while it ran.
  abandoned(): boolean;
};

const inFlight = $state<Record<ProjectId, StagingEntry[]>>({});
const completions = $state<Record<ProjectId, number>>({});

/// Register a copy that is starting.
export function beginStaging(projectId: ProjectId, name: string): StagingHandle {
  const id = crypto.randomUUID();
  inFlight[projectId] = [...(inFlight[projectId] ?? []), { id, name, blocking: true }];
  let released = false;
  return {
    release(): void {
      if (released) return;
      released = true;
      inFlight[projectId] = (inFlight[projectId] ?? []).filter((entry) => entry.id !== id);
      completions[projectId] = (completions[projectId] ?? 0) + 1;
    },
    abandoned(): boolean {
      const entry = (inFlight[projectId] ?? []).find((e) => e.id === id);
      return entry !== undefined && !entry.blocking;
    },
  };
}

/// Names of the copies still blocking sends for `projectId`. Empty when nothing
/// is holding Send.
export function stagingBlocking(projectId: ProjectId): readonly string[] {
  return (inFlight[projectId] ?? []).filter((entry) => entry.blocking).map((entry) => entry.name);
}

/// Every name reserved by a copy still running for `projectId`, blocking or not.
export function reservedStagingNames(projectId: ProjectId): readonly string[] {
  return (inFlight[projectId] ?? []).map((entry) => entry.name);
}

/// Stop waiting for every copy running for `projectId`: sends are no longer
/// held, but the copies keep running and still land their chips when they
/// finish. The way out of a copy that hangs.
export function abandonStaging(projectId: ProjectId): void {
  inFlight[projectId] = (inFlight[projectId] ?? []).map((entry) => ({ ...entry, blocking: false }));
}

/// How many copies have settled for `projectId` this session. Read reactively
/// to notice completions; the value itself is meaningless.
export function stagingCompletions(projectId: ProjectId): number {
  return completions[projectId] ?? 0;
}

export const _testing = {
  reset(): void {
    for (const key of Object.keys(inFlight)) delete inFlight[key];
    for (const key of Object.keys(completions)) delete completions[key];
  },
};
