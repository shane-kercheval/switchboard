<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  let reply = $state<string | null>(null);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      reply = await invoke<string>("ping", { name: "world" });
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  });
</script>

<div data-testid="ping" class="rounded border border-neutral-700 px-4 py-2 font-mono text-sm">
  {#if reply !== null}
    <span data-testid="ping-reply">{reply}</span>
  {:else if error !== null}
    <span class="text-red-400" data-testid="ping-error">ping failed: {error}</span>
  {:else}
    <span class="text-neutral-500" data-testid="ping-pending">pinging…</span>
  {/if}
</div>
