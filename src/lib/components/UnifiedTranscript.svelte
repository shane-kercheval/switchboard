<script lang="ts">
  import type { AgentRecord } from "$lib/types";
  import { transcripts, type Turn } from "$lib/state/index.svelte";
  import { cn } from "$lib/utils";

  let { agents }: { agents: AgentRecord[] } = $props();

  /// Build an attribution map agentId → AgentRecord for O(1) lookup when
  /// rendering each turn. The unified view merges across all the project's
  /// agents — without per-turn attribution the reader can't tell who said
  /// what.
  const agentById = $derived.by(() => {
    const map: Record<string, AgentRecord> = {};
    for (const a of agents) map[a.id] = a;
    return map;
  });

  /// Flatten + chronologically sort. ISO-8601 strings sort lexicographically;
  /// ties broken by turn_id (UUID v7 is time-ordered, so newer UUIDs sort
  /// after older ones — consistent with the timestamp primary sort).
  const allTurns = $derived.by(() => {
    const out: Turn[] = [];
    for (const agent of agents) {
      const slice = transcripts[agent.id] ?? [];
      for (const turn of slice) out.push(turn);
    }
    out.sort((a, b) => {
      const t = a.started_at.localeCompare(b.started_at);
      if (t !== 0) return t;
      return a.turn_id.localeCompare(b.turn_id);
    });
    return out;
  });

  function agentName(agentId: string): string {
    return agentById[agentId]?.name ?? "unknown";
  }

  function harnessBadgeClass(agentId: string): string {
    const harness = agentById[agentId]?.harness;
    if (harness === "claude_code") return "bg-orange-100 text-orange-800";
    if (harness === "codex") return "bg-blue-100 text-blue-800";
    return "bg-neutral-100 text-neutral-800";
  }

  function harnessLabel(agentId: string): string {
    const harness = agentById[agentId]?.harness;
    if (harness === "claude_code") return "Claude";
    if (harness === "codex") return "Codex";
    return "?";
  }

  // Auto-pin to bottom unless the user has scrolled up. Mirrors the
  // legacy Transcript.svelte behavior — keeps the reader at the live edge
  // during streaming, but doesn't yank them away if they scroll back to
  // read earlier turns.
  let container = $state<HTMLDivElement | null>(null);
  let pinned = $state<boolean>(true);

  function onScroll(): void {
    if (!container) return;
    const distanceFromBottom =
      container.scrollHeight - container.scrollTop - container.clientHeight;
    pinned = distanceFromBottom < 32;
  }

  $effect(() => {
    // Reactive read on the total turn count + last turn's content so the
    // effect re-runs on every chunk/tool arrival.
    void allTurns.length;
    if (allTurns.length > 0) {
      const last = allTurns[allTurns.length - 1];
      if (last?.role === "agent") {
        void last.items.length;
      }
    }
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
  {#if allTurns.length === 0}
    <p class="text-sm text-neutral-500">No messages yet. Type a prompt below.</p>
  {/if}
  <div class="space-y-4">
    {#each allTurns as turn (turn.turn_id)}
      <div class="space-y-1" data-testid="turn" data-role={turn.role}>
        <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
          {#if turn.role === "user"}
            <span class="text-neutral-500">You</span>
            <span class="text-neutral-400">→</span>
            <span class="font-mono text-neutral-700" data-testid="turn-recipient">
              {agentName(turn.agent_id)}
            </span>
          {:else}
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
            {/if}
          {/if}
        </div>
        {#if turn.role === "user"}
          <div class="text-sm whitespace-pre-wrap text-neutral-900">{turn.text}</div>
        {:else}
          <!--
            Render items in order. Codex emits one whole agent_message item
            per turn — until that lands, `items` is empty for the duration
            of the streaming state. Show a "processing…" indicator so the
            UI doesn't look frozen.
          -->
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
                {#if item.output !== undefined}
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
        {/if}
      </div>
    {/each}
  </div>
</div>
