<script lang="ts">
  import type { AgentRecord, AgentId } from "$lib/types";
  import { retryAgentHydration, runtimes, stopAgent, transcripts } from "$lib/state/index.svelte";
  import { removeAgent, renameAgent } from "$lib/state/workspace.svelte";
  import {
    agentSessionInfo,
    openSessionFile as apiOpenSessionFile,
    type AgentSessionInfo,
  } from "$lib/api";
  import { normalizeAgentName, validateAgentName, type NameValidation } from "$lib/agentName";
  import { cn, relativeTime } from "$lib/utils";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import PlusIcon from "$lib/components/ui/PlusIcon.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import Dialog from "$lib/components/ui/Dialog.svelte";
  import ErrorDetailsDialog from "$lib/components/ui/ErrorDetailsDialog.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
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

  const agentActionClass =
    "text-muted hover:bg-raised hover:text-fg focus-visible:ring-accent focus-visible:bg-raised focus-visible:text-fg inline-flex h-[26px] w-[26px] items-center justify-center rounded-full transition-colors focus-visible:ring-2 focus-visible:outline-none";
  const agentDangerActionClass =
    "border-transparent text-muted hover:border-status-failed/60 hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent focus-visible:border-status-failed/60 focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-[26px] w-[26px] items-center justify-center rounded-full border-[0.5px] transition-colors focus-visible:ring-2 focus-visible:outline-none";
  const agentStopActionClass =
    "border-muted/40 text-muted hover:border-status-failed/60 hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent focus-visible:border-status-failed/60 focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-[26px] w-[26px] items-center justify-center rounded-full border-[0.5px] transition-colors focus-visible:ring-2 focus-visible:outline-none";
  const AGENT_ACTION_DELAY_MS = 500;

  /// `onAddAgent` is the "+ Add agent" entry point in the sidebar header.
  /// Optional so existing callers + tests that don't pass it continue
  /// rendering; when absent, the button isn't shown.
  let { agents, onAddAgent }: { agents: AgentRecord[]; onAddAgent?: () => void } = $props();

  let sessionInfoByAgent = $state<Record<AgentId, AgentSessionInfo | null>>({});
  let sessionInfoStarted = $state<Record<AgentId, boolean>>({});
  let sessionInfoInFlight = $state<Record<AgentId, boolean>>({});
  let sessionInfoError = $state<{ agentId: AgentId; message: string } | null>(null);
  let resumeAgentId = $state<AgentId | null>(null);
  let resumeOpen = $state(false);
  /// Verbatim hydration-error dialog (per-agent "history failed to load"). The
  /// failure lives on `runtime.hydration_error`; this just tracks which agent's
  /// error is currently shown.
  let hydrationDetailsOpen = $state(false);
  let hydrationDetailsName = $state("");
  let hydrationDetailsError = $state("");
  let removeConfirmAgentId = $state<AgentId | null>(null);
  let removingAgentId = $state<AgentId | null>(null);
  let removeError = $state<{ agentId: AgentId; message: string } | null>(null);

  const resumeAgent = $derived(
    resumeAgentId === null ? null : (agents.find((agent) => agent.id === resumeAgentId) ?? null),
  );
  const resumeInfo = $derived(
    resumeAgentId === null ? null : (sessionInfoByAgent[resumeAgentId] ?? null),
  );

  function hasSessionActions(info: AgentSessionInfo | null | undefined): boolean {
    return Boolean(info?.session_file || info?.resume_command);
  }

  function refreshAgentSessionInfo(agentId: AgentId, force = false): void {
    if (sessionInfoInFlight[agentId] === true) return;
    if (!force && sessionInfoStarted[agentId] === true) return;
    if (force && hasSessionActions(sessionInfoByAgent[agentId])) return;

    sessionInfoStarted[agentId] = true;
    sessionInfoInFlight[agentId] = true;
    void agentSessionInfo(agentId)
      .then((info) => {
        sessionInfoByAgent[agentId] = info;
        if (sessionInfoError?.agentId === agentId) sessionInfoError = null;
      })
      .catch((err: unknown) => {
        sessionInfoByAgent[agentId] = null;
        sessionInfoError = {
          agentId,
          message: err instanceof Error ? err.message : String(err),
        };
      })
      .finally(() => {
        sessionInfoInFlight[agentId] = false;
      });
  }

  function agentActionWidth(count: number): string {
    const visibleCount = Math.max(count, 1);
    const iconWidthRem = 1.625;
    const gapRem = 0.125;
    const width = visibleCount * iconWidthRem + Math.max(visibleCount - 1, 0) * gapRem;
    return `${Math.max(width, 2)}rem`;
  }

  $effect(() => {
    const ids = new Set(agents.map((agent) => agent.id));
    for (const agent of agents) {
      refreshAgentSessionInfo(agent.id);
    }

    for (const id of Object.keys(sessionInfoByAgent)) {
      if (!ids.has(id)) delete sessionInfoByAgent[id];
    }
    for (const id of Object.keys(sessionInfoStarted)) {
      if (!ids.has(id)) delete sessionInfoStarted[id];
    }
    for (const id of Object.keys(sessionInfoInFlight)) {
      if (!ids.has(id)) delete sessionInfoInFlight[id];
    }
    if (removeConfirmAgentId !== null && !ids.has(removeConfirmAgentId))
      removeConfirmAgentId = null;
    if (resumeAgentId !== null && !ids.has(resumeAgentId)) {
      resumeAgentId = null;
      resumeOpen = false;
    }
  });

  function openSessionFile(agent: AgentRecord): void {
    if (!sessionInfoByAgent[agent.id]?.session_file) return;
    void apiOpenSessionFile(agent.id).catch((err: unknown) => {
      console.error("[switchboard] open session file failed", err);
    });
  }

  function startRemove(agent: AgentRecord): void {
    removeError = null;
    removeConfirmAgentId = agent.id;
  }

  function cancelRemove(agentId: AgentId): void {
    if (removeConfirmAgentId === agentId) removeConfirmAgentId = null;
  }

  function agentRowPointerActions(node: HTMLElement, agentId: AgentId): { destroy: () => void } {
    const handlePointerEnter = (): void => refreshAgentSessionInfo(agentId, true);
    const handlePointerLeave = (): void => cancelRemove(agentId);
    node.addEventListener("pointerenter", handlePointerEnter);
    node.addEventListener("pointerleave", handlePointerLeave);
    return {
      destroy: () => {
        node.removeEventListener("pointerenter", handlePointerEnter);
        node.removeEventListener("pointerleave", handlePointerLeave);
      },
    };
  }

  async function confirmRemove(agent: AgentRecord): Promise<void> {
    removingAgentId = agent.id;
    removeError = null;
    try {
      await removeAgent(agent.id);
      if (removeConfirmAgentId === agent.id) removeConfirmAgentId = null;
    } catch (err) {
      removeConfirmAgentId = null;
      removeError = {
        agentId: agent.id,
        message: err instanceof Error ? err.message : String(err),
      };
    } finally {
      removingAgentId = null;
    }
  }

  /// Context utilization — `(context_input_tokens + output_tokens) /
  /// context_window` from the most recent completed agent turn. Forward-looking
  /// signal ("how full will the next turn's context be").
  ///
  /// `context_input_tokens` is the harness-reconciled input-side occupancy
  /// (see `TurnUsage`): for Claude it sums the disjoint cache fields (cached +
  /// cache-creation, which `input_tokens` alone excludes — the cause of the
  /// near-0% bug), for Codex it is `input_tokens` alone (its cached count is a
  /// subset, so adding it would double-count). Consuming the pre-reconciled
  /// value keeps this formula harness-agnostic — do not re-add per-harness
  /// token summation here. Both it and `context_window` must be present;
  /// otherwise the bar is hidden.
  function contextUtilization(agentId: AgentId): number | undefined {
    const turns = transcripts[agentId] ?? [];
    for (let i = turns.length - 1; i >= 0; i--) {
      const turn = turns[i];
      if (turn?.role !== "agent" || turn.usage === undefined) continue;
      const window = turn.usage.context_window;
      if (window === undefined || window === null || window === 0) continue;
      const contextInput = turn.usage.context_input_tokens;
      if (contextInput === undefined || contextInput === null) continue;
      const outputTokens = turn.usage.output_tokens ?? 0;
      return (contextInput + outputTokens) / window;
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

  /// Inline rename editor. Only one card edits at a time, so a single
  /// `editingAgentId` + `draftName` suffices; `renameError` holds a backend
  /// rejection (the live format/uniqueness check is `renameValidation`, the
  /// frontend mirror of the backend rules — the backend stays authoritative).
  let editingAgentId = $state<AgentId | null>(null);
  let draftName = $state<string>("");
  let renaming = $state<boolean>(false);
  let renameError = $state<string | null>(null);

  /// Validate the draft against the live roster, excluding the agent being
  /// edited so re-saving its own (or a case/hyphen-variant) name isn't a false
  /// duplicate. `renameMessage` suppresses the `empty` reason so an emptied
  /// field disables save without nagging mid-edit (mirrors the create form).
  const renameValidation = $derived<NameValidation>(
    editingAgentId === null ? { ok: true } : validateAgentName(draftName, agents, editingAgentId),
  );
  const renameMessage = $derived(
    renameValidation.ok || renameValidation.reason === "empty" ? null : renameValidation.message,
  );
  const canSave = $derived(renameValidation.ok && !renaming);

  function startEdit(agent: AgentRecord): void {
    editingAgentId = agent.id;
    draftName = agent.name;
    renameError = null;
  }

  function cancelEdit(): void {
    editingAgentId = null;
    renameError = null;
  }

  /// Commit the draft. An unchanged verbatim name skips the round-trip (a no-op
  /// rename just exits edit mode). On success the roster updates and we leave
  /// edit mode; on a backend rejection we stay in edit mode and surface it.
  async function commitEdit(agent: AgentRecord): Promise<void> {
    // Same gate the save button uses (`!canSave`), so the Enter path can't
    // double-submit while a rename is already in flight. Preserves the
    // unchanged-name skip: validation ok + not renaming → proceeds → the
    // `next === agent.name` branch exits without a round-trip.
    if (!canSave) return;
    const next = normalizeAgentName(draftName);
    if (next === agent.name) {
      cancelEdit();
      return;
    }
    renaming = true;
    renameError = null;
    try {
      await renameAgent(agent.id, next);
      editingAgentId = null;
    } catch (err) {
      renameError = err instanceof Error ? err.message : String(err);
    } finally {
      renaming = false;
    }
  }

  function onRenameKeydown(event: KeyboardEvent, agent: AgentRecord): void {
    if (event.key === "Enter") {
      event.preventDefault();
      void commitEdit(agent);
    } else if (event.key === "Escape") {
      event.preventDefault();
      cancelEdit();
    }
  }

  /// Focus + select the edit field once it mounts. Deferred a frame so the
  /// input is mounted and ready before selection.
  function focusSelect(node: HTMLInputElement): void {
    requestAnimationFrame(() => {
      node.focus();
      node.select();
    });
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
              class="h-4 w-4"
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
        {@const active = isActive(agent.id)}
        {@const sessionInfo = sessionInfoByAgent[agent.id]}
        {@const confirmingRemove = removeConfirmAgentId === agent.id}
        {@const actionCount = confirmingRemove
          ? 2
          : (active ? 1 : 0) +
            (sessionInfo?.resume_command ? 1 : 0) +
            (sessionInfo?.session_file ? 1 : 0) +
            (!active ? 1 : 0)}
        <div
          class="group bg-raised/90 hover:bg-border/40 rounded-md px-2.5 py-2 transition-colors"
          data-testid="sidebar-agent"
          data-agent-id={agent.id}
          use:agentRowPointerActions={agent.id}
        >
          <div class="flex items-center justify-between gap-2">
            {#if editingAgentId === agent.id}
              <!-- Edit mode swaps the whole left side: an <input> can't nest in
                   the collapse-toggle <button>, and the harness icon becomes a
                   save (check) button. Blur cancels (never persist on blur); the
                   save button's mousedown-preventDefault keeps focus so its click
                   commits before blur-cancel can fire. -->
              <input
                use:focusSelect
                bind:value={draftName}
                class={cn(
                  "text-fg border-border bg-panel h-6 min-w-0 flex-1 rounded border px-1.5 text-[13px] font-semibold",
                  "focus-visible:ring-accent focus-visible:ring-1 focus-visible:outline-none",
                  renameMessage && "border-status-failed",
                )}
                aria-label="Agent name"
                aria-invalid={!renameValidation.ok}
                aria-describedby={renameError ? `agent-rename-error-${agent.id}` : undefined}
                title={renameMessage ?? undefined}
                data-testid="agent-rename-input"
                onkeydown={(event) => onRenameKeydown(event, agent)}
                onblur={cancelEdit}
              />
              <button
                type="button"
                class={cn(
                  ICON_BUTTON_CLASS,
                  "shrink-0 disabled:cursor-not-allowed disabled:opacity-50",
                )}
                disabled={!canSave}
                aria-label="Save name"
                title="Save"
                data-testid="agent-rename-save"
                onmousedown={(event) => event.preventDefault()}
                onclick={() => void commitEdit(agent)}
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  class="h-4 w-4"
                  aria-hidden="true"
                >
                  <path d="M20 6 9 17l-5-5" />
                </svg>
              </button>
            {:else}
              <!-- Double-click the name row to rename. Single-click still
                   toggles collapse; a double-click toggles it twice (net no
                   change) before edit mode opens. -->
              <button
                type="button"
                class="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                aria-expanded={!isCollapsed}
                onclick={() => toggleCollapsed(agent.id)}
                ondblclick={() => startEdit(agent)}
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
              <div
                class="relative flex h-8 w-8 shrink-0 items-center justify-end transition-[width] group-hover:w-[var(--agent-action-width)]"
                style={`--agent-action-width: ${agentActionWidth(actionCount)}`}
              >
                <div
                  class="absolute top-1/2 right-0 -translate-y-1/2 transition-opacity group-hover:opacity-0"
                >
                  <HarnessIcon harness={agent.harness} size="md" testid="agent-harness-icon" />
                </div>
                <div
                  class="pointer-events-none absolute top-1/2 right-0 flex max-w-0 -translate-y-1/2 items-center gap-0.5 overflow-hidden opacity-0 transition-[max-width,opacity] group-hover:pointer-events-auto group-hover:max-w-[var(--agent-action-width)] group-hover:opacity-100"
                  data-testid="agent-inline-actions"
                  style={`--agent-action-width: ${agentActionWidth(actionCount)}`}
                >
                  {#if confirmingRemove}
                    <Tooltip label="Cancel delete" delayDuration={AGENT_ACTION_DELAY_MS}>
                      {#snippet trigger(props)}
                        <button
                          {...props}
                          type="button"
                          class={agentActionClass}
                          aria-label="Cancel delete"
                          tabindex="-1"
                          data-testid="agent-remove-cancel"
                          onclick={() => cancelRemove(agent.id)}
                        >
                          <svg
                            viewBox="0 0 24 24"
                            fill="none"
                            stroke="currentColor"
                            stroke-width="2"
                            stroke-linecap="round"
                            stroke-linejoin="round"
                            class="h-4 w-4"
                            aria-hidden="true"
                          >
                            <path d="M18 6 6 18M6 6l12 12" />
                          </svg>
                        </button>
                      {/snippet}
                    </Tooltip>
                    <Tooltip label="Confirm delete" delayDuration={AGENT_ACTION_DELAY_MS}>
                      {#snippet trigger(props)}
                        <button
                          {...props}
                          type="button"
                          class={agentDangerActionClass}
                          disabled={removingAgentId === agent.id}
                          aria-label="Confirm delete"
                          tabindex="-1"
                          data-testid="agent-remove-confirm"
                          onclick={() => void confirmRemove(agent)}
                        >
                          <svg
                            viewBox="0 0 24 24"
                            fill="none"
                            stroke="currentColor"
                            stroke-width="2"
                            stroke-linecap="round"
                            stroke-linejoin="round"
                            class="h-4 w-4"
                            aria-hidden="true"
                          >
                            <path d="M20 6 9 17l-5-5" />
                          </svg>
                        </button>
                      {/snippet}
                    </Tooltip>
                  {:else}
                    {#if active}
                      <Tooltip label="Stop agent" delayDuration={AGENT_ACTION_DELAY_MS}>
                        {#snippet trigger(props)}
                          <button
                            {...props}
                            type="button"
                            class={agentStopActionClass}
                            aria-label="Stop agent"
                            tabindex="-1"
                            data-testid="agent-action-stop"
                            onclick={() => stopAgent(agent.id)}
                          >
                            <StopIcon class="h-5 w-5" />
                          </button>
                        {/snippet}
                      </Tooltip>
                    {/if}
                    {#if sessionInfo?.resume_command}
                      <Tooltip label="Resume in terminal" delayDuration={AGENT_ACTION_DELAY_MS}>
                        {#snippet trigger(props)}
                          <button
                            {...props}
                            type="button"
                            class={agentActionClass}
                            aria-label="Resume in terminal"
                            tabindex="-1"
                            data-testid="agent-action-resume"
                            onclick={() => {
                              resumeAgentId = agent.id;
                              resumeOpen = true;
                            }}
                          >
                            <svg
                              viewBox="0 0 24 24"
                              fill="none"
                              stroke="currentColor"
                              stroke-width="1.8"
                              stroke-linecap="round"
                              stroke-linejoin="round"
                              class="h-4 w-4"
                              aria-hidden="true"
                            >
                              <path d="M4 17 10 11 4 5" />
                              <path d="M12 19h8" />
                            </svg>
                          </button>
                        {/snippet}
                      </Tooltip>
                    {/if}
                    {#if sessionInfo?.session_file}
                      <Tooltip label="Open session file" delayDuration={AGENT_ACTION_DELAY_MS}>
                        {#snippet trigger(props)}
                          <button
                            {...props}
                            type="button"
                            class={agentActionClass}
                            aria-label="Open session file"
                            tabindex="-1"
                            data-testid="agent-action-open-session"
                            onclick={() => openSessionFile(agent)}
                          >
                            <svg
                              viewBox="0 0 24 24"
                              fill="none"
                              stroke="currentColor"
                              stroke-width="1.8"
                              stroke-linecap="round"
                              stroke-linejoin="round"
                              class="h-4 w-4"
                              aria-hidden="true"
                            >
                              <path
                                d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
                              />
                              <path d="M14 2v6h6" />
                              <path d="M8 13h8M8 17h5" />
                            </svg>
                          </button>
                        {/snippet}
                      </Tooltip>
                    {/if}
                    {#if !active}
                      <Tooltip delayDuration={AGENT_ACTION_DELAY_MS}>
                        {#snippet trigger(props)}
                          <button
                            {...props}
                            type="button"
                            class={agentDangerActionClass}
                            aria-label="Delete agent"
                            tabindex="-1"
                            data-testid="agent-action-remove"
                            onclick={() => startRemove(agent)}
                          >
                            <svg
                              viewBox="0 0 24 24"
                              fill="none"
                              stroke="currentColor"
                              stroke-width="1.8"
                              stroke-linecap="round"
                              stroke-linejoin="round"
                              class="h-4 w-4"
                              aria-hidden="true"
                            >
                              <path d="M3 6h18" />
                              <path d="M8 6V4h8v2" />
                              <path d="M19 6 18 20H6L5 6" />
                              <path d="M10 11v5M14 11v5" />
                            </svg>
                          </button>
                        {/snippet}
                        <div class="max-w-56">
                          <div class="text-[13px] font-medium">Delete agent</div>
                          <div class="text-primary-fg/75 mt-1 text-xs leading-4">
                            Deletes Switchboard's files for this agent; underlying session files are
                            kept, and its responses are removed from the conversation.
                          </div>
                        </div>
                      </Tooltip>
                    {/if}
                  {/if}
                </div>
              </div>
            {/if}
          </div>
          {#if sessionInfoError?.agentId === agent.id}
            <div class="text-status-failed mt-1 text-xs" data-testid="agent-actions-error">
              Couldn't read session state: {sessionInfoError.message}
            </div>
          {/if}
          {#if editingAgentId === agent.id && renameError}
            <div
              id={`agent-rename-error-${agent.id}`}
              class="text-status-failed mt-1 text-xs"
              data-testid="agent-rename-error"
            >
              {renameError}
            </div>
          {/if}
          {#if !isCollapsed}
            {#if runtime?.hydration_error}
              <div class="mt-1 space-y-1" data-testid="agent-hydration-error">
                <!-- Clamp the inline reason to two lines: it keeps an
                     at-a-glance "why" without a long path-bearing error (the
                     `LoadTranscriptError::Io` message now names the session
                     file) ballooning the narrow card. The full verbatim text
                     stays available via Details. -->
                <div class="text-status-failed line-clamp-2 text-xs break-words">
                  history failed to load: {runtime.hydration_error}
                </div>
                <div class="flex items-center gap-3 text-xs">
                  <button
                    type="button"
                    class="text-accent hover:underline"
                    data-testid="agent-hydration-retry"
                    onclick={() => void retryAgentHydration(agent.id)}
                  >
                    Retry
                  </button>
                  <button
                    type="button"
                    class="text-muted hover:text-fg hover:underline"
                    data-testid="agent-hydration-details"
                    onclick={() => {
                      hydrationDetailsName = agent.name;
                      hydrationDetailsError = runtime.hydration_error ?? "";
                      hydrationDetailsOpen = true;
                    }}
                  >
                    Details
                  </button>
                </div>
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
                  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
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
            <!-- Per-turn cost is deliberately NOT shown on the card — it
                 renders inline per-message in the transcript (real-spend turns
                 only). There is no per-agent cost total (system-design §2): the
                 old accumulating `$` figure read as a running total but wasn't
                 one. Do not re-add it. The current overage *status* below stays
                 (Bucket-A "as of now" state). -->
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
                  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
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
                  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
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
          {#if removeError?.agentId === agent.id}
            <div class="text-status-failed mt-1 text-xs" data-testid="agent-remove-error">
              Couldn't delete agent: {removeError.message}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </SidebarSection>
</SidebarPanel>

<Dialog
  bind:open={resumeOpen}
  onClose={() => (resumeAgentId = null)}
  title="Resume in terminal"
  contentClass="max-w-lg"
>
  <div class="space-y-3" data-testid="resume-panel">
    <p class="text-muted text-xs">
      Run this in your terminal to resume this session interactively.
    </p>
    <div class="flex items-center gap-2">
      <code
        class="bg-panel text-fg min-w-0 flex-1 overflow-x-auto rounded-md px-2.5 py-2 font-mono text-xs whitespace-pre"
        data-testid="resume-command">{resumeInfo?.resume_command ?? ""}</code
      >
      <CopyButton
        text={resumeInfo?.resume_command ?? ""}
        label="Copy command"
        testid="resume-copy"
        class="shrink-0"
      />
    </div>
    {#if resumeAgent !== null && isActive(resumeAgent.id)}
      <p class="text-status-failed text-xs" data-testid="resume-warning-active">
        ⚠ Switchboard is currently driving this session — stop the agent before running this
        command, or two processes will write one session file and corrupt it.
      </p>
    {:else}
      <p class="text-muted text-xs" data-testid="resume-warning">
        ⚠ Don't run this while the agent is active in Switchboard — two processes writing one
        session file will corrupt it.
      </p>
    {/if}
  </div>
</Dialog>

<ErrorDetailsDialog
  bind:open={hydrationDetailsOpen}
  title={`Couldn't load ${hydrationDetailsName}'s history`}
  message="This agent's history failed to load. The exact error is below — copy it into a bug report."
  details={hydrationDetailsError}
/>
