<script lang="ts">
  /// Icon button for async side effects: normal icon → spinner while pending →
  /// green check after success. Used for git actions that launch native apps or
  /// copy to the clipboard, where an unchanged icon reads like a missed click.
  import type { Snippet } from "svelte";
  import type { HTMLButtonAttributes } from "svelte/elements";
  import { Check } from "@lucide/svelte";
  import { cn } from "$lib/utils";
  import Spinner from "$lib/components/ui/Spinner.svelte";

  type Props = Omit<HTMLButtonAttributes, "onclick" | "type"> & {
    action: () => Promise<void>;
    label: string;
    testid?: string;
    children: Snippet;
    onError?: (error: unknown) => void;
    completeAfterMs?: number;
  };

  let {
    action,
    label,
    testid,
    children,
    onError,
    completeAfterMs,
    class: className,
    disabled = false,
    ...rest
  }: Props = $props();

  let state = $state<"idle" | "pending" | "done">("idle");
  let doneTimer: ReturnType<typeof setTimeout> | undefined;
  let optimisticTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    return () => {
      clearTimeout(doneTimer);
      clearTimeout(optimisticTimer);
    };
  });

  function markDone(): void {
    clearTimeout(doneTimer);
    clearTimeout(optimisticTimer);
    state = "done";
    doneTimer = setTimeout(() => {
      state = "idle";
    }, 1000);
  }

  function run(event: MouseEvent): void {
    if (disabled || state === "pending") return;
    if (event.detail > 0) {
      (event.currentTarget as HTMLButtonElement).blur();
    }
    clearTimeout(doneTimer);
    clearTimeout(optimisticTimer);
    state = "pending";
    if (completeAfterMs !== undefined) {
      optimisticTimer = setTimeout(markDone, completeAfterMs);
    }
    void action()
      .then(() => {
        markDone();
      })
      .catch((error: unknown) => {
        clearTimeout(optimisticTimer);
        clearTimeout(doneTimer);
        state = "idle";
        onError?.(error);
      });
  }
</script>

<button
  {...rest}
  type="button"
  class={cn(
    "inline-flex h-[26px] w-[26px] items-center justify-center rounded-full transition-colors",
    className,
    (disabled || state === "pending") && "cursor-not-allowed opacity-50",
    state === "done"
      ? "bg-accent-soft text-accent hover:bg-accent-soft hover:text-accent"
      : "text-muted hover:text-fg hover:bg-border/60",
  )}
  data-testid={testid}
  data-state={state}
  aria-label={state === "done" ? "Done" : label}
  aria-disabled={disabled || state === "pending" ? "true" : undefined}
  onclick={run}
>
  {#if state === "pending"}
    <Spinner class="h-4 w-4" />
  {:else if state === "done"}
    <Check size={16} strokeWidth={2.5} aria-hidden="true" />
  {:else}
    {@render children()}
  {/if}
</button>
