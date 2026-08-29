<script lang="ts">
  const rows = Array.from({ length: 80 }, (_, index) => `Row ${index + 1}`);
</script>

<main class="flex h-full flex-col" data-testid="root-scroll-host">
  <div class="h-11 shrink-0">Header</div>
  <div class="min-h-0 flex-1 overflow-y-auto" data-testid="inner-scroll-region">
    {#each rows as row (row)}
      <p class="m-0 py-2">{row}</p>
    {/each}
  </div>
</main>
