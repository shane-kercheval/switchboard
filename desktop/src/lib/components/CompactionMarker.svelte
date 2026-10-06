<script lang="ts">
  /// A compaction marker as a borderless row matching the tool-call and
  /// thinking rows: history icon · "Conversation compacted" label · muted
  /// one-line glimpse of the recap · chevron. No status glyph — a compaction
  /// has no success/error state. Collapsed by default: the recap is a large
  /// verbatim harness block the user rarely needs, and it mounts only while
  /// open, hanging under the row behind the same thin left rule.
  import { cn } from "$lib/utils";
  import { History } from "@lucide/svelte";

  let { summary }: { summary: string } = $props();

  const preview = $derived(
    summary
      .trim()
      .split("\n")
      .find((l) => l.trim() !== "") ?? "",
  );

  let open = $state(false);
</script>

<div class="text-xs" data-testid="compaction-marker">
  <button
    type="button"
    class="hover:bg-hover flex min-h-7 w-full items-center gap-2 rounded-md px-1.5 py-1 text-left"
    aria-expanded={open}
    data-testid="compaction-row"
    onclick={() => (open = !open)}
  >
    <History class="text-muted h-3.5 w-3.5 shrink-0" aria-hidden="true" />
    <span class="text-fg shrink-0 font-medium">Conversation compacted</span>
    {#if open}
      <span class="flex-1"></span>
    {:else}
      <span class="text-muted min-w-0 flex-1 truncate font-mono" data-testid="compaction-preview"
        >{preview}</span
      >
    {/if}
    <span
      class={cn(
        "text-muted flex h-4 w-4 shrink-0 items-center justify-center transition-transform",
        open && "rotate-90",
      )}
      aria-hidden="true">›</span
    >
  </button>

  {#if open}
    <div class="border-border/70 mt-1 ml-[13px] border-l py-0.5 pl-4" data-testid="compaction-body">
      <pre
        class="text-muted bg-panel max-h-44 overflow-y-auto rounded px-2 py-1.5 font-mono whitespace-pre-wrap">{summary}</pre>
    </div>
  {/if}
</div>
