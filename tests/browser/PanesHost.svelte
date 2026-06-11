<script lang="ts">
  import TranscriptPanes from "$lib/components/TranscriptPanes.svelte";
  import type { AgentRecord } from "$lib/types";

  // Fixed height AND width: pane-row tests measure real pane widths (gutter
  // drags, min-width clamps), so the row needs a definite, deterministic size
  // independent of the test-runner viewport.
  let {
    projectId,
    agents,
    width = 900,
  }: { projectId: string; agents: AgentRecord[]; width?: number } = $props();
</script>

<div style="height: 600px; width: {width}px; display: flex; flex-direction: column;">
  <TranscriptPanes {projectId} {agents} />
</div>
