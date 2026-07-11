<script lang="ts">
  /// Reasoning as a borderless row matching the tool-call rows: brain icon ·
  /// "Thinking" label · muted one-line preview · chevron. Collapsed it is
  /// chrome-free so a thinking block sits in the same visual set as the tool
  /// calls around it; expanded, the reasoning hangs under the row behind a
  /// thin left rule, directly on the reading surface — prose in a gray slab
  /// read as the heaviest block on screen. The preview hides while open (the
  /// body starts with the same line). The body (a full Markdown render of
  /// possibly-long reasoning) mounts only while open, same lazy contract as
  /// the tool row.
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import { cn } from "$lib/utils";
  import { Brain } from "@lucide/svelte";

  let { text }: { text: string } = $props();

  // The first non-empty line, for the collapsed glimpse. Reasoning reaches us
  // as a whole block (no harness delta-streams it), so there is nothing to
  // "watch build" — a one-line preview is all the closed state needs. Markdown
  // emphasis/heading markers are stripped: the preview renders as plain text,
  // where literal `**…**` is noise.
  const preview = $derived(
    (
      text
        .trim()
        .split("\n")
        .find((l) => l.trim() !== "") ?? ""
    ).replace(/[*_`#]/g, ""),
  );

  // Collapsed by default: reasoning is subordinate to the answer, so it stays
  // out of the way until the user opens it.
  let open = $state(false);
</script>

<div class="text-xs" data-testid="turn-thinking">
  <button
    type="button"
    class="hover:bg-hover flex min-h-7 w-full items-center gap-2 rounded-md px-1.5 py-1 text-left"
    aria-expanded={open}
    data-testid="thinking-row"
    onclick={() => (open = !open)}
  >
    <Brain class="text-muted h-3.5 w-3.5 shrink-0" aria-hidden="true" />
    <span class="text-fg shrink-0 font-medium">Thinking</span>
    {#if open}
      <span class="flex-1"></span>
    {:else}
      <span class="text-muted min-w-0 flex-1 truncate" data-testid="thinking-preview"
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
    <div class="border-border/70 mt-1 ml-[13px] border-l py-0.5 pl-4" data-testid="thinking-body">
      <!-- `markdown-thinking` mutes the body (incl. headings) so opened
           reasoning reads as subordinate to the answer; the base
           `.markdown-body` color would otherwise match the answer exactly. -->
      <Markdown {text} class="markdown-thinking" />
    </div>
  {/if}
</div>
