<script lang="ts">
  /// Single banner row. The `testid` is structured so multiple banners can
  /// coexist (binary-missing per harness + auth-missing for Codex) and
  /// component tests can target them individually. When `onDismiss` is
  /// provided the banner is dismissible (a trailing ✕) — used for transient,
  /// event-driven notices (e.g. an agent that failed to auto-create); the
  /// reactive binary-missing banners omit it and clear themselves.
  let { message, testid, onDismiss }: { message: string; testid: string; onDismiss?: () => void } =
    $props();
</script>

<div
  data-testid={testid}
  class="border-status-failed-soft bg-status-failed-soft text-status-failed flex items-center justify-between gap-3 border-b px-4 py-2 text-sm"
>
  <span>{message}</span>
  {#if onDismiss}
    <button
      type="button"
      class="text-status-failed/70 hover:text-status-failed shrink-0"
      aria-label="Dismiss"
      data-testid={`${testid}-dismiss`}
      onclick={onDismiss}
    >
      <svg
        width="14"
        height="14"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M18 6 6 18M6 6l12 12" />
      </svg>
    </button>
  {/if}
</div>
