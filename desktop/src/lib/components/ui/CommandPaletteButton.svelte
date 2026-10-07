<script lang="ts">
  import { cn } from "$lib/utils";
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { shortcut } from "$lib/platform";

  type Props = {
    testid?: string;
    onclick: () => void;
    class?: string;
  };

  let { testid, onclick, class: className }: Props = $props();
</script>

<Tooltip label="Command palette" shortcut={shortcut("mod", "shift", "P")} side="bottom">
  {#snippet trigger(props)}
    <button
      {...props}
      type="button"
      {onclick}
      aria-label="Open command palette"
      data-testid={testid}
      data-tauri-no-drag
      class={cn(ICON_BUTTON_CLASS, className)}
    >
      <svg
        width={ICON_SIZE}
        height={ICON_SIZE}
        viewBox="-3 -3 30 30"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        shape-rendering="geometricPrecision"
        aria-hidden="true"
      >
        <path d="M15 6v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3V6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3" />
      </svg>
    </button>
  {/snippet}
</Tooltip>
