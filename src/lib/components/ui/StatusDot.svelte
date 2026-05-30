<script lang="ts">
  /// Small filled dot signalling run state, token-driven so it themes in light
  /// and dark. Used where a label would be too heavy (e.g. the "background
  /// activity" indicator on a non-active project row).
  import { cn } from "$lib/utils";
  import type { BadgeStatus } from "$lib/status";

  type Props = {
    status: BadgeStatus;
    /// When set, the dot is the sole status signal: exposes an accessible name
    /// + tooltip. When omitted, the dot is decorative (a sibling text label
    /// carries the meaning) and is hidden from assistive tech.
    label?: string;
    testid?: string;
    class?: string;
  };

  let { status, label, testid, class: className }: Props = $props();

  const DOT: Record<BadgeStatus, string> = {
    idle: "bg-status-idle",
    processing: "bg-status-processing",
    failed: "bg-status-failed",
    cancelled: "bg-status-cancelled",
  };
</script>

<span
  class={cn("inline-block h-1.5 w-1.5 shrink-0 rounded-full", DOT[status], className)}
  data-testid={testid}
  title={label}
  aria-label={label}
  aria-hidden={label ? undefined : "true"}
  role={label ? "img" : undefined}
></span>
