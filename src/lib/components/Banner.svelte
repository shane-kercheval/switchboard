<script lang="ts">
  /// Single banner row. The `testid` is structured so multiple banners can
  /// coexist (binary-missing per harness + auth-missing for Codex) and
  /// component tests can target them individually.
  let { message, testid }: { message: string; testid: string } = $props();
</script>

<div data-testid={testid} class="border-b border-red-300 bg-red-50 px-4 py-2 text-sm text-red-900">
  {message}
</div>
