<script lang="ts">
  /// Indeterminate progress ring (muted). Size and any group-scoped show/hide
  /// behavior come from `class`. Shared by the projects-sidebar busy indicator,
  /// the transcript cancel control's resting state, and running tool calls so
  /// the spinner stays identical across all three. Decorative — the labelling
  /// is the caller's job (a button's aria-label, or a wrapping role="status").
  import { cn } from "$lib/utils";

  let { class: className = "h-5 w-5" }: { class?: string } = $props();
</script>

<span
  class={cn("border-muted/30 border-t-muted block animate-spin rounded-full border-2", className)}
  aria-hidden="true"
></span>
