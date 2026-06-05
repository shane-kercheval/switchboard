<script lang="ts">
  import type { HTMLTextareaAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  type Props = HTMLTextareaAttributes & {
    autosize?: boolean;
    value?: string;
    ref?: HTMLTextAreaElement;
  };

  let {
    autosize = false,
    class: className,
    value = $bindable(""),
    ref = $bindable(),
    oninput,
    ...rest
  }: Props = $props();

  function resizeToContent(textarea: HTMLTextAreaElement | undefined): void {
    if (textarea == null) return;
    textarea.style.height = "auto";
    const naturalHeight = textarea.scrollHeight;
    const maxHeight = Number.parseFloat(getComputedStyle(textarea).maxHeight);
    const cappedHeight = Number.isFinite(maxHeight)
      ? Math.min(naturalHeight, maxHeight)
      : naturalHeight;
    textarea.style.height = `${cappedHeight}px`;
    textarea.style.overflowY = naturalHeight > cappedHeight ? "auto" : "hidden";
  }

  $effect(() => {
    if (!autosize || typeof value !== "string") return;
    resizeToContent(ref);
  });

  function handleInput(event: Event & { currentTarget: EventTarget & HTMLTextAreaElement }): void {
    if (autosize) resizeToContent(event.currentTarget);
    oninput?.(event);
  }
</script>

<textarea
  bind:this={ref}
  bind:value
  oninput={handleInput}
  class={cn(
    "border-border bg-raised w-full resize-none rounded-md border px-3 py-2 text-sm",
    "text-fg placeholder:text-muted",
    "focus-visible:ring-accent focus-visible:ring-2 focus-visible:outline-none",
    "disabled:bg-panel disabled:cursor-not-allowed disabled:opacity-50",
    className,
  )}
  {...rest}
></textarea>
