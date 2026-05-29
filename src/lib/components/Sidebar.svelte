<script lang="ts">
  import type { AgentRecord, AgentId } from "$lib/types";
  import { runtimes, transcripts } from "$lib/state/index.svelte";
  import { cn, relativeTime } from "$lib/utils";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import AgentActionsMenu from "$lib/components/AgentActionsMenu.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";

  /// Cap the per-tooltip warning rows so a session with a long tail
  /// (50+) doesn't render a wall of text. Anything beyond is summarized
  /// as "+ N more"; the underlying `parse_warnings` array is untouched.
  const WARNING_ROW_CAP = 10;

  /// An agent is "active" — currently driving work — when its turn is in-flight
  /// (run_status) or it still has queued sends. Gates the "Stop agent" action and
  /// the resume panel's stronger collision warning.
  function isActive(agentId: AgentId): boolean {
    const rt = runtimes[agentId];
    if (rt === undefined) return false;
    return (
      rt.run_status === "starting" ||
      rt.run_status === "processing" ||
      (rt.pending_sends ?? []).length > 0
    );
  }

  /// `onAddAgent` is the "+ Add agent" entry point in the sidebar header.
  /// Optional so existing callers + tests that don't pass it continue
  /// rendering; when absent, the button isn't shown.
  let { agents, onAddAgent }: { agents: AgentRecord[]; onAddAgent?: () => void } = $props();

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

  /// Label for a Codex rate-limit window, from its `window_minutes` duration
  /// (300 = the ~5-hour primary, 10080 = the weekly secondary). Unknown/absent
  /// durations fall back to "quota" — which preserves the prior single-cell
  /// copy "quota used: N%" for payloads that carry only a bare `used_percent`.
  function codexWindowLabel(windowMinutes: unknown): string {
    if (windowMinutes === 300) return "5-hour";
    if (windowMinutes === 10080) return "weekly";
    return "quota";
  }

  /// Defensive read of Codex's opaque `last_rate_limit` into its independent
  /// windows (`primary` + `secondary`). Each is a usage gauge (`used_percent`)
  /// with a duration (`window_minutes`) and optional reset (`resets_at`, unix
  /// epoch seconds). Same reset-passed rule as the Claude reader: a window
  /// whose reset is in the past is dropped (it has cycled, so the % is from a
  /// stale window); a window with no `resets_at` is kept (can't prove it
  /// stale — older Codex shapes and minimal fixtures omit it). Codex
  /// rate-limit is session-file-backed (class B), so there's no snapshot-age
  /// qualifier. Returns `[]` when nothing is displayable.
  function codexRateLimitView(
    payload: unknown,
    nowMs: number,
  ): Array<{ label: string; usedPercent: number; resetsAtMs: number | null }> {
    if (typeof payload !== "object" || payload === null) return [];
    const windows: Array<{ label: string; usedPercent: number; resetsAtMs: number | null }> = [];
    for (const key of ["primary", "secondary"] as const) {
      const w = (payload as Record<string, unknown>)[key];
      if (typeof w !== "object" || w === null) continue;
      const ww = w as { used_percent?: unknown; resets_at?: unknown; window_minutes?: unknown };
      if (typeof ww.used_percent !== "number") continue;
      let resetsAtMs: number | null = null;
      if (typeof ww.resets_at === "number") {
        const ms = ww.resets_at * 1000;
        if (ms <= nowMs) continue; // reset-passed → window cycled, % is stale
        resetsAtMs = ms;
      }
      windows.push({
        label: codexWindowLabel(ww.window_minutes),
        usedPercent: ww.used_percent,
        resetsAtMs,
      });
    }
    return windows;
  }

  /// Human label for Claude's primary rate-limit window, derived from the
  /// payload's `rateLimitType` rather than hardcoded: the event tells us the
  /// window kind (observed: `"five_hour"`; other plans/tiers may differ), so
  /// we don't assert a duration the event could contradict. Unknown/absent
  /// types fall back to a generic "rate limit".
  function rateLimitLabel(rateLimitType: unknown): string {
    return rateLimitType === "five_hour" ? "5-hour limit" : "rate limit";
  }

  /// Defensive read of Claude's opaque `last_rate_limit` payload into the two
  /// **independent** signals the Sidebar shows. Each is gated on its own reset
  /// being in the *future*: a reset is an absolute timestamp, so it stays
  /// accurate however old the snapshot is — right until `nowMs` passes it, at
  /// which point the window has cycled and we no longer have its new reset, so
  /// that signal is dropped (showing a past "resets at" would be plainly
  /// wrong). This replaces an age-based staleness heuristic with a certainty.
  ///
  /// - `window`: the primary rate-limit window (`resetsAt` + `rateLimitType`
  ///   label) — emitted on every turn, **independent of overage**.
  /// - `overage`: the "using credits" escalation (`isUsingOverage`), with its
  ///   own credit/overage window (`overageResetsAt`, which can be days out so
  ///   it lives in the tooltip, not the inline clock). A null overage reset
  ///   ("flag set, no window time") is still shown — we can't prove it stale.
  ///
  /// Returns `null` when nothing is currently displayable.
  function rateLimitView(
    payload: unknown,
    nowMs: number,
  ): {
    window: { label: string; resetsAtMs: number } | null;
    overage: { resetsAtMs: number | null } | null;
  } | null {
    if (typeof payload !== "object" || payload === null) return null;
    const p = payload as {
      rateLimitType?: unknown;
      resetsAt?: unknown;
      isUsingOverage?: unknown;
      overageResetsAt?: unknown;
    };

    let window: { label: string; resetsAtMs: number } | null = null;
    if (typeof p.resetsAt === "number") {
      const resetsAtMs = p.resetsAt * 1000;
      if (resetsAtMs > nowMs) {
        window = { label: rateLimitLabel(p.rateLimitType), resetsAtMs };
      }
    }

    let overage: { resetsAtMs: number | null } | null = null;
    if (p.isUsingOverage === true) {
      if (typeof p.overageResetsAt === "number") {
        const overageMs = p.overageResetsAt * 1000;
        overage = overageMs > nowMs ? { resetsAtMs: overageMs } : null;
      } else {
        overage = { resetsAtMs: null };
      }
    }

    if (window === null && overage === null) return null;
    return { window, overage };
  }

  /// Compact clock time for an inline window line (the 5-hour window resets
  /// within hours, so same-day clock reads cleanly). Milliseconds since epoch.
  /// Display-only — never parsed back or scheduled against (no auto-retry).
  function formatResetTime(ms: number): string {
    return new Date(ms).toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
  }

  /// Full date+time for the tooltip's reset windows — a window (esp. the
  /// overage window) can be days out, so the tooltip carries the date the
  /// inline clock omits. Milliseconds since epoch. Display-only.
  function formatResetDateTime(ms: number): string {
    return new Date(ms).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  /// Per-agent collapsed state. Default expanded; ephemeral (resets on reload).
  let collapsed = $state<Record<string, boolean>>({});

  function toggleCollapsed(agentId: AgentId): void {
    collapsed[agentId] = !collapsed[agentId];
  }

  const allExpanded = $derived(agents.every((a) => !(collapsed[a.id] ?? false)));

  function toggleAll(): void {
    if (allExpanded) {
      for (const a of agents) collapsed[a.id] = true;
    } else {
      for (const a of agents) delete collapsed[a.id];
    }
  }
</script>

<SidebarPanel side="right" width="w-60" testid="sidebar">
  <SidebarSection title="Agents">
    {#snippet action()}
      <div class="flex items-center gap-0.5">
        {#if agents.length > 0}
          <button
            type="button"
            class={ICON_BUTTON_CLASS}
            aria-label={allExpanded ? "Collapse all agents" : "Expand all agents"}
            title={allExpanded ? "Collapse all" : "Expand all"}
            data-testid="sidebar-toggle-all"
            onclick={toggleAll}
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              class="h-[18px] w-[18px]"
              aria-hidden="true"
            >
              {#if allExpanded}
                <path d="m17 11-5-5-5 5" />
                <path d="m17 18-5-5-5 5" />
              {:else}
                <path d="m7 6 5 5 5-5" />
                <path d="m7 13 5 5 5-5" />
              {/if}
            </svg>
          </button>
        {/if}
        {#if onAddAgent}
          <button
            type="button"
            class={ICON_BUTTON_CLASS}
            title="Add agent"
            aria-label="Add agent"
            data-testid="sidebar-add-agent"
            onclick={onAddAgent}
          >
            <PlusIcon />
          </button>
        {/if}
      </div>
    {/snippet}

    {#if agents.length === 0}
      <p class="text-muted px-3 py-3 text-xs">No agents in this project yet.</p>
    {/if}
    <div class="flex flex-col gap-1.5 px-2 pb-2">
      {#each agents as agent (agent.id)}
        {@const runtime = runtimes[agent.id]}
        {@const cost = sessionTotalCost(agent.id)}
        {@const util = contextUtilization(agent.id)}
        {@const codexWindows =
          agent.harness === "codex" ? codexRateLimitView(runtime?.last_rate_limit, Date.now()) : []}
        <!-- `Date.now()` read once per render for the reset-in-the-future gate.
             Non-reactive: a reset that elapses while the app sits open won't
             auto-hide until the next render, which a new turn (or reopen)
             triggers — acceptable for a passive status cell. -->
        {@const rlView =
          agent.harness === "claude_code"
            ? rateLimitView(runtime?.last_rate_limit, Date.now())
            : null}
        {@const overageAsOf = runtime?.last_rate_limit_as_of}
        {@const isCollapsed = collapsed[agent.id] ?? false}
        <div
          class="bg-raised/90 hover:bg-border/40 rounded-md px-2.5 py-2 transition-colors"
          data-testid="sidebar-agent"
          data-agent-id={agent.id}
        >
          <div class="flex items-center justify-between gap-2">
            <button
              type="button"
              class="flex min-w-0 flex-1 items-center gap-1.5 text-left"
              aria-expanded={!isCollapsed}
              onclick={() => toggleCollapsed(agent.id)}
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
                class={cn(
                  "text-muted h-3 w-3 shrink-0 transition-transform",
                  isCollapsed && "-rotate-90",
                )}
                aria-hidden="true"
              >
                <path d="m6 9 6 6 6-6" />
              </svg>
              <span class="text-fg truncate text-[13px] font-semibold" data-testid="agent-name">
                {agent.name}
              </span>
            </button>
            <div class="flex shrink-0 items-center gap-1">
              <HarnessIcon harness={agent.harness} size="md" testid="agent-harness-icon" />
              <AgentActionsMenu {agent} active={isActive(agent.id)} />
            </div>
          </div>
          {#if !isCollapsed}
            {#if runtime?.hydration_error}
              <div class="text-status-failed mt-1 text-xs" data-testid="agent-hydration-error">
                history failed to load: {runtime.hydration_error}
              </div>
            {/if}
            {#if runtime?.parse_warnings && runtime.parse_warnings.length > 0}
              {@const warnings = runtime.parse_warnings}
              {@const visible = warnings.slice(0, WARNING_ROW_CAP)}
              {@const extra = warnings.length - visible.length}
              <Tooltip side="right">
                {#snippet trigger(props)}
                  <!-- tabindex=0: the indicator is purely hover/focus-driven
                       (no click). bits-ui Trigger spreads handler props but
                       doesn't make a <div> focusable on its own, so without
                       this keyboard-only users couldn't reach the warning
                       detail. Not promoted to <button> because a button
                       implies a click action that doesn't exist. -->
                  <div
                    {...props}
                    tabindex="0"
                    class="text-warning mt-1 cursor-default text-xs"
                    data-testid="agent-parse-warnings"
                  >
                    ⚠ {warnings.length} transcript warning{warnings.length === 1 ? "" : "s"}
                  </div>
                {/snippet}
                <ul class="max-w-md space-y-1 text-[13px]" data-testid="agent-parse-warnings-list">
                  {#each visible as warning (warning.line_number + ":" + warning.reason)}
                    <li class="flex gap-2" data-testid="agent-parse-warnings-row">
                      <span class="text-primary-fg/70 shrink-0 font-mono"
                        >line {warning.line_number}:</span
                      >
                      <span>{warning.reason}</span>
                    </li>
                  {/each}
                  {#if extra > 0}
                    <li
                      class="text-primary-fg/70 pt-0.5 text-xs"
                      data-testid="agent-parse-warnings-overflow"
                    >
                      + {extra} more
                    </li>
                  {/if}
                </ul>
              </Tooltip>
            {/if}
            {#if runtime?.meta}
              <div class="text-muted mt-1.5 space-y-0.5 text-xs leading-4" data-testid="agent-meta">
                <div class="truncate" title={runtime.meta.model}>
                  model: <span class="font-mono">{runtime.meta.model}</span>
                </div>
                {#if runtime.meta.mcp_servers.length > 0}
                  <div>mcp: {runtime.meta.mcp_servers.length}</div>
                {/if}
                {#if runtime.meta.skills.length > 0}
                  <div>skills: {runtime.meta.skills.length}</div>
                {/if}
              </div>
            {/if}
            {#if agent.harness === "claude_code" && cost > 0}
              <div class="text-fg mt-1.5 text-xs" data-testid="agent-cost">
                ${cost.toFixed(4)}
              </div>
            {/if}
            {#if rlView !== null}
              <!-- Claude rate-limit surface — two independent signals, each
                   shown only while its own reset is still in the future (a past
                   "resets at" would be wrong, so it clean-hides instead). The
                   primary window (neutral) is emitted on every turn, regardless
                   of overage; the overage escalation (amber `warning` token) is
                   layered on top only when billing to credits. One always-present
                   tooltip carries full dates (a window can be days out) and the
                   snapshot age when rehydrated. Survives restart via the M3
                   metadata sidecar. -->
              <Tooltip side="right">
                {#snippet trigger(props)}
                  <!-- tabindex=0 so keyboard users can open the tooltip; a <div>
                       (no click action) isn't focusable on its own. Mirrors the
                       parse-warnings indicator. -->
                  <div
                    {...props}
                    tabindex="0"
                    class="mt-1.5 cursor-default space-y-0.5 text-xs"
                    data-testid="agent-rate-limit-claude"
                  >
                    {#if rlView.window !== null}
                      <div class="text-fg" data-testid="agent-rate-window">
                        {rlView.window.label} resets {formatResetTime(rlView.window.resetsAtMs)}
                      </div>
                    {/if}
                    {#if rlView.overage !== null}
                      <!-- -ml-1 offsets the ⚡ glyph's left-side bearing so it
                           aligns with the text column above (the emoji box
                           carries a few px of transparent left padding). -->
                      <div class="text-warning -ml-1" data-testid="agent-overage">
                        ⚡ using credits
                      </div>
                    {/if}
                  </div>
                {/snippet}
                <div class="max-w-xs space-y-1 text-[13px]" data-testid="agent-rate-detail">
                  {#if rlView.window !== null}
                    <p>
                      {rlView.window.label} resets {formatResetDateTime(rlView.window.resetsAtMs)}
                    </p>
                  {/if}
                  {#if rlView.overage !== null}
                    <p>
                      Spending usage credits{rlView.overage.resetsAtMs !== null
                        ? ` — overage window resets ${formatResetDateTime(rlView.overage.resetsAtMs)}`
                        : "."}
                    </p>
                  {/if}
                  {#if overageAsOf != null}
                    <p class="text-primary-fg/70" data-testid="agent-rate-snapshot">
                      Snapshot from {relativeTime(overageAsOf)} — send a message to refresh.
                    </p>
                  {/if}
                </div>
              </Tooltip>
            {/if}
            {#if codexWindows.length > 0}
              <!-- Codex rate-limit: one neutral gauge line per independent
                   window (primary ~5-hour + secondary weekly), with full reset
                   dates in the tooltip (the weekly window is days out, beyond a
                   bare clock). Session-file-backed (class B, durable) — no
                   snapshot-age qualifier, unlike Claude's stream-only payload. -->
              <Tooltip side="right">
                {#snippet trigger(props)}
                  <div
                    {...props}
                    tabindex="0"
                    class="text-fg mt-1.5 cursor-default space-y-0.5 text-xs"
                    data-testid="agent-rate-limit"
                  >
                    {#each codexWindows as w, i (i)}
                      <div>{w.label} used: {w.usedPercent.toFixed(0)}%</div>
                    {/each}
                  </div>
                {/snippet}
                <div class="max-w-xs space-y-1 text-[13px]" data-testid="agent-rate-limit-detail">
                  {#each codexWindows as w, i (i)}
                    <p>
                      {w.label}: {w.usedPercent.toFixed(0)}% used{w.resetsAtMs !== null
                        ? ` · resets ${formatResetDateTime(w.resetsAtMs)}`
                        : ""}
                    </p>
                  {/each}
                </div>
              </Tooltip>
            {/if}
            <!-- Clean-hide convention: every metadata cell above and below is
                 presence-gated, so a value a harness never reports simply never
                 renders — no blank label, no empty bar, no "—" placeholder. These
                 absences are correct, not gaps: Gemini exposes no `context_window`
                 (the bar below never renders for it), and Antigravity reports no
                 cost / quota / context at all. A transient absence (a fresh agent
                 pre-first-turn) hides identically to a permanent one; that's the
                 intended behavior, not a case to distinguish. -->
            {#if util !== undefined}
              <div class="mt-1.5" data-testid="agent-context-bar">
                <div class="text-muted mb-0.5 text-[11px]">
                  context after last turn: {(util * 100).toFixed(0)}%
                </div>
                <div class="bg-border/80 h-1 w-full overflow-hidden rounded">
                  <div
                    class="bg-fg h-full"
                    style:width="{Math.min(util * 100, 100).toFixed(1)}%"
                  ></div>
                </div>
              </div>
            {/if}
          {/if}
        </div>
      {/each}
    </div>
  </SidebarSection>
</SidebarPanel>
