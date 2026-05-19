<script lang="ts">
  import type { AgentRecord, AgentId } from "$lib/types";
  import { runtimes, transcripts } from "$lib/state/index.svelte";
  import { cn } from "$lib/utils";

  /// `onAddAgent` is the "+ Add agent" entry point in the sidebar header.
  /// Optional so existing callers + tests that don't pass it continue
  /// rendering; when absent, the button isn't shown.
  let { agents, onAddAgent }: { agents: AgentRecord[]; onAddAgent?: () => void } = $props();

  function statusLabel(status: "idle" | "starting" | "processing" | undefined): string {
    if (status === "starting" || status === "processing") return "processing";
    return "idle";
  }

  function statusClass(status: "idle" | "starting" | "processing" | undefined): string {
    if (status === "starting" || status === "processing") return "text-amber-700";
    return "text-neutral-500";
  }

  /// Claude session-total cost — null-safe sum across the agent's completed
  /// turns. Codex turns have `total_cost_usd: null` so the `?? 0` is
  /// load-bearing (without it, the result is `NaN`). Codex agents typically
  /// return 0 here; the sidebar branches on harness for what to display.
  function sessionTotalCost(agentId: AgentId): number {
    const turns = transcripts[agentId] ?? [];
    let total = 0;
    for (const turn of turns) {
      if (turn.role !== "agent") continue;
      total += turn.usage?.total_cost_usd ?? 0;
    }
    return total;
  }

  /// Context utilization — `(input_tokens + output_tokens) / context_window`
  /// from the most recent completed agent turn. Forward-looking signal
  /// ("how full will the next turn's context be"). All three fields must
  /// be present; otherwise returns undefined and the bar is hidden.
  function contextUtilization(agentId: AgentId): number | undefined {
    const turns = transcripts[agentId] ?? [];
    for (let i = turns.length - 1; i >= 0; i--) {
      const turn = turns[i];
      if (turn?.role !== "agent" || turn.usage === undefined) continue;
      const inputTokens = turn.usage.input_tokens ?? 0;
      const outputTokens = turn.usage.output_tokens ?? 0;
      const window = turn.usage.context_window;
      if (window === undefined || window === null || window === 0) continue;
      return (inputTokens + outputTokens) / window;
    }
    return undefined;
  }

  /// Codex rate-limit signal — `primary.used_percent` from the most recent
  /// `RateLimitEvent`. Opaque on the wire so we do a defensive shape check;
  /// returns undefined if the payload doesn't match the documented shape.
  function rateLimitPercent(payload: unknown): number | undefined {
    if (typeof payload !== "object" || payload === null) return undefined;
    const primary = (payload as { primary?: unknown }).primary;
    if (typeof primary !== "object" || primary === null) return undefined;
    const pct = (primary as { used_percent?: unknown }).used_percent;
    return typeof pct === "number" ? pct : undefined;
  }
</script>

<aside class="flex w-64 flex-col border-r border-neutral-200 bg-neutral-50" data-testid="sidebar">
  <div
    class="flex items-center justify-between border-b border-neutral-200 px-4 py-3 text-xs font-semibold tracking-wide text-neutral-500 uppercase"
  >
    <span>Agents</span>
    {#if onAddAgent}
      <button
        type="button"
        class="rounded px-1.5 py-0.5 text-sm font-bold text-neutral-700 hover:bg-neutral-200"
        title="Add agent"
        aria-label="Add agent"
        data-testid="sidebar-add-agent"
        onclick={onAddAgent}
      >
        +
      </button>
    {/if}
  </div>
  <div class="flex-1 overflow-y-auto">
    {#if agents.length === 0}
      <p class="px-4 py-3 text-xs text-neutral-500">No agents in this project yet.</p>
    {/if}
    {#each agents as agent (agent.id)}
      {@const runtime = runtimes[agent.id]}
      {@const cost = sessionTotalCost(agent.id)}
      {@const util = contextUtilization(agent.id)}
      {@const rateLimit =
        agent.harness === "codex" ? rateLimitPercent(runtime?.last_rate_limit) : undefined}
      <div
        class="border-b border-neutral-200 px-4 py-3"
        data-testid="sidebar-agent"
        data-agent-id={agent.id}
      >
        <div class="flex items-baseline justify-between gap-2">
          <span class="font-mono text-sm font-semibold text-neutral-900" data-testid="agent-name">
            {agent.name}
          </span>
          <span
            class={cn(
              "rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase",
              agent.harness === "claude_code"
                ? "bg-orange-100 text-orange-800"
                : agent.harness === "codex"
                  ? "bg-blue-100 text-blue-800"
                  : "bg-emerald-100 text-emerald-800",
            )}
            data-testid="agent-harness-badge"
          >
            {agent.harness === "claude_code"
              ? "Claude"
              : agent.harness === "codex"
                ? "Codex"
                : "Gemini"}
          </span>
        </div>
        <div
          class={cn("mt-1 text-xs", statusClass(runtime?.run_status))}
          data-testid="agent-run-status"
        >
          {statusLabel(runtime?.run_status)}
        </div>
        {#if runtime?.last_error}
          <div class="mt-1 text-xs text-red-700" data-testid="agent-last-error">
            {runtime.last_error.message}
          </div>
        {/if}
        {#if runtime?.parse_warnings && runtime.parse_warnings.length > 0}
          <div
            class="mt-1 text-xs text-amber-700"
            data-testid="agent-parse-warnings"
            title={runtime.parse_warnings
              .map((w) => `line ${w.line_number}: ${w.reason}`)
              .join("\n")}
          >
            ⚠ {runtime.parse_warnings.length} transcript warning{runtime.parse_warnings.length === 1
              ? ""
              : "s"}
          </div>
        {/if}
        {#if runtime?.meta}
          <div class="mt-2 space-y-0.5 text-xs text-neutral-600" data-testid="agent-meta">
            <div>model: <span class="font-mono">{runtime.meta.model}</span></div>
            {#if runtime.meta.mcp_servers.length > 0}
              <div>mcp: {runtime.meta.mcp_servers.length}</div>
            {/if}
            {#if runtime.meta.skills.length > 0}
              <div>skills: {runtime.meta.skills.length}</div>
            {/if}
          </div>
        {/if}
        {#if agent.harness === "claude_code" && cost > 0}
          <div class="mt-2 text-xs text-neutral-700" data-testid="agent-cost">
            ${cost.toFixed(4)}
          </div>
        {/if}
        {#if rateLimit !== undefined}
          <div class="mt-2 text-xs text-neutral-700" data-testid="agent-rate-limit">
            quota used: {rateLimit.toFixed(0)}%
          </div>
        {/if}
        {#if util !== undefined}
          <div class="mt-2" data-testid="agent-context-bar">
            <div class="mb-0.5 text-[10px] text-neutral-500">
              context after last turn: {(util * 100).toFixed(0)}%
            </div>
            <div class="h-1 w-full overflow-hidden rounded bg-neutral-200">
              <div
                class="h-full bg-neutral-700"
                style:width="{Math.min(util * 100, 100).toFixed(1)}%"
              ></div>
            </div>
          </div>
        {/if}
      </div>
    {/each}
  </div>
</aside>
