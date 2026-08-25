<script lang="ts">
  import PinsSidebar from "$lib/components/PinsSidebar.svelte";
  import type { AgentRecord } from "$lib/types";

  let { projectId, agents }: { projectId: string; agents: AgentRecord[] } = $props();
  let mode = $state<"agents" | "pins">("pins");
</script>

<div style="display: flex; height: 360px; flex-direction: column;">
  <div>
    <button type="button" data-testid="show-agents" onclick={() => (mode = "agents")}>Agents</button
    >
    <button type="button" data-testid="show-pins" onclick={() => (mode = "pins")}>Pins</button>
  </div>
  <div style="display: flex; min-height: 0; flex: 1;">
    {#if mode === "pins"}
      <PinsSidebar {projectId} {agents} />
    {:else}
      <aside data-testid="agents-placeholder">Agents</aside>
    {/if}
  </div>
</div>
