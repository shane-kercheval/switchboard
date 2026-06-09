<script lang="ts">
  import UnifiedTranscript from "$lib/components/UnifiedTranscript.svelte";
  import type { AgentRecord } from "$lib/types";

  // The transcript is `flex-1` and only overflows/scrolls inside a parent with a
  // definite height. jsdom never needed this (it has no layout); in real WebKit
  // the mount target must be a sized flex column or `scrollHeight`/`clientHeight`
  // and the clip `max-height` carry no meaning. Height is fixed and modest so a
  // long message reliably overflows the transcript area under test.
  let { projectId, agents }: { projectId: string; agents: AgentRecord[] } = $props();
</script>

<div style="height: 600px; display: flex; flex-direction: column;">
  <UnifiedTranscript {projectId} {agents} />
</div>
