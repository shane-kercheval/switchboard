<script lang="ts">
  /// Single banner row. The `testid` lets component tests target transient,
  /// event-driven notices individually. When `onDismiss` is provided the banner
  /// is dismissible.
  let {
    message,
    testid,
    onDismiss,
    actionLabel,
    onAction,
  }: {
    message: string;
    testid: string;
    onDismiss?: () => void;
    actionLabel?: string;
    onAction?: () => void;
  } = $props();
</script>

<div
  data-testid={testid}
  class="border-status-failed-soft bg-status-failed-soft text-status-failed flex items-center justify-between gap-3 border-b px-4 py-2 text-sm"
>
  <span>{message}</span>
  {#if onDismiss || (actionLabel !== undefined && onAction !== undefined)}
    <div class="flex shrink-0 items-center gap-3">
      {#if actionLabel !== undefined && onAction !== undefined}
        <button
          type="button"
          class="font-medium underline underline-offset-2"
          data-testid={`${testid}-action`}
          onclick={onAction}
        >
          {actionLabel}
        </button>
      {/if}
      {#if onDismiss}
        <button
          type="button"
          class="text-status-failed/70 hover:text-status-failed"
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
  {/if}
</div>
