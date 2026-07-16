<script lang="ts">
  import TranscriptPanes from "$lib/components/TranscriptPanes.svelte";
  import ComposeBar from "$lib/components/ComposeBar.svelte";
  import type { AgentRecord } from "$lib/types";

  // The pane row above the composer, wired exactly as `App` wires them: a pane
  // Cmd+click bumps `composeFocusRequest`, which the composer watches to take
  // focus. Fixed height/width so the flicker spec has a real, focusable
  // textarea and real panes to modifier-click in one document.
  let {
    projectId,
    agents,
    width = 1000,
  }: { projectId: string; agents: AgentRecord[]; width?: number } = $props();

  let composeFocusRequest = $state(0);
</script>

<div style="height: 700px; width: {width}px; display: flex; flex-direction: column;">
  <div style="flex: 1 1 0%; min-height: 0; display: flex; flex-direction: column;">
    <TranscriptPanes {projectId} {agents} onRequestComposeFocus={() => (composeFocusRequest += 1)} />
  </div>
  <ComposeBar {projectId} {agents} focusRequest={composeFocusRequest} />
</div>
