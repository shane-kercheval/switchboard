<script lang="ts">
  // Test-only host that passes Markdown its text the way the transcript does:
  // through a row object the parent replaces wholesale on every update.
  import Markdown from "./Markdown.svelte";

  let { row }: { row: { text: string } } = $props();
</script>

<Markdown text={row.text} />
