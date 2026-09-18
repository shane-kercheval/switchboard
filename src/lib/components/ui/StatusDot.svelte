<script lang="ts">
  /// Small filled dot signalling run state, token-driven so it themes in light
  /// and dark. Used where a label would be too heavy (e.g. the "background
  /// activity" indicator on a non-active project row).
  ///
  /// `"success"` and `"warning"` are accepted alongside the run statuses for
  /// healthy and caution states that are not themselves agent run states.
  import { cn } from "$lib/utils";
  import type { BadgeStatus } from "$lib/status";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";

  type Props = {
    status: BadgeStatus | "success" | "warning";
    /// When set, the dot is the sole status signal: exposes an accessible name
    /// + tooltip. When omitted, the dot is decorative (a sibling text label
    /// carries the meaning) and is hidden from assistive tech.
    label?: string;
    /// Whether a labelled dot joins the Tab order. Keep this false when the
    /// dot sits inside a focus-managed surface such as a popover; its label
    /// remains exposed to assistive technology and its tooltip still opens on
    /// hover.
    focusable?: boolean;
    testid?: string;
    class?: string;
  };

  let { status, label, focusable = true, testid, class: className }: Props = $props();

  const DOT: Record<BadgeStatus | "success" | "warning", string> = {
    idle: "bg-status-idle",
    processing: "bg-status-processing",
    failed: "bg-status-failed",
    cancelled: "bg-status-cancelled",
    success: "bg-accent",
    warning: "bg-warning",
  };
</script>

{#snippet dot(props: Record<string, unknown> = {})}
  <span
    {...props}
    class={cn("inline-block h-1.5 w-1.5 shrink-0 rounded-full", DOT[status], className)}
    data-testid={testid}
    aria-label={label}
    aria-hidden={label ? undefined : "true"}
    role={label ? "img" : undefined}
  ></span>
{/snippet}

{#if label}
  <Tooltip {label} {focusable}>{#snippet trigger(props)}{@render dot(props)}{/snippet}</Tooltip>
{:else}
  {@render dot()}
{/if}
