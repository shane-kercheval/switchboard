<script lang="ts">
  import type { AgentRecord, ConversationItem } from "$lib/types";
  import { transcripts, type Turn } from "$lib/state/index.svelte";
  import { buildUnifiedRows } from "$lib/state/unified";
  import { cn } from "$lib/utils";
  import { HARNESS_BADGE_CLASS, HARNESS_LABEL } from "$lib/harnessDisplay";

  /// `agents` is the active project's roster (for attribution + flattening
  /// their per-agent transcripts). `overlay` is the project's hydrated
  /// journal items (user messages + outcome markers). `loadStatus` drives the
  /// first-activation loading indicator and the project-load-failed state.
  let {
    agents,
    overlay = [],
    loadStatus = "complete",
  }: {
    agents: AgentRecord[];
    overlay?: ConversationItem[];
    loadStatus?: "pending" | "loading" | "complete" | "failed";
  } = $props();

  const agentById = $derived.by(() => {
    const map: Record<string, AgentRecord> = {};
    for (const a of agents) map[a.id] = a;
    return map;
  });

  /// The active project's per-agent turns (live + hydrated agent content and
  /// this-session user turns), merged with the journal overlay into one
  /// chronological row list. The merge applies the `(timestamp, kind_rank)`
  /// tiebreak so a failed/cancelled marker never sorts above its own prompt.
  const rows = $derived.by(() => {
    const turns: Turn[] = [];
    for (const agent of agents) {
      const slice = transcripts[agent.id] ?? [];
      for (const turn of slice) turns.push(turn);
    }
    return buildUnifiedRows(turns, overlay);
  });

  function agentName(agentId: string): string {
    return agentById[agentId]?.name ?? "unknown";
  }

  function harnessBadgeClass(agentId: string): string {
    const harness = agentById[agentId]?.harness;
    return harness ? HARNESS_BADGE_CLASS[harness] : "bg-neutral-100 text-neutral-800";
  }

  function harnessLabel(agentId: string): string {
    const harness = agentById[agentId]?.harness;
    return harness ? HARNESS_LABEL[harness] : "?";
  }

  function recipientNames(agentIds: string[]): string {
    return agentIds.map(agentName).join(" | ");
  }

  // Auto-pin to bottom unless the user has scrolled up.
  let container = $state<HTMLDivElement | null>(null);
  let pinned = $state<boolean>(true);

  function onScroll(): void {
    if (!container) return;
    const distanceFromBottom =
      container.scrollHeight - container.scrollTop - container.clientHeight;
    pinned = distanceFromBottom < 32;
  }

  /// Fine-grained scroll signal — reads mutated fields (text, output,
  /// completed_at) so streaming in-place mutations (which leave row/item
  /// counts constant) still trigger the auto-scroll effect.
  const scrollSignal = $derived.by(() => {
    let n = rows.length;
    for (const row of rows) {
      if (row.kind === "user") {
        n += row.text.length;
      } else if (row.kind === "outcome") {
        n += row.reason?.length ?? 0;
      } else {
        n += row.turn.items.length;
        for (const item of row.turn.items) {
          if (item.item_kind === "text") {
            n += item.text.length;
          } else {
            if (item.completed_at !== undefined) n += 1;
            n += item.output?.length ?? 0;
          }
        }
      }
    }
    return n;
  });

  $effect(() => {
    void scrollSignal;
    if (pinned && container) {
      container.scrollTop = container.scrollHeight;
    }
  });
</script>

<div
  bind:this={container}
  onscroll={onScroll}
  data-testid="unified-transcript"
  class="flex-1 overflow-y-auto p-4"
>
  {#if loadStatus === "loading"}
    <p class="mb-3 text-xs text-neutral-500 italic" data-testid="transcript-loading">
      Loading history…
    </p>
  {:else if loadStatus === "failed"}
    <p class="mb-3 text-xs text-red-700" data-testid="transcript-load-failed">
      Couldn't load this project's conversation history.
    </p>
  {/if}

  {#if rows.length === 0 && loadStatus !== "loading"}
    <p class="text-sm text-neutral-500">No messages yet. Type a prompt below.</p>
  {/if}

  <div class="space-y-4">
    {#each rows as row (row.key)}
      {#if row.kind === "user"}
        <div class="space-y-1" data-testid="turn" data-role="user">
          <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
            <span class="text-neutral-500">You</span>
            <span class="text-neutral-400">→</span>
            <span class="font-mono text-neutral-700" data-testid="turn-recipient">
              {recipientNames(row.agent_ids)}
            </span>
          </div>
          <div class="text-sm whitespace-pre-wrap text-neutral-900">{row.text}</div>
        </div>
      {:else if row.kind === "outcome"}
        <div
          class="flex items-center gap-2 text-xs"
          data-testid="turn-outcome"
          data-status={row.status}
        >
          <span class="font-mono text-neutral-700">{agentName(row.agent_id)}</span>
          {#if row.status === "cancelled"}
            <span class="text-neutral-500" data-testid="outcome-cancelled">cancelled</span>
          {:else}
            <span class="text-red-700" data-testid="outcome-failed">failed</span>
          {/if}
          {#if row.reason}
            <span class="text-neutral-500">— {row.reason}</span>
          {/if}
        </div>
      {:else}
        {@const turn = row.turn}
        <div class="space-y-1" data-testid="turn" data-role="agent">
          <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
            <span class="font-mono text-neutral-700" data-testid="turn-agent-name">
              {agentName(turn.agent_id)}
            </span>
            <span class={cn("rounded px-1.5 py-0.5 text-[10px]", harnessBadgeClass(turn.agent_id))}>
              {harnessLabel(turn.agent_id)}
            </span>
            {#if turn.status === "streaming"}
              <span class="text-amber-700" data-testid="turn-streaming">streaming…</span>
            {:else if turn.status === "failed"}
              <span class="text-red-700">failed</span>
            {:else if turn.status === "cancelled"}
              <span class="text-neutral-500">cancelled</span>
            {/if}
          </div>
          {#if turn.status === "streaming" && turn.items.length === 0}
            <div class="text-xs text-neutral-500 italic" data-testid="turn-processing">
              processing…
            </div>
          {/if}
          {#each turn.items as item, i (i)}
            {#if item.item_kind === "text"}
              <div class="text-sm whitespace-pre-wrap text-neutral-800">{item.text}</div>
            {:else}
              <div
                class="rounded border border-neutral-200 bg-neutral-50 p-2 text-xs"
                data-testid="turn-tool"
                data-tool-use-id={item.tool_use_id}
              >
                <div class="flex items-center gap-1.5 font-semibold text-neutral-700">
                  <span
                    class={cn(
                      "rounded px-1 py-0.5 text-[9px] uppercase",
                      item.kind === "mcp"
                        ? "bg-blue-100 text-blue-800"
                        : "bg-neutral-200 text-neutral-700",
                    )}
                  >
                    {item.kind}
                  </span>
                  <span class="font-mono">{item.name}</span>
                  {#if item.completed_at === undefined}
                    <span class="ml-auto text-amber-700 italic">running…</span>
                  {:else if item.is_error}
                    <span class="ml-auto text-red-700">error</span>
                  {/if}
                </div>
                {#if item.output !== undefined && item.output !== ""}
                  <pre
                    class={cn(
                      "mt-1 max-h-40 overflow-y-auto font-mono text-[11px] whitespace-pre-wrap",
                      item.is_error ? "text-red-800" : "text-neutral-600",
                    )}>{item.output}</pre>
                {/if}
              </div>
            {/if}
          {/each}
          {#if turn.status === "failed" && turn.error}
            <div class="text-xs text-red-700" data-testid="turn-error">{turn.error}</div>
          {/if}
        </div>
      {/if}
    {/each}
  </div>
</div>
