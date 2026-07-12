<script lang="ts">
  /// A fixed-width side panel shell (the `<aside>` chrome shared by the
  /// projects and agents sidebars): token background, a divider on the inner
  /// edge, vertical layout. Content — including `SidebarSection`s — goes in
  /// `children`. The `side` picks which edge gets the border.
  ///
  /// Width is a pixel number (typically the persisted layout store's value, or
  /// a drag draft). The shell is positioned so a consumer can overlay a
  /// `ResizeHandle` on the inner edge from inside `children`.
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  type Props = {
    side?: "left" | "right";
    width: number;
    testid?: string;
    children: Snippet;
  };

  let { side = "left", width, testid, children }: Props = $props();
</script>

<!-- The max-width mirrors `sidebarMaxWidth()` (layout.svelte.ts) in CSS so the
     bound holds *live*: a width persisted on a large monitor is capped the
     moment the window shrinks, and re-expands when it grows — the stored
     preference is never rewritten. Keep the two formulas in sync. -->
<aside
  class={cn(
    "bg-panel relative flex max-w-[clamp(200px,40vw,480px)] shrink-0 flex-col",
    side === "left" ? "border-border/80 border-r" : "border-border/80 border-l",
  )}
  style={`width: ${width}px`}
  data-testid={testid}
>
  {@render children()}
</aside>
