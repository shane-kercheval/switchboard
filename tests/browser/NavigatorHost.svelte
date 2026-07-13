<script lang="ts">
  import TranscriptNavigator from "$lib/components/TranscriptNavigator.svelte";
  import type { AgentRecord } from "$lib/types";

  let { projectId, agents }: { projectId: string; agents: AgentRecord[] } = $props();
</script>

<TranscriptNavigator {projectId} {agents} />
