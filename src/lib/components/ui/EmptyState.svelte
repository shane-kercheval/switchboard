<script lang="ts">
  /// Centered placeholder for empty / loading / error center-pane states.
  /// Token-driven; `tone="error"` colors the title with the failed status
  /// token. An optional `action` snippet holds a retry/CTA control.
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  type Props = {
    title: string;
    description?: string;
    tone?: "default" | "error";
    testid?: string;
    action?: Snippet;
  };

  let { title, description, tone = "default", testid, action }: Props = $props();
</script>

<div
  class="flex flex-1 flex-col items-center justify-center gap-3 p-8 text-center"
  data-testid={testid}
>
  <p class={cn("text-sm", tone === "error" ? "text-status-failed" : "text-muted")}>{title}</p>
  {#if description}
    <p class="text-muted max-w-md text-xs">{description}</p>
  {/if}
  {#if action}
    {@render action()}
  {/if}
</div>
