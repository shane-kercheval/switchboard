<script lang="ts">
  import { tick } from "svelte";
  import ComposeBar from "$lib/components/ComposeBar.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import type { AgentRecord } from "$lib/types";

  // The compose bar beside a dialog wired exactly as the transcript navigator
  // wires it: auto-focus prevented, the dialog's own field focused a tick later.
  // That combination is what decides where focus sits when Escape arrives.
  let { projectId, agents }: { projectId: string; agents: AgentRecord[] } = $props();

  let open = $state(false);
  let fieldEl = $state<HTMLInputElement | null>(null);
</script>

<div style="height: 500px; display: flex; flex-direction: column;">
  <button type="button" data-testid="host-open-dialog" onclick={() => (open = true)}>
    Find message
  </button>
  <div style="flex: 1"></div>
  <ComposeBar {projectId} {agents} />
</div>

<Dialog
  {open}
  title="Messages"
  onOpenAutoFocus={(event) => {
    event.preventDefault();
    void tick().then(() => fieldEl?.focus());
  }}
  onClose={() => (open = false)}
>
  <input bind:this={fieldEl} data-testid="host-dialog-field" aria-label="Search messages" />
</Dialog>
