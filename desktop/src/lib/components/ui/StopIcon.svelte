<script lang="ts">
  /// Filled stop square (cancel / halt affordance). Shared by the compose-bar
  /// stop button and the transcript / projects-sidebar cancel controls so the
  /// glyph stays identical across all three.
  let { class: className = "h-5 w-5" }: { class?: string } = $props();
</script>

<svg viewBox="0 0 24 24" fill="currentColor" class={className} aria-hidden="true">
  <rect x="7" y="7" width="10" height="10" rx="2" />
</svg>
