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

  // The max-height cap comes from the instance's classes and differs per
  // consumer (compose bar vs. prompt composer), so it is read once per instance
  // — never module-level — and cached instead of paying a `getComputedStyle`
  // per keystroke. A capless instance caches `NaN` the same way. Assumes the
  // cap — and `autosize` itself — are static for the instance's lifetime; a
  // caller that varies either at runtime must also reset `lastResized` and the
  // inline height.
  let maxHeight: number | undefined;

  // Two distinct resize triggers, deduped: the input handler is the synchronous
  // typing path (it fires first, so it does the measuring), and the value
  // effect covers programmatic changes (send-clear, draft restoration). This
  // guard makes the overlap free — when a keystroke fires both, only the first
  // measures, so a value change costs exactly ONE forced layout (the
  // height-reset write + `scrollHeight` read below flush layout synchronously
  // for the whole document; making that flush cheap is the rest of the page's
  // job, so don't delete the resize thinking it's free). Neither trigger is
  // redundant: typing is the only way the DOM text can change without a
  // reactive signal (a non-reactive binding never propagates programmatic
  // writes to the DOM at all), and the input path covers exactly that case —
  // together the two are sufficient for every binding mode.
  //
  // The guard assumes height depends only on value — a width-driven re-wrap
  // needs `lastResized` cleared before resizing — and, like the cap cache, that
  // the <textarea> element lives exactly as long as this component instance.
  let lastResized: string | undefined;

  function resizeToContent(textarea: HTMLTextAreaElement | undefined): void {
    if (textarea == null || textarea.value === lastResized) return;
    lastResized = textarea.value;
    textarea.style.height = "auto";
    const naturalHeight = textarea.scrollHeight;
    maxHeight ??= Number.parseFloat(getComputedStyle(textarea).maxHeight);
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
