<script lang="ts">
  /// A fixed-width side panel shell (the `<aside>` chrome shared by the
  /// projects and agents sidebars): token background, a divider on the inner
  /// edge, vertical layout. Content — including `SidebarSection`s — goes in
  /// `children`. The `side` picks which edge gets the border.
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  type Props = {
    side?: "left" | "right";
    /// A Tailwind width class (e.g. `w-72`). Kept as a prop because the two
    /// sidebars use different widths.
    width: string;
    testid?: string;
    children: Snippet;
  };

  let { side = "left", width, testid, children }: Props = $props();
</script>

<aside
  class={cn(
    "bg-panel flex shrink-0 flex-col",
    width,
    side === "left" ? "border-border/80 border-r" : "border-border/80 border-l",
  )}
  data-testid={testid}
>
  {@render children()}
</aside>
