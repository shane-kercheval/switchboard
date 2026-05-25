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

  /// The sidebar is a flat list of projects — folders are never surfaced as a
  /// managed object. The single "+" opens a menu to create a new project or add
  /// an existing one; both delegate to App (which owns the folder dialog + the
  /// new-project modal). Project removal/archive are deferred to M8.
  let { onNewProject, onAddExisting }: { onNewProject: () => void; onAddExisting: () => void } =
    $props();

  let menuOpen = $state<boolean>(false);

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

  function choose(action: () => void): void {
    menuOpen = false;
    action();
  }
</script>

<aside
  class="flex w-72 flex-col border-r border-neutral-200 bg-neutral-50"
  data-testid="projects-sidebar"
>
  {#if !workspace.persistable}
    <div
      class="border-b border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800"
      data-testid="not-persistable-banner"
    >
      Couldn't read your saved workspace — your project list won't be saved this session.
    </div>
  {/if}

  <div
    class="relative flex items-center justify-between border-b border-neutral-200 px-4 py-3 text-xs font-semibold tracking-wide text-neutral-500 uppercase"
  >
    <span>Projects</span>
    <button
      type="button"
      class="rounded px-1.5 py-0.5 text-sm font-bold text-neutral-700 hover:bg-neutral-200"
      title="Add a project"
      aria-label="Add a project"
      data-testid="add-project"
      onclick={() => (menuOpen = !menuOpen)}
    >
      +
    </button>
    {#if menuOpen}
      <div
        class="absolute top-full right-2 z-10 mt-1 w-44 overflow-hidden rounded-md border border-neutral-200 bg-white text-sm font-normal normal-case shadow-lg"
        data-testid="add-project-menu"
      >
        <button
          type="button"
          class="block w-full px-3 py-2 text-left text-neutral-800 hover:bg-neutral-100"
          data-testid="menu-new-project"
          onclick={() => choose(onNewProject)}
        >
          New project
        </button>
        <button
          type="button"
          class="block w-full px-3 py-2 text-left text-neutral-800 hover:bg-neutral-100"
          data-testid="menu-add-existing"
          onclick={() => choose(onAddExisting)}
        >
          Add existing project
        </button>
      </div>
    {/if}
  </div>

  <div class="flex-1 overflow-y-auto">
    {#if projects.list.length === 0}
      <p class="px-4 py-3 text-xs text-neutral-500">No projects yet.</p>
    {/if}
    {#each projects.list as project (project.id)}
      <button
        type="button"
        class={cn(
          "flex w-full flex-col items-start gap-0.5 border-b border-neutral-200 px-4 py-2 text-left hover:bg-neutral-100",
          project.id === selection.activeProjectId && "bg-neutral-200 hover:bg-neutral-200",
        )}
        data-testid="project-row"
        data-project-id={project.id}
        data-active={project.id === selection.activeProjectId}
        onclick={() => activateProject(project.id)}
      >
        <div class="flex w-full items-center gap-2">
          <span class="truncate font-mono text-sm font-semibold text-neutral-900">
            {project.name}
          </span>
          {#if isBusy(project.id)}
            <span
              class="h-1.5 w-1.5 shrink-0 rounded-full bg-amber-500"
              title="working…"
              data-testid="project-busy"
            ></span>
          {/if}
          {#if !project.available}
            <span
              class="ml-auto shrink-0 rounded bg-neutral-200 px-1 py-0.5 text-[9px] text-neutral-600 uppercase"
              data-testid="project-unavailable"
            >
              unavailable
            </span>
          {/if}
        </div>
        <div class="flex w-full items-center gap-1 text-[11px] text-neutral-500">
          <span class="truncate" title={project.directory}>{basename(project.directory)}</span>
          <span>·</span>
          <span class="shrink-0">{relativeTime(project.last_activity)}</span>
        </div>
      </button>
    {/each}
  </div>
</aside>
