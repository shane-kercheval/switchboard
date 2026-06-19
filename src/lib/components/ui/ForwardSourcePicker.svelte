<script lang="ts">
  // The ↪ "forward from" picker — a dropdown of the project's agents and panes,
  // shared by the compose bar (its ↪ button) and the prompt composer (per
  // argument). Picking an agent forwards that agent's latest output; picking a
  // pane forwards each member's. The consumer owns what "pick" does (add a source
  // chip to the compose set, or to one argument), so this component is purely the
  // menu — open/position/keyboard/click-outside come from `DropdownMenu`.
  import type { AgentRecord, AgentId } from "$lib/types";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import { shortcut } from "$lib/platform";

  let {
    agents,
    panes,
    onPickAgent,
    onPickPane,
    disabled = false,
    agentHasOutput,
    triggerClass,
    triggerTestid = "forward-picker-trigger",
    triggerLabel = "Forward output from an agent or pane",
    triggerText,
    tooltipLabel,
    showPaneShortcuts = false,
  }: {
    agents: AgentRecord[];
    panes: TranscriptPane[];
    onPickAgent: (agent: AgentRecord) => void;
    onPickPane: (pane: TranscriptPane) => void;
    disabled?: boolean;
    /// Optional: flag agents with no completed output yet ("no output") so the
    /// user sees there's nothing to forward before picking.
    agentHasOutput?: (id: AgentId) => boolean;
    triggerClass?: string;
    triggerTestid?: string;
    triggerLabel?: string;
    /// Optional visible label after the ↪ glyph (e.g. "Forward" in the compose
    /// bar). Omitted for the per-argument icon-only trigger.
    triggerText?: string;
    tooltipLabel?: string;
    /// Show the `⌘⌃N` pane-forward chord on each pane row. Compose-bar only — the
    /// prompt composer's per-argument pickers have no such shortcut, so it stays
    /// off there (the index matches the pane's position in `panes`, mirroring the
    /// compose bar's handler).
    showPaneShortcuts?: boolean;
  } = $props();

  // Panes are only meaningful targets once the user has actually split (≥2): with
  // the single default pane, "forward from {that pane}" == "forward from every
  // agent", which the agent rows already cover.
  const multiPane = $derived(panes.filter((p) => p.members.length > 0).length > 1);

  function paneMemberNames(pane: TranscriptPane): string {
    return agents
      .filter((a) => pane.members.includes(a.id))
      .map((a) => a.name)
      .join(", ");
  }
</script>

<DropdownMenu
  {triggerClass}
  {triggerTestid}
  {triggerLabel}
  {tooltipLabel}
  contentTestid="forward-picker-menu"
  align="start"
>
  {#snippet trigger()}
    <!-- ↪ glyph, matching the forward source chips. -->
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
      <polyline points="15 17 20 12 15 7" />
      <path d="M4 18v-2a4 4 0 0 1 4-4h12" />
    </svg>
    {#if triggerText}{triggerText}{/if}
  {/snippet}

  {#if multiPane}
    {#each panes as pane, index (pane.id)}
      {#if pane.members.length > 0}
        <DropdownMenuItem
          onSelect={() => onPickPane(pane)}
          class="gap-2"
          data-testid={`forward-picker-pane-${pane.id}`}
          {disabled}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="text-accent h-4 w-4 shrink-0"
            aria-hidden="true"
          >
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <path d="M12 4v16" />
          </svg>
          <span class="text-fg shrink-0">{pane.name}</span>
          <span class="text-muted min-w-0 truncate text-[11px]">{paneMemberNames(pane)}</span>
          {#if showPaneShortcuts && index < 9}
            <span class="text-muted ml-auto shrink-0 pl-2 font-mono text-[11px]"
              >{shortcut("mod", "ctrl", String(index + 1))}</span
            >
          {/if}
        </DropdownMenuItem>
      {/if}
    {/each}
  {/if}
  {#each agents as agent (agent.id)}
    <DropdownMenuItem
      onSelect={() => onPickAgent(agent)}
      class="gap-2"
      data-testid={`forward-picker-agent-${agent.id}`}
      {disabled}
    >
      <HarnessIcon harness={agent.harness} size="sm" class="h-4 w-4 shrink-0" />
      <span class="text-fg">{agent.name}</span>
      {#if agentHasOutput && !agentHasOutput(agent.id)}
        <span class="text-muted ml-auto text-[11px] italic">no output</span>
      {/if}
    </DropdownMenuItem>
  {/each}
</DropdownMenu>
