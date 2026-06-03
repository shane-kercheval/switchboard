<script lang="ts">
  /// One git-status badge, tone-mapped per the D2 calm/one-tier scheme. Pure
  /// presentation over a `GitBadge` from `gitBadges.ts`; the mapping logic lives
  /// there so it's testable without rendering.
  import { cn } from "$lib/utils";
  import { badgeToneClass, type GitBadge } from "$lib/gitBadges";

  let { badge, testid }: { badge: GitBadge; testid?: string } = $props();
</script>

<span
  class={cn(
    "inline-flex items-center rounded px-1.5 py-0.5 text-[10px] font-semibold",
    badgeToneClass(badge.tone),
  )}
  title={badge.title}
  data-testid={testid}
  data-badge-key={badge.key}
>
  {badge.label}
</span>
