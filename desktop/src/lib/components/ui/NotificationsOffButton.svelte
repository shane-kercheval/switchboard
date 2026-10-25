<script lang="ts">
  import { BellOff } from "@lucide/svelte";
  import { cn } from "$lib/utils";
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { shortcut } from "$lib/platform";

  /// Shown in the header only while the master notification switch is off: the
  /// switch has no visible effect of its own, so this is both the reminder that
  /// it is off and the way back on.
  type Props = {
    testid?: string;
    onclick: () => void;
    class?: string;
  };

  let { testid, onclick, class: className }: Props = $props();
</script>

<Tooltip
  label="Notifications are off. Click to turn them on"
  shortcut={shortcut("mod", "shift", "M")}
  side="bottom"
>
  {#snippet trigger(props)}
    <button
      {...props}
      type="button"
      {onclick}
      aria-label="Turn notifications on"
      data-testid={testid}
      data-tauri-no-drag
      class={cn(ICON_BUTTON_CLASS, "text-muted", className)}
    >
      <BellOff size={ICON_SIZE} strokeWidth={1.5} aria-hidden="true" />
    </button>
  {/snippet}
</Tooltip>
