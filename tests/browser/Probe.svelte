<script lang="ts">
  // Minimal fixture for the browser-stack compatibility gate — proves a Svelte 5
  // component mounts and renders in real WebKit via vitest-browser-svelte.
  let { label }: { label: string } = $props();
</script>

<p data-testid="probe">{label}</p>
