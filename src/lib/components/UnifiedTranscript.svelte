<script lang="ts">
  import type { AgentRecord, ConversationItem } from "$lib/types";
  import { HEARTBEAT_TIMEOUT_MS } from "$lib/types";
  import { formatDuration } from "$lib/utils";
  import { cancelSend, runtimes, transcripts, type Turn } from "$lib/state/index.svelte";
  import {
    buildUnifiedRows,
    copyTextOf,
    groupRenderBlocks,
    type UnifiedRow,
  } from "$lib/state/unified";
  import { agentCopy } from "$lib/agentCopy.svelte";
  import { HARNESS_COLOR } from "$lib/harnessDisplay";
  import Badge from "$lib/components/ui/Badge.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import StatusChip from "$lib/components/ui/StatusChip.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import ToolCallWidget from "$lib/components/ToolCallWidget.svelte";
  import ThinkingWidget from "$lib/components/ThinkingWidget.svelte";

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
    // Filter to the live roster so a removed agent leaves no orphan column
    // (the journal overlay retains its original recipient set).
    const knownAgentIds = new Set(agents.map((a) => a.id));
    return buildUnifiedRows(turns, overlay, knownAgentIds);
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

  /// A fan-out column's copyable prose: the joined answer text of its agent
  /// turns (reasoning + tool calls excluded — see `answerTextOf`).
  function columnText(colRows: NonUserRow[]): string {
    return colRows
      .filter((r) => r.kind === "agent")
      .map((r) => copyTextOf(r.turn, agentCopy.mode))
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

  /// Ticking clock for the live "quiet" counter. Updated once per second while
  /// the component is mounted; `now` is only read inside the quiet footer, so
  /// when nothing is quiet these ticks trigger no re-render.
  let now = $state(Date.now());
  $effect(() => {
    const id = setInterval(() => {
      now = Date.now();
    }, 1000);
    return () => clearInterval(id);
  });

  /// Elapsed silence for a quiet turn: the timer fired one full
  /// HEARTBEAT_TIMEOUT_MS after the last activity, so true silence is the time
  /// since `quiet_since` plus that threshold. Starts at one HEARTBEAT_TIMEOUT_MS
  /// when the indicator first appears and counts up.
  function quietElapsedMs(quietSince: string): number {
    return now - Date.parse(quietSince) + HEARTBEAT_TIMEOUT_MS;
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
  {#each turn.items as item, i (i)}
    {#if item.item_kind === "text"}
      {#if item.kind === "thinking"}
        <ThinkingWidget text={item.text} />
      {:else}
        <Markdown text={item.text} />
      {/if}
    {:else}
      <ToolCallWidget tool={item} />
    {/if}
  {/each}
  {#if turn.status === "failed" && turn.error}
    <div class="text-status-failed text-xs" data-testid="turn-error">{turn.error}</div>
  {/if}
  {#if turn.status === "streaming"}
    {@render workingFooter(turn)}
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
    class="border-muted/40 text-muted hover:border-status-failed/60 hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent focus-visible:border-status-failed/60 focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-6 w-6 items-center justify-center rounded-full border-[0.5px] transition-colors focus-visible:ring-2 focus-visible:outline-none"
    data-testid={testid}
    aria-label={label}
    {onclick}
  >
    <StopIcon class="size-5 -translate-x-[0.5px]" />
  </button>
{/snippet}

{#snippet workingFooter(turn: AgentTurn)}
  {@const rt = runtimes[turn.agent_id]}
  {@const quietSince =
    rt?.quiet_since !== undefined && rt?.in_flight_turn_id === turn.turn_id
      ? rt.quiet_since
      : undefined}
  <div
    class="mt-2 flex items-center gap-2 text-xs {quietSince !== undefined
      ? 'text-warning'
      : 'text-muted'}"
    data-testid="turn-working"
    data-quiet={quietSince !== undefined}
  >
    <!-- Until the turn has been silent past HEARTBEAT_TIMEOUT_MS it just shows
         "Working..." (no counter — the number would otherwise reset on every
         event). Once quiet, it's still alive on the backend, so this is a soft
         caution that counts up the silence — never a failure — and reverts to
         "Working..." the moment activity resumes. -->
    <span class="animate-pulse">
      {#if quietSince !== undefined}
        No response ({formatDuration(quietElapsedMs(quietSince))})...
      {:else}
        Working...
      {/if}
    </span>
    {#if turn.send_id !== undefined}
      {@const sendId = turn.send_id}
      {@render liveTurnControl(
        () => cancelSend(sendId, [turn.agent_id]),
        `Cancel turn for ${agentName(turn.agent_id)}`,
        "turn-live-control",
      )}
    {/if}
  </div>
{/snippet}

{#snippet queuedFooter(agentId: string, sendId: string, labelTestid: string, controlTestid: string)}
  <div class="text-muted mt-2 flex items-center gap-2 text-xs" data-testid={labelTestid}>
    <span class="animate-pulse">Queued...</span>
    {@render liveTurnControl(
      () => cancelSend(sendId, [agentId]),
      `Cancel queued send for ${agentName(agentId)}`,
      controlTestid,
    )}
  </div>
{/snippet}

{#snippet messageMeta(
  at: string,
  copyable: string,
  label: string,
  mt = "mt-1",
  spend: AgentTurn["spend"] = undefined,
  costUsd: number | null | undefined = undefined,
)}
  <!-- `justify-between` pins the always-visible spend group to the LEFT and the
       hover-revealed timestamp/copy to the RIGHT. The left group is always
       rendered (empty when not real-spend) so a no-spend row still keeps
       timestamp/copy right-aligned — and the spend never floats mid-row when
       nothing is hovered. -->
  <div class={`${mt} flex items-center justify-between gap-2`} data-testid="message-meta">
    <!-- Left: cost + overage marker, always visible. Two distinct gates (no
         `match harness`): the **cost** shows on `spend.real_spend` (the turn
         cost real money — for subscription Claude that's overage, since
         `total_cost_usd` is otherwise notional); the **"using credits" marker**
         shows on `spend.is_overage` specifically. They coincide for Claude, but
         a future pay-per-use harness would set `real_spend` without `is_overage`
         → cost shows, marker correctly stays hidden. Caller passes `spend`/
         `costUsd` only at agent-turn sites; a real-money signal isn't hover-hidden. -->
    <div class="flex items-center gap-2">
      {#if spend?.is_overage}
        <span
          class="text-warning text-xs"
          data-testid="message-overage"
          title={spend.overage_resets_at
            ? `Spending overage credits — window resets ${new Date(spend.overage_resets_at).toLocaleString()}`
            : "Spending overage credits"}>⚡ using credits</span
        >
      {/if}
      {#if spend?.real_spend && costUsd != null}
        <span class="text-muted text-xs" data-testid="message-cost">${costUsd.toFixed(4)}</span>
      {/if}
    </div>
    <!-- Right: timestamp + copy stay hover/focus-revealed (unchanged). -->
    <div
      class="flex items-center gap-2 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100"
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
  </div>
{/snippet}

{#snippet userMessage(row: Extract<UnifiedRow, { kind: "user" }>)}
  <div class="group min-w-0 flex-1" data-testid="turn" data-role="user">
    <div class="-mx-3 rounded-md bg-blue-100/20 px-3 py-2">
      <Markdown text={row.text} />
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
    <div class="border-l-[0.5px] pl-3" style:border-left-color={agentBorderColor(row.agent_id)}>
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
    </div>
    <div class="border-l-[0.5px] pl-3" style:border-left-color={agentBorderColor(agentId)}>
      {@render queuedFooter(agentId, sendId, "turn-queued", "turn-live-control")}
    </div>
  </div>
{/snippet}

{#snippet agentRow(turn: AgentTurn)}
  {@const harness = agentById[turn.agent_id]?.harness}
  {@const copyable = copyTextOf(turn, agentCopy.mode)}
  <div class="group space-y-1.5" data-testid="turn" data-role="agent">
    <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(turn.agent_id)}</span>
      {#if harness}<HarnessIcon {harness} testid="turn-harness-icon" />{:else}<Badge>?</Badge>{/if}
    </div>
    <div
      class="space-y-1.5 border-l-[0.5px] pl-3"
      style:border-left-color={agentBorderColor(turn.agent_id)}
    >
      {@render turnStatusLabel(turn.status)}
      {@render turnBody(turn)}
    </div>
    {@render messageMeta(
      turn.started_at,
      copyable,
      "Copy message",
      "mt-1",
      turn.spend,
      turn.usage?.total_cost_usd,
    )}
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
                </div>
                <div
                  class="space-y-1.5 border-l-[0.5px] pl-3"
                  style:border-left-color={agentBorderColor(col.agent_id)}
                >
                  {#if state === "queued"}
                    {@render queuedFooter(
                      col.agent_id,
                      block.send_id,
                      "fanout-queued",
                      "fanout-card-cancel",
                    )}
                  {/if}
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
