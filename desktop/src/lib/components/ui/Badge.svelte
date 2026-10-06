<script lang="ts">
  /// Small uppercase neutral chip for incidental labels (e.g. an unknown
  /// transcript item kind). Token-driven so it themes in light and dark.
  /// Harness identity renders via `HarnessIcon`, and run state via `StatusDot`
  /// + the `status-*` tokens — not this component.
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  type Props = {
    class?: string;
    testid?: string;
    children: Snippet;
  };

  let { class: className, testid, children }: Props = $props();
</script>

<span
  class={cn(
    "bg-panel text-muted inline-flex items-center rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase",
    className,
  )}
  data-testid={testid}
>
  {@render children()}
</span>
