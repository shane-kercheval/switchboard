<script lang="ts">
  import Button from "$lib/components/ui/Button.svelte";

  let { onNewProject, onAddExisting }: { onNewProject: () => void; onAddExisting: () => void } =
    $props();
</script>

<div class="flex h-full flex-col items-center justify-center gap-6 p-8">
  <h1 class="text-fg text-3xl font-semibold">Switchboard</h1>
  <p class="text-muted max-w-md text-center text-sm">
    Create a project to orchestrate AI coding agents, or add one you've already started. A project
    lives in a folder; Switchboard keeps its state in a
    <code class="bg-panel text-fg rounded px-1 font-mono">.switchboard/</code> directory inside it.
  </p>
  <div class="flex gap-3">
    <Button data-testid="welcome-new-project" onclick={onNewProject}>New project</Button>
    <Button data-testid="welcome-add-existing" onclick={onAddExisting}>Add existing project</Button>
  </div>
</div>
