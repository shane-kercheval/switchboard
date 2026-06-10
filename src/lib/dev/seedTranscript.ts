// Dev-only transcript seeding for the performance measurement protocol
// (docs/implementation_plans/2026-06-09-performance-improvements.md, M3):
// ⌃⌥⇧S prepends a large synthetic history to every agent in the active
// project, so typing cost can be profiled against a reproducible large
// transcript instead of whatever real project happens to be long.
//
// Gated the way DevIndicator is: outside `import.meta.env.DEV` the install
// body is statically dead, and the generator is loaded via dynamic import on
// first use, so neither ships in production builds.

import { transcripts } from "$lib/state/index.svelte";
import type { AgentRecord } from "$lib/types";

export function installDevTranscriptSeed(getAgents: () => AgentRecord[]): () => void {
  if (!import.meta.env.DEV) return () => undefined;

  const onKeydown = (event: KeyboardEvent): void => {
    // `event.code`, not `event.key`: with ⌥ held, macOS remaps the produced
    // character and `key` would never equal "s". Deliberately NOT gated on
    // editable focus (unlike the app's shortcuts): the chord types nothing,
    // and during a measurement session focus usually sits in the compose bar.
    if (!(event.ctrlKey && event.altKey && event.shiftKey) || event.metaKey) return;
    if (event.code !== "KeyS") return;
    event.preventDefault();
    void (async () => {
      // Skip agents already carrying seeded turns: a repeat press would
      // prepend duplicate turn ids and crash the keyed transcript render.
      // Derived from the data (not a flag) so a second project in the same
      // session still seeds. Press only after the project finishes loading —
      // seeding mid-hydration is harmless for display order (render sorts by
      // timestamp) but makes the fixture state confusing to reason about.
      const agents = getAgents().filter(
        (a) => !(transcripts[a.id] ?? []).some((t) => t.turn_id.startsWith("seed-")),
      );
      if (agents.length === 0) {
        console.info("[dev-seed] nothing to seed (no agents, or already seeded)");
        return;
      }
      const { buildLargeTranscript } = await import("./largeTranscript");
      const seeded = buildLargeTranscript({ agentIds: agents.map((a) => a.id) });
      for (const [agentId, turns] of Object.entries(seeded)) {
        transcripts[agentId] = [...turns, ...(transcripts[agentId] ?? [])];
      }
      console.info(
        `[dev-seed] prepended ${Object.values(seeded).reduce((n, t) => n + t.length, 0)} synthetic turns across ${agents.length} agent(s)`,
      );
    })();
  };

  window.addEventListener("keydown", onKeydown);
  return () => window.removeEventListener("keydown", onKeydown);
}
