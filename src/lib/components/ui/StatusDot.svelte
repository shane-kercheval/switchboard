<script lang="ts">
  /// Small filled dot signalling run state, token-driven so it themes in light
  /// and dark. Used where a label would be too heavy (e.g. the "background
  /// activity" indicator on a non-active project row).
  ///
  /// `"warning"` is accepted alongside the run statuses and names the caution role
  /// rather than a run state — an MCP server the harness reports as
  /// `needs-auth` is not "failed", it is something the user can fix. It is
  /// deliberately **not** added to `BadgeStatus`: that type is the run-state
  /// vocabulary, and `app.css` keeps `warning` a separate role precisely so
  /// re-tuning run colors cannot drag warnings along.
  import { cn } from "$lib/utils";
  import type { BadgeStatus } from "$lib/status";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";

  type Props = {
    status: BadgeStatus | "warning";
    /// When set, the dot is the sole status signal: exposes an accessible name
    /// + tooltip. When omitted, the dot is decorative (a sibling text label
    /// carries the meaning) and is hidden from assistive tech.
    label?: string;
    testid?: string;
    class?: string;
  };

  let { status, label, testid, class: className }: Props = $props();

  const DOT: Record<BadgeStatus | "warning", string> = {
    idle: "bg-status-idle",
    processing: "bg-status-processing",
    failed: "bg-status-failed",
    cancelled: "bg-status-cancelled",
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
  <Tooltip {label}>{#snippet trigger(props)}{@render dot(props)}{/snippet}</Tooltip>
{:else}
  {@render dot()}
{/if}
