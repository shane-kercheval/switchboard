<script lang="ts">
  // The ↪ "forward from" picker — a dropdown of the project's agents and panes,
  // shared by the compose bar (its ↪ button) and the prompt composer (per
  // argument). Picking an agent forwards that agent's latest output; picking a
  // pane forwards each member's. The consumer owns what "pick" does (add a source
  // chip to the compose set, or to one argument), so this component is purely the
  // menu — open/position/keyboard/click-outside come from `DropdownMenu`.
  import type { AgentRecord, AgentId, ProjectId } from "$lib/types";
  import { SvelteMap } from "svelte/reactivity";
  import type { ForwardReadiness } from "$lib/state/heldForwards.svelte";
  import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import { shortcut } from "$lib/platform";

  /// The parts of cross-project sourcing every picker shares: what to browse,
  /// how to read a roster, how to open and validate. Consumers spread this into
  /// their own [`CrossProjectConfig`] rather than passing it directly.
  export type CrossProjectBase = {
    /// `directory` is the owner locator: the roster read targets exactly this
    /// directory rather than scanning, so an unrelated corrupt one can't interfere.
    projects: { id: ProjectId; name: string; directory: string }[];
    /// Read a project's roster for **display**. Side-effect-free — no load, no lock.
    loadAgents: (projectId: ProjectId) => Promise<AgentRecord[]>;
    /// Open and validate the project. Shared so validation isn't reimplemented
    /// per consumer; rejects when the project can't be activated.
    activate: (projectId: ProjectId) => Promise<void>;
  };

  /// Cross-project sourcing as a picker receives it: the shared base **plus a
  /// required commit step**.
  ///
  /// `onPickForeign` is required by the *type*, not by a comment. Where a picked
  /// source lands differs per consumer, and a shared commit closure is precisely
  /// how prompt-field and workflow-field picks once landed in the compose bar's
  /// plain-message list instead of the field — visible in workflow mode only
  /// after leaving it, and persisted. Making it optional would leave the same
  /// silent-nothing failure one prop narrower: rows render, `activate` takes the
  /// project's lock, the menu closes, no source is added. A missing commit step
  /// must be a compile error.
  export type CrossProjectConfig = CrossProjectBase & {
    /// Commit a foreign pick into **this consumer's** target (the compose set,
    /// one prompt argument, one workflow field). Called only after `activate`
    /// resolves.
    onPickForeign: (agent: AgentRecord, project: { id: ProjectId; name: string }) => void;
  };

  let {
    agents,
    panes,
    onPickAgent,
    onPickPane,
    disabled = false,
    agentReadiness,
    triggerClass,
    triggerTestid = "forward-picker-trigger",
    triggerLabel = "Forward output from an agent or pane",
    triggerText,
    tooltipLabel,
    tooltipDisableHoverableContent = true,
    showPaneShortcuts = false,
    crossProject,
  }: {
    agents: AgentRecord[];
    panes: TranscriptPane[];
    onPickAgent: (agent: AgentRecord) => void;
    onPickPane: (pane: TranscriptPane) => void;
    disabled?: boolean;
    /// Optional: classify what each agent would contribute, so the user sees
    /// before picking that an empty source would block the send. Only `empty` is flagged —
    /// a `pending` agent is still generating and will be waited for.
    agentReadiness?: (id: AgentId) => ForwardReadiness;
    triggerClass?: string;
    triggerTestid?: string;
    triggerLabel?: string;
    /// Optional visible label after the ↪ glyph (e.g. "Forward" in the compose
    /// bar). Omitted for the per-argument icon-only trigger.
    triggerText?: string;
    tooltipLabel?: string;
    tooltipDisableHoverableContent?: boolean;
    /// Show the `⌘⌃N` pane-forward chord on each pane row. Compose-bar only — the
    /// prompt composer's per-argument pickers have no such shortcut, so it stays
    /// off there (the index matches the pane's position in `panes`, mirroring the
    /// compose bar's handler).
    showPaneShortcuts?: boolean;
    /// Cross-project sourcing, shown under a `Projects` section. **One object,
    /// not three optional props** — the parts are all-or-nothing, and as separate
    /// optionals a consumer that passed the list and the loader but forgot the
    /// pick handler would render a menu whose agent rows silently do nothing.
    /// Omit entirely to hide the section (the picker then behaves as before).
    crossProject?: CrossProjectConfig;
  } = $props();

  /// Per-project roster state for the Projects section. Browsing is read-only, so
  /// a failure here is a row-level condition (an unpickable project with a
  /// reason), never a thrown error from a hover.
  type RosterState =
    | { status: "loading" }
    | { status: "ready"; agents: AgentRecord[] }
    | { status: "error"; message: string };
  /// Bound so a *successful* pick can dismiss the menu. The rows carry
  /// `closeOnSelect={false}` so expanding a project keeps the menu open; that
  /// same flag would otherwise leave it open after a completed pick too.
  let menuOpen = $state(false);
  $effect(() => {
    // A closed menu keeps no failure state — see the collapse path above.
    if (!menuOpen) pickErrors.clear();
  });
  /// The one expanded project, or `null`. **Separate from `rosters`** — using
  /// "do I have roster data?" as "is this expanded?" made a loaded project
  /// impossible to collapse and reported the wrong `aria-expanded` for a project
  /// showing its error row. One at a time, matching the accordion behavior; the
  /// cache survives collapse so re-expanding doesn't re-read the registry.
  let expandedProjectId = $state<ProjectId | null>(null);
  const rosters = new SvelteMap<ProjectId, RosterState>();
  /// Per-agent pick failure (a locked or unreadable project), rendered at the row
  /// the user clicked — the error's home is the pick, not the hover.
  const pickErrors = new SvelteMap<string, string>();

  async function toggleProject(projectId: ProjectId): Promise<void> {
    if (!crossProject) return;
    const existing = rosters.get(projectId);
    // Selecting an errored project **retries** rather than collapsing — the row
    // says "select again to retry", and collapsing there would make that copy a
    // lie and put the retry two clicks away.
    if (expandedProjectId === projectId && existing?.status !== "error") {
      expandedProjectId = null;
      // Pick failures are scoped to the expansion that produced them. Clearing on
      // collapse *and* on close (below) keeps one rule; clearing on close alone
      // would leave a stale message reappearing on collapse/re-expand.
      pickErrors.clear();
      return;
    }
    expandedProjectId = projectId;
    // Cached roster: show it, no re-read. An *errored* entry re-reads, so a
    // transient failure doesn't make the project permanently unpickable.
    if (existing !== undefined && existing.status !== "error") return;
    rosters.set(projectId, { status: "loading" });
    try {
      rosters.set(projectId, { status: "ready", agents: await crossProject.loadAgents(projectId) });
    } catch (e) {
      rosters.set(projectId, { status: "error", message: errorText(e) });
    }
  }

  async function pickForeign(
    agent: AgentRecord,
    project: { id: ProjectId; name: string },
  ): Promise<void> {
    if (!crossProject) return;
    pickErrors.delete(agent.id);
    try {
      await crossProject.activate(project.id);
    } catch (e) {
      pickErrors.set(agent.id, errorText(e));
      return;
    }
    // Only after the project is proven usable does the source land anywhere.
    crossProject.onPickForeign(agent, project);
    menuOpen = false;
  }

  /// Tauri rejects a structured command error with a **plain object**, so
  /// `e instanceof Error` is false and `String(e)` yields `[object Object]`.
  /// Unwrap the tagged-variant shape (`{ variant: { message } }`) before falling
  /// back, so the user reads the real cause.
  function errorText(e: unknown): string {
    if (e instanceof Error) return e.message;
    if (typeof e === "object" && e !== null) {
      const direct = (e as { message?: unknown }).message;
      if (typeof direct === "string") return direct;
      const first = Object.values(e as Record<string, unknown>)[0];
      if (typeof first === "object" && first !== null) {
        const nested = (first as { message?: unknown }).message;
        if (typeof nested === "string") return nested;
      }
    }
    return String(e);
  }

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
  bind:open={menuOpen}
  {triggerClass}
  {triggerTestid}
  {triggerLabel}
  {tooltipLabel}
  {tooltipDisableHoverableContent}
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
      {#if agentReadiness?.(agent.id) === "empty"}
        <span class="text-muted ml-auto text-[11px] italic">no output — blocks the send</span>
      {:else if agentReadiness?.(agent.id) === "pending"}
        <span class="text-muted ml-auto text-[11px] italic">still generating</span>
      {/if}
    </DropdownMenuItem>
  {/each}
  {#if crossProject && crossProject.projects.length > 0}
    <!-- Cross-project sources. Expanding a project only *reads* its roster; the
         project is opened and validated when the user actually picks an agent.
         Every row is a `DropdownMenuItem`, including the expand toggle: bits-ui's
         roving focus and typeahead track registered menu items only, so a raw
         `<button>` here would be unreachable by keyboard and carry no disclosure
         semantics — the whole section would be mouse-only. `closeOnSelect={false}`
         is what lets a menu item expand in place instead of dismissing. -->
    <div
      class="text-muted border-border mt-1 border-t px-2 pt-2 pb-1 text-[11px] font-medium"
      data-testid="forward-picker-projects-heading"
    >
      Projects
    </div>
    {#each crossProject.projects as project (project.id)}
      {@const roster = rosters.get(project.id)}
      <div data-testid={`forward-picker-project-${project.id}`}>
        <DropdownMenuItem
          onSelect={() => void toggleProject(project.id)}
          closeOnSelect={false}
          class="gap-2"
          data-testid={`forward-picker-project-toggle-${project.id}`}
          aria-expanded={expandedProjectId === project.id}
          {disabled}
        >
          <span class="min-w-0 truncate">{project.name}</span>
          {#if expandedProjectId === project.id && roster?.status === "loading"}
            <span class="text-muted ml-auto text-[11px] italic">loading…</span>
          {/if}
        </DropdownMenuItem>
        {#if expandedProjectId === project.id && roster?.status === "error"}
          <p
            class="text-muted px-2 pb-1 pl-6 text-[11px] italic"
            data-testid={`forward-picker-project-error-${project.id}`}
          >
            can't read this project — {roster.message} (select again to retry)
          </p>
        {:else if expandedProjectId === project.id && roster?.status === "ready"}
          {#if roster.agents.length === 0}
            <p class="text-muted px-2 pb-1 pl-6 text-[11px] italic">no agents</p>
          {/if}
          {#each roster.agents as agent (agent.id)}
            <DropdownMenuItem
              onSelect={() => void pickForeign(agent, project)}
              closeOnSelect={false}
              class="gap-2 pl-6"
              data-testid={`forward-picker-foreign-agent-${agent.id}`}
              {disabled}
            >
              <HarnessIcon harness={agent.harness} size="sm" class="h-4 w-4 shrink-0" />
              <span class="text-fg">{agent.name}</span>
            </DropdownMenuItem>
            {#if pickErrors.has(agent.id)}
              <p
                class="text-status-failed px-2 pb-1 pl-6 text-[11px]"
                data-testid={`forward-picker-pick-error-${agent.id}`}
              >
                {pickErrors.get(agent.id)}
              </p>
            {/if}
          {/each}
        {/if}
      </div>
    {/each}
  {/if}
</DropdownMenu>
