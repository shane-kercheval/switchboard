<script lang="ts">
  /// One-line text that exposes its full value through the app tooltip only
  /// when CSS has actually clipped it. ResizeObserver matters for flex rows:
  /// sibling controls can appear on hover and take space without changing the
  /// viewport or the text itself.
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import { cn } from "$lib/utils";

  let {
    text,
    class: className,
    testid,
    side = "top",
  }: {
    text: string;
    class?: string;
    testid?: string;
    side?: "top" | "bottom" | "left" | "right";
  } = $props();

  let truncated = $state(false);

  function observeOverflow(
    node: HTMLElement,
    _text: string,
  ): { update: (nextText: string) => void; destroy: () => void } {
    const measure = (): void => {
      truncated = node.scrollWidth - node.clientWidth > 1;
    };
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    node.addEventListener("pointerenter", measure);
    measure();
    return {
      update: () => queueMicrotask(measure),
      destroy: () => {
        observer.disconnect();
        node.removeEventListener("pointerenter", measure);
      },
    };
  }
</script>

<Tooltip
  label={text}
  {side}
  delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
  focusable={false}
  disabled={!truncated}
>
  {#snippet trigger(props)}
    <span
      {...props}
      use:observeOverflow={text}
      class={cn("min-w-0 truncate", className)}
      data-testid={testid}
      data-truncated={truncated}
    >
      {text}
    </span>
  {/snippet}
</Tooltip>
