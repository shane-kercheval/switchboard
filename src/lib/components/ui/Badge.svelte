<script lang="ts">
  /// Small uppercase chip. Three variants, each token-driven so they theme in
  /// light and dark: `harness` (per-harness identity), `status` (run state),
  /// and `neutral` (default — incidental labels like "unavailable").
  import type { Snippet } from "svelte";
  import type { HarnessKind } from "$lib/types";
  import type { BadgeStatus } from "$lib/status";
  import { HARNESS_BADGE_TOKEN } from "$lib/harnessDisplay";
  import { cn } from "$lib/utils";

  /// Discriminated union: `harness`/`status` are required by their variant, so
  /// a misuse (e.g. `variant="harness"` without `harness`) is a compile error
  /// rather than a silent degrade to a blank neutral chip.
  type Props =
    | { variant: "harness"; harness: HarnessKind; class?: string; children: Snippet }
    | { variant: "status"; status: BadgeStatus; class?: string; children: Snippet }
    | { variant?: "neutral"; class?: string; children: Snippet };

  let props: Props = $props();

  const STATUS_TOKEN: Record<BadgeStatus, string> = {
    idle: "bg-status-idle-soft text-status-idle",
    processing: "bg-status-processing-soft text-status-processing",
    failed: "bg-status-failed-soft text-status-failed",
    cancelled: "bg-status-cancelled-soft text-status-cancelled",
  };

  const tone = $derived(
    props.variant === "harness"
      ? HARNESS_BADGE_TOKEN[props.harness]
      : props.variant === "status"
        ? STATUS_TOKEN[props.status]
        : "bg-panel text-muted",
  );
</script>

<span
  class={cn(
    "inline-flex items-center rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase",
    tone,
    props.class,
  )}
>
  {@render props.children()}
</span>
