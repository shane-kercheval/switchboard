<script lang="ts">
  import Sidebar from "$lib/components/Sidebar.svelte";
  import type { AgentRecord } from "$lib/types";

  // Wraps Sidebar so browser specs can mount it with real WebKit layout.
  // The sidebar is a fixed-width `<aside>` that sizes its own children off the
  // height it is given, so the wrapper is a **row** flex container of fixed
  // height — the shape the app puts it in. A column wrapper would let the aside
  // grow to its content instead of stretching, and then nothing inside it ever
  // overflows: the roster's scroll container and anything pinned below it would
  // measure as if the panel were unbounded.
  let { projectId, agents }: { projectId: string; agents: AgentRecord[] } = $props();
</script>

<div style="height: 600px; display: flex; flex-direction: row;">
  <Sidebar {projectId} {agents} />
</div>
