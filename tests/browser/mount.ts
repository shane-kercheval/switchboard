// The render side is kept out of any `vi.mock`-bearing module: `vitest-browser-
// svelte` registers a top-level `beforeEach(cleanup)` at import, which throws
// "failed to find the runner" when it shares a module with `vi.mock` hoisting.
// Specs apply the IPC mocks (from ./harness) at their own top level and import
// `mountTranscript` from here.
import { render } from "vitest-browser-svelte";
import type { AgentRecord } from "$lib/types";
import type { DiffTarget } from "$lib/state/gitView.svelte";
import TranscriptHost from "./TranscriptHost.svelte";
import DiffPanelHost from "./DiffPanelHost.svelte";

/**
 * Mount `UnifiedTranscript` in real WebKit inside a fixed-height flex column so
 * overflow and scrolling are meaningful (see TranscriptHost). Returns the
 * `vitest-browser-svelte` render result.
 */
export function mountTranscript(opts: {
  projectId: string;
  agents: AgentRecord[];
  width?: number;
}): ReturnType<typeof render> {
  return render(TranscriptHost, {
    projectId: opts.projectId,
    agents: opts.agents,
    ...(opts.width !== undefined ? { width: opts.width } : {}),
  });
}

/** Mount `DiffPanel` (Git view) in a sized container for hit-target/layout checks. */
export function mountDiffPanel(opts: { target: DiffTarget }): ReturnType<typeof render> {
  // Pass props under `props` — a bare `{ target }` collides with Svelte's reserved
  // mount-target option.
  return render(DiffPanelHost, { props: { target: opts.target } });
}
