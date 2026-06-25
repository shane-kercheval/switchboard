<script lang="ts">
  /// One git-status indicator: an unframed Lucide icon with a tooltip. The
  /// status mapping lives in `gitStatusIndicators.ts`; this component owns only
  /// icon choice and shared visual treatment.
  import {
    ArrowDown,
    ArrowUp,
    Cloud,
    CloudOff,
    GitCompareArrows,
    GitFork,
    GitMerge,
    HardDrive,
    PencilLine,
    TriangleAlert,
  } from "@lucide/svelte";
  import { cn } from "$lib/utils";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { indicatorToneClass, type GitStatusIndicator } from "$lib/gitStatusIndicators";

  const GIT_ICON_SIZE = 16;
  const GIT_PENCIL_ICON_SIZE = 14;
  const GIT_ICON_STROKE = 1.8;

  let { indicator, testid }: { indicator: GitStatusIndicator; testid?: string } = $props();
</script>

<Tooltip side="top" delayDuration={0} skipDelayDuration={0} disableHoverableContent>
  {#snippet trigger(props)}
    <!-- A read-only status indicator: footprint + tone color only, no hover
         affordance (background or icon-color change) — a hover effect would
         imply it's clickable when it isn't. The tooltip carries the meaning. -->
    <span
      {...props}
      class={cn(
        "inline-flex h-[26px] w-[26px] shrink-0 items-center justify-center",
        indicatorToneClass(indicator.tone),
      )}
      aria-label={indicator.label}
      data-testid={testid}
      data-indicator-key={indicator.key}
    >
      {#if indicator.key === "uncommitted"}
        <PencilLine size={GIT_PENCIL_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else if indicator.key === "behind_base" || indicator.key === "behind"}
        <ArrowDown size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else if indicator.key === "ahead"}
        <ArrowUp size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else if indicator.key === "diverged"}
        <GitCompareArrows size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else if indicator.key === "local_only"}
        <HardDrive size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else if indicator.key === "dangling"}
        <CloudOff size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else if indicator.key === "upstream" || indicator.key === "remote_only"}
        <Cloud size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else if indicator.key === "merged"}
        <GitMerge size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else if indicator.key === "orphaned" || indicator.key === "prunable"}
        <TriangleAlert size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {:else}
        <GitFork size={GIT_ICON_SIZE} strokeWidth={GIT_ICON_STROKE} aria-hidden="true" />
      {/if}
    </span>
  {/snippet}

  <div class="max-w-56">
    <div class="text-[13px] leading-4 font-medium">{indicator.title}</div>
    <div class="text-primary-fg/70 mt-1 text-xs leading-4">{indicator.description}</div>
  </div>
</Tooltip>
