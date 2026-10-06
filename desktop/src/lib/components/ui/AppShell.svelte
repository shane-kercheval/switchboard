<script lang="ts">
  /// Two-pane horizontal layout (left | center), encoding the flex + overflow
  /// behavior once. App-level chrome (banners, footer, modals) stays in the
  /// consumer — this owns only the pane row.
  ///
  /// There is intentionally no right slot. The agents sidebar lives inside the
  /// center snippet so the title bar (which must span both the content column
  /// and the sidebar) can be a single element at the top of the center pane.
  /// A right slot at this level would make the title bar stop at the content
  /// column edge, leaving the sidebar without a header.
  import type { Snippet } from "svelte";

  type Props = {
    left: Snippet;
    center: Snippet;
    centerTestid?: string;
  };

  let { left, center, centerTestid }: Props = $props();
</script>

<div class="flex flex-1 overflow-hidden">
  {@render left()}
  <div class="bg-raised flex flex-1 flex-col overflow-hidden" data-testid={centerTestid}>
    {@render center()}
  </div>
</div>
