<script lang="ts">
  import Ping from "./lib/components/Ping.svelte";
</script>

<main class="flex h-full flex-col items-center justify-center gap-4 p-8 text-neutral-100">
  <h1 class="text-2xl font-semibold">Switchboard</h1>
  <p class="text-sm text-neutral-400">M1.1 shell — IPC smoke test below.</p>
  <Ping />
</main>

<style>
  main {
    background-color: #1a1a1a;
  }
</style>
