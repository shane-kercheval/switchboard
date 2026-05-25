<script lang="ts">
  import {
    activateProject,
    agentsByProject,
    projects,
    selection,
    workspace,
  } from "$lib/state/workspace.svelte";
  import { runtimes } from "$lib/state/index.svelte";
  import { basename, cn, relativeTime } from "$lib/utils";
  import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
  import SidebarSection from "$lib/components/ui/SidebarSection.svelte";
  import DropdownMenu from "$lib/components/ui/DropdownMenu.svelte";
  import DropdownMenuItem from "$lib/components/ui/DropdownMenuItem.svelte";
  import StatusDot from "$lib/components/ui/StatusDot.svelte";
  import Badge from "$lib/components/ui/Badge.svelte";
  import ThemeToggle from "$lib/components/ui/ThemeToggle.svelte";

  /// The sidebar is a flat list of projects — folders are never surfaced as a
  /// managed object. The single "+" opens a menu to create a new project or add
  /// an existing one; both delegate to App (which owns the folder dialog + the
  /// new-project modal). Project removal/archive are deferred to M8.
  let { onNewProject, onAddExisting }: { onNewProject: () => void; onAddExisting: () => void } =
    $props();

  /// Whether any agent in a project is mid-turn — drives the "background
  /// activity" dot so the user knows a non-displayed project is doing
  /// something. A project whose roster hasn't loaded yet contributes nothing.
  function isBusy(projectId: string): boolean {
    const roster = agentsByProject[projectId] ?? [];
    return roster.some((a) => {
      const status = runtimes[a.id]?.run_status;
      return status === "starting" || status === "processing";
    });
  }
</script>

<SidebarPanel side="left" width="w-72" testid="projects-sidebar">
  {#if !workspace.persistable}
    <div
      class="border-warning/30 bg-warning-soft text-warning border-b px-3 py-2 text-xs"
      data-testid="not-persistable-banner"
    >
      Couldn't read your saved workspace — your project list won't be saved this session.
    </div>
  {/if}

  <SidebarSection title="Projects">
    {#snippet action()}
      <DropdownMenu
        triggerTestid="add-project"
        triggerLabel="Add a project"
        triggerClass="rounded px-1.5 py-0.5 text-sm font-bold text-fg hover:bg-raised"
        contentTestid="add-project-menu"
      >
        {#snippet trigger()}
          +
        {/snippet}
        <DropdownMenuItem onSelect={onNewProject} data-testid="menu-new-project">
          New project
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={onAddExisting} data-testid="menu-add-existing">
          Add existing project
        </DropdownMenuItem>
      </DropdownMenu>
    {/snippet}

    {#if projects.list.length === 0}
      <p class="text-muted px-4 py-3 text-xs">No projects yet.</p>
    {/if}
    <div class="flex flex-col gap-0.5 p-2">
      {#each projects.list as project (project.id)}
        <button
          type="button"
          class={cn(
            "hover:bg-raised/60 flex w-full flex-col items-start gap-0.5 rounded-md px-3 py-2 text-left transition-colors",
            project.id === selection.activeProjectId && "bg-raised hover:bg-raised shadow-sm",
          )}
          data-testid="project-row"
          data-project-id={project.id}
          data-active={project.id === selection.activeProjectId}
          onclick={() => activateProject(project.id)}
        >
          <div class="flex w-full items-center gap-2">
            <span class="text-fg truncate font-mono text-sm font-semibold">
              {project.name}
            </span>
            {#if isBusy(project.id)}
              <StatusDot status="processing" label="working…" testid="project-busy" />
            {/if}
            {#if !project.available}
              <Badge class="ml-auto" testid="project-unavailable">unavailable</Badge>
            {/if}
          </div>
          <div class="text-muted flex w-full items-center gap-1 text-[11px]">
            <span class="truncate" title={project.directory}>{basename(project.directory)}</span>
            <span>·</span>
            <span class="shrink-0">{relativeTime(project.last_activity)}</span>
          </div>
        </button>
      {/each}
    </div>
  </SidebarSection>

  <div class="border-border flex shrink-0 items-center justify-end border-t px-2 py-1.5">
    <ThemeToggle />
  </div>
</SidebarPanel>
