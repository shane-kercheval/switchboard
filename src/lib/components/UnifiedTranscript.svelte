<script lang="ts">
  import type { AgentRecord, ConversationItem } from "$lib/types";
  import { cancelSend, transcripts, type Turn } from "$lib/state/index.svelte";
  import { buildUnifiedRows, groupRenderBlocks, type UnifiedRow } from "$lib/state/unified";
  import { cn } from "$lib/utils";
  import { HARNESS_COLOR } from "$lib/harnessDisplay";
  import Badge from "$lib/components/ui/Badge.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";

  type AgentTurn = Extract<Turn, { role: "agent" }>;
  type NonUserRow = Exclude<UnifiedRow, { kind: "user" }>;

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

  const rows = $derived.by(() => {
    const turns: Turn[] = [];
    for (const agent of agents) {
      const slice = transcripts[agent.id] ?? [];
      for (const turn of slice) turns.push(turn);
    }
    return buildUnifiedRows(turns, overlay);
  });

  /// Group the flat rows into render blocks: standalone rows, plus one block per
  /// fan-out whose responses lay out as per-recipient columns.
  const blocks = $derived(
    groupRenderBlocks(
      rows,
      agents.map((a) => a.id),
    ),
  );

  function agentName(agentId: string): string {
    return agentById[agentId]?.name ?? "unknown";
  }

  function agentBorderColor(agentId: string): string {
    const harness = agentById[agentId]?.harness;
    return harness ? HARNESS_COLOR[harness] : "var(--border)";
  }

  /// A fan-out column's state, derived from its rows: the response turn's status
  /// if present, else an outcome marker's status, else "queued" (dispatched, no
  /// turn yet). "streaming"/"queued" are *live* — they keep cancel-send active.
  type ColumnState = "queued" | "streaming" | "complete" | "failed" | "cancelled";
  function columnState(colRows: NonUserRow[]): ColumnState {
    for (let i = colRows.length - 1; i >= 0; i--) {
      const r = colRows[i]!;
      if (r.kind === "agent") return r.turn.status;
    }
    for (let i = colRows.length - 1; i >= 0; i--) {
      const r = colRows[i]!;
      if (r.kind === "outcome") return r.status;
    }
    return "queued";
  }
  const isLive = (s: ColumnState): boolean => s === "queued" || s === "streaming";

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

{#snippet turnBody(turn: AgentTurn)}
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
{/snippet}

{#snippet turnStatusLabel(status: AgentTurn["status"])}
  {#if status === "streaming"}
    <span class="text-status-processing" data-testid="turn-streaming">streaming…</span>
  {:else if status === "failed"}
    <span class="text-status-failed">failed</span>
  {:else if status === "cancelled"}
    <span class="text-muted">cancelled</span>
  {/if}
{/snippet}

{#snippet userMessage(row: Extract<UnifiedRow, { kind: "user" }>)}
  <div class="space-y-1.5" data-testid="turn" data-role="user">
    <div class="flex items-center gap-2 text-[11px] font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-author">User</span>
    </div>
    <div class="border-border border-l pl-3">
      <div class="text-fg text-sm leading-6 whitespace-pre-wrap">{row.text}</div>
    </div>
  </div>
{/snippet}

{#snippet outcomeRow(row: Extract<UnifiedRow, { kind: "outcome" }>)}
  <div
    class="flex items-center gap-2 border-l pl-3 text-xs"
    style:border-left-color={agentBorderColor(row.agent_id)}
    data-testid="turn-outcome"
    data-status={row.status}
  >
    <span class="text-fg font-semibold">{agentName(row.agent_id)}</span>
    {#if row.status === "cancelled"}
      <span class="text-muted" data-testid="outcome-cancelled">cancelled</span>
    {:else}
      <span class="text-status-failed" data-testid="outcome-failed">failed</span>
    {/if}
    {#if row.reason}<span class="text-muted">— {row.reason}</span>{/if}
  </div>
{/snippet}

{#snippet agentRow(turn: AgentTurn)}
  {@const harness = agentById[turn.agent_id]?.harness}
  <div class="space-y-1.5" data-testid="turn" data-role="agent">
    <div class="flex items-center gap-2 text-[11px] font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(turn.agent_id)}</span>
      {#if harness}<HarnessIcon {harness} testid="turn-harness-icon" />{:else}<Badge>?</Badge>{/if}
    </div>
    <div
      class="space-y-1.5 border-l pl-3"
      style:border-left-color={agentBorderColor(turn.agent_id)}
    >
      {@render turnStatusLabel(turn.status)}
      {@render turnBody(turn)}
    </div>
  </div>
{/snippet}

<div
  bind:this={container}
  onscroll={onScroll}
  data-testid="unified-transcript"
  class="flex-1 overflow-y-auto px-8 py-4"
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

  <div class="space-y-5">
    {#each blocks as block (block.kind === "fanout" ? block.key : block.row.key)}
      {#if block.kind === "row"}
        {#if block.row.kind === "user"}
          {@render userMessage(block.row)}
        {:else if block.row.kind === "outcome"}
          {@render outcomeRow(block.row)}
        {:else}
          {@render agentRow(block.row.turn)}
        {/if}
      {:else}
        {@const liveAgents = block.columns
          .filter((c) => isLive(columnState(c.rows)))
          .map((c) => c.agent_id)}
        <div class="space-y-2" data-testid="fanout-group">
          <div class="flex items-start justify-between gap-2">
            {@render userMessage(block.user)}
            {#if liveAgents.length > 0}
              <Button
                variant="ghost"
                size="sm"
                class="text-status-failed shrink-0"
                data-testid="fanout-cancel-send"
                onclick={() => cancelSend(block.send_id, liveAgents)}
              >
                Cancel send
              </Button>
            {/if}
          </div>
          <!-- Side-by-side on wide viewports; stacks vertically below `lg`. -->
          <div
            class="grid gap-4"
            style:grid-template-columns={`repeat(${block.columns.length}, minmax(0, 1fr))`}
          >
            {#each block.columns as col (col.agent_id)}
              {@const state = columnState(col.rows)}
              {@const harness = agentById[col.agent_id]?.harness}
              <div
                class="space-y-1.5"
                data-testid="fanout-column"
                data-agent-id={col.agent_id}
                data-state={state}
              >
                <div
                  class="flex items-center gap-2 text-[11px] font-semibold tracking-wide uppercase"
                >
                  <span class="text-fg" data-testid="turn-agent-name"
                    >{agentName(col.agent_id)}</span
                  >
                  {#if harness}<HarnessIcon {harness} />{/if}
                  {#if state === "queued"}
                    <span class="text-status-processing" data-testid="fanout-queued">queued…</span>
                  {/if}
                  {#if isLive(state)}
                    <button
                      type="button"
                      class="text-muted hover:text-status-failed ml-auto text-[10px] normal-case"
                      data-testid="fanout-card-cancel"
                      aria-label={`Cancel ${agentName(col.agent_id)}`}
                      onclick={() => cancelSend(block.send_id, [col.agent_id])}>Cancel</button
                    >
                  {/if}
                </div>
                <div
                  class="space-y-1.5 border-l pl-3"
                  style:border-left-color={agentBorderColor(col.agent_id)}
                >
                  {#each col.rows as r (r.key)}
                    {#if r.kind === "agent"}
                      {@render turnStatusLabel(r.turn.status)}
                      {@render turnBody(r.turn)}
                    {:else}
                      {#if r.status === "cancelled"}
                        <span class="text-muted text-xs" data-testid="outcome-cancelled"
                          >cancelled</span
                        >
                      {:else}
                        <span class="text-status-failed text-xs" data-testid="outcome-failed"
                          >failed</span
                        >
                      {/if}
                      {#if r.reason}<span class="text-muted text-xs">— {r.reason}</span>{/if}
                    {/if}
                  {/each}
                </div>
              </div>
            {/each}
          </div>
        </div>
      {/if}
    {/each}
  </div>
</div>
