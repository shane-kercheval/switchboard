<script lang="ts">
  /// Indeterminate progress ring (muted). Size, color, and any group-scoped
  /// show/hide behavior come from `class`. Decorative — the labelling is the
  /// caller's job (a button's aria-label, or a wrapping role="status").
  ///
  /// A closed ring rather than an arc glyph: rotation is centered on the box,
  /// and an arc's ink is not, so an arc spinner visibly orbits — Lucide's
  /// loader is 1.2px off center at a 14px render.
  ///
  /// `stillWhenReducedMotion` is for a spinner that marks a *state* rather than
  /// a wait (an agent at work): under `prefers-reduced-motion` it stops and
  /// draws a full circle. It takes its color from `currentColor`, so the caller
  /// sets a `text-*` color instead of border colors. Plain loaders keep
  /// spinning: a still ring beside no text reads as a stuck loader.
  import { cn } from "$lib/utils";

  let {
    class: className = "h-5 w-5",
    stillWhenReducedMotion = false,
  }: { class?: string; stillWhenReducedMotion?: boolean } = $props();
</script>

<span
  class={cn(
    "block animate-spin rounded-full border-2",
    stillWhenReducedMotion
      ? "border-current/25 border-t-current motion-reduce:animate-none motion-reduce:border-current"
      : "border-muted/30 border-t-muted",
    className,
  )}
  aria-hidden="true"
></span>
