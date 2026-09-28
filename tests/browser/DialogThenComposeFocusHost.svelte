<script lang="ts">
  import { tick } from "svelte";
  import ComposeBar from "$lib/components/ComposeBar.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import type { AgentRecord } from "$lib/types";

  // The add-agent flow's shape: a "+" opens a dialog whose submit closes it and
  // asks the composer for focus — either after the dialog has closed (what
  // `App.handleAddAgent` does) or in the same update that closes it.
  let {
    projectId,
    agents,
    waitForClose,
  }: { projectId: string; agents: AgentRecord[]; waitForClose: boolean } = $props();

  let open = $state(false);
  let focusRequest = $state(0);

  async function submit(): Promise<void> {
    open = false;
    if (waitForClose) await tick();
    focusRequest += 1;
  }
</script>

<div style="height: 500px; display: flex; flex-direction: column;">
  <button type="button" data-testid="host-open-dialog" onclick={() => (open = true)}>+</button>
  <div style="flex: 1"></div>
  <ComposeBar {projectId} {agents} {focusRequest} />
</div>

<Dialog {open} title="Add agent" onClose={() => (open = false)}>
  <button type="button" data-testid="host-submit" onclick={() => void submit()}>Create</button>
</Dialog>
