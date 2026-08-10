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
    widthProfile?: "rail" | "reading";
    width: number;
    testid?: string;
    children: Snippet;
  };

  let { side = "left", widthProfile = "rail", width, testid, children }: Props = $props();
</script>

<!-- These max-widths mirror `sidebarMaxWidth()` / `rightSidebarMaxWidth()` in
     layout.svelte.ts so the bound holds live as the window changes without
     rewriting the stored preference. Side controls the divider; width profile
     distinguishes a narrow navigation rail from the Pins reading surface. -->
<aside
  class={cn(
    "bg-panel relative flex shrink-0 flex-col",
    widthProfile === "reading"
      ? "max-w-[clamp(200px,60vw,960px)]"
      : "max-w-[clamp(200px,40vw,480px)]",
    side === "left" ? "border-border/80 border-r" : "border-border/80 border-l",
  )}
  style={`width: ${width}px`}
  data-testid={testid}
>
  {@render children()}
</aside>
