<script lang="ts">
  import type { AgentRecord, ConversationItem } from "$lib/types";
  import { transcripts, type Turn } from "$lib/state/index.svelte";
  import { buildUnifiedRows } from "$lib/state/unified";
  import { cn } from "$lib/utils";
  import { HARNESS_LABEL } from "$lib/harnessDisplay";
  import Badge from "$lib/components/ui/Badge.svelte";

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
  class="flex-1 overflow-y-auto px-5 py-4"
>
  {#if loadStatus === "loading"}
    <p class="text-muted mb-3 text-xs italic" data-testid="transcript-loading">Loading history…</p>
  {:else if loadStatus === "failed"}
    <p class="text-status-failed mb-3 text-xs" data-testid="transcript-load-failed">
      Couldn't load this project's conversation history.
    </p>
  {/if}

  {#if rows.length === 0 && loadStatus !== "loading"}
    <p class="text-muted text-sm">No messages yet. Type a prompt below.</p>
  {/if}

  <div class="mx-auto max-w-4xl space-y-5">
    {#each rows as row (row.key)}
      {#if row.kind === "user"}
        <div class="space-y-1.5" data-testid="turn" data-role="user">
          <div class="flex items-center gap-2 text-[11px] font-semibold tracking-wide uppercase">
            <span class="text-muted">You</span>
            <span class="text-muted">→</span>
            <span class="text-fg" data-testid="turn-recipient">
              {recipientNames(row.agent_ids)}
            </span>
          </div>
          <div class="text-fg text-sm leading-6 whitespace-pre-wrap">{row.text}</div>
        </div>
      {:else if row.kind === "outcome"}
        <div
          class="flex items-center gap-2 text-xs"
          data-testid="turn-outcome"
          data-status={row.status}
        >
          <span class="text-fg font-semibold">{agentName(row.agent_id)}</span>
          {#if row.status === "cancelled"}
            <span class="text-muted" data-testid="outcome-cancelled">cancelled</span>
          {:else}
            <span class="text-status-failed" data-testid="outcome-failed">failed</span>
          {/if}
          {#if row.reason}
            <span class="text-muted">— {row.reason}</span>
          {/if}
        </div>
      {:else}
        {@const turn = row.turn}
        {@const harness = agentById[turn.agent_id]?.harness}
        <div class="space-y-1.5" data-testid="turn" data-role="agent">
          <div class="flex items-center gap-2 text-[11px] font-semibold tracking-wide uppercase">
            <span class="text-fg" data-testid="turn-agent-name">
              {agentName(turn.agent_id)}
            </span>
            {#if harness}
              <Badge variant="harness" {harness}>{HARNESS_LABEL[harness]}</Badge>
            {:else}
              <Badge>?</Badge>
            {/if}
            {#if turn.status === "streaming"}
              <span class="text-status-processing" data-testid="turn-streaming">streaming…</span>
            {:else if turn.status === "failed"}
              <span class="text-status-failed">failed</span>
            {:else if turn.status === "cancelled"}
              <span class="text-muted">cancelled</span>
            {/if}
          </div>
          {#if turn.status === "streaming" && turn.items.length === 0}
            <div class="text-muted text-xs italic" data-testid="turn-processing">processing…</div>
          {/if}
          {#each turn.items as item, i (i)}
            {#if item.item_kind === "text"}
              <div class="text-fg text-sm leading-6 whitespace-pre-wrap">{item.text}</div>
            {:else}
              <div
                class="border-border/80 bg-panel/80 rounded-md border p-2 text-xs"
                data-testid="turn-tool"
                data-tool-use-id={item.tool_use_id}
              >
                <div class="text-fg flex items-center gap-1.5 font-semibold">
                  <Badge>{item.kind}</Badge>
                  <span class="font-mono">{item.name}</span>
                  {#if item.completed_at === undefined}
                    <span class="text-status-processing ml-auto italic">running…</span>
                  {:else if item.is_error}
                    <span class="text-status-failed ml-auto">error</span>
                  {/if}
                </div>
                {#if item.output !== undefined && item.output !== ""}
                  <pre
                    class={cn(
                      "mt-1 max-h-40 overflow-y-auto font-mono text-[11px] whitespace-pre-wrap",
                      item.is_error ? "text-status-failed" : "text-muted",
                    )}>{item.output}</pre>
                {/if}
              </div>
            {/if}
          {/each}
          {#if turn.status === "failed" && turn.error}
            <div class="text-status-failed text-xs" data-testid="turn-error">{turn.error}</div>
          {/if}
        </div>
      {/if}
    {/each}
  </div>
</div>
