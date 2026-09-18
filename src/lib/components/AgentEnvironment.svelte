<script lang="ts">
  /// The agent card's Environment row: what the harness has loaded, and
  /// whether it is usable.
  ///
  /// Replaces the two count chips that used to sit here (an MCP count and a
  /// skill count, each a number in a tooltip). Those told the user how many
  /// but never *which*, and never that two of the seven servers needed auth —
  /// the one fact about the list that cannot wait.
  ///
  /// **Disclosure, never omission.** Collapsed, the row is one line of counts
  /// with the needs-attention count called out. Expanded, every list is
  /// reachable: the short ones inline, the three-digit ones (tools, commands,
  /// the approved-command allowlist) behind their own count line that expands
  /// in place. Nothing is withheld for being long.
  import { SUPPLEMENTAL_TOOLTIP_DELAY } from "$lib/components/ui/tooltip";
  import ExpandCollapseIcon from "$lib/components/ui/ExpandCollapseIcon.svelte";
  import StatusDot from "$lib/components/ui/StatusDot.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import { environmentView } from "$lib/agentEnvironment";
  import type { SessionInventory } from "$lib/types";

  type Props = {
    inventory: SessionInventory | undefined;
    /// Capture time of a rehydrated inventory (ISO-8601). When set, the list
    /// is what the agent's last turn loaded rather than what it has now, and
    /// the row says so — the convention the rate-limit snapshot set: a stale
    /// reading is fine as long as it admits to being one.
    asOf?: string | null;
  };

  let { inventory, asOf }: Props = $props();

  const view = $derived(environmentView(inventory));

  let expanded = $state(false);
  /// Which count-line lists are open. Ephemeral, like the card's other
  /// disclosure — a sidebar that remembered every expansion across reloads
  /// would reopen a 109-row list on project open.
  let openLists = $state<Record<string, boolean>>({});

  function formatAsOf(iso: string): string {
    return new Date(iso).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  const SECTION_LABEL = "text-muted text-[10px] tracking-wide uppercase";
  const ROW = "text-muted flex items-baseline gap-1.5 text-[11px]";
</script>

{#if view !== null}
  <div class="mt-1.5" data-testid="agent-meta">
    <button
      type="button"
      onclick={() => (expanded = !expanded)}
      class="text-muted hover:text-fg flex w-full items-start gap-1.5 text-left text-[11px]"
      aria-expanded={expanded}
      data-testid="agent-env-toggle"
    >
      <!-- Wraps rather than truncates. A busy account's counts line needs
           ~367px against the ~224px a default-width card gives it (measured
           in WebKit: 127px clipped), and truncating dropped the Skills and
           Memory counts entirely — which is exactly the "manage clutter with
           disclosure, never by dropping it" rule this row exists to follow.
           Two lines of 11px text is the cheaper price. -->
      <span class="min-w-0" data-testid="agent-env-summary">{view.summary}</span>
      <ExpandCollapseIcon {expanded} size={12} strokeWidth={1.8} class="mt-0.5 ml-auto shrink-0" />
    </button>

    {#if expanded}
      <div class="mt-1 space-y-1.5" data-testid="agent-env-detail">
        {#if asOf != null}
          <p class="text-muted text-[10px] italic" data-testid="agent-env-as-of">
            as of {formatAsOf(asOf)}
          </p>
        {/if}

        <!-- The harness-supplied lists below are keyed by index, never by
             name. They are replaced wholesale on every event and hold no
             per-row state, so a name key buys no reconciliation — and it
             imposes a uniqueness the sources cannot honor: a recorded Claude
             `init` lists `deep-research` twice, and a user copying a bundled
             skill to customize it is the ordinary way two entries share a
             name. Svelte throws on a duplicate key in production as well as
             dev, with no error boundary here to catch it. -->
        {#if view.servers !== null}
          <div data-testid="agent-env-mcp">
            <p class={SECTION_LABEL}>MCP servers</p>
            {#each view.servers as server, i (i)}
              <div class={ROW}>
                {#if server.tone !== undefined}
                  <!-- The dot is the sole signal only when the status is
                       plain `connected`, and then its accessible name is that
                       status — the sibling text carries the server's name,
                       not its health. Otherwise the raw status renders beside
                       it as visible text and the dot is decorative. -->
                  <StatusDot
                    status={server.tone}
                    label={server.statusLabel === undefined ? server.status : undefined}
                    class="translate-y-[-1px]"
                    testid="agent-env-mcp-dot"
                  />
                {/if}
                <span class="min-w-0 truncate">{server.name}</span>
                {#if server.statusLabel !== undefined}
                  <span class="text-warning shrink-0">{server.statusLabel}</span>
                {/if}
                {#if server.source !== undefined}
                  <span class="ml-auto shrink-0 opacity-70">{server.source}</span>
                {/if}
              </div>
            {/each}
          </div>
        {/if}

        {#if view.agents !== null}
          <div data-testid="agent-env-agents">
            <p class={SECTION_LABEL}>Custom agents</p>
            <p class="text-muted text-[11px]">{view.agents.join(", ")}</p>
          </div>
        {/if}

        {#if view.plugins !== null}
          <div data-testid="agent-env-plugins">
            <p class={SECTION_LABEL}>Plugins</p>
            {#each view.plugins as plugin, i (i)}
              <p class="text-muted text-[11px]">
                {plugin.name}{plugin.version === undefined ? "" : ` @ ${plugin.version}`}
              </p>
            {/each}
          </div>
        {/if}

        {#if view.memory !== null}
          <div data-testid="agent-env-memory">
            <p class={SECTION_LABEL}>Memory</p>
            {#each view.memory as entry, i (i)}
              <!-- The basename alone is ambiguous across scopes, so the full
                   path is on hover rather than wrapped onto the card. -->
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
        {/if}

        {#if view.skills !== null}
          {@const open = openLists.skills ?? false}
          <div data-testid="agent-env-skills">
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
              {#each view.skills as skill, i (i)}
                <div class="text-muted text-[11px]">
                  <span>{skill.name}</span>
                  {#if skill.description !== undefined}
                    <span class="opacity-70"> — {skill.description}</span>
                  {/if}
                </div>
              {/each}
            {/if}
          </div>
        {/if}

        {#each view.lists as list (list.key)}
          {@const open = openLists[list.key] ?? false}
          <div data-testid="agent-env-list-{list.key}">
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
              <p class="text-muted text-[11px] break-words">{list.items.join(", ")}</p>
            {/if}
          </div>
        {/each}

        {#if view.settings !== null}
          <div data-testid="agent-env-settings">
            <p class={SECTION_LABEL}>Settings</p>
            <p class="text-muted text-[11px]">
              {view.settings.map((s) => `${s.label}: ${s.value}`).join(" · ")}
            </p>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}
