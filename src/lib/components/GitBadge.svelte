<script lang="ts">
  /// One git-status indicator, tone-mapped per the calm/one-tier scheme. The
  /// mapping logic lives in `gitBadges.ts`; this component only renders a compact
  /// icon with the shared custom tooltip treatment.
  import { cn } from "$lib/utils";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { badgeToneClass, type GitBadge } from "$lib/gitBadges";

  let { badge, testid }: { badge: GitBadge; testid?: string } = $props();

  function iconPath(key: string): string {
    switch (key) {
      case "uncommitted":
        return "M12 20h9 M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z";
      case "behind_base":
      case "behind":
        return "M12 5v14 M19 12l-7 7-7-7";
      case "ahead":
        return "M12 19V5 M5 12l7-7 7 7";
      case "diverged":
        return "M6 3v6a6 6 0 0 0 6 6h6 M18 9v6h-6 M18 15l-4-4";
      case "local_only":
        return "M12 19V5 M5 12l7-7 7 7 M5 19h14";
      case "dangling":
        return "M10 13a5 5 0 0 0 7.54.54l1.41-1.41a5 5 0 0 0-7.07-7.07L10.5 6.44 M14 11a5 5 0 0 0-7.54-.54L5.05 11.87a5 5 0 0 0 7.07 7.07l1.38-1.38 M4 4l16 16";
      case "upstream":
        return "M17 18a5 5 0 0 0-4.9-6h-.2A7 7 0 1 0 5 20h12a4 4 0 0 0 0-8 M9 16l2 2 4-4";
      case "merged":
        return "M20 6 9 17l-5-5";
      case "orphaned":
      case "prunable":
        return "M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z M12 9v4 M12 17h.01";
      case "remote_only":
        return "M17 18a5 5 0 0 0-4.9-6h-.2A7 7 0 1 0 5 20h12a4 4 0 0 0 0-8";
      default:
        return "M12 6v6l4 2";
    }
  }
</script>

<Tooltip side="top" delayDuration={250}>
  {#snippet trigger(props)}
    <span
      {...props}
      class={cn(
        "inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border",
        badgeToneClass(badge.tone),
      )}
      role="img"
      aria-label={badge.label}
      data-testid={testid}
      data-badge-key={badge.key}
    >
      <svg
        viewBox="0 0 24 24"
        class="h-3.5 w-3.5"
        fill="none"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d={iconPath(badge.key)} />
      </svg>
    </span>
  {/snippet}

  <div class="max-w-56">
    <div class="text-[13px] leading-4 font-medium">{badge.title}</div>
    <div class="text-primary-fg/70 mt-1 text-xs leading-4">{badge.description}</div>
  </div>
</Tooltip>
