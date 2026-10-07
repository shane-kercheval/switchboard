/// Reading mode — "I'm watching this project, but treat me as though I'm not in
/// it." The compose box hides, and a completion in the project notifies exactly
/// as a background project's would (it obeys the user's "also notify me about
/// other projects" preference instead of being suppressed as already-on-screen).
///
/// **Deliberately in-memory and not persisted.** This is a posture, not a
/// preference: the user turns it on because *this run* is worth watching, and it
/// turns itself off when the project goes quiet. Persisting it would mean opening
/// the app tomorrow to a missing compose box with no memory of why — a setting the
/// user never meant to set.
///
/// **The notification behavior is not implemented here.** Reading mode works
/// *through* the existing suppression gate rather than around it: `App.svelte`
/// folds this flag into the `visibleProjectId` it pushes to the backend, so the
/// project reads as "not on screen" and the Rust gate is untouched. There is no
/// second gate on the frontend.
///
/// **Turning it off is owned by the completion flush**, not by this module — see
/// `sendCompletion.ts`, which clears the project only *after* handing its
/// notification to the backend. Clearing restores the project's real visibility,
/// so clearing first would suppress the very notification that ended the mode.
///
/// **A leaf, like `sendCompletion.ts`** — it imports only types, which is what
/// lets that module clear a project without acquiring a dependency on workspace
/// state. Keep it that way.

import type { ProjectId } from "$lib/types";

/// Projects currently in reading mode. Presence is the flag; a record rather
/// than a `Set` so Svelte's deep reactivity covers per-key reads (the same shape
/// `backgroundCompletedProjectIds` uses).
const readingProjects = $state<Record<ProjectId, true>>({});

export function isReadingMode(projectId: ProjectId): boolean {
  return projectId in readingProjects;
}

/// Toggle reading mode for one project, returning the new state.
///
/// **Allowed on a project that is already quiet**, where it simply stays on until
/// the user turns it off or until later activity starts and settles. Reading a
/// finished transcript without the compose box in the way is a legitimate use of
/// a feature called "reading mode"; the toggle's latched on-state is what keeps
/// that from reading as a lost compose box. Do not add an idle guard here — it
/// would couple this module, the toggle's enabled state, and the palette entry to
/// the idle predicate for no functional gain.
export function toggleReadingMode(projectId: ProjectId): boolean {
  if (projectId in readingProjects) {
    delete readingProjects[projectId];
    return false;
  }
  readingProjects[projectId] = true;
  return true;
}

/// Turn reading mode on for a project. Idempotent — and that is load-bearing,
/// not convenience: the auto-on-send preference calls this from the dispatch
/// path, which can run while the mode is already on (a held forward resolving
/// after the user entered reading mode manually). Routing that through
/// `toggleReadingMode` would turn the mode *off* on exactly the send the user
/// wanted it kept on for.
export function enterReadingMode(projectId: ProjectId): void {
  readingProjects[projectId] = true;
}

/// Turn reading mode off for a project. Idempotent — the flush calls this on
/// every project quiet-down, most of which were never in reading mode.
export function clearReadingMode(projectId: ProjectId): void {
  delete readingProjects[projectId];
}

/// Drop reading mode for projects being torn down (directory removal, project
/// delete), alongside the other per-project teardown.
export function forgetReadingMode(projectIds: ProjectId[]): void {
  for (const projectId of projectIds) delete readingProjects[projectId];
}

/// Test-only reset — module state that would otherwise leak between cases.
export const _testing = {
  reset(): void {
    for (const key of Object.keys(readingProjects)) delete readingProjects[key];
  },
};
