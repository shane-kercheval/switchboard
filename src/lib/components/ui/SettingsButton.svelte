<script lang="ts">
  import { cn } from "$lib/utils";
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { shortcut } from "$lib/platform";

  type Props = {
    label?: string;
    pressed?: boolean;
    testid?: string;
    onclick: () => void;
    class?: string;
  };

  let { label, pressed = false, testid, onclick, class: className }: Props = $props();

  const resolvedLabel = $derived(label ?? (pressed ? "Close settings" : "Open settings"));
</script>

<Tooltip label="Settings" shortcut={shortcut("mod", ",")} side="bottom">
  {#snippet trigger(props)}
    <button
      {...props}
      type="button"
      {onclick}
      aria-label={resolvedLabel}
      aria-pressed={pressed}
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
        <path
          d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"
        />
        <circle cx="12" cy="12" r="3" />
      </svg>
    </button>
  {/snippet}
</Tooltip>
