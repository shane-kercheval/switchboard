<script lang="ts">
  import Button from "$lib/components/ui/Button.svelte";

  let { onPickDirectory }: { onPickDirectory: () => void } = $props();
</script>

<div class="flex h-full flex-col items-center justify-center gap-6 p-8">
  <h1 class="text-3xl font-semibold text-neutral-900">Switchboard</h1>
  <p class="max-w-md text-center text-sm text-neutral-600">
    Open a working directory to get started. Switchboard stores its state in a
    <code class="rounded bg-neutral-100 px-1 font-mono text-neutral-700">.switchboard/</code> folder inside
    it.
  </p>
  <Button onclick={onPickDirectory}>Open working directory</Button>
</div>
