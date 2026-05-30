<script lang="ts">
  /// Small icon button that copies `text` and briefly swaps its copy glyph for a
  /// green checkmark on success (the affordance used on code blocks and the
  /// resume dialog). Confirms only after the clipboard write resolves, so a
  /// failed write doesn't show a false "Copied".
  import { copyText } from "$lib/native";
  import { cn } from "$lib/utils";

  let {
    text,
    label = "Copy",
    testid,
    class: className = "",
  }: { text: string; label?: string; testid?: string; class?: string } = $props();

  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => () => clearTimeout(timer));

  function copy(): void {
    void copyText(text)
      .then(() => {
        copied = true;
        clearTimeout(timer);
        timer = setTimeout(() => (copied = false), 1000);
      })
      .catch((err: unknown) => {
        console.error("[switchboard] copy failed", err);
      });
  }
</script>

<button
  type="button"
  class={cn(
    "flex h-7 w-7 items-center justify-center rounded-full border border-transparent transition-colors",
    copied ? "text-accent bg-panel" : "text-muted hover:text-fg hover:bg-border/60",
    className,
  )}
  data-testid={testid}
  data-copied={copied ? "true" : undefined}
  aria-label={copied ? "Copied" : label}
  onclick={copy}
>
  {#if copied}
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      class="h-[17px] w-[17px]"
      aria-hidden="true"
    >
      <path d="M20 6 9 17l-5-5" />
    </svg>
  {:else}
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      class="h-[17px] w-[17px]"
      aria-hidden="true"
    >
      <rect x="9" y="9" width="11" height="11" rx="2.5" />
      <path
        d="M14 9V5.5A2.5 2.5 0 0 0 11.5 3H5.5A2.5 2.5 0 0 0 3 5.5V11.5A2.5 2.5 0 0 0 5.5 14H9"
      />
    </svg>
  {/if}
</button>
