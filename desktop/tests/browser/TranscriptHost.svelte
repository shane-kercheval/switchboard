<script lang="ts">
  import UnifiedTranscript from "$lib/components/UnifiedTranscript.svelte";
  import type { AgentRecord } from "$lib/types";

  // The transcript is `flex-1` and only overflows/scrolls inside a parent with a
  // definite height. jsdom never needed this (it has no layout); in real WebKit
  // the mount target must be a sized flex column or `scrollHeight`/`clientHeight`
  // and the clip `max-height` carry no meaning. Height is fixed and modest so a
  // long message reliably overflows the transcript area under test.
  // `width` constrains the mount for layout cases that only manifest in a narrow
  // container (e.g. fan-out columns / a small window); unset = full width.
  let { projectId, agents, width }: { projectId: string; agents: AgentRecord[]; width?: number } =
    $props();
</script>

<div
  style="height: 600px; display: flex; flex-direction: column;{width !== undefined
    ? ` width: ${width}px;`
    : ''}"
>
  <UnifiedTranscript {projectId} {agents} />
</div>
