<script lang="ts">
  import { cn } from "$lib/utils";
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";

  type Props = {
    side: "left" | "right";
    expanded: boolean;
    label: string;
    testid?: string;
    onclick: () => void;
    class?: string;
  };

  let { side, expanded, label, testid, onclick, class: className }: Props = $props();
</script>

<button
  type="button"
  {onclick}
  title={label}
  aria-label={label}
  aria-expanded={expanded}
  data-testid={testid}
  data-tauri-no-drag
  class={cn(ICON_BUTTON_CLASS, className)}
>
  <svg
    width={ICON_SIZE}
    height={ICON_SIZE}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
    shape-rendering="geometricPrecision"
    aria-hidden="true"
  >
    <rect x="5" y="4" width="14" height="16" rx="1.75" />
    <path d={side === "left" ? "M10 4.5v15" : "M14 4.5v15"} />
  </svg>
</button>
