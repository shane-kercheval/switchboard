<script lang="ts">
  /// Indeterminate progress ring (muted). Size, color, and any group-scoped
  /// show/hide behavior come from `class`. Decorative — the labelling is the
  /// caller's job (a button's aria-label, or a wrapping role="status").
  ///
  /// A closed ring rather than an arc glyph: rotation is centered on the box,
  /// and an arc's ink is not, so an arc spinner visibly orbits — Lucide's
  /// loader is 1.2px off center at a 14px render.
  import { cn } from "$lib/utils";

  let { class: className = "h-5 w-5" }: { class?: string } = $props();
</script>

<span
  class={cn("border-muted/30 border-t-muted block animate-spin rounded-full border-2", className)}
  aria-hidden="true"
></span>
