<script lang="ts">
  let { message }: { message: string } = $props();
</script>

<div
  data-testid="binary-not-found-banner"
  class="border-b border-red-300 bg-red-50 px-4 py-2 text-sm text-red-900"
>
  {message}
</div>
