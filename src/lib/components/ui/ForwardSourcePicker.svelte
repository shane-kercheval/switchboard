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
  import DropdownMenuSub from "$lib/components/ui/DropdownMenuSub.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import { shortcut } from "$lib/platform";

  /// The parts of cross-project sourcing every picker shares: what to browse,
  /// how to read a roster, how to open and validate. Consumers spread this into
  /// their own [`CrossProjectConfig`] rather than passing it directly.
  export type CrossProjectBase = {
    /// No `directory`: it existed only to locate the roster read, which now
    /// resolves from the project id alone.
    projects: { id: ProjectId; name: string }[];
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
    /// Optional: classify what each agent would contribute. An `empty` agent is
    /// annotated *and disabled* — it has nothing to forward, so picking it could
    /// only end in the backend refusing the send. A `pending` agent stays
    /// pickable: it is still generating, and the send waits for it.
    ///
    /// Foreign agents (under `Projects`) are never classified — their transcripts
    /// aren't loaded here, so their rows carry no readiness at all rather than a
    /// guess. See `sourceReadinessFor`.
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
  /// Bound so a *successful* pick can dismiss the menu. A foreign-agent row
  /// carries `closeOnSelect={false}` because its pick is async and may fail into
  /// an inline error; dismissal is this flag, set once the pick has committed.
  let menuOpen = $state(false);
  $effect(() => {
    // A closed menu keeps no failure state.
    if (!menuOpen) pickErrors.clear();
  });
  const rosters = new SvelteMap<ProjectId, RosterState>();
  /// Per-agent pick failure (a locked or unreadable project), rendered at the row
  /// the user clicked — the error's home is the pick, not the hover.
  const pickErrors = new SvelteMap<string, string>();

  /// Read a project's roster when its submenu opens. bits-ui mounts sub-content
  /// on demand, so this is the moment the rows first need data — and browsing a
  /// project the user never opens costs nothing.
  ///
  /// The cache survives a close, so reopening doesn't re-read the registry; an
  /// *errored* entry does re-read, which makes reopening the submenu the retry
  /// path and stops a transient failure from making a project permanently
  /// unpickable.
  async function loadRoster(projectId: ProjectId): Promise<void> {
    if (!crossProject) return;
    const existing = rosters.get(projectId);
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
    {@const readiness = agentReadiness?.(agent.id)}
    {@const rowDisabled = disabled || readiness === "empty"}
    <!-- An `empty` agent is unpickable, not merely annotated: picking it can only
         end in the backend refusing the whole send, so the row states the fact
         ("no output") and the menu declines the choice rather than explaining a
         consequence the user then has to avoid. `pending` stays pickable — the
         send waits for that agent's in-flight turn. -->
    <DropdownMenuItem
      onSelect={() => onPickAgent(agent)}
      class="gap-2"
      data-testid={`forward-picker-agent-${agent.id}`}
      disabled={rowDisabled}
    >
      <HarnessIcon harness={agent.harness} size="sm" class="h-4 w-4 shrink-0" />
      <span class={rowDisabled ? "text-muted/50" : "text-fg"}>{agent.name}</span>
      {#if readiness === "empty"}
        <span class="text-muted ml-auto text-[11px] italic">no output</span>
      {:else if readiness === "pending"}
        <span class="text-muted ml-auto text-[11px] italic">still generating</span>
      {/if}
    </DropdownMenuItem>
  {/each}
  {#if crossProject && crossProject.projects.length > 0}
    <!-- Cross-project sources, behind one `Projects` row rather than listed
         inline. Nested submenus, not in-place expansion: the project list is
         unbounded, and expanding it inside the parent menu pushes the local
         agents the user came for off the bottom of a scrolling popover. Each
         level is its own bits-ui menu, so arrow keys, typeahead, and escape scope
         to the level you are in.

         Opening a project's submenu only *reads* its roster; the project is
         opened and validated when the user actually picks an agent. -->
    <DropdownMenuSub
      side="left"
      class="border-border mt-1 border-t"
      contentTestid="forward-picker-projects-menu"
      data-testid="forward-picker-projects-trigger"
      {disabled}
    >
      {#snippet trigger()}
        <span>Projects</span>
      {/snippet}
      {#each crossProject.projects as project (project.id)}
        {@const roster = rosters.get(project.id)}
        <DropdownMenuSub
          side="left"
          contentTestid={`forward-picker-project-menu-${project.id}`}
          data-testid={`forward-picker-project-toggle-${project.id}`}
          onOpenChange={(open) => {
            if (open) void loadRoster(project.id);
            // Pick failures are scoped to the expansion that produced them, so a
            // stale message can't reappear on reopen.
            else pickErrors.clear();
          }}
          {disabled}
        >
          {#snippet trigger()}
            <span class="min-w-0 truncate">{project.name}</span>
          {/snippet}
          {#if roster === undefined || roster.status === "loading"}
            <p class="text-muted px-2.5 py-1.5 text-[11px] italic">loading…</p>
          {:else if roster.status === "error"}
            <p
              class="text-muted max-w-64 px-2.5 py-1.5 text-[11px] italic"
              data-testid={`forward-picker-project-error-${project.id}`}
            >
              can't read this project — {roster.message} (reopen to retry)
            </p>
          {:else}
            {#if roster.agents.length === 0}
              <p class="text-muted px-2.5 py-1.5 text-[11px] italic">no agents</p>
            {/if}
            {#each roster.agents as agent (agent.id)}
              <DropdownMenuItem
                onSelect={() => void pickForeign(agent, project)}
                closeOnSelect={false}
                class="gap-2"
                data-testid={`forward-picker-foreign-agent-${agent.id}`}
                {disabled}
              >
                <HarnessIcon harness={agent.harness} size="sm" class="h-4 w-4 shrink-0" />
                <span class="text-fg">{agent.name}</span>
              </DropdownMenuItem>
              {#if pickErrors.has(agent.id)}
                <p
                  class="text-status-failed max-w-64 px-2.5 pb-1 text-[11px]"
                  data-testid={`forward-picker-pick-error-${agent.id}`}
                >
                  {pickErrors.get(agent.id)}
                </p>
              {/if}
            {/each}
          {/if}
        </DropdownMenuSub>
      {/each}
    </DropdownMenuSub>
  {/if}
</DropdownMenu>
