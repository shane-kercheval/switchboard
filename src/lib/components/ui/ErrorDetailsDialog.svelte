<script lang="ts">
  /// Presentational dialog for surfacing an exact error verbatim with a Copy
  /// action, so a user can paste it into a bug report. Driven from failure
  /// state (hydration failures today); deliberately a reusable *component*, not
  /// a global error store/pipeline — a future shared error surface would adopt
  /// this, but the broader bus is out of scope (see the hydration-robustness
  /// plan, M1). Owns no failure state itself: the caller controls `open` and
  /// passes the title / human message / verbatim details.
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";

  type Props = {
    /// Two-way bound open state — the caller controls visibility.
    open: boolean;
    title: string;
    /// Plain-language framing shown above the verbatim block.
    message: string;
    /// The exact error text, rendered verbatim and offered for copy.
    details: string;
    onClose?: () => void;
  };

  let { open = $bindable(), title, message, details, onClose }: Props = $props();
</script>

<Dialog bind:open {title} {onClose} contentClass="max-w-lg">
  <div class="space-y-3" data-testid="error-details">
    <p class="text-muted text-xs">{message}</p>
    <div class="flex items-start gap-2">
      <pre
        class="bg-panel text-fg max-h-60 min-w-0 flex-1 overflow-auto rounded-md px-2.5 py-2 font-mono text-xs whitespace-pre-wrap"
        data-testid="error-details-text">{details}</pre>
      <CopyButton text={details} label="Copy error" testid="error-details-copy" class="shrink-0" />
    </div>
  </div>
</Dialog>
