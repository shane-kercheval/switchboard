<script lang="ts">
  /// Small status pill used across the transcript for a turn's terminal/live
  /// state. Maps a status to its `status-*` token pair (soft bg + strong fg);
  /// `queued` shares the processing palette (both are "live, working" states).
  /// Defaults its label to the status word; pass children to override.
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  type Status = "processing" | "queued" | "failed" | "cancelled";

  type Props = {
    status: Status;
    testid?: string;
    class?: string;
    children?: Snippet;
  };

  let { status, testid, class: className, children }: Props = $props();

  const TOKENS: Record<Status, string> = {
    processing: "bg-status-processing-soft text-status-processing",
    queued: "bg-status-processing-soft text-status-processing",
    failed: "bg-status-failed-soft text-status-failed",
    cancelled: "bg-status-cancelled-soft text-status-cancelled",
  };
</script>

<span
  class={cn(
    "inline-flex items-center rounded px-1.5 py-0.5 text-xs font-medium",
    TOKENS[status],
    className,
  )}
  data-testid={testid}
>
  {#if children}{@render children()}{:else}{status}{/if}
</span>
