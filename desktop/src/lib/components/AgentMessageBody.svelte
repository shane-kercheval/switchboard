<script lang="ts">
  import type { Turn } from "$lib/state/index.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import FindingsCard from "$lib/components/FindingsCard.svelte";
  import ThinkingWidget from "$lib/components/ThinkingWidget.svelte";
  import ToolCallWidget from "$lib/components/ToolCallWidget.svelte";
  import { rendersAsFindingsCard } from "$lib/findings";

  type AgentTurn = Extract<Turn, { role: "agent" }>;

  let { turn, settled = true }: { turn: AgentTurn; settled?: boolean } = $props();
</script>

{#each turn.items as item, i (i)}
  {#if item.item_kind === "text"}
    {#if item.kind === "thinking"}
      <ThinkingWidget text={item.text} />
    {:else}
      <Markdown text={item.text} />
    {/if}
  {:else if item.facet.facet_kind === "findings" && rendersAsFindingsCard(item, settled)}
    <FindingsCard facet={item.facet} />
  {:else}
    <ToolCallWidget tool={item} turnSettled={settled} />
  {/if}
{/each}

{#if settled && turn.status === "failed" && turn.error}
  <div class="text-status-failed text-xs" data-testid="turn-error">{turn.error}</div>
{/if}
