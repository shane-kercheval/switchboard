<script lang="ts">
  // The ⊘ "clear / remove" glyph, shared by every clear affordance (the recipient
  // "Clear", the @-menu clear row, and the per-field forward-source clears) so the
  // mark is defined once. Color comes from the parent via `currentColor`; pass
  // `class` only to override it (e.g. a muted row icon).
  let { size = 16, class: className }: { size?: number; class?: string } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.75"
  stroke-linecap="round"
  class={className}
  aria-hidden="true"
>
  <circle cx="12" cy="12" r="9" />
  <path d="m5.6 5.6 12.8 12.8" />
</svg>
