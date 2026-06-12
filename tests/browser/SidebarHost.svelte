<script lang="ts">
  import Sidebar from "$lib/components/Sidebar.svelte";
  import type { AgentRecord } from "$lib/types";

  // Wraps Sidebar so browser specs can mount it with real WebKit layout.
  // The sidebar is `flex-1` and positions itself inside its parent; a fixed-
  // height outer container gives the scrollable regions real dimensions.
  let { projectId, agents }: { projectId: string; agents: AgentRecord[] } = $props();
</script>

<div style="height: 600px; display: flex; flex-direction: column;">
  <Sidebar {projectId} {agents} />
</div>
