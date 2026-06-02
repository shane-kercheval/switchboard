<script lang="ts">
  import Disclosure from "$lib/components/ui/Disclosure.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";

  let { text }: { text: string } = $props();

  // The first non-empty line, for the collapsed header glimpse. Reasoning
  // reaches us as a whole block (no harness delta-streams it), so there is
  // nothing to "watch build" — a one-line preview is all the closed state needs.
  const preview = $derived(
    text
      .trim()
      .split("\n")
      .find((l) => l.trim() !== "") ?? "",
  );
</script>

<!-- Collapsed by default and left uncontrolled: reasoning is subordinate to the
     answer, so it stays out of the way until the user opens it. -->
<Disclosure testid="turn-thinking">
  {#snippet header()}
    <span class="text-muted shrink-0 text-[10px] font-semibold tracking-wide uppercase"
      >Thinking</span
    >
    <span class="text-muted min-w-0 truncate" data-testid="thinking-preview">{preview}</span>
  {/snippet}

  <div class="border-border/70 border-t px-2.5 py-2" data-testid="thinking-body">
    <!-- `markdown-thinking` mutes the body (incl. headings) so opened reasoning
         reads as subordinate to the answer; the base `.markdown-body` color
         would otherwise match the answer exactly. -->
    <Markdown {text} class="markdown-thinking" />
  </div>
</Disclosure>
