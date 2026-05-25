<script lang="ts">
  /// Single banner row. The `testid` is structured so multiple banners can
  /// coexist (binary-missing per harness + auth-missing for Codex) and
  /// component tests can target them individually.
  let { message, testid }: { message: string; testid: string } = $props();
</script>

<div
  data-testid={testid}
  class="border-status-failed-soft bg-status-failed-soft text-status-failed border-b px-4 py-2 text-sm"
>
  {message}
</div>
