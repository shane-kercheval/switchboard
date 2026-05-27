<script lang="ts">
  import type { AgentRecord, ConversationItem } from "$lib/types";
  import { cancelSend, runtimes, transcripts, type Turn } from "$lib/state/index.svelte";
  import { buildUnifiedRows, groupRenderBlocks, type UnifiedRow } from "$lib/state/unified";
  import { cn } from "$lib/utils";
  import { HARNESS_COLOR } from "$lib/harnessDisplay";
  import Badge from "$lib/components/ui/Badge.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import StatusChip from "$lib/components/ui/StatusChip.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";

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

  /// Send ids that are queued (dispatched, not yet started) across the project's
  /// agents — the per-agent `pending_sends` minus any being cancelled. A
  /// single-recipient queued send has a user message but no agent turn yet
  /// (turn_start removes the pending entry), so we render a "queued…" affordance
  /// under it — the single-send equivalent of a fan-out column's queued state.
  const queuedSendIds = $derived.by(() => {
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const set = new Set<string>();
    for (const agent of agents) {
      for (const p of runtimes[agent.id]?.pending_sends ?? []) {
        if (!p.cancel_requested) set.add(p.send_id);
      }
    }
    return set;
  });

  function agentName(agentId: string): string {
    return agentById[agentId]?.name ?? "unknown";
  }

  function agentBorderColor(agentId: string): string {
    const harness = agentById[agentId]?.harness;
    return harness ? HARNESS_COLOR[harness] : "var(--border)";
  }

  /// The copyable prose of an agent turn: its text segments joined, with tool
  /// calls omitted (people copy the response, not the tool I/O). A turn that is
  /// only tool calls (or still empty) yields "", which suppresses the button.
  function agentTurnText(turn: AgentTurn): string {
    return turn.items
      .filter((i) => i.item_kind === "text")
      .map((i) => i.text)
      .join("\n\n")
      .trim();
  }

  /// A fan-out column's copyable prose: the joined text of its agent turns.
  function columnText(colRows: NonUserRow[]): string {
    return colRows
      .filter((r) => r.kind === "agent")
      .map((r) => agentTurnText(r.turn))
      .filter((t) => t.length > 0)
      .join("\n\n");
  }

  /// A fan-out column's timestamp: its latest agent turn's start, or "" (queued).
  function columnAt(colRows: NonUserRow[]): string {
    for (let i = colRows.length - 1; i >= 0; i--) {
      const r = colRows[i]!;
      if (r.kind === "agent") return r.turn.started_at;
    }
    return "";
  }

  function formatTime(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return "";
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
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
    <StatusChip status="processing" testid="turn-processing" />
  {/if}
  {#each turn.items as item, i (i)}
    {#if item.item_kind === "text"}
      <Markdown text={item.text} />
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
              "mt-1 max-h-40 overflow-y-auto font-mono text-xs whitespace-pre-wrap",
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
  {#if status === "failed"}
    <StatusChip status="failed" />
  {:else if status === "cancelled"}
    <StatusChip status="cancelled" />
  {/if}
{/snippet}

{#snippet liveTurnControl(onclick: () => void, label: string, testid: string)}
  <button
    type="button"
    class="group text-muted hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-6 w-6 items-center justify-center rounded-full transition-colors focus-visible:ring-2 focus-visible:outline-none"
    data-testid={testid}
    aria-label={label}
    {onclick}
  >
    <span
      class="border-muted/30 border-t-muted block h-5 w-5 animate-spin rounded-full border-2 group-hover:hidden group-focus-visible:hidden"
      aria-hidden="true"
    ></span>
    <StopIcon class="hidden h-5 w-5 group-hover:block group-focus-visible:block" />
  </button>
{/snippet}

{#snippet messageMeta(at: string, copyable: string, label: string, mt = "mt-1")}
  <div
    class={`${mt} flex items-center gap-2 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100`}
    data-testid="message-meta"
  >
    {#if at}
      <time class="text-muted text-xs" datetime={at} title={at} data-testid="message-time"
        >{formatTime(at)}</time
      >
    {/if}
    {#if copyable}
      <CopyButton text={copyable} {label} testid="message-copy" />
    {/if}
  </div>
{/snippet}

{#snippet userMessage(row: Extract<UnifiedRow, { kind: "user" }>)}
  <div class="group min-w-0 flex-1" data-testid="turn" data-role="user">
    <div class="bg-panel/45 -mx-3 space-y-1.5 rounded-md px-3 py-2">
      <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
        <span class="text-fg" data-testid="turn-author">User</span>
      </div>
      <div class="border-border border-l pl-3">
        <Markdown text={row.text} />
      </div>
    </div>
    {@render messageMeta(row.at, row.text, "Copy message")}
  </div>
{/snippet}

{#snippet outcomeRow(row: Extract<UnifiedRow, { kind: "outcome" }>)}
  {@const harness = agentById[row.agent_id]?.harness}
  <div
    class="group space-y-1.5"
    data-testid="turn-outcome"
    data-role="agent"
    data-status={row.status}
  >
    <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(row.agent_id)}</span>
      {#if harness}<HarnessIcon {harness} testid="turn-harness-icon" />{:else}<Badge>?</Badge>{/if}
    </div>
    <div class="border-l pl-3" style:border-left-color={agentBorderColor(row.agent_id)}>
      {#if row.status === "cancelled"}
        <StatusChip status="cancelled" testid="outcome-cancelled" />
      {:else}
        <StatusChip status="failed" testid="outcome-failed" />
        {#if row.reason}<span class="text-muted text-xs"> — {row.reason}</span>{/if}
      {/if}
    </div>
    {@render messageMeta(row.at, "", "", "mt-2.5")}
  </div>
{/snippet}

{#snippet queuedRow(agentId: string, sendId: string)}
  <div class="space-y-1.5" data-testid="turn" data-role="agent">
    <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(agentId)}</span>
      {#if agentById[agentId]?.harness}
        <HarnessIcon harness={agentById[agentId]!.harness} testid="turn-harness-icon" />
      {/if}
      {@render liveTurnControl(
        () => cancelSend(sendId, [agentId]),
        `Cancel queued send for ${agentName(agentId)}`,
        "turn-live-control",
      )}
    </div>
    <div class="border-l pl-3" style:border-left-color={agentBorderColor(agentId)}>
      <StatusChip status="queued" testid="turn-queued" />
    </div>
  </div>
{/snippet}

{#snippet agentRow(turn: AgentTurn)}
  {@const harness = agentById[turn.agent_id]?.harness}
  {@const copyable = agentTurnText(turn)}
  <div class="group space-y-1.5" data-testid="turn" data-role="agent">
    <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(turn.agent_id)}</span>
      {#if harness}<HarnessIcon {harness} testid="turn-harness-icon" />{:else}<Badge>?</Badge>{/if}
      {#if turn.status === "streaming" && turn.send_id !== undefined}
        {@const sendId = turn.send_id}
        {@render liveTurnControl(
          () => cancelSend(sendId, [turn.agent_id]),
          `Cancel turn for ${agentName(turn.agent_id)}`,
          "turn-live-control",
        )}
      {/if}
    </div>
    <div
      class="space-y-1.5 border-l pl-3"
      style:border-left-color={agentBorderColor(turn.agent_id)}
    >
      {@render turnStatusLabel(turn.status)}
      {@render turnBody(turn)}
    </div>
    {@render messageMeta(turn.started_at, copyable, "Copy message")}
  </div>
{/snippet}

<div
  bind:this={container}
  onscroll={onScroll}
  data-testid="unified-transcript"
  class="bg-transcript flex-1 overflow-y-auto px-8 py-4"
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
          {#if block.row.send_id !== undefined && block.row.agent_ids.length === 1 && queuedSendIds.has(block.row.send_id)}
            {@render queuedRow(block.row.agent_ids[0]!, block.row.send_id)}
          {/if}
        {:else if block.row.kind === "outcome"}
          {@render outcomeRow(block.row)}
        {:else}
          {@render agentRow(block.row.turn)}
        {/if}
      {:else}
        <div class="space-y-4" data-testid="fanout-group">
          <div class="flex items-start justify-between gap-2">
            {@render userMessage(block.user)}
          </div>
          <!-- Side-by-side on wide viewports; stacks vertically below `lg`. -->
          <div
            class="grid gap-4"
            style:grid-template-columns={`repeat(${block.columns.length}, minmax(0, 1fr))`}
          >
            {#each block.columns as col (col.agent_id)}
              {@const state = columnState(col.rows)}
              {@const harness = agentById[col.agent_id]?.harness}
              {@const colCopyable = columnText(col.rows)}
              <div
                class="group space-y-1.5"
                data-testid="fanout-column"
                data-role="agent"
                data-agent-id={col.agent_id}
                data-state={state}
              >
                <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
                  <span class="text-fg" data-testid="turn-agent-name"
                    >{agentName(col.agent_id)}</span
                  >
                  {#if harness}<HarnessIcon {harness} />{/if}
                  {#if isLive(state)}
                    {@render liveTurnControl(
                      () => cancelSend(block.send_id, [col.agent_id]),
                      `Cancel turn for ${agentName(col.agent_id)}`,
                      "fanout-card-cancel",
                    )}
                  {/if}
                  {#if state === "queued"}
                    <span class="text-status-processing" data-testid="fanout-queued">queued…</span>
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
                    {:else if r.status === "cancelled"}
                      <StatusChip status="cancelled" testid="outcome-cancelled" />
                    {:else}
                      <StatusChip status="failed" testid="outcome-failed" />
                      {#if r.reason}<span class="text-muted text-xs"> — {r.reason}</span>{/if}
                    {/if}
                  {/each}
                </div>
                {@render messageMeta(
                  columnAt(col.rows),
                  colCopyable,
                  `Copy ${agentName(col.agent_id)}'s message`,
                )}
              </div>
            {/each}
          </div>
        </div>
      {/if}
    {/each}
  </div>
</div>
