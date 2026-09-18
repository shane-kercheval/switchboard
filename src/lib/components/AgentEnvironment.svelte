<script lang="ts">
  /// Compact agent-card entry for the harness environment. The narrow card
  /// carries only the actionable status; the complete inventory opens in a
  /// bounded popover so long lists never change card height.
  import { ChevronRight } from "@lucide/svelte";
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import ExpandCollapseIcon from "$lib/components/ui/ExpandCollapseIcon.svelte";
  import Popover from "$lib/components/ui/Popover.svelte";
  import StatusDot from "$lib/components/ui/StatusDot.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { environmentView } from "$lib/agentEnvironment";
  import type { SessionInventory } from "$lib/types";

  type Props = {
    inventory: SessionInventory | undefined;
    asOf?: string | null;
  };

  let { inventory, asOf }: Props = $props();

  const view = $derived(environmentView(inventory));
  let openLists = $state<Record<string, boolean>>({});

  function formatAsOf(iso: string): string {
    return new Date(iso).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  const SECTION_LABEL = "text-muted text-[10px] font-medium tracking-wide uppercase";
  const ROW = "text-muted flex min-w-0 items-baseline gap-1.5 text-[11px]";
</script>

{#if view !== null}
  <div class="mt-2" data-testid="agent-meta">
    <Popover
      side="left"
      align="start"
      triggerClass="text-muted hover:bg-hover hover:text-fg focus-visible:ring-focus flex w-full min-w-0 items-center gap-2 rounded-md px-1.5 py-1 text-left text-[11px] transition-colors focus-visible:ring-1 focus-visible:outline-none"
      triggerLabel={`Environment details${view.attentionSummary === null ? "" : `, ${view.attentionSummary}`}`}
      triggerTestid="agent-env-toggle"
      contentTestid="agent-env-detail"
    >
      {#snippet trigger()}
        <span class="text-fg font-medium">Environment</span>
        <span
          class={view.attentionSummary === null
            ? "text-muted ml-auto min-w-0 truncate"
            : "text-warning ml-auto min-w-0 truncate"}
          data-testid="agent-env-summary"
        >
          {view.attentionSummary ?? "View details"}
        </span>
        <ChevronRight size={12} strokeWidth={1.8} class="shrink-0" aria-hidden="true" />
      {/snippet}

      <div class="border-border border-b pb-2.5">
        <div class="flex items-baseline gap-3">
          <h3 class="text-fg text-sm font-medium">Environment</h3>
          {#if asOf != null}
            <span class="text-muted ml-auto text-[11px]" data-testid="agent-env-as-of">
              as of {formatAsOf(asOf)}
            </span>
          {/if}
        </div>
        <p class="text-muted mt-1 text-[11px] leading-4" data-testid="agent-env-inventory-summary">
          {view.summary}
        </p>
      </div>

      <div class="mt-3 space-y-3">
        {#if view.servers !== null}
          <section data-testid="agent-env-mcp">
            <h4 class={SECTION_LABEL}>MCP servers · {view.servers.length}</h4>
            <div class="mt-1 space-y-1">
              {#each view.servers as server, i (i)}
                <div class={ROW}>
                  {#if server.tone !== undefined}
                    <StatusDot
                      status={server.tone}
                      label={server.statusLabel === undefined ? server.status : undefined}
                      focusable={false}
                      class="translate-y-[-1px]"
                      testid="agent-env-mcp-dot"
                    />
                  {/if}
                  <span class="text-fg min-w-0 truncate">{server.name}</span>
                  {#if server.statusLabel !== undefined}
                    <span class="text-warning shrink-0">{server.statusLabel}</span>
                  {/if}
                  {#if server.source !== undefined}
                    <span class="ml-auto shrink-0 opacity-70">{server.source}</span>
                  {/if}
                </div>
              {/each}
            </div>
          </section>
        {/if}

        {#if view.plugins !== null}
          <section data-testid="agent-env-plugins">
            <h4 class={SECTION_LABEL}>Plugins · {view.plugins.length}</h4>
            <div class="mt-1 space-y-0.5">
              {#each view.plugins as plugin, i (i)}
                <p class="text-muted text-[11px]">
                  <span class="text-fg">{plugin.name}</span>{plugin.version === undefined
                    ? ""
                    : ` @ ${plugin.version}`}
                </p>
              {/each}
            </div>
          </section>
        {/if}

        {#if view.memory !== null}
          <section data-testid="agent-env-memory">
            <h4 class={SECTION_LABEL}>Memory · {view.memory.length}</h4>
            <div class="mt-1 space-y-0.5">
              {#each view.memory as entry, i (i)}
                <Tooltip
                  label={entry.path}
                  delayDuration={SUPPLEMENTAL_TOOLTIP_DELAY}
                  focusable={false}
                >
                  {#snippet trigger(props)}
                    <span
                      {...props}
                      class="text-muted block cursor-default truncate text-[11px]"
                      data-testid="agent-env-memory-entry">{entry.label}</span
                    >
                  {/snippet}
                </Tooltip>
              {/each}
            </div>
          </section>
        {/if}

        {#if view.skills !== null}
          {@const open = openLists.skills ?? false}
          <section data-testid="agent-env-skills">
            <button
              type="button"
              onclick={() => (openLists.skills = !open)}
              class="text-muted hover:text-fg flex w-full items-center gap-1.5 text-left"
              aria-expanded={open}
              data-testid="agent-env-skills-toggle"
            >
              <span class={SECTION_LABEL}>Skills · {view.skills.length}</span>
              <ExpandCollapseIcon expanded={open} size={11} strokeWidth={1.8} class="ml-auto" />
            </button>
            {#if open}
              <div class="mt-1 space-y-1.5">
                {#each view.skills as skill, i (i)}
                  <div class="text-muted text-[11px] leading-4">
                    <div class="text-fg">{skill.name}</div>
                    {#if skill.description !== undefined}
                      <div>{skill.description}</div>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          </section>
        {/if}

        {#each view.lists as list (list.key)}
          {@const open = openLists[list.key] ?? false}
          <section data-testid="agent-env-list-{list.key}">
            <button
              type="button"
              onclick={() => (openLists[list.key] = !open)}
              class="text-muted hover:text-fg flex w-full items-center gap-1.5 text-left"
              aria-expanded={open}
              data-testid="agent-env-list-toggle-{list.key}"
            >
              <span class={SECTION_LABEL}>{list.label} · {list.items.length}</span>
              <ExpandCollapseIcon expanded={open} size={11} strokeWidth={1.8} class="ml-auto" />
            </button>
            {#if open}
              <div class="mt-1 space-y-1">
                {#each list.items as item, i (i)}
                  <p class="text-muted font-mono text-[11px] break-all">{item}</p>
                {/each}
              </div>
            {/if}
          </section>
        {/each}

        {#if view.agents !== null}
          {@const open = openLists.agents ?? false}
          <section data-testid="agent-env-agents">
            <button
              type="button"
              onclick={() => (openLists.agents = !open)}
              class="text-muted hover:text-fg flex w-full items-center gap-1.5 text-left"
              aria-expanded={open}
              data-testid="agent-env-agents-toggle"
            >
              <span class={SECTION_LABEL}>Custom agents · {view.agents.length}</span>
              <ExpandCollapseIcon expanded={open} size={11} strokeWidth={1.8} class="ml-auto" />
            </button>
            {#if open}
              <div class="mt-1 space-y-0.5">
                {#each view.agents as agent, i (i)}
                  <p class="text-muted text-[11px]">{agent}</p>
                {/each}
              </div>
            {/if}
          </section>
        {/if}

        {#if view.settings !== null}
          <section data-testid="agent-env-settings">
            <h4 class={SECTION_LABEL}>Settings</h4>
            <dl class="mt-1 space-y-1 text-[11px]">
              {#each view.settings as setting, i (i)}
                <div class="grid grid-cols-[auto_1fr] gap-3">
                  <dt class="text-muted">{setting.label}</dt>
                  <dd class="text-fg min-w-0 text-right break-words">{setting.value}</dd>
                </div>
              {/each}
            </dl>
          </section>
        {/if}
      </div>
    </Popover>
  </div>
{/if}
